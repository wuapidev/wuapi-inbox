//! The operating system's language, as far as the application asks it.
//!
//! Two things read it: the language of the interface ([`crate::i18n`])
//! and the one the emoji are searched in ([`crate::emoji::locale`]). They
//! are separate settings, and both default to what this finds.

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
}
