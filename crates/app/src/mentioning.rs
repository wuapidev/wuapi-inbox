//! Mentioning somebody while writing: the parts that are decisions, not
//! drawing.
//!
//! The composer shows people by name ("@Ana Rojas"); what is sent names
//! them the way the provider writes a mention (on WhatsApp, by the digits
//! of their id), with the ids riding along. A name in the text is a mention only if it was picked
//! from the list: typing "@Ana" by hand mentions nobody.

use client_provider::ContactId;

/// The longest stretch after an `@` that is still a search.
const MAX_QUERY: usize = 32;

/// Somebody picked from the list, as the composer's text names them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Picked {
    /// The name after the `@` in the composer.
    pub name: String,
    /// Who.
    pub id: ContactId,
    /// What follows the `@` in what is sent.
    pub handle: String,
}

/// What is being typed after an `@` at the end of `text` (the composer's
/// text up to the caret), if it ends in the start of a mention: `Some("")` right after the `@`, `Some("an")`
/// two letters in. The `@` has to begin a word (an e-mail address is not
/// a mention) and the search is one word.
pub fn typing(text: &str) -> Option<&str> {
    let at = text.rfind('@')?;
    let query = &text[at + 1..];
    let begins_word = text[..at]
        .chars()
        .next_back()
        .is_none_or(|before| before.is_whitespace());
    let one_word = query.chars().count() <= MAX_QUERY && !query.chars().any(char::is_whitespace);
    (begins_word && one_word).then_some(query)
}

/// `text` with the mention being typed at its end replaced by `name`,
/// and a space after it to go on writing.
pub fn insert(text: &str, name: &str) -> String {
    let at = text.rfind('@').unwrap_or(text.len());
    format!("{}@{name} ", &text[..at])
}

/// Where `@name` stands in `text` as a whole word, from `from` on.
fn find(text: &str, name: &str, from: usize) -> Option<usize> {
    let needle = format!("@{name}");
    let mut start = from;
    while let Some(found) = text[start..].find(&needle) {
        let at = start + found;
        let end = at + needle.len();
        let free_before = text[..at]
            .chars()
            .next_back()
            .is_none_or(|before| !before.is_alphanumeric());
        let free_after = text[end..]
            .chars()
            .next()
            .is_none_or(|after| !after.is_alphanumeric());
        if free_before && free_after {
            return Some(at);
        }
        start = end;
    }
    None
}

/// What is sent for what the composer holds: the text with every picked
/// name still in it written as `@` and the handle of that person's id,
/// and the people it mentions, in the order they first appear. A picked
/// name that was deleted or edited since mentions nobody.
pub fn outgoing(text: &str, picked: &[Picked]) -> (String, Vec<ContactId>) {
    // Longest names first: "Ana Rojas" before "Ana".
    let mut picked: Vec<&Picked> = picked
        .iter()
        .filter(|picked| !picked.name.is_empty() && !picked.handle.is_empty())
        .collect();
    picked.sort_by_key(|picked| std::cmp::Reverse(picked.name.len()));
    let mut out = String::with_capacity(text.len());
    let mut mentioned: Vec<ContactId> = Vec::new();
    let mut at = 0;
    while at < text.len() {
        let next = picked
            .iter()
            .filter_map(|picked| find(text, &picked.name, at).map(|found| (found, *picked)))
            // The earliest; of two at one place, the longer name.
            .min_by_key(|(found, picked)| (*found, std::cmp::Reverse(picked.name.len())));
        let Some((found, who)) = next else { break };
        out.push_str(&text[at..found]);
        out.push('@');
        out.push_str(&who.handle);
        if !mentioned.contains(&who.id) {
            mentioned.push(who.id.clone());
        }
        at = found + 1 + who.name.len();
    }
    out.push_str(&text[at..]);
    (out, mentioned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picked(name: &str, id: &str) -> Picked {
        let id = ContactId::new(id);
        Picked {
            name: name.into(),
            handle: client_provider::mention_handle(&id),
            id,
        }
    }

    #[test]
    fn an_at_that_begins_a_word_starts_a_search() {
        assert_eq!(typing("@"), Some(""));
        assert_eq!(typing("hi @an"), Some("an"));
        assert_eq!(typing("line\n@Zo"), Some("Zo"));
        // Not at the end, not a word of its own, or more than one word.
        assert_eq!(typing("hi"), None);
        assert_eq!(typing("mail me at ana@ex"), None);
        assert_eq!(typing("@ana and"), None);
        assert_eq!(typing("@ana "), None);
        // The last `@` is the one being typed.
        assert_eq!(typing("@Ana Rojas @lu"), Some("lu"));
    }

    #[test]
    fn picking_replaces_what_was_typed_and_leaves_room_to_go_on() {
        assert_eq!(insert("hi @an", "Ana Rojas"), "hi @Ana Rojas ");
        assert_eq!(insert("@", "Luis"), "@Luis ");
    }

    #[test]
    fn what_is_sent_names_people_by_their_ids() {
        let people = [
            picked("Ana", "+584245550199"),
            picked("Ana Rojas", "lid:200055501000001"),
            picked("Luis", "+584125556677"),
        ];
        let (text, mentioned) = outgoing("@Ana Rojas and @Ana, then @Ana Rojas again", &people);
        assert_eq!(
            text,
            "@200055501000001 and @584245550199, then @200055501000001 again"
        );
        assert_eq!(
            mentioned,
            [
                ContactId::new("lid:200055501000001"),
                ContactId::new("+584245550199")
            ],
            "each once, in the order they appear; Luis is not in the text"
        );
    }

    #[test]
    fn a_name_typed_by_hand_or_edited_mentions_nobody() {
        // Nothing was picked.
        assert_eq!(outgoing("@Ana hello", &[]), ("@Ana hello".into(), vec![]));
        // Picked, then edited into another word, or part of an address.
        let people = [picked("Ana", "+584245550199")];
        assert_eq!(
            outgoing("@Anabel and x@Ana", &people),
            ("@Anabel and x@Ana".into(), vec![])
        );
        // Text without any `@` goes as it is.
        assert_eq!(outgoing("plain", &people), ("plain".into(), vec![]));
    }
}
