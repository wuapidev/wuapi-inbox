//! Phone numbers as people write them, and where a number is from.

/// Country calling codes and the country each belongs to, for guessing
/// where a number is from. The shared codes (`+1`, `+7`) name their
/// largest country; the guess only preselects a place the user can change.
const CALLING_CODES: &[(&str, &str)] = &[
    ("1", "US"),
    ("7", "RU"),
    ("20", "EG"),
    ("27", "ZA"),
    ("30", "GR"),
    ("31", "NL"),
    ("32", "BE"),
    ("33", "FR"),
    ("34", "ES"),
    ("351", "PT"),
    ("353", "IE"),
    ("39", "IT"),
    ("40", "RO"),
    ("41", "CH"),
    ("43", "AT"),
    ("44", "GB"),
    ("45", "DK"),
    ("46", "SE"),
    ("47", "NO"),
    ("48", "PL"),
    ("49", "DE"),
    ("51", "PE"),
    ("52", "MX"),
    ("53", "CU"),
    ("54", "AR"),
    ("55", "BR"),
    ("56", "CL"),
    ("57", "CO"),
    ("58", "VE"),
    ("591", "BO"),
    ("593", "EC"),
    ("595", "PY"),
    ("598", "UY"),
    ("502", "GT"),
    ("503", "SV"),
    ("504", "HN"),
    ("505", "NI"),
    ("506", "CR"),
    ("507", "PA"),
    ("60", "MY"),
    ("61", "AU"),
    ("62", "ID"),
    ("63", "PH"),
    ("64", "NZ"),
    ("65", "SG"),
    ("66", "TH"),
    ("81", "JP"),
    ("82", "KR"),
    ("84", "VN"),
    ("86", "CN"),
    ("90", "TR"),
    ("91", "IN"),
    ("92", "PK"),
    ("94", "LK"),
    ("234", "NG"),
    ("254", "KE"),
    ("880", "BD"),
    ("966", "SA"),
    ("971", "AE"),
    ("972", "IL"),
];

/// The country calling code a number starts with, when it is a known one.
pub fn calling_code(phone: &str) -> Option<&'static str> {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    CALLING_CODES
        .iter()
        .filter(|(code, _)| digits.starts_with(code))
        .max_by_key(|(code, _)| code.len())
        .map(|(code, _)| *code)
}

/// The calling code of a country, from its ISO code (`VE` is `58`). The
/// shared codes answer for the country they name here.
pub fn calling_code_of(country: &str) -> Option<&'static str> {
    CALLING_CODES
        .iter()
        .find(|(_, known)| known.eq_ignore_ascii_case(country))
        .map(|(code, _)| *code)
}

/// The country a phone number is most likely from, as an ISO code. The
/// longest calling code that matches wins.
pub fn country_of(phone: &str) -> Option<&'static str> {
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    CALLING_CODES
        .iter()
        .filter(|(code, _)| digits.starts_with(code))
        .max_by_key(|(code, _)| code.len())
        .map(|(_, country)| *country)
}

/// A phone number as people write it: the country code, then the rest in
/// groups (`+58 424 555 0199`). What is not a number in E.164 with a known
/// country code is returned as it is.
pub fn format(number: &str) -> String {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    let Some(code) = calling_code(number).filter(|_| is_e164(number)) else {
        return number.to_owned();
    };
    let rest = &digits[code.len()..];
    let sizes: &[usize] = match rest.len() {
        7 => &[3, 4],
        8 => &[4, 4],
        9 => &[3, 3, 3],
        10 => &[3, 3, 4],
        11 => &[3, 4, 4],
        _ => return number.to_owned(),
    };
    let mut out = format!("+{code}");
    let mut at = 0;
    for size in sizes {
        out.push(' ');
        out.push_str(&rest[at..at + size]);
        at += size;
    }
    out
}

/// Whether `text` is a phone number in E.164: `+` and digits, nothing
/// else. An id of that shape is the person's number.
pub fn is_e164(text: &str) -> bool {
    text.strip_prefix('+')
        .is_some_and(|rest| rest.len() >= 6 && rest.chars().all(|c| c.is_ascii_digit()))
}
