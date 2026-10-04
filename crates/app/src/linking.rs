//! Linking a number: the parts that are decisions, not drawing.
//!
//! * which place a new number should connect from by default (its own
//!   country, told from a phone number);
//! * how the list of places narrows as the user types;
//! * what each step of linking says, and when waiting gives up;
//! * the number a pairing code is asked for, and how the code is shown;
//! * the placeholder drawn where the QR code will be.
//!
//! The screen itself is `ui/numbers.rs`.

use client_provider::{LinkPlace, LinkStep, Timestamp};
use std::time::Duration;

/// How often the linking screen asks where linking stands.
pub const POLL: Duration = Duration::from_secs(2);

/// How long the linking screen waits for the phone before it stops asking
/// and offers to try again. A QR code that nobody scans is refreshed for a
/// while and then given up on by the provider too.
pub const GIVE_UP: Duration = Duration::from_secs(15 * 60);

use client_core::phone::{calling_code, calling_code_of, country_of};

/// The place to preselect: the first one in the country of the number
/// being linked, or else of a number already linked. `None` when nothing
/// points at a country: the user chooses.
pub fn default_place<'a>(
    places: &'a [LinkPlace],
    phones: impl IntoIterator<Item = &'a str>,
) -> Option<&'a LinkPlace> {
    phones
        .into_iter()
        .filter_map(country_of)
        .find_map(|country| places.iter().find(|place| place.country == country))
}

/// The places matching what was typed: by city, country or country code,
/// ignoring case. Everything when nothing was typed.
pub fn matching_places<'a>(places: &'a [LinkPlace], typed: &str) -> Vec<&'a LinkPlace> {
    let typed = typed.trim().to_lowercase();
    places
        .iter()
        .filter(|place| {
            typed.is_empty()
                || place.city_name.to_lowercase().contains(&typed)
                || place.country_name.to_lowercase().contains(&typed)
                || place.city.to_lowercase().contains(&typed)
                || place.country.to_lowercase() == typed
        })
        .collect()
}

/// A place as one line: `Maracaibo, Venezuela`.
pub fn place_name(place: &LinkPlace) -> String {
    format!("{}, {}", place.city_name, place.country_name)
}

/// What the linking screen says at a step: a title and what to do.
pub fn step_text(step: &LinkStep) -> (&'static str, &'static str) {
    match step {
        LinkStep::Starting => (
            "Starting the session",
            "The code appears here in a few seconds.",
        ),
        LinkStep::Scan { .. } => (
            "Scan this code with the phone",
            "On the phone: WhatsApp > Linked devices > Link a device. The code \
             refreshes by itself.",
        ),
        LinkStep::TypeCode { .. } => (
            "Type this code on the phone",
            "On the phone: WhatsApp > Linked devices > Link a device > Link with \
             phone number instead.",
        ),
        LinkStep::Finishing => ("Finishing", "The phone accepted. This takes a few seconds."),
        LinkStep::Linked => ("Linked", "The number is connected."),
        LinkStep::Stopped { .. } => ("Linking stopped", ""),
    }
}

/// What the screen says while the number to link with a code is typed.
pub const PHONE_TITLE: &str = "Link with phone number";

/// What to do on the phone with a pairing code, one step to a line.
pub const CODE_STEPS: [&str; 3] = [
    "On the phone, open WhatsApp > Linked devices > Link a device.",
    "Tap \"Link with phone number instead\".",
    "Type the code shown here.",
];

/// The number a pairing code is asked for, in E.164, or what is wrong with
/// what was typed. Spaces, dashes, dots and brackets are how people write
/// numbers; anything else is not a number.
pub fn pairing_phone(typed: &str) -> Result<String, &'static str> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Type the number to link, with its country code.");
    }
    let written = |c: char| c.is_ascii_digit() || matches!(c, ' ' | '-' | '.' | '(' | ')');
    let rest = typed.strip_prefix('+').unwrap_or(typed);
    if !rest.chars().all(written) {
        return Err("A phone number has only digits, after the + of its country code.");
    }
    let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
    if digits.starts_with('0') {
        return Err("Start with the country code, without zeros in front: +58 412 123 4567.");
    }
    if digits.len() < 8 {
        return Err("That number is too short. Include the country code: +58 412 123 4567.");
    }
    if digits.len() > 15 {
        return Err("That number is too long: a phone number has at most 15 digits.");
    }
    Ok(format!("+{digits}"))
}

/// What to start the number with: the country code of the place the
/// number connects from, or else of a number already linked (`+58 `).
pub fn phone_prefix<'a>(
    place: Option<&LinkPlace>,
    phones: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    place
        .and_then(|place| calling_code_of(&place.country))
        .or_else(|| phones.into_iter().find_map(calling_code))
        .map(|code| format!("+{code} "))
}

/// A pairing code in the groups it is read in: `ABCD1234` and `abcd-1234`
/// are `["ABCD", "1234"]`. A code of another length keeps its own groups.
pub fn code_groups(code: &str) -> Vec<String> {
    let clean: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if clean.len() == 8 {
        return vec![clean[..4].to_owned(), clean[4..].to_owned()];
    }
    code.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|group| !group.is_empty())
        .map(str::to_uppercase)
        .collect()
}

/// How a pairing code is written and copied: `ABCD-1234`.
pub fn code_text(code: &str) -> String {
    code_groups(code).join("-")
}

/// How long a code still works, as `2:05`. `None` once it is past its
/// time; a code with no time is never past it.
pub fn code_life(expires_at: Option<Timestamp>, now: Timestamp) -> Option<Option<String>> {
    let Some(expires_at) = expires_at else {
        return Some(None);
    };
    let left = expires_at.as_millis() - now.as_millis();
    if left <= 0 {
        return None;
    }
    let seconds = (left + 999) / 1000;
    Some(Some(format!("{}:{:02}", seconds / 60, seconds % 60)))
}

/// The modules along one side of the placeholder drawn where the QR code
/// will be (a version 2 code has as many).
pub const SKELETON_SIDE: usize = 25;
/// The modules along one side of a finder square.
const FINDER: usize = 7;
/// The modules left clear in the middle, for the mark.
const SKELETON_CLEAR: usize = 9;

/// One module of the placeholder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkeletonModule {
    /// Nothing drawn.
    Clear,
    /// Part of a finder square: drawn firmly, and it does not shimmer.
    Finder,
    /// One of the modules in between.
    Dot,
}

/// What the placeholder draws at a module: the three finder squares in
/// their corners, a clearing in the middle and, around them, a pattern
/// that is the same every time. It looks like a QR code and is not one:
/// there is nothing in it to scan.
pub fn skeleton_module(x: usize, y: usize) -> SkeletonModule {
    let side = SKELETON_SIDE;
    if x >= side || y >= side {
        return SkeletonModule::Clear;
    }
    let far = side - FINDER;
    for (left, top) in [(0, 0), (far, 0), (0, far)] {
        // The square and the quiet module around it.
        let near_x = x + 1 >= left && x <= left + FINDER;
        let near_y = y + 1 >= top && y <= top + FINDER;
        if !(near_x && near_y) {
            continue;
        }
        let inside = (left..left + FINDER).contains(&x) && (top..top + FINDER).contains(&y);
        if !inside {
            return SkeletonModule::Clear;
        }
        let ring = (x - left)
            .abs_diff(FINDER / 2)
            .max((y - top).abs_diff(FINDER / 2));
        return if ring == 2 {
            SkeletonModule::Clear
        } else {
            SkeletonModule::Finder
        };
    }
    let clear = (side - SKELETON_CLEAR) / 2..(side + SKELETON_CLEAR) / 2;
    if clear.contains(&x) && clear.contains(&y) {
        return SkeletonModule::Clear;
    }
    // A fixed scramble of the position: about half of the modules.
    let mixed = (x as u32)
        .wrapping_mul(0x9e37_79b1)
        .wrapping_add((y as u32).wrapping_mul(0x85eb_ca6b));
    let mixed = (mixed ^ (mixed >> 15)).wrapping_mul(0x2c1b_3c6d);
    if (mixed ^ (mixed >> 13)) % 100 < 46 {
        SkeletonModule::Dot
    } else {
        SkeletonModule::Clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(country: &str, country_name: &str, city: &str, city_name: &str) -> LinkPlace {
        LinkPlace {
            country: country.into(),
            country_name: country_name.into(),
            city: city.into(),
            city_name: city_name.into(),
        }
    }

    fn places() -> Vec<LinkPlace> {
        vec![
            place("CL", "Chile", "santiago", "Santiago"),
            place("US", "United States", "miami", "Miami"),
            place("VE", "Venezuela", "caracas", "Caracas"),
            place("VE", "Venezuela", "maracaibo", "Maracaibo"),
        ]
    }

    #[test]
    fn a_number_says_where_it_is_from() {
        assert_eq!(country_of("+58 424 555 0199"), Some("VE"));
        assert_eq!(country_of("56955501234"), Some("CL"));
        assert_eq!(country_of("+1 415 555 0142"), Some("US"));
        // The longest code wins: +593 is Ecuador, not +59x of anyone else.
        assert_eq!(country_of("+593 99 123 4567"), Some("EC"));
        assert_eq!(country_of("+999 1"), None);
        assert_eq!(country_of(""), None);
    }

    #[test]
    fn the_default_place_follows_the_number_then_the_numbers_already_linked() {
        let places = places();
        // The number being linked decides.
        let chosen = default_place(&places, ["+56 9 5550 1234", "+58 424 555 0199"]).unwrap();
        assert_eq!(chosen.city, "santiago");
        // No number typed: the country of a number already linked.
        let chosen = default_place(&places, ["+58 424 555 0199"]).unwrap();
        assert_eq!(chosen.country, "VE");
        // A country with no place is skipped for the next hint.
        let chosen = default_place(&places, ["+91 98 7654 3210", "+1 415 555 0142"]).unwrap();
        assert_eq!(chosen.country, "US");
        // Nothing to go by: the user chooses.
        assert_eq!(default_place(&places, []), None);
    }

    #[test]
    fn typing_narrows_the_places() {
        let places = places();
        assert_eq!(matching_places(&places, "").len(), 4);
        assert_eq!(matching_places(&places, "venez").len(), 2);
        assert_eq!(matching_places(&places, " MARA ")[0].city, "maracaibo");
        assert_eq!(matching_places(&places, "us")[0].city, "miami");
        assert!(matching_places(&places, "zzz").is_empty());
        assert_eq!(place_name(&places[3]), "Maracaibo, Venezuela");
    }

    #[test]
    fn the_number_for_a_code_is_international_or_it_is_said_why() {
        assert_eq!(
            pairing_phone(" +58 412-123.4567 "),
            Ok("+584121234567".into())
        );
        assert_eq!(pairing_phone("56 (9) 5550 1234"), Ok("+56955501234".into()));
        assert!(pairing_phone("").unwrap_err().contains("country code"));
        assert!(pairing_phone("   ").is_err());
        assert!(pairing_phone("maria").unwrap_err().contains("only digits"));
        assert!(pairing_phone("+58 412 12x 4567").is_err());
        assert!(pairing_phone("0412 123 4567")
            .unwrap_err()
            .contains("zeros"));
        assert!(pairing_phone("+58 412").unwrap_err().contains("too short"));
        assert!(pairing_phone("+1234567890123456")
            .unwrap_err()
            .contains("too long"));
        // A plus in the middle is not a way of writing a number.
        assert!(pairing_phone("+58 +412 123 4567").is_err());
    }

    #[test]
    fn the_number_starts_with_the_code_of_the_place_or_of_a_known_number() {
        let places = places();
        assert_eq!(
            phone_prefix(Some(&places[3]), ["+56 9 5550 1234"]).as_deref(),
            Some("+58 ")
        );
        assert_eq!(
            phone_prefix(None, ["nobody", "+56 9 5550 1234"]).as_deref(),
            Some("+56 ")
        );
        // A place whose code is not known falls back on the numbers.
        let elsewhere = place("ZZ", "Nowhere", "none", "None");
        assert_eq!(
            phone_prefix(Some(&elsewhere), ["+1 415 555 0142"]).as_deref(),
            Some("+1 ")
        );
        assert_eq!(phone_prefix(None, []), None);
    }

    #[test]
    fn a_code_is_shown_in_two_groups_of_four() {
        assert_eq!(code_groups("ABCD1234"), ["ABCD", "1234"]);
        assert_eq!(code_groups("abcd-1234"), ["ABCD", "1234"]);
        assert_eq!(code_groups("AB CD 12 34"), ["ABCD", "1234"]);
        assert_eq!(code_text("abcd1234"), "ABCD-1234");
        // Not eight characters: as the provider grouped it.
        assert_eq!(code_groups("ABC-DEF-123"), ["ABC", "DEF", "123"]);
        assert_eq!(code_text("12345"), "12345");
        assert!(code_groups("").is_empty());
    }

    #[test]
    fn a_code_counts_down_and_then_it_is_over() {
        let now = Timestamp::from_millis(1_000_000);
        let at = |ms: i64| Some(Timestamp::from_millis(1_000_000 + ms));
        assert_eq!(code_life(at(125_000), now), Some(Some("2:05".into())));
        assert_eq!(code_life(at(400), now), Some(Some("0:01".into())));
        assert_eq!(code_life(at(0), now), None);
        assert_eq!(code_life(at(-5_000), now), None);
        assert_eq!(code_life(None, now), Some(None));
    }

    #[test]
    fn the_placeholder_looks_like_a_code_and_is_the_same_every_time() {
        use SkeletonModule::{Clear, Dot, Finder};
        let side = SKELETON_SIDE;
        // The three finder squares: a ring, a gap, a 3x3 heart.
        for (left, top) in [(0, 0), (side - 7, 0), (0, side - 7)] {
            assert_eq!(skeleton_module(left, top), Finder);
            assert_eq!(skeleton_module(left + 6, top + 6), Finder);
            assert_eq!(skeleton_module(left + 1, top + 1), Clear);
            assert_eq!(skeleton_module(left + 3, top + 3), Finder);
        }
        // A quiet module around each, and no fourth square.
        assert_eq!(skeleton_module(7, 3), Clear);
        assert_eq!(skeleton_module(3, 7), Clear);
        assert_eq!(skeleton_module(side - 8, 0), Clear);
        assert_ne!(skeleton_module(side - 1, side - 1), Finder);
        // The middle is clear for the mark.
        for offset in 0..9 {
            assert_eq!(skeleton_module(8 + offset, 12), Clear);
            assert_eq!(skeleton_module(12, 8 + offset), Clear);
        }
        assert_eq!(skeleton_module(side, 0), Clear);
        // About half of what is left is drawn, the same on every call.
        let all = || {
            (0..side)
                .flat_map(|y| (0..side).map(move |x| skeleton_module(x, y)))
                .collect::<Vec<_>>()
        };
        let first = all();
        assert_eq!(first, all());
        let dots = first.iter().filter(|module| **module == Dot).count();
        assert!((120..260).contains(&dots), "{dots} dots");
        assert_eq!(
            first.iter().filter(|module| **module == Finder).count(),
            3 * 33
        );
    }
}
