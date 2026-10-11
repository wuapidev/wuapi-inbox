//! WhatsApp addresses and the neutral ids, both ways.
//!
//! A person is `+<digits>` when WhatsApp names them by their number and
//! `lid:<digits>` when it names them by a hidden-number id, which is how
//! the client reads a number out of an id (see "One person, several ids"
//! in docs/PROVIDERS.md). A direct chat has the id of its contact. Groups,
//! channels and the status list keep their address as it is on the wire.

use client_provider::{ChatId, ClientMessageId, ContactId};
use sha2::{Digest, Sha256};
use whatsapp_rust::{Jid, Server};

/// The chat every story is a message of.
pub const STATUS_CHAT: &str = "status@broadcast";

/// The neutral id of an address, without its device part.
pub fn id_of(jid: &Jid) -> String {
    if jid.server.is_pn_family() {
        format!("+{}", jid.user_base())
    } else if jid.server.is_lid_family() {
        format!("lid:{}", jid.user_base())
    } else {
        jid.to_non_ad_string()
    }
}

/// The chat an address is.
pub fn chat_id(jid: &Jid) -> ChatId {
    ChatId::new(id_of(jid))
}

/// The person an address is.
pub fn contact_id(jid: &Jid) -> ContactId {
    ContactId::new(id_of(jid))
}

/// The address a neutral id stands for. `None` for a string that is none
/// of ours.
pub fn jid_of(id: &str) -> Option<Jid> {
    if let Some(digits) = id.strip_prefix('+') {
        return all_digits(digits).then(|| Jid::pn(digits));
    }
    if let Some(digits) = id.strip_prefix("lid:") {
        return all_digits(digits).then(|| Jid::lid(digits));
    }
    id.contains('@')
        .then(|| id.parse::<Jid>().ok())
        .flatten()
        .map(Jid::into_non_ad)
}

/// The neutral id of an address written the way the wire writes it
/// (`5842…@s.whatsapp.net`). Text that is not an address is kept as it is.
pub fn id_of_wire(address: &str) -> String {
    address
        .parse::<Jid>()
        .map(|jid| id_of(&jid))
        .unwrap_or_else(|_| address.to_owned())
}

/// The number of an id that carries one, in E.164.
pub fn phone_of(id: &str) -> Option<String> {
    id.strip_prefix('+')
        .filter(|digits| all_digits(digits))
        .map(|_| id.to_owned())
}

/// A phone number as the user typed it, as the id of its chat. `None` when
/// what is left is not a number.
pub fn id_of_phone(typed: &str) -> Option<String> {
    let digits: String = typed.chars().filter(char::is_ascii_digit).collect();
    (5..=15)
        .contains(&digits.len())
        .then(|| format!("+{digits}"))
}

/// True for a group's address.
pub fn is_group(id: &str) -> bool {
    id.ends_with("@g.us")
}

/// True for an address that is a conversation: a person or a group. A
/// channel, a broadcast list and the status list are not.
pub fn is_conversation(jid: &Jid) -> bool {
    matches!(jid.server, Server::Group) || jid.server.is_pn_family() || jid.server.is_lid_family()
}

/// The WhatsApp id a message sent under `client_id` goes out with: always
/// the same for the same key, in the shape WhatsApp Web's own ids have
/// (`3EB0` and eighteen hexadecimal digits), so that a send repeated after
/// a lost answer is the same message and not a second one.
pub fn message_id_for(client_id: &ClientMessageId) -> String {
    let hash = Sha256::digest(client_id.as_str().as_bytes());
    let mut id = String::with_capacity(22);
    id.push_str("3EB0");
    for byte in &hash[..9] {
        id.push_str(&format!("{byte:02X}"));
    }
    id
}

fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn people_are_numbers_or_hidden_ids_and_groups_keep_their_address() {
        let number: Jid = "584245550199:7@s.whatsapp.net".parse().unwrap();
        assert_eq!(id_of(&number), "+584245550199");
        let hidden: Jid = "200055501000001@lid".parse().unwrap();
        assert_eq!(id_of(&hidden), "lid:200055501000001");
        let group: Jid = "120363000000000001@g.us".parse().unwrap();
        assert_eq!(id_of(&group), "120363000000000001@g.us");

        for id in [
            "+584245550199",
            "lid:200055501000001",
            "120363000000000001@g.us",
        ] {
            assert_eq!(id_of(&jid_of(id).unwrap()), id, "round trip of {id}");
        }
        assert!(jid_of("somebody").is_none());
        assert!(jid_of("+58 424").is_none());
        assert_eq!(phone_of("+584245550199").as_deref(), Some("+584245550199"));
        assert_eq!(phone_of("lid:200055501000001"), None);
    }

    #[test]
    fn a_typed_number_is_its_chat() {
        assert_eq!(
            id_of_phone("+58 (424) 555-0199").as_deref(),
            Some("+584245550199")
        );
        assert_eq!(id_of_phone("hello"), None);
    }

    #[test]
    fn the_same_key_is_the_same_whatsapp_id() {
        let key = ClientMessageId::new("9b0c7e0e-0d5a-4c57-a4a4-0c5b8d1f8a11");
        let id = message_id_for(&key);
        assert_eq!(id, message_id_for(&key));
        assert_eq!(id.len(), 22);
        assert!(id.starts_with("3EB0"));
        assert!(id
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase()));
        assert_ne!(id, message_id_for(&ClientMessageId::new("another")));
    }
}
