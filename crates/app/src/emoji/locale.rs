//! The language the emoji are searched in: the operating system's, unless
//! the user picked another one.
//!
//! English is searched whatever this says; this decides the second
//! language and the one the names are shown in.

use super::data::Lang;
pub use crate::locale::{system, Locale};
use serde::{Deserialize, Serialize};

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

    fn locale(language: &str, region: Option<&str>) -> Locale {
        Locale {
            language: language.into(),
            region: region.map(Into::into),
        }
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
