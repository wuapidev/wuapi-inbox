//! The language of the interface.
//!
//! Every word the window shows is a *key* (`onboarding.step1.title`) looked
//! up in the catalog of the language in use: [`t`] for a plain text, [`tr`]
//! for one with `{name}` holes, [`plural`] for one that depends on a count.
//! A language is one JSON file in `assets/locales`, compiled in like the
//! emoji tables; English is the one every key is written in first, and what
//! a language has not translated yet is shown in English rather than as a
//! key. Adding a language is a file, an entry in [`Language`] and a line in
//! [`catalog`]; `catalogs_agree` (tests) holds the files to the English one.
//!
//! The language is the operating system's unless one is picked in
//! Settings. It is process-wide, like the interface scale in
//! [`theme`](crate::theme): the text is looked up where it is drawn, and
//! `settings::update` refreshes the windows when it changes. The emoji
//! search language is a separate setting that shares only the detection
//! ([`crate::locale`]).
//!
//! Tests see English whatever the machine says, unless they ask for
//! another language with [`with_language`].

use crate::locale::Locale;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::OnceLock;

/// A language the interface is translated into.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Es,
}

impl Language {
    /// Every language, in the order Settings lists them.
    pub const ALL: [Language; 2] = [Language::En, Language::Es];

    /// ISO 639 code, lower case: the catalog's file name.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Es => "es",
        }
    }

    /// The language in its own words, as a language picker shows it.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Es => "Español",
        }
    }

    /// The language for an ISO 639 code, if it is translated.
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|lang| lang.code().eq_ignore_ascii_case(code))
    }
}

/// What the settings say about the language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LanguageChoice {
    /// The operating system's.
    #[default]
    System,
    /// This one, whatever the system says.
    #[serde(untagged)]
    Lang(Language),
}

/// The language for a choice on a system. A language there is no
/// translation for is English.
pub fn resolve(choice: LanguageChoice, system: Option<&Locale>) -> Language {
    match choice {
        LanguageChoice::Lang(lang) => lang,
        LanguageChoice::System => system
            .and_then(|locale| Language::from_code(&locale.language))
            .unwrap_or_default(),
    }
}

type Catalog = HashMap<String, String>;

/// The files a language is written in, one per area of the interface; the
/// keys of `<area>.json` all start with `<area>.`. Areas are files so that
/// the people translating one do not meet in the same file.
const AREAS: [&str; 6] = ["common", "commands", "chat", "accounts", "status", "groups"];

/// The files of a language, in the order of [`AREAS`].
macro_rules! files {
    ($code:literal) => {
        [
            include_str!(concat!("../assets/locales/", $code, "/common.json")),
            include_str!(concat!("../assets/locales/", $code, "/commands.json")),
            include_str!(concat!("../assets/locales/", $code, "/chat.json")),
            include_str!(concat!("../assets/locales/", $code, "/accounts.json")),
            include_str!(concat!("../assets/locales/", $code, "/status.json")),
            include_str!(concat!("../assets/locales/", $code, "/groups.json")),
        ]
    };
}
/// The files of a language, in the order of [`AREAS`].
fn files_of(lang: Language) -> [&'static str; AREAS.len()] {
    match lang {
        Language::En => files!("en"),
        Language::Es => files!("es"),
    }
}

/// The catalog of a language, read the first time it is asked for.
fn catalog(lang: Language) -> &'static Catalog {
    static EN: OnceLock<Catalog> = OnceLock::new();
    static ES: OnceLock<Catalog> = OnceLock::new();
    let cell = match lang {
        Language::En => &EN,
        Language::Es => &ES,
    };
    cell.get_or_init(|| {
        let mut all = Catalog::new();
        for (area, source) in AREAS.into_iter().zip(files_of(lang)) {
            match parse_catalog(source) {
                Ok(texts) => all.extend(texts),
                // A catalog that does not parse is a bug the tests catch;
                // a release shows English rather than nothing.
                Err(error) => tracing::error!(%error, area, language = lang.code(), "unreadable"),
            }
        }
        all
    })
}

/// A catalog file: one flat JSON object of keys and texts.
fn parse_catalog(source: &str) -> serde_json::Result<Catalog> {
    serde_json::from_str(source)
}

#[cfg(not(test))]
mod current {
    use super::Language;
    use std::sync::atomic::{AtomicU8, Ordering};

    static CURRENT: AtomicU8 = AtomicU8::new(0);

    pub fn get() -> Language {
        Language::ALL[usize::from(CURRENT.load(Ordering::Relaxed)).min(Language::ALL.len() - 1)]
    }

    pub fn set(lang: Language) {
        let index = Language::ALL.iter().position(|other| *other == lang);
        CURRENT.store(index.unwrap_or(0) as u8, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod current {
    use super::Language;
    use std::cell::Cell;

    thread_local! {
        static CURRENT: Cell<Language> = const { Cell::new(Language::En) };
    }

    pub fn get() -> Language {
        CURRENT.with(Cell::get)
    }

    pub fn set(lang: Language) {
        CURRENT.with(|cell| cell.set(lang));
    }
}

/// The language in use.
pub fn language() -> Language {
    current::get()
}

/// Puts a language in use. `settings` calls this; nothing else should.
pub fn set_language(lang: Language) {
    current::set(lang);
}

/// Runs `body` with the interface in `lang`, then puts back the language
/// that was in use. For tests, which otherwise see English.
#[cfg(test)]
pub fn with_language<T>(lang: Language, body: impl FnOnce() -> T) -> T {
    let before = language();
    set_language(lang);
    let result = body();
    set_language(before);
    result
}

/// The text of a key in the language in use; English where that language
/// has none; the key itself where English has none either.
pub fn t(key: &str) -> &'static str {
    lookup(key).unwrap_or_else(|| missing(key))
}

fn lookup(key: &str) -> Option<&'static str> {
    catalog(language())
        .get(key)
        .or_else(|| catalog(Language::En).get(key))
        .map(String::as_str)
}

/// A key nothing is written for. The tests find these in the source; a
/// release shows the key, which names where to look.
fn missing(key: &str) -> &'static str {
    // Leaked once per distinct key, which only a bug produces.
    static SEEN: OnceLock<std::sync::Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
    seen.entry(key.to_owned()).or_insert_with(|| {
        tracing::warn!(key, "no text for this key");
        Box::leak(key.to_owned().into_boxed_str())
    })
}

/// The text of a key with its `{name}` holes filled.
pub fn tr(key: &str, args: &[(&str, &dyn Display)]) -> String {
    fill(t(key), args)
}

/// The text of a key that depends on a count: `key.one`, `key.other`...
/// as the language in use counts (see [`category`]). `{n}` is the count
/// itself, in every form.
pub fn plural(key: &str, n: u64, args: &[(&str, &dyn Display)]) -> String {
    let form = category(language(), n);
    let text = lookup(&format!("{key}.{form}"))
        .or_else(|| lookup(&format!("{key}.other")))
        .unwrap_or_else(|| missing(key));
    let mut all: Vec<(&str, &dyn Display)> = vec![("n", &n)];
    all.extend_from_slice(args);
    fill(text, &all)
}

/// The plural category (CLDR) of a count in a language. English and
/// Spanish tell one from the rest; a language that tells more adds its
/// rules here and its forms to the catalog.
pub fn category(lang: Language, n: u64) -> &'static str {
    match lang {
        Language::En | Language::Es if n == 1 => "one",
        Language::En | Language::Es => "other",
    }
}

/// Replaces `{name}` with the argument of that name; `{{` and `}}` are
/// braces. A hole nobody gave an argument for stays as written.
fn fill(text: &str, args: &[(&str, &dyn Display)]) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(at) = rest.find(['{', '}']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(escaped) = tail.strip_prefix("{{").or_else(|| tail.strip_prefix("}}")) {
            out.push_str(&tail[..1]);
            rest = escaped;
        } else if let Some(end) = tail.strip_prefix('{').and_then(|t| t.find('}')) {
            let name = &tail[1..=end];
            match args.iter().find(|(known, _)| *known == name) {
                Some((_, value)) => out.push_str(&value.to_string()),
                None => out.push_str(&tail[..end + 2]),
            }
            rest = &tail[end + 2..];
        } else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn a_text_is_found_in_the_language_in_use() {
        assert_eq!(t("common.language.name"), "English");
        with_language(Language::Es, || {
            assert_eq!(t("common.language.name"), "Español")
        });
        assert_eq!(language(), Language::En, "the language is put back");
    }

    #[test]
    fn what_is_not_translated_is_english_and_what_is_unknown_is_its_key() {
        assert_eq!(t("no.such.key"), "no.such.key");
        let only_english = catalog(Language::En)
            .keys()
            .find(|key| !catalog(Language::Es).contains_key(*key));
        if let Some(key) = only_english {
            with_language(Language::Es, || {
                assert_eq!(t(key), catalog(Language::En)[key])
            });
        }
    }

    #[test]
    fn holes_are_filled_and_braces_can_be_written() {
        let name: &dyn Display = &"Ana";
        assert_eq!(fill("Hi {name}!", &[("name", name)]), "Hi Ana!");
        assert_eq!(
            fill("{{name}} is {name}", &[("name", name)]),
            "{name} is Ana"
        );
        assert_eq!(fill("{other}", &[("name", name)]), "{other}");
        assert_eq!(fill("100% {", &[]), "100% {");
        assert_eq!(fill("naïve {name} ✓", &[("name", name)]), "naïve Ana ✓");
    }

    #[test]
    fn a_count_picks_its_form() {
        assert_eq!(plural("common.test.messages", 1, &[]), "1 message");
        assert_eq!(plural("common.test.messages", 0, &[]), "0 messages");
        assert_eq!(plural("common.test.messages", 5, &[]), "5 messages");
        with_language(Language::Es, || {
            assert_eq!(plural("common.test.messages", 1, &[]), "1 mensaje");
            assert_eq!(plural("common.test.messages", 2, &[]), "2 mensajes");
        });
    }

    #[test]
    fn the_system_language_is_used_unless_one_is_picked() {
        let es = Locale {
            language: "es".into(),
            region: Some("VE".into()),
        };
        let de = Locale {
            language: "de".into(),
            region: None,
        };
        assert_eq!(resolve(LanguageChoice::System, Some(&es)), Language::Es);
        assert_eq!(resolve(LanguageChoice::System, Some(&de)), Language::En);
        assert_eq!(resolve(LanguageChoice::System, None), Language::En);
        assert_eq!(
            resolve(LanguageChoice::Lang(Language::En), Some(&es)),
            Language::En
        );
        assert_eq!(
            resolve(LanguageChoice::Lang(Language::Es), None),
            Language::Es
        );
    }

    #[test]
    fn the_choice_is_written_as_a_word() {
        let json = |choice| serde_json::to_string(&choice).unwrap();
        assert_eq!(json(LanguageChoice::System), "\"system\"");
        assert_eq!(json(LanguageChoice::Lang(Language::Es)), "\"es\"");
        assert_eq!(
            serde_json::from_str::<LanguageChoice>("\"en\"").unwrap(),
            LanguageChoice::Lang(Language::En)
        );
    }

    /// The `{holes}` of a text.
    fn holes(text: &str) -> BTreeSet<String> {
        let text = text.replace("{{", "").replace("}}", "");
        text.split('{')
            .skip(1)
            .filter_map(|part| part.split_once('}'))
            .map(|(name, _)| name.to_owned())
            .collect()
    }

    #[test]
    fn catalogs_agree() {
        let english = catalog(Language::En);
        for lang in Language::ALL {
            let catalog = catalog(lang);
            assert!(!catalog.is_empty(), "{} did not parse", lang.code());
            for (key, text) in catalog {
                let Some(original) = english.get(key) else {
                    panic!("{}: {key} is not in the English catalog", lang.code());
                };
                // `{n}` is the count itself: a form may leave it out
                // ("one message"), the other texts may not change holes.
                let mut mine = holes(text);
                let mut theirs = holes(original);
                if key.contains('.') && (key.ends_with(".one") || key.ends_with(".other")) {
                    mine.remove("n");
                    theirs.remove("n");
                }
                assert_eq!(mine, theirs, "{}: the holes of {key}", lang.code());
            }
        }
        // A count has the form every count falls back to.
        for key in english.keys().filter(|key| key.ends_with(".one")) {
            let base = key.trim_end_matches(".one");
            assert!(
                english.contains_key(&format!("{base}.other")),
                "{base}.other"
            );
        }
        // A file holds the keys of its area, and only those.
        for lang in Language::ALL {
            for (area, source) in AREAS.into_iter().zip(files_of(lang)) {
                for key in parse_catalog(source).unwrap().keys() {
                    assert!(
                        key.starts_with(&format!("{area}.")),
                        "{}/{area}.json: {key} belongs to another area",
                        lang.code()
                    );
                }
            }
        }
        assert_eq!(Language::from_code("ES"), Some(Language::Es));
        assert_eq!(Language::ALL.map(Language::code), ["en", "es"]);
    }

    /// Every `i18n::t("key")`, `i18n::tr("key", ..)` and
    /// `i18n::plural("key", ..)` in the sources names a key English has.
    #[test]
    fn every_key_asked_for_exists() {
        fn walk(dir: &Path, found: &mut Vec<(String, String)>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    // The tests of this module quote calls that fail.
                    if path.ends_with("i18n.rs") {
                        continue;
                    }
                    let source = std::fs::read_to_string(&path).unwrap();
                    for call in ["i18n::t(\"", "i18n::tr(\"", "i18n::plural(\""] {
                        for piece in source.split(call).skip(1) {
                            if let Some((key, _)) = piece.split_once('"') {
                                found.push((key.to_owned(), path.display().to_string()));
                            }
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        walk(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut found,
        );
        let english = catalog(Language::En);
        for (key, file) in found {
            assert!(
                english.contains_key(&key) || english.contains_key(&format!("{key}.other")),
                "{file}: {key} is not in the English catalog"
            );
        }
    }
}
