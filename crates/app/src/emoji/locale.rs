//! The language the emoji are searched in: the operating system's, unless
//! the user picked another one.
//!
//! English is searched whatever this says; this decides the second
//! language and the one the names are shown in.

use super::data::Lang;
use serde::{Deserialize, Serialize};

/// A locale, as far as it matters here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locale {
    /// ISO 639 language, lower case (`es`).
    pub language: String,
    /// ISO 3166 region, upper case (`VE`), or a UN M.49 area (`419`).
    pub region: Option<String>,
}

/// Reads a locale as POSIX writes it (`es_VE.UTF-8@euro`) or as BCP 47
/// does (`es-419`, `pt-BR`, `zh-Hant-TW`). `None` for `C`, `POSIX` and
/// anything that does not start with a language.
pub fn parse(tag: &str) -> Option<Locale> {
    // The encoding and the modifier say nothing about the language.
    let tag = tag.split(['.', '@']).next()?.trim();
    if tag.is_empty() || tag.eq_ignore_ascii_case("C") || tag.eq_ignore_ascii_case("POSIX") {
        return None;
    }
    let mut parts = tag.split(['_', '-']);
    let language = parts.next()?.to_ascii_lowercase();
    if !(2..=3).contains(&language.len()) || !language.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    // After the language: an optional script (four letters), then the
    // region (two letters or three digits).
    let region = parts
        .find(|part| {
            (part.len() == 2 && part.chars().all(|ch| ch.is_ascii_alphabetic()))
                || (part.len() == 3 && part.chars().all(|ch| ch.is_ascii_digit()))
        })
        .map(str::to_ascii_uppercase);
    Some(Locale { language, region })
}

/// The locale of messages on a POSIX system, from its environment, in the
/// order the C library and gettext read it: `LC_ALL`, then `LC_MESSAGES`,
/// then `LANG`; and `LANGUAGE` (a list, GNU's way of asking for another
/// language than the locale's) before all three, unless the locale is `C`.
// Asked on Linux and the other Unixes only; tested everywhere.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Option<Locale> {
    let set = |name: &str| var(name).filter(|value| !value.trim().is_empty());
    let locale = set("LC_ALL")
        .or_else(|| set("LC_MESSAGES"))
        .or_else(|| set("LANG"))
        .as_deref()
        .and_then(parse);
    if locale.is_some() {
        if let Some(first) = set("LANGUAGE")
            .as_deref()
            .and_then(|list| list.split(':').find_map(parse))
        {
            return Some(first);
        }
    }
    locale
}

/// The operating system's language: the environment on Linux and the other
/// Unixes, the user's preferred languages on macOS and Windows.
pub fn system() -> Option<Locale> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(locale) = from_env(|name| std::env::var(name).ok()) {
            return Some(locale);
        }
    }
    sys_locale::get_locale().as_deref().and_then(parse)
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
    Lang(Lang),
}

/// The language the emoji are searched in, beside English.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchLanguage {
    pub lang: Lang,
    /// Names are shown as the region says them where it has its own:
    /// Latin America's for Spanish, Portugal's for Portuguese.
    pub regional: bool,
}

/// Regions whose Portuguese is Portugal's (CLDR: children of `pt_PT`).
const PORTUGAL: [&str; 12] = [
    "PT", "AO", "CH", "CV", "GQ", "GW", "LU", "MO", "MZ", "ST", "TL", "FR",
];
/// Regions whose Spanish is Spain's (CLDR: `es` itself and its children
/// outside Latin America).
const SPAIN: [&str; 5] = ["ES", "EA", "IC", "GQ", "PH"];

/// The search language for a choice on a system. A language there are no
/// names for is English.
pub fn resolve(choice: LanguageChoice, system: Option<&Locale>) -> SearchLanguage {
    let lang = match choice {
        LanguageChoice::Lang(lang) => lang,
        LanguageChoice::System => system
            .and_then(|locale| Lang::from_code(&locale.language))
            .unwrap_or(Lang::En),
    };
    // The region counts when the system speaks the language searched in.
    let region = system
        .filter(|locale| Lang::from_code(&locale.language) == Some(lang))
        .map(|locale| locale.region.as_deref());
    let regional = match (lang, region) {
        // Spanish is Latin America's unless the system says Spain (or
        // says Spanish and no region, which CLDR reads as Spain).
        (Lang::Es, Some(None)) => false,
        (Lang::Es, Some(Some(region))) => !SPAIN.contains(&region),
        (Lang::Es, None) => true,
        // Portuguese is Brazil's unless the system says Portugal.
        (Lang::Pt, Some(Some(region))) => PORTUGAL.contains(&region),
        _ => false,
    };
    SearchLanguage { lang, regional }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn locale(language: &str, region: Option<&str>) -> Locale {
        Locale {
            language: language.into(),
            region: region.map(Into::into),
        }
    }

    fn env(pairs: &[(&str, &str)]) -> Option<Locale> {
        let vars: HashMap<String, String> = pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        from_env(|name| vars.get(name).cloned())
    }

    #[test]
    fn locales_are_read_as_posix_and_as_bcp_47_write_them() {
        assert_eq!(parse("es_VE.UTF-8"), Some(locale("es", Some("VE"))));
        assert_eq!(parse("en_US.UTF-8"), Some(locale("en", Some("US"))));
        assert_eq!(
            parse("ca_ES.UTF-8@valencia"),
            Some(locale("ca", Some("ES")))
        );
        assert_eq!(parse("es-419"), Some(locale("es", Some("419"))));
        assert_eq!(parse("pt-BR"), Some(locale("pt", Some("BR"))));
        assert_eq!(parse("zh-Hant-TW"), Some(locale("zh", Some("TW"))));
        assert_eq!(parse("sr_RS@latin"), Some(locale("sr", Some("RS"))));
        assert_eq!(parse("fr"), Some(locale("fr", None)));
        assert_eq!(parse("HI_in"), Some(locale("hi", Some("IN"))));
        for nothing in ["C", "POSIX", "C.UTF-8", "", "  ", "1234", "_ES"] {
            assert_eq!(parse(nothing), None, "{nothing:?}");
        }
    }

    #[test]
    fn the_environment_is_read_in_the_c_librarys_order() {
        // LC_ALL wins over LC_MESSAGES, which wins over LANG.
        assert_eq!(
            env(&[
                ("LC_ALL", "de_DE.UTF-8"),
                ("LC_MESSAGES", "es_VE.UTF-8"),
                ("LANG", "en_US.UTF-8")
            ]),
            Some(locale("de", Some("DE")))
        );
        assert_eq!(
            env(&[("LC_MESSAGES", "es_VE.UTF-8"), ("LANG", "en_US.UTF-8")]),
            Some(locale("es", Some("VE")))
        );
        assert_eq!(
            env(&[("LANG", "en_US.UTF-8")]),
            Some(locale("en", Some("US")))
        );
        // The formats of numbers and dates say nothing about messages.
        assert_eq!(
            env(&[("LC_TIME", "es_VE.UTF-8"), ("LANG", "en_US.UTF-8")]),
            Some(locale("en", Some("US")))
        );
        // An empty variable is as good as none.
        assert_eq!(
            env(&[("LC_ALL", ""), ("LANG", "pt_BR.UTF-8")]),
            Some(locale("pt", Some("BR")))
        );
        // LANGUAGE asks for other languages than the locale's, in order.
        assert_eq!(
            env(&[("LANGUAGE", "es_VE:es:en"), ("LANG", "en_US.UTF-8")]),
            Some(locale("es", Some("VE")))
        );
        // It is not looked at in the C locale, or without any.
        assert_eq!(env(&[("LANGUAGE", "es"), ("LANG", "C")]), None);
        assert_eq!(env(&[("LANGUAGE", "es")]), None);
        assert_eq!(env(&[]), None);
    }

    #[test]
    fn the_system_language_is_used_unless_one_is_picked() {
        let system = LanguageChoice::System;
        let english = SearchLanguage {
            lang: Lang::En,
            regional: false,
        };
        // This machine: en_US.
        assert_eq!(resolve(system, Some(&locale("en", Some("US")))), english);
        // No locale, or one there are no names for: English.
        assert_eq!(resolve(system, None), english);
        assert_eq!(resolve(system, Some(&locale("ja", Some("JP")))), english);

        // A Spanish system in Latin America says it the region's way; in
        // Spain, Spain's.
        let spanish = |regional| SearchLanguage {
            lang: Lang::Es,
            regional,
        };
        assert_eq!(
            resolve(system, Some(&locale("es", Some("VE")))),
            spanish(true)
        );
        assert_eq!(
            resolve(system, Some(&locale("es", Some("419")))),
            spanish(true)
        );
        assert_eq!(
            resolve(system, Some(&locale("es", Some("US")))),
            spanish(true)
        );
        assert_eq!(
            resolve(system, Some(&locale("es", Some("ES")))),
            spanish(false)
        );
        assert_eq!(resolve(system, Some(&locale("es", None))), spanish(false));
        // Spanish picked on a system in English: Latin America's.
        assert_eq!(
            resolve(
                LanguageChoice::Lang(Lang::Es),
                Some(&locale("en", Some("US")))
            ),
            spanish(true)
        );
        assert_eq!(
            resolve(
                LanguageChoice::Lang(Lang::Es),
                Some(&locale("es", Some("ES")))
            ),
            spanish(false)
        );

        // Portuguese is Brazil's unless the system is Portugal's.
        let portuguese = |regional| SearchLanguage {
            lang: Lang::Pt,
            regional,
        };
        assert_eq!(
            resolve(system, Some(&locale("pt", Some("BR")))),
            portuguese(false)
        );
        assert_eq!(
            resolve(system, Some(&locale("pt", Some("PT")))),
            portuguese(true)
        );
        assert_eq!(
            resolve(system, Some(&locale("pt", None))),
            portuguese(false)
        );
        assert_eq!(
            resolve(
                LanguageChoice::Lang(Lang::Pt),
                Some(&locale("en", Some("US")))
            ),
            portuguese(false)
        );
        // A pick wins over the system.
        assert_eq!(
            resolve(
                LanguageChoice::Lang(Lang::Hi),
                Some(&locale("es", Some("VE")))
            )
            .lang,
            Lang::Hi
        );
    }

    #[test]
    fn the_choice_is_written_as_a_word() {
        let json = |choice: LanguageChoice| serde_json::to_string(&choice).unwrap();
        assert_eq!(json(LanguageChoice::System), r#""system""#);
        assert_eq!(json(LanguageChoice::Lang(Lang::Es)), r#""es""#);
        for choice in [LanguageChoice::System, LanguageChoice::Lang(Lang::Pt)] {
            assert_eq!(
                serde_json::from_str::<LanguageChoice>(&json(choice)).unwrap(),
                choice
            );
        }
    }
}
