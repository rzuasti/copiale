//! Text pools and message body generation. Emoji use escapes so variation
//! selectors and ZWJ joiners survive editors and tooling.

use crate::model::Reaction;
use crate::rng::Rng;

pub const CONTACT_NAMES: &[&str] = &[
    "Aiko Tanaka",
    "Omar Haddad",
    "Noa Levi",
    "Lukas Becker",
    "Zoë Martin",
    "Siobhán Kelly",
    "陈伟",
    "Олена Коваль",
    "Søren Nielsen",
    "Amara Okafor",
    "Mateo García",
    "Priya Raman",
    "Kwame Mensah",
    "Ingrid Holm",
    "Yusuf Demir",
    "Camille Dubois",
    "Mei Lin",
    "Diego Herrera",
    "Hana Novák",
    "Tomasz Wójcik",
    "نور حداد",
    "דנה כהן",
    "Leilani Kahale",
    "Jonas Weber",
    "Sakura Ito",
    "Arjun Mehta",
    "Elif Yılmaz",
    "Nikos Papadopoulos",
    "Marta Rossi",
    "Björn Svensson",
    "Ana Sousa",
    "Dmitri Volkov",
    "Grace Kim",
    "Rafael Costa",
    "Chiara Bianchi",
    "Ewa Nowak",
    "Hiroshi Sato",
    "Lucía Fernández",
    "Ngozi Adeyemi",
    "Maximilian Alexander von Hohenberg-Schwarzenfeld",
];

pub const GROUP_NAMES: &[&str] = &[
    "Climbing crew \u{1F9D7}",
    "Family \u{2764}\u{FE0F}",
    "Copiale dev",
    "Book club \u{1F4DA}",
    "Weekend plans",
    "Flat 4B",
    "Ζ team",
    "Trip to Kyoto \u{1F1EF}\u{1F1F5}",
    "A very long group name that will definitely need truncating in the sidebar",
    "Board games \u{1F3B2}",
    "読書会",
];

const WORDS: &[&str] = &[
    "the",
    "a",
    "just",
    "really",
    "maybe",
    "tomorrow",
    "tonight",
    "meeting",
    "coffee",
    "later",
    "think",
    "know",
    "sure",
    "going",
    "weekend",
    "project",
    "time",
    "call",
    "after",
    "before",
    "work",
    "home",
    "pretty",
    "good",
    "great",
    "idea",
    "plan",
    "train",
    "late",
    "early",
    "dinner",
    "lunch",
    "sounds",
    "check",
    "send",
    "photo",
    "link",
    "code",
    "build",
    "release",
    "bug",
    "fix",
    "review",
    "test",
    "again",
    "still",
    "already",
    "almost",
    "done",
    "busy",
    "free",
    "wait",
    "need",
    "want",
    "should",
    "could",
    "would",
    "we",
    "you",
    "they",
    "it",
    "this",
    "that",
    "with",
    "without",
    "about",
    "from",
    "into",
    "over",
    "under",
    "around",
    "there",
    "here",
    "now",
    "then",
    "today",
    "yesterday",
    "morning",
    "evening",
    "night",
    "quick",
    "slow",
    "long",
    "short",
    "new",
    "old",
    "nice",
    "weird",
    "funny",
    "honestly",
    "literally",
    "basically",
    "probably",
    "definitely",
    "weather",
    "rain",
    "sun",
    "walk",
    "run",
    "bike",
    "ride",
    "climb",
    "read",
    "book",
    "movie",
    "music",
    "album",
    "concert",
    "ticket",
    "station",
    "airport",
    "flight",
    "hotel",
    "beach",
    "mountain",
    "city",
    "street",
    "park",
    "garden",
    "kitchen",
    "cat",
    "dog",
    "plant",
    "window",
    "door",
    "keys",
    "phone",
    "laptop",
    "battery",
    "charger",
    "screen",
    "update",
    "install",
    "message",
    "group",
    "chat",
    "notes",
    "list",
];

const SHORT_REPLIES: &[&str] = &[
    "ok",
    "okay",
    "lol",
    "sure",
    "yes",
    "nope",
    "thanks!",
    "on my way",
    "sounds good",
    "haha",
    "nice",
    "wait what",
    "omg",
    "brb",
    "got it",
    "love it",
    "see you soon",
    "\u{1F44D}",
    "\u{2764}\u{FE0F}",
    "\u{1F64F}",
];

const CJK_PHRASES: &[&str] = &[
    "今日はいい天気ですね。",
    "明日の会議は何時からですか？",
    "了解しました！",
    "駅の前で待っています。",
    "写真ありがとう、すごくきれい！",
    "我们明天见。",
    "这个项目进展得怎么样？",
    "周末一起去爬山吧！",
    "我刚到家，累死了。",
    "안녕하세요, 잘 지내세요?",
    "내일 저녁에 시간 있어요?",
    "고마워요! 정말 좋아요.",
];

const RTL_PHRASES: &[&str] = &[
    "مرحبا، كيف حالك؟",
    "سأراك غدا إن شاء الله.",
    "شكرا جزيلا على المساعدة!",
    "الاجتماع في الساعة الثالثة.",
    "שלום, מה שלומך?",
    "נתראה מחר בבוקר.",
    "תודה רבה על העזרה!",
    "הפגישה בשעה שלוש.",
];

/// Short RTL fragments embedded in English sentences (bidi test).
const RTL_FRAGMENTS: &[&str] = &["مكتب الشركة", "القاهرة", "ירושלים", "בית קפה", "تل أبيب"];

const EMOJI: &[&str] = &[
    "\u{1F600}",                                                    // grinning
    "\u{1F602}",                                                    // tears of joy
    "\u{1F972}",                                                    // smiling with tear
    "\u{1F60D}",                                                    // heart eyes
    "\u{1F914}",                                                    // thinking
    "\u{1F643}",                                                    // upside-down
    "\u{1F60E}",                                                    // sunglasses
    "\u{1F973}",                                                    // party face
    "\u{1F62D}",                                                    // crying
    "\u{1F44D}",                                                    // thumbs up
    "\u{1F44F}",                                                    // clapping
    "\u{1F64C}",                                                    // raised hands
    "\u{1F525}",                                                    // fire
    "\u{2728}",                                                     // sparkles
    "\u{1F389}",                                                    // party popper
    "\u{2764}\u{FE0F}",                                             // red heart (VS16)
    "\u{1F49C}",                                                    // purple heart
    "\u{1F355}",                                                    // pizza
    "\u{2615}",                                                     // coffee
    "\u{1F327}\u{FE0F}",                                            // cloud with rain (VS16)
    "\u{1F6B2}",                                                    // bicycle
    "\u{1F3D4}\u{FE0F}",                                            // snow-capped mountain
    "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}\u{200D}\u{1F466}", // family (ZWJ)
    "\u{1F469}\u{1F3FD}\u{200D}\u{1F4BB}",                          // technologist, skin tone (ZWJ)
    "\u{1F9D1}\u{1F3FF}\u{200D}\u{1F680}",                          // astronaut, skin tone (ZWJ)
    "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}",                           // rainbow flag (ZWJ)
    "\u{1F3F4}\u{200D}\u{2620}\u{FE0F}",                            // pirate flag (ZWJ)
    "\u{2764}\u{FE0F}\u{200D}\u{1F525}",                            // heart on fire (ZWJ)
    "\u{1F9D7}\u{200D}\u{2640}\u{FE0F}",                            // woman climbing (ZWJ)
    "\u{1F469}\u{200D}\u{2764}\u{FE0F}\u{200D}\u{1F468}",           // couple with heart (ZWJ)
    "\u{1F44B}\u{1F3FB}",                                           // waving, skin tone
    "\u{1F44D}\u{1F3FE}",                                           // thumbs up, skin tone
    "\u{1F64F}\u{1F3FC}",                                           // folded hands, skin tone
    "\u{1F1EF}\u{1F1F5}",                                           // flag: Japan
    "\u{1F1E7}\u{1F1F7}",                                           // flag: Brazil
    "\u{1F1FA}\u{1F1E6}",                                           // flag: Ukraine
    "\u{1F1EE}\u{1F1EA}",                                           // flag: Ireland
    "1\u{FE0F}\u{20E3}",                                            // keycap 1
];

const REACTIONS: &[&str] = &[
    "\u{1F44D}",        // thumbs up
    "\u{2764}\u{FE0F}", // red heart
    "\u{1F602}",        // tears of joy
    "\u{1F62E}",        // open mouth
    "\u{1F622}",        // crying
    "\u{1F64F}",        // folded hands
];

#[derive(Clone, Copy)]
enum BodyKind {
    Short,
    Sentences,
    Paragraphs,
    LongUrl,
    InlineLink,
    Cjk,
    Rtl,
    MixedBidi,
    EmojiOnly,
    SentenceWithEmoji,
}

/// Weights sum to 100, so each weight reads as a percentage.
const BODY_KINDS: &[(BodyKind, u32)] = &[
    (BodyKind::Short, 22),
    (BodyKind::Sentences, 30),
    (BodyKind::Paragraphs, 8),
    (BodyKind::LongUrl, 5),
    (BodyKind::InlineLink, 5),
    (BodyKind::Cjk, 8),
    (BodyKind::Rtl, 7),
    (BodyKind::MixedBidi, 3),
    (BodyKind::EmojiOnly, 7),
    (BodyKind::SentenceWithEmoji, 5),
];

pub fn body(rng: &mut Rng) -> String {
    match rng.weighted(BODY_KINDS) {
        BodyKind::Short => rng.pick(SHORT_REPLIES).to_string(),
        BodyKind::Sentences => sentences(rng, 1, 4),
        BodyKind::Paragraphs => {
            let count = rng.range(2, 6) as usize;
            (0..count)
                .map(|_| sentences(rng, 2, 6))
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        BodyKind::LongUrl => {
            let url = long_url(rng);
            if rng.chance(0.5) {
                url
            } else {
                format!("check this out {url}")
            }
        }
        BodyKind::InlineLink => {
            let link = format!(
                "https://example.org/{}/{}",
                rng.pick(WORDS),
                alphanumeric(rng, 6)
            );
            format!("{} {link} {}", sentence(rng), sentence(rng))
        }
        BodyKind::Cjk => phrases(rng, CJK_PHRASES),
        BodyKind::Rtl => phrases(rng, RTL_PHRASES),
        BodyKind::MixedBidi => format!(
            "{} {} {}",
            capitalized_words(rng, 3, 7),
            rng.pick(RTL_FRAGMENTS),
            words(rng, 2, 6)
        ),
        BodyKind::EmojiOnly => {
            let count = rng.range(1, 7) as usize;
            (0..count).map(|_| *rng.pick(EMOJI)).collect()
        }
        BodyKind::SentenceWithEmoji => format!("{} {}", sentence(rng), rng.pick(EMOJI)),
    }
}

/// 1-3 distinct reactions, each with at most `max_count` reactors.
pub fn reactions(rng: &mut Rng, max_count: u32) -> Vec<Reaction> {
    let mut emoji = REACTIONS.to_vec();
    rng.shuffle(&mut emoji);
    let kinds = rng.range(1, 4) as usize;
    emoji
        .into_iter()
        .take(kinds)
        .map(|emoji| Reaction {
            emoji,
            count: rng.range(1, u64::from(max_count.max(1)) + 1) as u32,
        })
        .collect()
}

fn sentences(rng: &mut Rng, min: u64, max: u64) -> String {
    let count = rng.range(min, max) as usize;
    (0..count)
        .map(|_| sentence(rng))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sentence(rng: &mut Rng) -> String {
    let ending = rng.pick(&[".", ".", ".", "?", "!"]);
    format!("{}{ending}", capitalized_words(rng, 4, 15))
}

fn capitalized_words(rng: &mut Rng, min: u64, max: u64) -> String {
    let text = words(rng, min, max);
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

fn words(rng: &mut Rng, min: u64, max: u64) -> String {
    let count = rng.range(min, max) as usize;
    (0..count)
        .map(|_| *rng.pick(WORDS))
        .collect::<Vec<_>>()
        .join(" ")
}

fn phrases(rng: &mut Rng, pool: &[&str]) -> String {
    let count = rng.range(1, 4) as usize;
    (0..count)
        .map(|_| *rng.pick(pool))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Unbroken URL of roughly 120-300 characters (line-wrapping test).
fn long_url(rng: &mut Rng) -> String {
    let segments = rng.range(3, 7) as usize;
    let path = (0..segments)
        .map(|_| format!("{}-{}", rng.pick(WORDS), rng.pick(WORDS)))
        .collect::<Vec<_>>()
        .join("/");
    let token_len = rng.range(40, 160) as usize;
    format!(
        "https://example.com/{path}?ref={}&utm_source=chat&session={}",
        alphanumeric(rng, token_len),
        alphanumeric(rng, 32)
    )
}

fn alphanumeric(rng: &mut Rng, len: usize) -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len).map(|_| char::from(*rng.pick(CHARS))).collect()
}
