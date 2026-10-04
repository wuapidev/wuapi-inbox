//! Calendar events: when one takes place, in words, and the `.ics` file
//! that hands it to the system's calendar.

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use client_provider::{CalendarEvent, Timestamp};

fn local(ts: Timestamp) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ts.as_millis()).single()
}

/// When an event takes place, in the local timezone:
/// `Sat 3 Oct 2026 · 20:00 – 22:00`, or with both days when it ends on
/// another one. `None` without a start.
pub fn time_range(event: &CalendarEvent) -> Option<String> {
    range_in(event, local)
}

fn range_in<Tz: TimeZone>(
    event: &CalendarEvent,
    zone: impl Fn(Timestamp) -> Option<DateTime<Tz>>,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    const DAY: &str = "%a %-d %b %Y";
    let start = zone(event.starts_at?)?;
    let end = event
        .ends_at
        .filter(|end| *end > event.starts_at.unwrap_or_default())
        .and_then(zone);
    Some(match end {
        None => format!("{} · {}", start.format(DAY), start.format("%H:%M")),
        Some(end) if end.date_naive() == start.date_naive() => format!(
            "{} · {} – {}",
            start.format(DAY),
            start.format("%H:%M"),
            end.format("%H:%M")
        ),
        Some(end) => {
            let day = if end.year() == start.year() {
                "%a %-d %b"
            } else {
                DAY
            };
            format!(
                "{} {} – {} {}",
                start.format(day),
                start.format("%H:%M"),
                end.format(DAY),
                end.format("%H:%M")
            )
        }
    })
}

/// A text value of an iCalendar property (RFC 5545 §3.3.11): backslash,
/// comma and semicolon escaped, line breaks as `\n`, other control
/// characters dropped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A content line, folded at 75 octets (RFC 5545 §3.1) and ended.
fn line(out: &mut String, text: &str) {
    let mut width = 0;
    for c in text.chars() {
        if width + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += c.len_utf8();
    }
    out.push_str("\r\n");
}

fn utc(ts: Timestamp) -> Option<String> {
    Utc.timestamp_millis_opt(ts.as_millis())
        .single()
        .map(|at| at.format("%Y%m%dT%H%M%SZ").to_string())
}

/// The event as an iCalendar file. `uid` identifies it (the message's id),
/// so adding it twice updates the entry instead of making a second one.
/// `None` without a start: a calendar has nowhere to put it.
pub fn ics(event: &CalendarEvent, uid: &str, now: Timestamp) -> Option<String> {
    let start = utc(event.starts_at?)?;
    let uid: String = uid
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c))
        .collect();
    let mut out = String::new();
    line(&mut out, "BEGIN:VCALENDAR");
    line(&mut out, "VERSION:2.0");
    line(&mut out, "PRODID:-//wuapi//wuapi Inbox//EN");
    line(&mut out, "BEGIN:VEVENT");
    line(&mut out, &format!("UID:{uid}@inbox.wuapi.dev"));
    line(
        &mut out,
        &format!("DTSTAMP:{}", utc(now).unwrap_or_else(|| start.clone())),
    );
    line(&mut out, &format!("DTSTART:{start}"));
    if let Some(end) = event
        .ends_at
        .filter(|end| Some(*end) > event.starts_at)
        .and_then(utc)
    {
        line(&mut out, &format!("DTEND:{end}"));
    }
    line(&mut out, &format!("SUMMARY:{}", escape(&event.title)));
    if let Some(description) = &event.description {
        line(&mut out, &format!("DESCRIPTION:{}", escape(description)));
    }
    if let Some(place) = &event.place {
        let named: Vec<&str> = [&place.name, &place.address]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if !named.is_empty() {
            line(&mut out, &format!("LOCATION:{}", escape(&named.join(", "))));
        }
        if let Some(point) = place.point {
            line(
                &mut out,
                &format!("GEO:{:.6};{:.6}", point.latitude(), point.longitude()),
            );
        }
    }
    if let Some(url) = event
        .join_url
        .as_deref()
        .filter(|url| crate::markup::opens_in_browser(url))
    {
        // A URI value is not escaped; one with a line break is not a URI.
        if !url.chars().any(char::is_control) {
            line(&mut out, &format!("URL:{url}"));
        }
    }
    if event.cancelled {
        line(&mut out, "STATUS:CANCELLED");
    }
    line(&mut out, "END:VEVENT");
    line(&mut out, "END:VCALENDAR");
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;
    use client_provider::{EventPlace, GeoPoint};

    fn event() -> CalendarEvent {
        CalendarEvent {
            title: "Dinner; at Ramiro, Lisbon".into(),
            description: Some("Table for five.\nBring cash\\cards".into()),
            // 2026-10-03T19:00:00Z to 21:30:00Z.
            starts_at: Some(Timestamp::from_millis(1_791_054_000_000)),
            ends_at: Some(Timestamp::from_millis(1_791_063_000_000)),
            place: Some(EventPlace {
                name: Some("Cervejaria Ramiro".into()),
                address: Some("Av. Almirante Reis 1".into()),
                point: GeoPoint::new(38.720_37, -9.135_57),
            }),
            call: None,
            join_url: Some("https://call.example/abc".into()),
            cancelled: false,
        }
    }

    fn at(hours: i32) -> impl Fn(Timestamp) -> Option<DateTime<FixedOffset>> {
        move |ts| {
            FixedOffset::east_opt(hours * 3600)?
                .timestamp_millis_opt(ts.as_millis())
                .single()
        }
    }

    #[test]
    fn the_time_range_is_said_in_the_readers_timezone() {
        let mut event = event();
        assert_eq!(
            range_in(&event, at(1)).unwrap(),
            "Sat 3 Oct 2026 · 20:00 – 22:30"
        );
        // Further east it runs past midnight: both days are named.
        assert_eq!(
            range_in(&event, at(4)).unwrap(),
            "Sat 3 Oct 23:00 – Sun 4 Oct 2026 01:30"
        );
        event.ends_at = None;
        assert_eq!(range_in(&event, at(-4)).unwrap(), "Sat 3 Oct 2026 · 15:00");
        // An end before the start is no end.
        event.ends_at = Some(Timestamp::from_millis(1));
        assert_eq!(range_in(&event, at(-4)).unwrap(), "Sat 3 Oct 2026 · 15:00");
        event.starts_at = None;
        assert_eq!(range_in(&event, at(0)), None);
        assert_eq!(time_range(&event), None);
    }

    #[test]
    fn the_ics_file_is_escaped_folded_and_in_utc() {
        let file = ics(&event(), "m17/..\\evil\r\nX:1", Timestamp::from_millis(0)).unwrap();
        let lines: Vec<&str> = file.split("\r\n").collect();
        assert_eq!(lines[0], "BEGIN:VCALENDAR");
        assert!(lines.contains(&"UID:m17..evilX1@inbox.wuapi.dev"), "{file}");
        assert!(lines.contains(&"DTSTAMP:19700101T000000Z"));
        assert!(lines.contains(&"DTSTART:20261003T190000Z"));
        assert!(lines.contains(&"DTEND:20261003T213000Z"));
        assert!(lines.contains(&"SUMMARY:Dinner\\; at Ramiro\\, Lisbon"));
        assert!(lines.contains(&"DESCRIPTION:Table for five.\\nBring cash\\\\cards"));
        assert!(lines.contains(&"LOCATION:Cervejaria Ramiro\\, Av. Almirante Reis 1"));
        assert!(lines.contains(&"GEO:38.720370;-9.135570"));
        assert!(lines.contains(&"URL:https://call.example/abc"));
        assert!(!file.contains("STATUS:CANCELLED"));
        assert!(file.ends_with("END:VEVENT\r\nEND:VCALENDAR\r\n"));
        // No bare line break survives: a value cannot start a new property.
        assert!(!file.replace("\r\n", "").contains(['\r', '\n']));

        let mut long = event();
        long.description = Some("é".repeat(100));
        long.cancelled = true;
        long.join_url = Some("javascript:alert(1)".into());
        let file = ics(&long, "m1", Timestamp::from_millis(0)).unwrap();
        assert!(file.split("\r\n").all(|line| line.len() <= 75), "{file}");
        assert!(file.contains("STATUS:CANCELLED") && !file.contains("URL:"));

        long.starts_at = None;
        assert_eq!(ics(&long, "m1", Timestamp::from_millis(0)), None);
    }
}
