//! Text as a search compares it: lower case, without accents, so that
//! "rene" finds "René" and "Lomeli" finds "Lomelí".
//!
//! The store gives SQLite the same function (`fold(text)`), so what a
//! query keeps and what the interface ranks agree.

/// `text` in lower case with its letters' accents taken off: "Ñandú" is
/// "nandu", "Straße" is "strasse". Anything that is not a Latin letter
/// with a mark is only lowered.
pub fn fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        match base(c) {
            Some(plain) => folded.push_str(plain),
            None => folded.push(c),
        }
    }
    folded
}

/// The letters a lower-case Latin letter with a mark stands for.
fn base(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ǎ' => "a",
        'æ' => "ae",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ǐ' => "i",
        'ĵ' => "j",
        'ķ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ǒ' => "o",
        'œ' => "oe",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'š' | 'ș' => "s",
        'ß' => "ss",
        'ţ' | 'ť' | 'ŧ' | 'ț' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ǔ' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ŷ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::fold;

    #[test]
    fn accents_and_case_do_not_matter() {
        assert_eq!(fold("René Lomelí"), "rene lomeli");
        assert_eq!(fold("ÑANDÚ"), "nandu");
        assert_eq!(fold("Straße"), "strasse");
        assert_eq!(fold("São João"), "sao joao");
        assert_eq!(fold("Zoë Łukasz Øre"), "zoe lukasz ore");
        // What has no accent is only lowered; the rest is kept.
        assert_eq!(fold("Lisbon trip ✈️ 2026"), "lisbon trip ✈️ 2026");
        assert_eq!(fold("Сергей"), "сергей");
    }
}
