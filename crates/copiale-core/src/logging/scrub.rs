//! Redacts identifiers from formatted log output.
//!
//! - UUIDs (`8-4-4-4-12` hex) and runs of 32+ hex digits become `…` plus
//!   the last 4 digits, matching `ServiceId`'s `Debug`.
//! - `+` followed by 7–15 digits (E.164) becomes `+…` plus the last 2.
//! - `Debug`-formatted byte lists of 16+ bytes (raw IDs, server responses)
//!   become `[N bytes]`.
//!
//! Bare digit runs are kept: they are mostly timestamps.

use std::io::Write;

use tracing_subscriber::fmt::MakeWriter;

const HEX_RUN_MIN: usize = 32;
const UUID_GROUPS: [usize; 5] = [8, 4, 4, 4, 12];
const PHONE_DIGITS: std::ops::RangeInclusive<usize> = 7..=15;
const BYTE_LIST_MIN: usize = 16;

/// Wraps a [`MakeWriter`] so every write is scrubbed first.
pub(super) struct Scrubbing<W>(pub(super) W);

impl<'a, W: MakeWriter<'a>> MakeWriter<'a> for Scrubbing<W> {
    type Writer = ScrubbingWriter<W::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        ScrubbingWriter(self.0.make_writer())
    }
}

/// Scrubs each write; the fmt layer writes one whole event per call.
pub(super) struct ScrubbingWriter<W>(W);

impl<W: Write> Write for ScrubbingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        self.0.write_all(scrub(&text).as_bytes())?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

pub(crate) fn scrub(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_hexdigit() {
            let end = uuid_end(bytes, i).unwrap_or_else(|| hex_run_end(bytes, i));
            let token = &text[i..end];
            let hex_digits = token.bytes().filter(u8::is_ascii_hexdigit).count();
            if token.contains('-') || hex_digits >= HEX_RUN_MIN {
                out.push('…');
                out.push_str(&token[token.len() - 4..]);
            } else {
                out.push_str(token);
            }
            i = end;
        } else if bytes[i] == b'['
            && let Some((end, count)) = byte_list_end(bytes, i)
        {
            out.push_str(&format!("[{count} bytes]"));
            i = end;
        } else if bytes[i] == b'+'
            && let Some(end) = phone_end(bytes, i)
        {
            out.push_str("+…");
            out.push_str(&text[end - 2..end]);
            i = end;
        } else {
            let len = utf8_len(bytes[i]);
            out.push_str(&text[i..i + len]);
            i += len;
        }
    }
    out
}

/// End of a hyphenated UUID starting at `start`.
fn uuid_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    for (n, len) in UUID_GROUPS.iter().enumerate() {
        if n > 0 {
            if bytes.get(i) != Some(&b'-') {
                return None;
            }
            i += 1;
        }
        let group = bytes.get(i..i + len)?;
        if !group.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        i += len;
    }
    let next_is_word = bytes.get(i).is_some_and(u8::is_ascii_alphanumeric);
    (!next_is_word).then_some(i)
}

/// End of the run of hex digits starting at `start`.
fn hex_run_end(bytes: &[u8], start: usize) -> usize {
    let len = bytes[start..]
        .iter()
        .take_while(|byte| byte.is_ascii_hexdigit())
        .count();
    start + len
}

/// End of an E.164 number starting with the `+` at `start`.
fn phone_end(bytes: &[u8], start: usize) -> Option<usize> {
    let digits = bytes[start + 1..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    let end = start + 1 + digits;
    let next_is_word = bytes.get(end).is_some_and(u8::is_ascii_alphanumeric);
    (PHONE_DIGITS.contains(&digits) && !next_is_word).then_some(end)
}

/// End and length of a list of at least [`BYTE_LIST_MIN`] numbers up to
/// 255 starting with the `[` at `start`, in compact or pretty `Debug` form.
fn byte_list_end(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    let skip_space = |i: usize| {
        i + bytes[i..]
            .iter()
            .take_while(|byte| byte.is_ascii_whitespace())
            .count()
    };
    let mut i = skip_space(start + 1);
    let mut count = 0;
    loop {
        let digits = bytes[i..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if digits == 0 {
            break;
        }
        std::str::from_utf8(&bytes[i..i + digits])
            .ok()?
            .parse::<u8>()
            .ok()?;
        count += 1;
        i = skip_space(i + digits);
        if bytes.get(i) != Some(&b',') {
            break;
        }
        i = skip_space(i + 1);
    }
    (bytes.get(i) == Some(&b']') && count >= BYTE_LIST_MIN).then_some((i + 1, count))
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xf0.. => 4,
        0xe0.. => 3,
        0xc0.. => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_uuids_keeping_last_digits() {
        assert_eq!(
            scrub("Thread(contact=0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0)"),
            "Thread(contact=…e1f0)"
        );
        assert_eq!(
            scrub("PNI:0F1E2D3C-4B5A-6978-8796-A5B4C3D2E1F0"),
            "PNI:…E1F0"
        );
    }

    #[test]
    fn scrubs_protocol_addresses() {
        assert_eq!(
            scrub("session corruption for 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0.2, deleting"),
            "session corruption for …e1f0.2, deleting"
        );
    }

    #[test]
    fn scrubs_long_hex_runs_only() {
        let key = "ab".repeat(32);
        assert_eq!(scrub(&format!("key={key}")), "key=…abab");
        let short = "a".repeat(31);
        assert_eq!(scrub(&format!("id={short}")), format!("id={short}"));
    }

    #[test]
    fn scrubs_phone_numbers() {
        assert_eq!(scrub("from +14155550123: hi"), "from +…23: hi");
        assert_eq!(scrub("+447700900123"), "+…23");
    }

    #[test]
    fn scrubs_byte_lists() {
        let body: Vec<u8> = br#"{"number":"+14155550123"}"#.to_vec();
        assert_eq!(
            scrub(&format!("body: Some({body:?}) }}")),
            "body: Some([25 bytes]) }"
        );
        let id = [7u8; 16];
        assert_eq!(scrub(&format!("{id:#?}")), "[16 bytes]");
    }

    #[test]
    fn keeps_short_and_non_byte_lists() {
        for text in [
            "[1, 2, 3]",
            "[]",
            "[300, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]",
            "headers: [\"Content-Type:application/json\"]",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn keeps_timestamps_and_short_numbers() {
        for text in [
            "ts=1700000000123",
            "count +12",
            "+1234567890123456 too long",
            "deadbeef",
            "2026-10-13T14:00:00Z",
            "v1.2.3-beta",
        ] {
            assert_eq!(scrub(text), text);
        }
    }

    #[test]
    fn scrubs_hex_runs_after_letters_too() {
        let key = "ab".repeat(32);
        assert_eq!(scrub(&format!("x{key}")), "x…abab");
        assert_eq!(scrub("cafebabe"), "cafebabe");
    }

    #[test]
    fn keeps_non_ascii_text() {
        assert_eq!(scrub("café ☕ naïve"), "café ☕ naïve");
    }

    #[test]
    fn keeps_already_redacted_forms() {
        let text = "ServiceId(…e1f0) PhoneNumber(…23) GroupId(…abcd)";
        assert_eq!(scrub(text), text);
    }
}
