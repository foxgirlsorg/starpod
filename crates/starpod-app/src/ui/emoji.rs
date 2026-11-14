//! Emoji in text, drawn as Twemoji pictures.
//!
//! The Slint software renderer draws only monochrome glyphs, and Windows has
//! no flag glyphs, so an emoji in a name or a message shows as a blank. The
//! app therefore splits every display string into runs: text runs (one word
//! each, for the UI to wrap) and emoji runs, each carrying the Twemoji SVG
//! from the blob compiled into the binary (`assets/twemoji.bin`, built by
//! `tools/pack-twemoji.py`).
//!
//! Runs are built on the UI thread (Slint images are not `Send`), and the
//! decoded images are cached per Twemoji key.

use std::cell::RefCell;
use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::sync::OnceLock;

use flate2::read::GzDecoder;
use slint::{Image, ModelRc, VecModel};
use starpod_ui::TextRun;
use unicode_segmentation::UnicodeSegmentation;

/// The gzip'd Twemoji blob. The format is documented in `tools/pack-twemoji.py`.
static BLOB: &[u8] = include_bytes!("../../assets/twemoji.bin");

const VS16: char = '\u{FE0F}';
const KEYCAP: char = '\u{20E3}';

/// One piece of a display string, before it becomes a [`TextRun`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunData {
    /// The characters of this piece. Text runs keep their trailing space.
    pub text: String,
    /// Whether this piece is an emoji.
    pub emoji: bool,
    /// The Twemoji key of an emoji (lowercase hex codepoints joined with `-`,
    /// without `.svg`). `None` for text runs and for emoji with no picture.
    pub key: Option<String>,
}

/// The decoded blob: every SVG, by its Twemoji key.
struct Table {
    bytes: Vec<u8>,
    entries: HashMap<String, Range<usize>>,
}

impl Table {
    fn decode(gzipped: &[u8]) -> Option<Self> {
        let mut bytes = Vec::new();
        GzDecoder::new(gzipped).read_to_end(&mut bytes).ok()?;
        let count = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let mut at = 4;
        let mut headers = Vec::with_capacity(count);
        for _ in 0..count {
            let name_len = *bytes.get(at)? as usize;
            let name = std::str::from_utf8(bytes.get(at + 1..at + 1 + name_len)?).ok()?;
            at += 1 + name_len;
            let len = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?) as usize;
            at += 4;
            headers.push((name.strip_suffix(".svg")?.to_string(), len));
        }
        let mut entries = HashMap::with_capacity(count);
        let mut start = at;
        for (key, len) in headers {
            let end = start.checked_add(len)?;
            if end > bytes.len() {
                return None;
            }
            entries.insert(key, start..end);
            start = end;
        }
        Some(Self { bytes, entries })
    }
}

fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| Table::decode(BLOB).expect("the bundled twemoji.bin is valid"))
}

/// The SVG for a Twemoji key, if the blob has one.
fn svg(key: &str) -> Option<&'static [u8]> {
    let table = table();
    table
        .entries
        .get(key)
        .map(|range| &table.bytes[range.clone()])
}

thread_local! {
    static ICONS: RefCell<HashMap<String, Image>> = RefCell::new(HashMap::new());
}

/// The picture for a Twemoji key. One decoded image per key is cached, so
/// the same emoji costs one decode. An unknown key gives an empty image.
pub fn icon(key: &str) -> Image {
    ICONS.with(|cache| {
        if let Some(image) = cache.borrow().get(key) {
            return image.clone();
        }
        let Some(data) = svg(key) else {
            return Image::default();
        };
        let image = Image::load_from_svg_data(data).unwrap_or_default();
        cache.borrow_mut().insert(key.to_string(), image.clone());
        image
    })
}

/// Split a display string into runs. Every character is kept, so the runs
/// concatenate back to `text`. Each run of plain text is cut into words, a
/// word keeping the space after it, so the UI can wrap them.
pub fn runs(text: &str) -> Vec<RunData> {
    let mut out = Vec::new();
    let mut plain = String::new();
    for grapheme in text.graphemes(true) {
        if is_emoji(grapheme) {
            push_words(&mut out, &mut plain);
            out.push(RunData {
                text: grapheme.to_string(),
                emoji: true,
                key: emoji_key(grapheme),
            });
        } else {
            plain.push_str(grapheme);
        }
    }
    push_words(&mut out, &mut plain);
    out
}

fn push_words(out: &mut Vec<RunData>, plain: &mut String) {
    for word in plain.split_inclusive(' ') {
        out.push(RunData {
            text: word.to_string(),
            emoji: false,
            key: None,
        });
    }
    plain.clear();
}

/// One run as the Bridge carries it.
pub fn text_run(run: RunData) -> TextRun {
    TextRun {
        text: run.text.into(),
        icon: run.key.as_deref().map(icon).unwrap_or_default(),
        emoji: run.emoji,
    }
}

/// The runs of a display string, as a Slint model. Empty for an empty string.
pub fn runs_model(text: &str) -> ModelRc<TextRun> {
    let rows: Vec<TextRun> = runs(text).into_iter().map(text_run).collect();
    ModelRc::new(VecModel::from(rows))
}

/// Whether a grapheme cluster is an emoji: it has a regional indicator (a
/// flag), a keycap, a variation selector 16, or an emoji-presentation
/// character. Text such as `©` or `✓` counts only with VS16.
fn is_emoji(grapheme: &str) -> bool {
    grapheme
        .chars()
        .any(|c| is_regional(c) || c == VS16 || c == KEYCAP || is_default_pictograph(c))
}

/// The Twemoji key of an emoji cluster. Twemoji drops VS16 from most file
/// names, so the key without it is tried first, then the key with it.
fn emoji_key(grapheme: &str) -> Option<String> {
    let bare = key_of(grapheme.chars().filter(|&c| c != VS16));
    if svg(&bare).is_some() {
        return Some(bare);
    }
    let full = key_of(grapheme.chars());
    svg(&full).map(|_| full)
}

fn key_of(chars: impl Iterator<Item = char>) -> String {
    chars
        .map(|c| format!("{:x}", u32::from(c)))
        .collect::<Vec<_>>()
        .join("-")
}

fn is_regional(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Characters that are emoji by default (Emoji_Presentation), and the
/// pictographic block, which is emoji-presentation almost throughout.
fn is_default_pictograph(c: char) -> bool {
    matches!(
        u32::from(c),
        0x1F000..=0x1FAFF
            | 0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use slint::Model;

    /// (text, is_emoji, key) for each run.
    fn shape(text: &str) -> Vec<(String, bool, Option<String>)> {
        runs(text)
            .into_iter()
            .map(|run| (run.text, run.emoji, run.key))
            .collect()
    }

    fn emoji_keys(text: &str) -> Vec<String> {
        runs(text).into_iter().filter_map(|run| run.key).collect()
    }

    fn assert_keys_exist(text: &str) {
        for key in emoji_keys(text) {
            assert!(svg(&key).is_some(), "{key} is not in the blob");
        }
    }

    fn text(value: &str) -> (String, bool, Option<String>) {
        (value.to_string(), false, None)
    }

    fn emoji(value: &str, key: &str) -> (String, bool, Option<String>) {
        (value.to_string(), true, Some(key.to_string()))
    }

    #[test]
    fn flag_and_symbol_names_split_into_runs() {
        assert_eq!(
            shape("🇩🇪 Frankfurt 🚀 01"),
            vec![
                emoji("🇩🇪", "1f1e9-1f1ea"),
                text(" "),
                text("Frankfurt "),
                emoji("🚀", "1f680"),
                text(" "),
                text("01"),
            ]
        );
        assert_keys_exist("🇩🇪 Frankfurt 🚀 01");
    }

    #[test]
    fn the_three_test_flags_have_their_pictures() {
        assert_eq!(emoji_keys("🇺🇸"), vec!["1f1fa-1f1f8"]);
        assert_eq!(emoji_keys("🇬🇧"), vec!["1f1ec-1f1e7"]);
        assert_eq!(
            shape("🇺🇸 New York"),
            vec![
                emoji("🇺🇸", "1f1fa-1f1f8"),
                text(" "),
                text("New "),
                text("York"),
            ]
        );
    }

    #[test]
    fn a_rocket_before_a_word_is_one_run() {
        assert_eq!(
            shape("🚀 Proxy"),
            vec![emoji("🚀", "1f680"), text(" "), text("Proxy"),]
        );
    }

    #[test]
    fn a_variation_selector_is_dropped_from_the_key_when_the_file_has_no_it() {
        assert_eq!(
            shape("⚡️ Auto"),
            vec![emoji("⚡️", "26a1"), text(" "), text("Auto"),]
        );
    }

    #[test]
    fn a_flag_inside_a_name_keeps_its_place() {
        assert_eq!(
            shape("Frankfurt 🇩🇪 01"),
            vec![
                text("Frankfurt "),
                emoji("🇩🇪", "1f1e9-1f1ea"),
                text(" "),
                text("01"),
            ]
        );
    }

    #[test]
    fn a_zwj_sequence_keeps_its_variation_selector_in_the_key() {
        assert_eq!(emoji_keys("🏳️‍🌈"), vec!["1f3f3-fe0f-200d-1f308"]);
        assert_eq!(shape("🏳️‍🌈 Pride").len(), 3);
        assert_keys_exist("🏳️‍🌈");
    }

    #[test]
    fn a_keycap_is_one_emoji() {
        assert_eq!(
            shape("Step 1️⃣"),
            vec![text("Step "), emoji("1️⃣", "31-20e3"),]
        );
    }

    #[test]
    fn a_skin_tone_modifier_is_part_of_its_emoji() {
        assert_eq!(emoji_keys("👍🏽 Ok"), vec!["1f44d-1f3fd"]);
        assert_keys_exist("👍🏽 Ok");
    }

    #[test]
    fn text_without_emoji_is_word_runs_only() {
        assert_eq!(shape("Frankfurt 01"), vec![text("Frankfurt "), text("01")]);
        assert!(shape("plain").iter().all(|run| !run.1));
    }

    #[test]
    fn text_symbols_without_a_variation_selector_are_not_emoji() {
        assert_eq!(shape("✓ done"), vec![text("✓ "), text("done")]);
    }

    #[test]
    fn emoji_only_text_is_one_emoji_run() {
        assert_eq!(shape("🚀"), vec![emoji("🚀", "1f680")]);
    }

    #[test]
    fn cyrillic_text_with_emoji_keeps_its_words() {
        assert_eq!(
            shape("🚫 Недоступные сайты"),
            vec![
                emoji("🚫", "1f6ab"),
                text(" "),
                text("Недоступные "),
                text("сайты"),
            ]
        );
        assert_keys_exist("🚫 Недоступные сайты");
    }

    #[test]
    fn the_runs_keep_every_character() {
        for sample in [
            "🇩🇪 Frankfurt 🚀 01",
            "Step 1️⃣ and  two spaces",
            "🏳️‍🌈 ⚡️ 👍🏽 ✓ ©",
            "",
            "plain text",
        ] {
            let joined: String = runs(sample).into_iter().map(|run| run.text).collect();
            assert_eq!(joined, sample);
        }
    }

    #[test]
    fn an_unknown_emoji_has_no_key() {
        // U+1FAFF is unassigned, so Twemoji has no picture for it.
        let found = runs("\u{1FAFF}");
        assert_eq!(found.len(), 1);
        assert!(found[0].emoji);
        assert_eq!(found[0].key, None);
    }

    #[test]
    fn icons_load_for_known_keys_and_are_empty_for_unknown_ones() {
        assert!(icon("1f680").size().width > 0);
        assert_eq!(icon("not-a-key").size().width, 0);
    }

    #[test]
    fn the_runs_model_has_one_row_per_run() {
        let model = runs_model("🚀 A");
        assert_eq!(model.row_count(), 3);
        let first = model.row_data(0).expect("a first run");
        assert!(first.emoji);
        assert!(first.icon.size().width > 0);
        assert_eq!(runs_model("").row_count(), 0);
    }
}
