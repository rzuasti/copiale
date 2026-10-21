//! How content the log can't show reads: placeholder and system lines.

use copiale_core::{Attachment, Content, GroupUpdate, ServiceId, Unsupported};

use super::text::{isolate, visible};

const ON_PHONE: &str = "open on your phone";

/// Muted lines standing in for what the log can't show, above any text.
pub fn placeholders(content: &Content) -> Vec<String> {
    match content {
        Content::Text { attachments, .. } => attachment_lines(attachments),
        Content::Sticker { emoji } => {
            let emoji = emoji
                .as_deref()
                .map(|e| format!("{e} "))
                .unwrap_or_default();
            vec![format!("[sticker] {emoji}{ON_PHONE}")]
        }
        Content::Reaction { emoji } => vec![format!("[reaction] {emoji}")],
        Content::Call { video } => {
            let kind = if *video { "video" } else { "voice" };
            vec![format!("[call] {kind} call · answer on your phone")]
        }
        Content::Unsupported(kind) => {
            let label = match kind {
                Unsupported::Contact => "contact",
                Unsupported::Payment => "payment",
                Unsupported::Poll => "poll",
                Unsupported::ViewOnce => "view-once media",
                Unsupported::GiftBadge => "gift badge",
            };
            vec![format!("[{label}] {ON_PHONE}")]
        }
        Content::Deleted => vec!["this message was deleted".into()],
        Content::Undecryptable => vec!["this message couldn't be decrypted".into()],
        Content::GroupChanged(_) | Content::TimerChanged(_) | Content::SafetyNumberChanged => {
            Vec::new()
        }
    }
}

/// One line quoting the message a reply answers; `author` names its
/// author, isolated.
pub fn quote_line(
    content: &Content,
    author: impl Fn(Option<ServiceId>) -> String,
) -> Option<String> {
    let Content::Text {
        quote: Some(quote), ..
    } = content
    else {
        return None;
    };
    let Some(original) = &quote.original else {
        return Some("↳ original message not found".into());
    };
    let text = original
        .text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let quoted = if !text.is_empty() {
        text
    } else {
        match &original.attachment {
            Some(attachment) => attachment_lines(std::slice::from_ref(attachment))
                .first()
                .and_then(|line| line.split(']').next())
                .map(|label| format!("{label}]"))
                .unwrap_or_default(),
            None => "…".into(),
        }
    };
    Some(format!("↳ {}: {quoted}", isolate(&author(quote.author))))
}

/// Attachments by kind, counted, in order of appearance; files one per
/// line with their names, hidden characters spelled out.
fn attachment_lines(attachments: &[Attachment]) -> Vec<String> {
    let mut counts: Vec<((&str, &str), usize)> = Vec::new();
    for label in attachments.iter().filter_map(label) {
        match counts.iter_mut().find(|(counted, _)| *counted == label) {
            Some((_, count)) => *count += 1,
            None => counts.push((label, 1)),
        }
    }
    let mut lines: Vec<_> = counts
        .into_iter()
        .map(|((one, many), count)| match count {
            1 => format!("[{one}] {ON_PHONE}"),
            n => format!("[{n} {many}] {ON_PHONE}"),
        })
        .collect();
    for attachment in attachments {
        if let Attachment::File { name } = attachment {
            match name.as_deref().filter(|name| !name.trim().is_empty()) {
                Some(name) => lines.push(format!("[file] {} · {ON_PHONE}", visible(name))),
                None => lines.push(format!("[file] {ON_PHONE}")),
            }
        }
    }
    lines
}

/// Singular and plural names of a counted kind; `None` for files.
fn label(attachment: &Attachment) -> Option<(&'static str, &'static str)> {
    match attachment {
        Attachment::Image => Some(("image", "images")),
        Attachment::Gif => Some(("GIF", "GIFs")),
        Attachment::Video => Some(("video", "videos")),
        Attachment::Voice => Some(("voice message", "voice messages")),
        Attachment::Audio => Some(("audio", "audio files")),
        Attachment::File { .. } => None,
    }
}

/// System lines for a group, timer or safety number change by `actor`;
/// empty for other content. `name` names other accounts. Names are
/// isolated.
pub fn system_lines(
    content: &Content,
    actor: &str,
    name: impl Fn(ServiceId) -> String,
) -> Vec<String> {
    let actor = &isolate(actor);
    let name = |id: ServiceId| isolate(&name(id));
    match content {
        Content::GroupChanged(updates) => updates
            .iter()
            .map(|update| group_line(update, actor, name))
            .collect(),
        Content::TimerChanged(seconds) => vec![timer_line(*seconds, actor)],
        Content::SafetyNumberChanged => vec![format!("your safety number with {actor} changed")],
        _ => Vec::new(),
    }
}

fn group_line(update: &GroupUpdate, actor: &str, name: impl Fn(ServiceId) -> String) -> String {
    match update {
        GroupUpdate::Title(title) => format!("{actor} renamed the group to “{}”", isolate(title)),
        GroupUpdate::Description => format!("{actor} changed the group description"),
        GroupUpdate::Avatar => format!("{actor} changed the group picture"),
        GroupUpdate::Timer(seconds) => timer_line(*seconds, actor),
        GroupUpdate::Added(id) => format!("{actor} added {}", name(*id)),
        GroupUpdate::Removed(id) => format!("{actor} removed {}", name(*id)),
        GroupUpdate::Joined => format!("{actor} joined the group"),
        GroupUpdate::Left => format!("{actor} left the group"),
        GroupUpdate::Invited(id) => format!("{actor} invited {}", name(*id)),
        GroupUpdate::Requested => format!("{actor} asked to join"),
        GroupUpdate::Admin {
            member,
            admin: true,
        } => format!("{actor} made {} an admin", name(*member)),
        GroupUpdate::Admin {
            member,
            admin: false,
        } => format!("{actor} removed {} as admin", name(*member)),
        GroupUpdate::Settings => format!("{actor} changed the group settings"),
    }
}

fn timer_line(seconds: Option<u32>, actor: &str) -> String {
    match seconds {
        Some(seconds) => format!("{actor} set disappearing messages to {}", duration(seconds)),
        None => format!("{actor} turned off disappearing messages"),
    }
}

/// `seconds` in the largest unit that divides it.
fn duration(seconds: u32) -> String {
    const UNITS: [(u32, &str); 5] = [
        (7 * 24 * 3600, "week"),
        (24 * 3600, "day"),
        (3600, "hour"),
        (60, "minute"),
        (1, "second"),
    ];
    let (size, unit) = UNITS
        .into_iter()
        .find(|(size, _)| seconds.is_multiple_of(*size))
        .unwrap_or((1, "second"));
    let count = seconds / size;
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural}")
}

#[cfg(test)]
mod tests {
    use copiale_core::MessageBody;
    use uuid::Uuid;

    use super::*;

    fn text(attachments: Vec<Attachment>) -> Content {
        Content::Text {
            body: MessageBody::new("caption"),
            attachments,
            quote: None,
        }
    }

    fn named(id: ServiceId) -> String {
        format!("member{}", id.uuid().as_u128())
    }

    fn id(n: u128) -> ServiceId {
        ServiceId::new(Uuid::from_u128(n))
    }

    #[test]
    fn plain_text_has_no_placeholders() {
        assert!(placeholders(&Content::text("hi")).is_empty());
    }

    #[test]
    fn attachments_are_counted_by_kind_and_files_named() {
        let content = text(vec![
            Attachment::Image,
            Attachment::Image,
            Attachment::Voice,
            Attachment::File {
                name: Some("report.pdf".into()),
            },
            Attachment::File { name: None },
        ]);
        assert_eq!(
            placeholders(&content),
            [
                "[2 images] open on your phone",
                "[voice message] open on your phone",
                "[file] \u{2068}report.pdf\u{2069} · open on your phone",
                "[file] open on your phone",
            ]
        );
    }

    #[test]
    fn file_names_spell_out_hidden_characters() {
        let content = text(vec![Attachment::File {
            name: Some("\u{5d0}\u{202e}fdp.exe".into()),
        }]);
        assert_eq!(
            placeholders(&content),
            ["[file] \u{2068}\u{5d0}<U+202E>fdp.exe\u{2069} · open on your phone"]
        );
    }

    #[test]
    fn each_kind_has_a_placeholder() {
        let cases = [
            (
                Content::Sticker {
                    emoji: Some("🐱".into()),
                },
                "[sticker] 🐱 open on your phone",
            ),
            (
                Content::Sticker { emoji: None },
                "[sticker] open on your phone",
            ),
            (
                Content::Reaction {
                    emoji: "👍".into()
                },
                "[reaction] 👍",
            ),
            (
                Content::Call { video: true },
                "[call] video call · answer on your phone",
            ),
            (
                Content::Unsupported(Unsupported::ViewOnce),
                "[view-once media] open on your phone",
            ),
            (
                Content::Unsupported(Unsupported::Poll),
                "[poll] open on your phone",
            ),
            (text(vec![Attachment::Gif]), "[GIF] open on your phone"),
            (text(vec![Attachment::Video]), "[video] open on your phone"),
            (text(vec![Attachment::Audio]), "[audio] open on your phone"),
        ];
        for (content, expected) in cases {
            assert_eq!(placeholders(&content), [expected]);
        }
    }

    #[test]
    fn deleted_messages_say_so() {
        assert_eq!(
            placeholders(&Content::Deleted),
            ["this message was deleted"]
        );
    }

    #[test]
    fn undecryptable_messages_say_so() {
        assert_eq!(
            placeholders(&Content::Undecryptable),
            ["this message couldn't be decrypted"]
        );
    }

    fn reply(author: Option<ServiceId>, text: &str, attachment: Option<Attachment>) -> Content {
        Content::Text {
            body: MessageBody::new("yes"),
            attachments: Vec::new(),
            quote: Some(copiale_core::Quote {
                author,
                sent_at: copiale_core::Timestamp::from_millis(1),
                original: Some(copiale_core::Quoted {
                    text: text.into(),
                    attachment,
                }),
            }),
        }
    }

    #[test]
    fn quotes_read_as_one_line() {
        let author = |id: Option<ServiceId>| id.map_or("you".into(), named);
        assert_eq!(quote_line(&Content::text("hi"), author), None);
        assert_eq!(
            quote_line(&reply(Some(id(2)), "ship\n it?", None), author).as_deref(),
            Some("↳ \u{2068}member2\u{2069}: ship it?")
        );
        assert_eq!(
            quote_line(&reply(None, "", Some(Attachment::Image)), author).as_deref(),
            Some("↳ \u{2068}you\u{2069}: [image]")
        );
        assert_eq!(
            quote_line(&reply(None, "", None), author).as_deref(),
            Some("↳ \u{2068}you\u{2069}: …")
        );
        let Content::Text {
            quote: Some(mut quote),
            ..
        } = reply(Some(id(2)), "ship it?", None)
        else {
            unreachable!();
        };
        quote.original = None;
        let not_found = Content::Text {
            body: MessageBody::new("yes"),
            attachments: Vec::new(),
            quote: Some(quote),
        };
        assert_eq!(
            quote_line(&not_found, author).as_deref(),
            Some("↳ original message not found")
        );
    }

    #[test]
    fn group_updates_read_as_system_lines() {
        let content = Content::GroupChanged(vec![
            GroupUpdate::Title("Dev team".into()),
            GroupUpdate::Added(id(2)),
            GroupUpdate::Removed(id(3)),
            GroupUpdate::Joined,
            GroupUpdate::Left,
            GroupUpdate::Invited(id(4)),
            GroupUpdate::Requested,
            GroupUpdate::Admin {
                member: id(2),
                admin: true,
            },
            GroupUpdate::Admin {
                member: id(2),
                admin: false,
            },
            GroupUpdate::Description,
            GroupUpdate::Avatar,
            GroupUpdate::Settings,
            GroupUpdate::Timer(Some(3600)),
            GroupUpdate::Timer(None),
        ]);
        assert_eq!(
            plain(system_lines(&content, "alice", named)),
            [
                "alice renamed the group to “Dev team”",
                "alice added member2",
                "alice removed member3",
                "alice joined the group",
                "alice left the group",
                "alice invited member4",
                "alice asked to join",
                "alice made member2 an admin",
                "alice removed member2 as admin",
                "alice changed the group description",
                "alice changed the group picture",
                "alice changed the group settings",
                "alice set disappearing messages to 1 hour",
                "alice turned off disappearing messages",
            ]
        );
        assert!(placeholders(&content).is_empty());
        assert!(system_lines(&Content::text("hi"), "alice", named).is_empty());
    }

    /// `lines` without bidi isolates.
    fn plain(lines: Vec<String>) -> Vec<String> {
        lines
            .into_iter()
            .map(|line| line.replace(['\u{2068}', '\u{2069}'], ""))
            .collect()
    }

    #[test]
    fn system_lines_isolate_names() {
        let content = Content::GroupChanged(vec![
            GroupUpdate::Title("\u{5e6}\u{5d5}\u{5d5}\u{5ea}".into()),
            GroupUpdate::Added(id(2)),
        ]);
        assert_eq!(
            system_lines(&content, "\u{5d0}", named),
            [
                "\u{2068}\u{5d0}\u{2069} renamed the group to “\u{2068}\u{5e6}\u{5d5}\u{5d5}\u{5ea}\u{2069}”",
                "\u{2068}\u{5d0}\u{2069} added \u{2068}member2\u{2069}",
            ]
        );
    }

    #[test]
    fn timer_durations_use_the_largest_whole_unit() {
        for (seconds, expected) in [
            (30, "30 seconds"),
            (300, "5 minutes"),
            (3600, "1 hour"),
            (86_400, "1 day"),
            (604_800, "1 week"),
            (2_419_200, "4 weeks"),
            (90, "90 seconds"),
        ] {
            assert_eq!(duration(seconds), expected);
        }
        assert_eq!(
            system_lines(&Content::TimerChanged(Some(86_400)), "you", named),
            ["\u{2068}you\u{2069} set disappearing messages to 1 day"]
        );
    }

    #[test]
    fn safety_number_changes_name_the_account() {
        assert_eq!(
            system_lines(&Content::SafetyNumberChanged, "Alice", named),
            ["your safety number with \u{2068}Alice\u{2069} changed"]
        );
        assert!(placeholders(&Content::SafetyNumberChanged).is_empty());
    }
}
