//! WhatsApp's links: `wa.me/<number>`, `whatsapp://send?phone=…`, the
//! `api.whatsapp.com/send` and `web.whatsapp.com/send` pages, and a
//! group's invite (`chat.whatsapp.com/<code>`, `whatsapp://chat?code=…`).
//!
//! They come from the system, when this application opens the
//! `whatsapp:` scheme, from another start of it handing one over, and
//! from a click on one in a message. Each is read here into what it asks
//! for; the shell does it (`ui/links.rs`). A text in a link is put in the
//! composer and never sent by itself.

/// What a link asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaLink {
    /// A conversation: with this number, its digits alone, and with this
    /// text ready to send. A link may have the text and no number: the
    /// person is then chosen here.
    Chat {
        phone: Option<String>,
        text: Option<String>,
    },
    /// A group's invite, by its code.
    Invite(String),
}

/// Reads a link. `None` for anything that is not one of WhatsApp's, or
/// one of them this application cannot follow (a business's short
/// `wa.me/message/…` link, which only WhatsApp's servers can resolve).
pub fn parse(link: &str) -> Option<WaLink> {
    let link = link.trim();
    let lower = link.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("whatsapp://") {
        let rest = &link[link.len() - rest.len()..];
        let (action, query) = split_query(rest);
        return match action.trim_end_matches('/').to_ascii_lowercase().as_str() {
            "send" | "" => chat(query_value(query, "phone"), query_value(query, "text")),
            "chat" => query_value(query, "code").and_then(|code| invite(&code)),
            _ => None,
        };
    }
    let rest = ["https://", "http://"]
        .iter()
        .find_map(|scheme| lower.strip_prefix(scheme))
        .map(|rest| &link[link.len() - rest.len()..])
        .unwrap_or(link);
    let rest = rest.split('#').next().unwrap_or_default();
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (host, after) = rest.split_at(end);
    let host = host.to_ascii_lowercase();
    let (path, query) = split_query(after.strip_prefix('/').unwrap_or(after));
    let path = path.trim_end_matches('/');
    match host.as_str() {
        "wa.me" | "www.wa.me" => {
            if path.is_empty() {
                return chat(None, query_value(query, "text"));
            }
            if path.contains('/') {
                return None;
            }
            let phone = digits(path)?;
            Some(WaLink::Chat {
                phone: Some(phone),
                text: query_value(query, "text"),
            })
        }
        "api.whatsapp.com" | "web.whatsapp.com" | "www.whatsapp.com" | "whatsapp.com" => {
            if !path.eq_ignore_ascii_case("send") {
                return None;
            }
            chat(query_value(query, "phone"), query_value(query, "text"))
        }
        "chat.whatsapp.com" => invite(path),
        _ => None,
    }
}

/// A conversation link, when it asks for something.
fn chat(phone: Option<String>, text: Option<String>) -> Option<WaLink> {
    let phone = phone.as_deref().and_then(digits);
    if phone.is_none() && text.is_none() {
        return None;
    }
    Some(WaLink::Chat { phone, text })
}

/// An invite's code: letters and digits, never empty.
fn invite(code: &str) -> Option<WaLink> {
    let code = code.trim();
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| WaLink::Invite(code.to_owned()))
}

/// The digits of a number written any way (`+58 412-555 0100`). `None`
/// when it has none, or something that is not a number.
fn digits(phone: &str) -> Option<String> {
    let phone = percent_decode(phone);
    if !phone
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '.'))
    {
        return None;
    }
    let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
    (!digits.is_empty()).then_some(digits)
}

fn split_query(rest: &str) -> (&str, &str) {
    rest.split_once('?').unwrap_or((rest, ""))
}

/// A field of a query, decoded; `None` when it is not there or empty.
fn query_value(query: &str, name: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').or(Some((pair, ""))))
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| percent_decode(&value.replace('+', " ")))
        .filter(|value| !value.trim().is_empty())
}

/// `%XX` escapes as their bytes, read as UTF-8. A broken escape stays
/// as it was written.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' && at + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                at += 3;
                continue;
            }
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_of(phone: Option<&str>, text: Option<&str>) -> Option<WaLink> {
        Some(WaLink::Chat {
            phone: phone.map(str::to_owned),
            text: text.map(str::to_owned),
        })
    }

    #[test]
    fn a_number_link_is_read_in_each_of_its_forms() {
        let hello = Some("Hola, ¿qué tal?");
        for link in [
            "https://wa.me/584125550100?text=Hola%2C%20%C2%BFqu%C3%A9%20tal%3F",
            "http://wa.me/584125550100/?text=Hola,+%C2%BFqu%C3%A9+tal%3F",
            "wa.me/584125550100?text=Hola%2C%20%C2%BFqu%C3%A9%20tal%3F",
            "whatsapp://send?phone=584125550100&text=Hola%2C%20%C2%BFqu%C3%A9%20tal%3F",
            "WhatsApp://send/?text=Hola%2C+%C2%BFqu%C3%A9+tal%3F&phone=%2B58%20412-555%200100",
            "https://api.whatsapp.com/send?phone=+584125550100&text=Hola%2C%20%C2%BFqu%C3%A9%20tal%3F",
            "https://web.whatsapp.com/send?phone=584125550100&text=Hola%2C%20%C2%BFqu%C3%A9%20tal%3F#x",
        ] {
            assert_eq!(parse(link), chat_of(Some("584125550100"), hello), "{link}");
        }
        assert_eq!(
            parse("https://wa.me/584125550100"),
            chat_of(Some("584125550100"), None)
        );
    }

    #[test]
    fn a_link_with_only_a_text_leaves_the_person_to_be_chosen() {
        assert_eq!(
            parse("https://wa.me/?text=look%20at%20this"),
            chat_of(None, Some("look at this"))
        );
        assert_eq!(parse("whatsapp://send?text=hi"), chat_of(None, Some("hi")));
    }

    #[test]
    fn a_group_invite_is_its_code() {
        let invite = Some(WaLink::Invite("AbC123xyz".into()));
        assert_eq!(parse("https://chat.whatsapp.com/AbC123xyz"), invite);
        assert_eq!(parse("chat.whatsapp.com/AbC123xyz/"), invite);
        assert_eq!(parse("whatsapp://chat?code=AbC123xyz"), invite);
        // A link shared from the phone carries a query of its own.
        assert_eq!(
            parse("https://chat.whatsapp.com/DvdrsKja5Lw3GQVfSSkXLV?s=cl&p=i&mlu=4&ilr=4&iam=0"),
            Some(WaLink::Invite("DvdrsKja5Lw3GQVfSSkXLV".into()))
        );
    }

    #[test]
    fn what_is_not_a_link_to_follow_is_left_alone() {
        for link in [
            "https://example.com/send?phone=1",
            "https://wa.me/message/ABCDEF",
            "https://wa.me/not-a-number",
            "https://api.whatsapp.com/other?phone=1",
            "whatsapp://send",
            "whatsapp://chat?code=",
            "https://chat.whatsapp.com/",
            "https://chat.whatsapp.com/a/b",
            "https://www.whatsapp.com/",
            "",
        ] {
            assert_eq!(parse(link), None, "{link}");
        }
    }

    #[test]
    fn a_broken_escape_stays_as_written() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%4"), "%zz%4");
        assert_eq!(percent_decode("%41b"), "Ab");
    }
}
