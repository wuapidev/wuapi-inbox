//! The emoji tables compiled into the binary.
//!
//! `base.txt.gz` lists every fully-qualified emoji of Unicode's
//! `emoji-test.txt` in keyboard order, by group, with its English name,
//! shortcodes and keywords; the skin-tone variants of an emoji follow it.
//! One `<lang>.txt.gz` per language has a line per emoji in the same
//! order. `assets/ASSETS.md` says where each comes from.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

const BASE: &[u8] = include_bytes!("../../assets/emoji/base.txt.gz");

/// A skin tone, as its modifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    Light,
    MediumLight,
    Medium,
    MediumDark,
    Dark,
}

impl Tone {
    /// The tone a character stands for, when it is a modifier.
    pub fn of(ch: char) -> Option<Tone> {
        Some(match ch {
            '\u{1F3FB}' => Tone::Light,
            '\u{1F3FC}' => Tone::MediumLight,
            '\u{1F3FD}' => Tone::Medium,
            '\u{1F3FE}' => Tone::MediumDark,
            '\u{1F3FF}' => Tone::Dark,
            _ => return None,
        })
    }
}

/// An emoji in one skin tone, or in two for the emoji of two people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub text: Box<str>,
    /// The modifiers it carries, in order.
    pub tones: Vec<Tone>,
}

impl Variant {
    /// The tone, when everybody in the emoji has the same one.
    pub fn uniform(&self) -> Option<Tone> {
        let first = *self.tones.first()?;
        self.tones
            .iter()
            .all(|tone| *tone == first)
            .then_some(first)
    }
}

/// One emoji, as the keyboard lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Emoji {
    /// The fully-qualified sequence.
    pub text: Box<str>,
    /// Its English name.
    pub name: Box<str>,
    /// The group it is listed under: an index of [`EmojiSet::groups`].
    pub group: usize,
    /// What it is called between colons (`fire`, `thumbsup`).
    pub shortcodes: Vec<Box<str>>,
    /// English keywords.
    pub keywords: Vec<Box<str>>,
    /// Its skin-tone variants; empty when it has none.
    pub variants: Vec<Variant>,
}

/// A group of the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub name: Box<str>,
    /// Its emoji, among [`EmojiSet::emojis`].
    pub range: Range<usize>,
}

/// Every emoji.
#[derive(Debug, Default)]
pub struct EmojiSet {
    pub emojis: Vec<Emoji>,
    pub groups: Vec<Group>,
    /// Every sequence, without variation selectors, to its emoji and,
    /// for a skin-tone variant, which one.
    lookup: HashMap<Box<str>, (usize, Option<usize>)>,
}

/// An emoji as CLDR writes it: without variation selectors. Two ways of
/// writing the same emoji have the same key.
pub fn key(text: &str) -> String {
    text.chars().filter(|ch| *ch != '\u{FE0F}').collect()
}

impl EmojiSet {
    /// Reads the table (the unpacked `base.txt`).
    pub fn parse(table: &str) -> Result<Self, String> {
        let mut set = EmojiSet::default();
        for (number, line) in table.lines().enumerate() {
            let mut fields = line.split('\t');
            match fields.next() {
                Some("G") => {
                    let name = fields.next().ok_or(format!("line {number}: no group"))?;
                    let at = set.emojis.len();
                    set.groups.push(Group {
                        name: name.into(),
                        range: at..at,
                    });
                }
                Some("E") => {
                    let text = fields.next().ok_or(format!("line {number}: no emoji"))?;
                    let name = fields.next().unwrap_or_default();
                    let codes = fields.next().unwrap_or_default();
                    let words = fields.next().unwrap_or_default();
                    let group = set
                        .groups
                        .len()
                        .checked_sub(1)
                        .ok_or(format!("line {number}: an emoji before any group"))?;
                    set.lookup
                        .insert(key(text).into(), (set.emojis.len(), None));
                    set.emojis.push(Emoji {
                        text: text.into(),
                        name: name.into(),
                        group,
                        shortcodes: list(codes, ','),
                        keywords: list(words, '|'),
                        variants: Vec::new(),
                    });
                    set.groups[group].range.end = set.emojis.len();
                }
                Some("V") => {
                    let text = fields.next().ok_or(format!("line {number}: no variant"))?;
                    let at = set
                        .emojis
                        .len()
                        .checked_sub(1)
                        .ok_or(format!("line {number}: a variant of nothing"))?;
                    let emoji = &mut set.emojis[at];
                    set.lookup
                        .insert(key(text).into(), (at, Some(emoji.variants.len())));
                    emoji.variants.push(Variant {
                        text: text.into(),
                        tones: text.chars().filter_map(Tone::of).collect(),
                    });
                }
                _ => {}
            }
        }
        if set.emojis.is_empty() {
            return Err("no emoji in the table".into());
        }
        Ok(set)
    }

    /// The table of the binary, unpacked the first time it is asked for
    /// and kept. Call it off the UI thread.
    pub fn load() -> Arc<EmojiSet> {
        static SET: OnceLock<Arc<EmojiSet>> = OnceLock::new();
        SET.get_or_init(|| {
            let set = unpack(BASE)
                .and_then(|table| EmojiSet::parse(&table))
                .unwrap_or_else(|error| {
                    tracing::error!(%error, "the emoji table could not be read");
                    EmojiSet::default()
                });
            Arc::new(set)
        })
        .clone()
    }

    /// The emoji a sequence is, or is a skin-tone variant of.
    pub fn find(&self, text: &str) -> Option<usize> {
        self.lookup.get(key(text).as_str()).map(|(at, _)| *at)
    }

    /// The emoji a sequence is and, for a skin-tone variant, which.
    pub fn locate(&self, text: &str) -> Option<(usize, Option<usize>)> {
        self.lookup.get(key(text).as_str()).copied()
    }

    /// The emoji at `index` in a skin tone: the variant in which everybody
    /// has that tone, and the emoji itself when it has none or `tone` is
    /// `None`. With the place of the variant among the emoji's.
    pub fn in_tone(&self, index: usize, tone: Option<Tone>) -> (&str, Option<usize>) {
        let emoji = &self.emojis[index];
        let found = tone.and_then(|tone| {
            emoji
                .variants
                .iter()
                .position(|variant| variant.uniform() == Some(tone))
        });
        match found {
            Some(at) => (&emoji.variants[at].text, Some(at)),
            None => (&emoji.text, None),
        }
    }

    /// How many sequences there are, variants included.
    #[cfg(test)]
    pub fn sequences(&self) -> usize {
        self.lookup.len()
    }
}

fn list(field: &str, separator: char) -> Vec<Box<str>> {
    field
        .split(separator)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(Into::into)
        .collect()
}

fn unpack(packed: &[u8]) -> Result<String, String> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(packed)
        .read_to_string(&mut text)
        .map_err(|error| error.to_string())?;
    Ok(text)
}

/// A language the emoji can be searched in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    En,
    Es,
    Pt,
    Hi,
    Fr,
    De,
    It,
    Id,
    Ar,
    Ru,
    Tr,
}

impl Lang {
    /// Every language there are names for.
    pub const ALL: [Lang; 11] = [
        Lang::En,
        Lang::Es,
        Lang::Pt,
        Lang::Hi,
        Lang::Fr,
        Lang::De,
        Lang::It,
        Lang::Id,
        Lang::Ar,
        Lang::Ru,
        Lang::Tr,
    ];

    /// Its ISO 639 code.
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Es => "es",
            Lang::Pt => "pt",
            Lang::Hi => "hi",
            Lang::Fr => "fr",
            Lang::De => "de",
            Lang::It => "it",
            Lang::Id => "id",
            Lang::Ar => "ar",
            Lang::Ru => "ru",
            Lang::Tr => "tr",
        }
    }

    /// The language of a code, when there are names for it.
    pub fn from_code(code: &str) -> Option<Lang> {
        let code = code.to_ascii_lowercase();
        Lang::ALL.into_iter().find(|lang| {
            lang.code() == code
                // ISO 639-2 and the old code of Indonesian.
                || matches!((lang, code.as_str()), (Lang::Id, "in" | "ind"))
        })
    }

    /// What it is called in the settings.
    pub fn label(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Es => "Spanish",
            Lang::Pt => "Portuguese",
            Lang::Hi => "Hindi",
            Lang::Fr => "French",
            Lang::De => "German",
            Lang::It => "Italian",
            Lang::Id => "Indonesian",
            Lang::Ar => "Arabic",
            Lang::Ru => "Russian",
            Lang::Tr => "Turkish",
        }
    }

    /// The packed names of the language. English has none of its own: its
    /// names are in the base table.
    fn packed(self) -> Option<&'static [u8]> {
        Some(match self {
            Lang::En => return None,
            Lang::Es => include_bytes!("../../assets/emoji/es.txt.gz"),
            Lang::Pt => include_bytes!("../../assets/emoji/pt.txt.gz"),
            Lang::Hi => include_bytes!("../../assets/emoji/hi.txt.gz"),
            Lang::Fr => include_bytes!("../../assets/emoji/fr.txt.gz"),
            Lang::De => include_bytes!("../../assets/emoji/de.txt.gz"),
            Lang::It => include_bytes!("../../assets/emoji/it.txt.gz"),
            Lang::Id => include_bytes!("../../assets/emoji/id.txt.gz"),
            Lang::Ar => include_bytes!("../../assets/emoji/ar.txt.gz"),
            Lang::Ru => include_bytes!("../../assets/emoji/ru.txt.gz"),
            Lang::Tr => include_bytes!("../../assets/emoji/tr.txt.gz"),
        })
    }

    /// How many bytes its names add to the binary.
    #[cfg(test)]
    pub fn packed_size(self) -> usize {
        self.packed().map_or(0, <[u8]>::len)
    }
}

/// What the emoji are called in one language.
#[derive(Debug, PartialEq, Eq)]
pub struct LangPack {
    pub lang: Lang,
    /// One entry per emoji of the set, in its order.
    entries: Vec<Local>,
}

/// An emoji in a language.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Local {
    /// Its name; empty when CLDR has none yet.
    pub name: Box<str>,
    /// Its name where the language is spoken differently (Latin America
    /// for Spanish, Portugal for Portuguese), when that is another one.
    pub regional: Box<str>,
    /// Keywords, of both.
    pub keywords: Vec<Box<str>>,
}

impl LangPack {
    /// Reads a language's table (an unpacked `<lang>.txt`).
    pub fn parse(lang: Lang, table: &str) -> Self {
        let entries = table
            .lines()
            .map(|line| {
                let mut fields = line.split('\t');
                Local {
                    name: fields.next().unwrap_or_default().into(),
                    regional: fields.next().unwrap_or_default().into(),
                    keywords: list(fields.next().unwrap_or_default(), '|'),
                }
            })
            .collect();
        Self { lang, entries }
    }

    /// The names of a language, unpacked the first time they are asked
    /// for and kept. `None` for English. Call it off the UI thread.
    pub fn load(lang: Lang) -> Option<Arc<LangPack>> {
        static PACKS: OnceLock<Mutex<HashMap<Lang, Arc<LangPack>>>> = OnceLock::new();
        let packed = lang.packed()?;
        let packs = PACKS.get_or_init(Default::default);
        if let Some(pack) = packs.lock().ok()?.get(&lang) {
            return Some(pack.clone());
        }
        let table = match unpack(packed) {
            Ok(table) => table,
            Err(error) => {
                tracing::error!(%error, lang = lang.code(), "emoji names could not be read");
                return None;
            }
        };
        let pack = Arc::new(LangPack::parse(lang, &table));
        packs.lock().ok()?.insert(lang, pack.clone());
        Some(pack)
    }

    /// The emoji at `index` of the set, in this language.
    pub fn get(&self, index: usize) -> Option<&Local> {
        self.entries.get(index)
    }

    /// Its name: the regional one when asked for and there is one. `None`
    /// when the language has no name for it.
    pub fn name(&self, index: usize, regional: bool) -> Option<&str> {
        let local = self.entries.get(index)?;
        let name = if regional && !local.regional.is_empty() {
            &local.regional
        } else {
            &local.name
        };
        (!name.is_empty()).then_some(&**name)
    }

    /// How many emoji it has a line for.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nine groups of the keyboard, in its order.
    const GROUPS: [&str; 9] = [
        "Smileys & Emotion",
        "People & Body",
        "Animals & Nature",
        "Food & Drink",
        "Travel & Places",
        "Activities",
        "Objects",
        "Symbols",
        "Flags",
    ];

    #[test]
    fn the_table_has_every_emoji_of_the_data_file_in_its_groups() {
        let set = EmojiSet::load();
        // Emoji 18.0: 3963 fully-qualified sequences, of which 2040 are
        // skin-tone variants of the 1923 others.
        assert_eq!(set.emojis.len(), 1923);
        let variants: usize = set.emojis.iter().map(|emoji| emoji.variants.len()).sum();
        assert_eq!(variants, 2040);
        assert_eq!(set.sequences(), 3963);

        let names: Vec<&str> = set.groups.iter().map(|group| &*group.name).collect();
        assert_eq!(names, GROUPS);
        // The groups follow each other and leave nothing out.
        let mut at = 0;
        for (index, group) in set.groups.iter().enumerate() {
            assert_eq!(group.range.start, at, "{}", group.name);
            assert!(!group.range.is_empty(), "{}", group.name);
            for emoji in &set.emojis[group.range.clone()] {
                assert_eq!(emoji.group, index);
            }
            at = group.range.end;
        }
        assert_eq!(at, set.emojis.len());
    }

    #[test]
    fn every_emoji_has_an_english_name_and_a_shortcode() {
        let set = EmojiSet::load();
        let mut codes = std::collections::HashSet::new();
        for emoji in &set.emojis {
            assert!(!emoji.text.is_empty());
            assert!(!emoji.name.trim().is_empty(), "{} has no name", emoji.text);
            assert!(
                !emoji.shortcodes.is_empty(),
                "{} has no shortcode",
                emoji.text
            );
            for code in &emoji.shortcodes {
                assert!(codes.insert(code.clone()), ":{code}: is two emoji");
                assert!(!code.contains([':', ' ']), ":{code}:");
            }
        }
        let at = |text: &str| &set.emojis[set.find(text).unwrap()];
        assert_eq!(&*at("🔥").name, "fire");
        assert!(at("🔥").shortcodes.iter().any(|code| &**code == "fire"));
        assert!(at("👍").shortcodes.iter().any(|code| &**code == "thumbsup"));
        assert!(at("👍").shortcodes.iter().any(|code| &**code == "+1"));
        // The newest ones are called what the data file calls them.
        assert_eq!(&*at("\u{1FADD}").name, "pickle");
    }

    #[test]
    fn skin_tones_belong_to_their_emoji() {
        let set = EmojiSet::load();
        let wave = set.find("👋").unwrap();
        assert_eq!(set.emojis[wave].variants.len(), 5);
        assert_eq!(set.find("👋🏽"), Some(wave));
        assert_eq!(set.locate("👋🏽"), Some((wave, Some(2))));
        assert_eq!(set.in_tone(wave, Some(Tone::Medium)).0, "👋🏽");
        assert_eq!(set.in_tone(wave, None), ("👋", None));
        // Written with or without the variation selector, it is the same.
        assert_eq!(set.find("❤"), set.find("❤️"));
        assert_eq!(set.find("☝🏿"), set.find("☝️"));

        // An emoji without tones stays as it is, whatever is preferred.
        let fire = set.find("🔥").unwrap();
        assert!(set.emojis[fire].variants.is_empty());
        assert_eq!(set.in_tone(fire, Some(Tone::Dark)), ("🔥", None));

        // Two people: every pair of tones, the mixed ones written with
        // other characters than the emoji itself.
        let shake = set.find("🤝").unwrap();
        assert_eq!(set.emojis[shake].variants.len(), 25);
        assert_eq!(set.find("🫱🏻‍🫲🏿"), Some(shake));
        assert_eq!(set.in_tone(shake, Some(Tone::Dark)).0, "🤝🏿");
        let mixed = &set.emojis[shake].variants[set.locate("🫱🏻‍🫲🏿").unwrap().1.unwrap()];
        assert_eq!(mixed.tones, [Tone::Light, Tone::Dark]);
        assert_eq!(mixed.uniform(), None);
        let couple = set.find("🧑‍🤝‍🧑").unwrap();
        assert_eq!(set.in_tone(couple, Some(Tone::Medium)).0, "🧑🏽‍🤝‍🧑🏽");

        // No variant is listed as an emoji of its own, and every variant
        // carries a tone.
        for emoji in &set.emojis {
            assert!(
                !emoji.text.chars().any(|ch| Tone::of(ch).is_some()),
                "{}",
                emoji.text
            );
            for variant in &emoji.variants {
                assert!(!variant.tones.is_empty(), "{}", variant.text);
            }
        }
    }

    #[test]
    fn sequences_are_listed_as_the_data_defines_them() {
        let set = EmojiSet::load();
        // A gendered profession, a family and a flag made of tags are
        // each one emoji.
        for text in ["👩‍🚒", "👨‍👩‍👧‍👦", "🏴󠁧󠁢󠁳󠁣󠁴󠁿", "🇻🇪", "#️⃣", "🏳️‍🌈"]
        {
            let at = set
                .find(text)
                .unwrap_or_else(|| panic!("{text} is missing"));
            assert_eq!(&*set.emojis[at].text, text);
        }
        assert_eq!(set.find("not an emoji"), None);
    }

    #[test]
    fn every_sequence_fits_a_reaction() {
        // The wuapi API takes a reaction of at most 32 UTF-16 units; the
        // longest sequence (a kiss between two people of two skin tones)
        // is 15.
        let set = EmojiSet::load();
        let longest = set
            .emojis
            .iter()
            .flat_map(|emoji| {
                std::iter::once(&*emoji.text).chain(emoji.variants.iter().map(|v| &*v.text))
            })
            .map(|text| text.encode_utf16().count())
            .max()
            .unwrap();
        assert_eq!(longest, 15);
    }

    #[test]
    fn every_language_has_a_line_per_emoji() {
        let set = EmojiSet::load();
        assert!(LangPack::load(Lang::En).is_none(), "English is the table");
        for lang in Lang::ALL.into_iter().filter(|lang| *lang != Lang::En) {
            let pack = LangPack::load(lang).unwrap();
            assert_eq!(pack.len(), set.emojis.len(), "{}", lang.code());
            // CLDR is a release behind the newest emoji: a few have no
            // name yet, and nothing else.
            let unnamed = (0..pack.len())
                .filter(|index| pack.name(*index, false).is_none())
                .count();
            assert!(unnamed <= 20, "{}: {unnamed} without a name", lang.code());
            assert_eq!(Lang::from_code(lang.code()), Some(lang));
            assert!(lang.packed_size() > 20_000);
        }
        let fire = set.find("🔥").unwrap();
        let spanish = LangPack::load(Lang::Es).unwrap();
        assert_eq!(spanish.name(fire, false), Some("fuego"));
        assert_eq!(
            LangPack::load(Lang::Pt).unwrap().name(fire, false),
            Some("fogo")
        );
        // Where Latin America says it differently, both are there.
        let zipper = set.find("🤐").unwrap();
        assert_eq!(
            spanish.name(zipper, false),
            Some("cara con la boca cerrada con cremallera")
        );
        assert_eq!(
            spanish.name(zipper, true),
            Some("cara con la boca cerrada con cierre")
        );
        // The second load is the first one's.
        assert!(Arc::ptr_eq(&spanish, &LangPack::load(Lang::Es).unwrap()));
        assert_eq!(Lang::from_code("xx"), None);
    }
}
