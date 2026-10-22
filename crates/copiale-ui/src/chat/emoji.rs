//! Emoji from typed text: emoticons, `:shortcodes:` and shortcode search.

use std::ops::Range;
use std::sync::LazyLock;

/// `name\temoji` per line, in emoji order (`data/README.md`).
const SHORTCODES: &str = include_str!("../../data/emoji_shortcodes.tsv");

/// Emoticons and their emoji; whole words only.
const EMOTICONS: &[(&str, &str)] = &[
    (":)", "🙂"),
    (":-)", "🙂"),
    ("(:", "🙂"),
    ("=)", "🙂"),
    (":D", "😄"),
    (":-D", "😄"),
    ("xD", "😆"),
    ("XD", "😆"),
    (":>", "😆"),
    (";)", "😉"),
    (";-)", "😉"),
    (":(", "😞"),
    (":-(", "😞"),
    ("):", "😞"),
    (":'(", "😢"),
    (">:(", "😠"),
    (">:-(", "😠"),
    (":P", "😛"),
    (":-P", "😛"),
    (":p", "😛"),
    (":-p", "😛"),
    (";P", "😜"),
    (";-P", "😜"),
    (";p", "😜"),
    (";-p", "😜"),
    (":O", "😮"),
    (":-O", "😮"),
    (":o", "😮"),
    (":-o", "😮"),
    (":|", "😐"),
    (":-|", "😐"),
    (":/", "😕"),
    (":-/", "😕"),
    (":\\", "😕"),
    (":*", "😘"),
    (":-*", "😘"),
    ("<3", "❤️"),
    ("</3", "💔"),
];

/// Characters ending a word that may be an emoticon.
const BOUNDARIES: &[char] = &[',', '.', '!', '?'];

/// Fewest letters after `:` before suggesting.
const MIN_QUERY: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcode {
    pub name: &'static str,
    pub emoji: &'static str,
    pub category: &'static str,
}

static TABLE: LazyLock<Vec<Shortcode>> = LazyLock::new(|| {
    SHORTCODES
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\t');
            Some(Shortcode {
                name: fields.next()?,
                emoji: fields.next()?,
                category: fields.next()?,
            })
        })
        .collect()
});

/// Every shortcode, in emoji order; an emoji's aliases follow each other.
pub(super) fn table() -> &'static [Shortcode] {
    &TABLE
}

/// The emoji named `name`, without colons.
pub fn shortcode(name: &str) -> Option<&'static str> {
    TABLE
        .iter()
        .find(|shortcode| shortcode.name == name)
        .map(|shortcode| shortcode.emoji)
}

pub fn emoticon(word: &str) -> Option<&'static str> {
    EMOTICONS
        .iter()
        .find(|(emoticon, _)| *emoticon == word)
        .map(|(_, emoji)| *emoji)
}

/// Emoji whose names start with `query`, or have a `_` part that does;
/// exact and whole-name matches first, one entry per emoji.
pub fn search(query: &str, limit: usize) -> Vec<Shortcode> {
    let rank = |name: &str| {
        if name == query {
            Some(0)
        } else if name.starts_with(query) {
            Some(1)
        } else if name.split('_').skip(1).any(|part| part.starts_with(query)) {
            Some(2)
        } else {
            None
        }
    };
    let mut ranked: Vec<(usize, usize, Shortcode)> = TABLE
        .iter()
        .enumerate()
        .filter_map(|(ix, shortcode)| Some((rank(shortcode.name)?, ix, *shortcode)))
        .collect();
    ranked.sort_by_key(|(rank, ix, _)| (*rank, *ix));
    let mut matches: Vec<Shortcode> = Vec::new();
    for (_, _, shortcode) in ranked {
        if !matches.iter().any(|seen| seen.emoji == shortcode.emoji) {
            matches.push(shortcode);
        }
        if matches.len() == limit {
            break;
        }
    }
    matches
}

/// Replaces `range` of the composer's text with `text`; the cursor goes
/// after it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    fn apply(&self, text: &str) -> String {
        let mut edited = text.to_owned();
        edited.replace_range(self.range.clone(), &self.text);
        edited
    }
}

/// Shortcode suggestions for the `:query` before the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picker {
    /// The `:query` the pick replaces.
    pub range: Range<usize>,
    pub matches: Vec<Shortcode>,
    pub selected: usize,
}

impl Picker {
    pub fn step(&mut self, step: isize) {
        let len = self.matches.len() as isize;
        self.selected = (self.selected as isize + step).rem_euclid(len) as usize;
    }

    /// The edit putting the highlighted emoji in place of the query.
    pub fn pick(&self) -> Edit {
        Edit {
            range: self.range.clone(),
            text: self.matches[self.selected].emoji.to_owned(),
        }
    }
}

/// An emoticon just converted.
#[derive(Clone, Debug)]
struct Converted {
    /// The text a Backspace right after it leaves.
    erased: String,
    /// Where the emoji starts.
    start: usize,
    emoji: &'static str,
    emoticon: String,
}

/// The composer's emoji state between edits.
#[derive(Debug, Default)]
pub struct EmojiTyping {
    /// The text an edit of ours will leave, ignored when it changes to it.
    own: Option<String>,
    converted: Option<Converted>,
    /// An emoticon put back by Backspace, left as typed where it is.
    kept: Option<(usize, String)>,
    picker: Option<Picker>,
}

impl EmojiTyping {
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref()
    }

    pub fn picker_mut(&mut self) -> Option<&mut Picker> {
        self.picker.as_mut()
    }

    pub fn close_picker(&mut self) {
        self.picker = None;
    }

    /// Forgets everything, as when the composer is cleared.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Notes an edit about to be made, so its change isn't taken for typing.
    pub fn applying(&mut self, edit: &Edit, text: &str) {
        self.own = Some(edit.apply(text));
        self.picker = None;
    }

    /// After the text changed: the edit to make, if any. Converts
    /// `:shortcodes:` once closed, emoticons (if `emoticons`) once followed by
    /// a space or punctuation, and puts an emoticon back on a Backspace right
    /// after converting it.
    pub fn changed(&mut self, text: &str, cursor: usize, emoticons: bool) -> Option<Edit> {
        if self.own.as_deref() == Some(text) {
            self.own = None;
            self.picker = picker_at(text, cursor);
            return None;
        }
        self.own = None;
        if let Some((start, emoticon)) = &self.kept
            && !text
                .get(*start..)
                .is_some_and(|rest| rest.starts_with(emoticon.as_str()))
        {
            self.kept = None;
        }
        let edit = match self.converted.take() {
            Some(converted)
                if converted.erased == text
                    && cursor == converted.start + converted.emoji.len() =>
            {
                self.kept = Some((converted.start, converted.emoticon.clone()));
                Some(Edit {
                    range: converted.start..cursor,
                    text: converted.emoticon,
                })
            }
            _ => self.convert(text, cursor, emoticons),
        };
        match &edit {
            Some(edit) => self.applying(edit, text),
            None => self.picker = picker_at(text, cursor),
        }
        edit
    }

    fn convert(&mut self, text: &str, cursor: usize, emoticons: bool) -> Option<Edit> {
        let before = text.get(..cursor)?;
        let last = before.chars().next_back()?;
        if last == ':' {
            return closed_shortcode(text, cursor);
        }
        if !emoticons || !(last.is_whitespace() || BOUNDARIES.contains(&last)) {
            return None;
        }
        let end = cursor - last.len_utf8();
        let (range, emoji) = emoticon_before(text, end)?;
        let emoticon = &text[range.clone()];
        if self
            .kept
            .as_ref()
            .is_some_and(|(start, kept)| *start == range.start && kept == emoticon)
        {
            return None;
        }
        let edit = Edit {
            range: range.start..cursor,
            text: format!("{emoji}{last}"),
        };
        let mut erased = edit.apply(text);
        erased.remove(range.start + emoji.len());
        self.converted = Some(Converted {
            erased,
            start: range.start,
            emoji,
            emoticon: emoticon.to_owned(),
        });
        Some(edit)
    }
}

/// The text to send: an emoticon ending it converted too, as Enter ends the
/// word.
pub fn on_send(text: &str, emoticons: bool) -> String {
    let converted = emoticons
        .then(|| emoticon_before(text, text.len()))
        .flatten();
    match converted {
        Some((range, emoji)) => Edit {
            range,
            text: emoji.into(),
        }
        .apply(text),
        None => text.to_owned(),
    }
}

/// The emoticon word ending at `end`, outside code and links.
fn emoticon_before(text: &str, end: usize) -> Option<(Range<usize>, &'static str)> {
    let before = text.get(..end)?;
    let start = before
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map_or(0, |(ix, ch)| ix + ch.len_utf8());
    let emoji = emoticon(&before[start..])?;
    (!in_code(text, start)).then_some((start..end, emoji))
}

/// A `:name:` ending at `cursor` naming an emoji.
fn closed_shortcode(text: &str, cursor: usize) -> Option<Edit> {
    let name_end = cursor - 1;
    let open = query_start(text, name_end)?;
    let emoji = shortcode(&text[open + 1..name_end])?;
    Some(Edit {
        range: open..cursor,
        text: emoji.into(),
    })
}

/// Suggestions for a `:query` of at least [`MIN_QUERY`] letters before
/// `cursor`.
fn picker_at(text: &str, cursor: usize) -> Option<Picker> {
    let open = query_start(text, cursor)?;
    let query = &text[open + 1..cursor];
    if query.chars().count() < MIN_QUERY {
        return None;
    }
    let matches = search(&query.to_lowercase(), MAX_MATCHES);
    (!matches.is_empty()).then_some(Picker {
        range: open..cursor,
        matches,
        selected: 0,
    })
}

/// Most suggestions shown.
pub const MAX_MATCHES: usize = 8;

/// Where the `:` opening a shortcode name that ends at `end` is: preceded
/// by nothing, space or punctuation, outside code and links.
fn query_start(text: &str, end: usize) -> Option<usize> {
    let before = text.get(..end)?;
    let open = before.rfind(':')?;
    let name = &before[open + 1..];
    if name.is_empty() || !name.chars().all(is_name_char) {
        return None;
    }
    let ahead = before[..open].chars().next_back();
    if ahead.is_some_and(|ch| ch.is_alphanumeric() || ch == ':') {
        return None;
    }
    let word_start = before[..open]
        .rfind(char::is_whitespace)
        .map_or(0, |ix| ix + 1);
    if before[word_start..open].contains("//") || in_code(text, open) {
        return None;
    }
    Some(open)
}

fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '+' | '-')
}

/// Inside a `code` span or block: an odd number of backticks before.
fn in_code(text: &str, at: usize) -> bool {
    text[..at].matches('`').count() % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types `text` one character at a time, applying edits as the view
    /// does; returns the result.
    fn type_text(typing: &mut EmojiTyping, start: &str, text: &str, emoticons: bool) -> String {
        let mut value = start.to_owned();
        for ch in text.chars() {
            value.push(ch);
            settle(typing, &mut value, emoticons);
        }
        value
    }

    /// Reports a change, as the composer does, and applies the edit; that
    /// edit's own change is reported too.
    fn settle(typing: &mut EmojiTyping, value: &mut String, emoticons: bool) {
        if let Some(edit) = typing.changed(value, value.len(), emoticons) {
            *value = edit.apply(value);
            assert_eq!(typing.changed(value, value.len(), emoticons), None);
        }
    }

    fn typed(text: &str) -> String {
        type_text(&mut EmojiTyping::default(), "", text, true)
    }

    #[test]
    fn table_names_every_emoji() {
        assert_eq!(shortcode("heart"), Some("❤️"));
        assert_eq!(shortcode("+1"), Some("👍"));
        assert_eq!(shortcode("thumbsup"), Some("👍"));
        assert_eq!(shortcode("face_holding_back_tears"), Some("🥹"));
        assert_eq!(shortcode("nope"), None);
        assert!(TABLE.len() > 1900);
    }

    #[test]
    fn emoticons_convert_once_the_word_ends() {
        assert_eq!(typed("hello :)"), "hello :)");
        assert_eq!(typed("hello :) "), "hello 🙂 ");
        assert_eq!(typed("ok :D!"), "ok 😄!");
        assert_eq!(typed("<3\n"), "❤️\n");
        assert_eq!(typed(":'( ;) :P "), "😢 😉 😛 ");
    }

    #[test]
    fn emoticons_need_a_word_of_their_own() {
        assert_eq!(typed("a:) "), "a:) ");
        assert_eq!(typed("http://x "), "http://x ");
        assert_eq!(typed("C: drive "), "C: drive ");
        assert_eq!(typed("8) item "), "8) item ");
    }

    #[test]
    fn emoticons_in_code_stay() {
        assert_eq!(typed("`:) ` :) "), "`:) ` 🙂 ");
    }

    #[test]
    fn emoticons_can_be_turned_off() {
        let mut typing = EmojiTyping::default();
        assert_eq!(type_text(&mut typing, "", ":) :heart:", false), ":) ❤️");
    }

    #[test]
    fn backspace_right_after_puts_the_emoticon_back_for_good() {
        let mut typing = EmojiTyping::default();
        let mut value = type_text(&mut typing, "", "hi :) ", true);
        assert_eq!(value, "hi 🙂 ");
        value.pop();
        settle(&mut typing, &mut value, true);
        assert_eq!(value, "hi :)");
        let value = type_text(&mut typing, &value, " there :) ", true);
        assert_eq!(value, "hi :) there 🙂 ");
    }

    #[test]
    fn backspace_later_keeps_the_emoji() {
        let mut typing = EmojiTyping::default();
        let mut value = type_text(&mut typing, "", "hi :) x", true);
        value.pop();
        settle(&mut typing, &mut value, true);
        value.pop();
        settle(&mut typing, &mut value, true);
        assert_eq!(value, "hi 🙂");
    }

    #[test]
    fn shortcodes_convert_when_closed() {
        assert_eq!(typed(":heart:"), "❤️");
        assert_eq!(typed("so :+1: ok"), "so 👍 ok");
        assert_eq!(typed("(:fire:)"), "(🔥)");
        assert_eq!(typed(":nope: "), ":nope: ");
        assert_eq!(typed("12:30:45"), "12:30:45");
        assert_eq!(typed("https://x.org/:fire:"), "https://x.org/:fire:");
        assert_eq!(typed("`:fire:`"), "`:fire:`");
    }

    #[test]
    fn picker_suggests_after_two_letters() {
        let mut typing = EmojiTyping::default();
        type_text(&mut typing, "", "hi :h", true);
        assert_eq!(typing.picker(), None);
        type_text(&mut typing, "hi :h", "e", true);
        let picker = typing.picker().unwrap();
        assert_eq!(picker.range, 3..6);
        assert!(picker.matches.len() <= MAX_MATCHES);

        let value = type_text(&mut typing, "hi :he", "art", true);
        let picker = typing.picker().unwrap();
        assert_eq!(picker.matches[0].name, "heart");
        let edit = picker.pick();
        typing.applying(&edit, &value);
        assert_eq!(edit.apply(&value), "hi ❤️");
        assert_eq!(typing.picker(), None);
    }

    #[test]
    fn picker_steps_around() {
        let mut typing = EmojiTyping::default();
        type_text(&mut typing, "", ":smi", true);
        let picker = typing.picker_mut().unwrap();
        let len = picker.matches.len();
        picker.step(-1);
        assert_eq!(picker.selected, len - 1);
        picker.step(1);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn search_ranks_whole_and_prefix_names_first() {
        let names: Vec<_> = search("joy", 3).iter().map(|s| s.name).collect();
        assert_eq!(names[0], "joy");
        assert!(names.contains(&"joy_cat"));
        // "cat" matches the start of a later word too.
        assert!(search("cat", 50).iter().any(|s| s.name == "smiley_cat"));
        let emoji: Vec<_> = search("thumbs", 8).iter().map(|s| s.emoji).collect();
        let mut unique = emoji.clone();
        unique.dedup();
        assert_eq!(emoji, unique);
    }

    #[test]
    fn sending_converts_a_trailing_emoticon() {
        assert_eq!(on_send("hello :)", true), "hello 🙂");
        assert_eq!(on_send("hello :)", false), "hello :)");
        assert_eq!(on_send("x:)", true), "x:)");
    }
}
