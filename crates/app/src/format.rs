//! Small text helpers: times, initials, sizes.

use chrono::{DateTime, Datelike, Local, TimeZone};
use client_provider::Timestamp;

fn local(ts: Timestamp) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ts.as_millis())
        .single()
        .unwrap_or_else(Local::now)
}

/// Whole days between the local dates of `ts` and `now`.
fn days_ago(ts: Timestamp, now: Timestamp) -> i64 {
    (local(now).date_naive() - local(ts).date_naive()).num_days()
}

/// The time of day, as shown inside a bubble: `14:05`.
pub fn clock(ts: Timestamp) -> String {
    local(ts).format("%H:%M").to_string()
}

/// The time shown on a chat row: the time today, then "Yesterday", the
/// weekday within the last week, and the date before that.
pub fn list_time(ts: Timestamp, now: Timestamp) -> String {
    let at = local(ts);
    match days_ago(ts, now) {
        i64::MIN..=0 => at.format("%H:%M").to_string(),
        1 => "Yesterday".to_owned(),
        2..=6 => at.format("%A").to_string(),
        _ => at.format("%-d/%-m/%Y").to_string(),
    }
}

/// The label of a day separator in a conversation.
pub fn day_label(ts: Timestamp, now: Timestamp) -> String {
    let at = local(ts);
    match days_ago(ts, now) {
        i64::MIN..=0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => at.format("%A").to_string(),
        _ if at.year() == local(now).year() => at.format("%-d %B").to_string(),
        _ => at.format("%-d %B %Y").to_string(),
    }
}

/// A moment as part of a file's name: `2026-10-01 15.04.05`, with nothing
/// a file system minds.
pub fn file_stamp(ts: Timestamp) -> String {
    local(ts).format("%Y-%m-%d %H.%M.%S").to_string()
}

/// A date in full, for "created on": `1 September 2026`.
pub fn date(ts: Timestamp) -> String {
    local(ts).format("%-d %B %Y").to_string()
}

/// A phone number as people write it: the country code, then the rest in
/// groups (`+58 424 555 0199`). What is not a number in E.164 with a known
/// country code is returned as it is.
pub fn phone(number: &str) -> String {
    client_core::phone::format(number)
}

/// True when two timestamps fall on the same local day.
pub fn same_day(a: Timestamp, b: Timestamp) -> bool {
    local(a).date_naive() == local(b).date_naive()
}

/// Up to two initials for an avatar without a picture.
pub fn initials(name: &str) -> String {
    let mut letters = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .flat_map(char::to_uppercase);
    let first = letters.next();
    let second = letters.next();
    match (first, second) {
        (Some(a), Some(b)) => format!("{a}{b}"),
        (Some(a), None) => a.to_string(),
        _ => "?".to_owned(),
    }
}

/// A duration as `m:ss`.
pub fn duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// A file size in the largest sensible unit.
pub fn file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = bytes as f64;
    if bytes < KB {
        format!("{bytes:.0} B")
    } else if bytes < KB * KB {
        format!("{:.0} kB", bytes / KB)
    } else {
        format!("{:.1} MB", bytes / KB / KB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_take_the_first_two_words() {
        assert_eq!(initials("Valentina Rojas"), "VR");
        assert_eq!(initials("Mamá"), "M");
        assert_eq!(initials("Weekend football ⚽"), "WF");
        assert_eq!(initials("+58 424 5550199"), "54");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("álvaro núñez"), "ÁN");
    }

    #[test]
    fn phone_numbers_are_grouped_and_anything_else_is_left_alone() {
        assert_eq!(phone("+584245550199"), "+58 424 555 0199");
        assert_eq!(phone("+14155550142"), "+1 415 555 0142");
        assert_eq!(phone("+56955501234"), "+56 955 501 234");
        assert_eq!(phone("lid:200055501000001"), "lid:200055501000001");
        assert_eq!(phone("contact:ana"), "contact:ana");
        assert_eq!(phone("+99912"), "+99912");
    }

    #[test]
    fn durations_and_sizes() {
        assert_eq!(duration(5), "0:05");
        assert_eq!(duration(95), "1:35");
        assert_eq!(file_size(512), "512 B");
        assert_eq!(file_size(80_000), "78 kB");
        assert_eq!(file_size(4_200_000), "4.0 MB");
    }

    #[test]
    fn day_labels_are_relative() {
        let now = Timestamp::now();
        let day = 24 * 60 * 60 * 1000;
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(
            day_label(Timestamp::from_millis(now.as_millis() - day), now),
            "Yesterday"
        );
        assert_eq!(
            list_time(Timestamp::from_millis(now.as_millis() - day), now),
            "Yesterday"
        );
        assert!(same_day(now, now));
        assert!(!same_day(
            Timestamp::from_millis(now.as_millis() - 2 * day),
            now
        ));
    }
}
