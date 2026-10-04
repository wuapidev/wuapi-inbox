//! Mentions: which stretch of a message's text names which of the people
//! the message says it mentions.
//!
//! A text names a mentioned person as `@` and a run of digits: the digits
//! of the id the sender's phone mentioned them by, which is more and more
//! often a hidden-number id and not the phone number. The list of
//! mentioned people that rides along may name the same person by another
//! of their ids. So a mention is tied to the text by trying every id the
//! person is known by, and only the people the message lists are ever
//! looked for: text that merely looks like a mention stays as typed.

use client_provider::Mention;

/// What stands for a person nobody has a name or a number for.
pub const UNKNOWN_PERSON: &str = "someone";

/// How the account itself is named where it is mentioned.
pub const YOU: &str = "You";

/// The shortest run of digits after an `@` that can be an id nobody
/// listed a handle for.
const MIN_GUESSED: usize = 5;

/// One `@` followed by digits, in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Token {
    /// Byte range of the whole token, the `@` included.
    start: usize,
    end: usize,
}

/// Every `@<digits>` of `text` that could be a mention: not glued to a
/// word before it (an e-mail address), nor to one after it.
fn tokens(text: &str) -> Vec<Token> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut previous: Option<char> = None;
    for (at, c) in text.char_indices() {
        let free = !previous.is_some_and(char::is_alphanumeric);
        previous = Some(c);
        if c != '@' || !free {
            continue;
        }
        let mut end = at + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() {
            end += 1;
        }
        if end == at + 1 {
            continue;
        }
        let glued = text[end..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric);
        if !glued {
            found.push(Token { start: at, end });
        }
    }
    found
}

fn digits_of<'t>(text: &'t str, token: &Token) -> &'t str {
    &text[token.start + 1..token.end]
}

/// Ties each mention to the text: `handle` becomes what follows the `@`
/// where the text names that person, or empty when the text does not.
///
/// `candidates[i]` are the handles mention `i` may go by (the digits of
/// every id the person is known by), most likely first. A mention none of
/// whose handles is in the text takes, in order, a stretch of the text
/// that looks like a mention and belongs to nobody, but only when there
/// are exactly as many of those as mentions left: one of each is the
/// usual case, a person mentioned by an id this client cannot yet connect
/// to the one the provider listed.
pub fn bind(text: &str, mentions: &mut [Mention], candidates: &[Vec<String>]) {
    let tokens = tokens(text);
    let mut claimed = vec![false; tokens.len()];
    let mut bound = vec![false; mentions.len()];
    for (index, mention) in mentions.iter_mut().enumerate() {
        let own = std::iter::once(mention.handle.clone());
        let others = candidates.get(index).into_iter().flatten().cloned();
        let handle = own
            .chain(others)
            .filter(|handle| !handle.is_empty())
            .find(|handle| {
                tokens
                    .iter()
                    .any(|token| digits_of(text, token) == handle.as_str())
            });
        if let Some(handle) = handle {
            for (token, claimed) in tokens.iter().zip(claimed.iter_mut()) {
                *claimed |= digits_of(text, token) == handle.as_str();
            }
            mention.handle = handle;
            bound[index] = true;
        }
    }
    // What is left on both sides, each stretch counted once.
    let mut free: Vec<&str> = Vec::new();
    for (token, claimed) in tokens.iter().zip(&claimed) {
        let digits = digits_of(text, token);
        if !claimed && digits.len() >= MIN_GUESSED && !free.contains(&digits) {
            free.push(digits);
        }
    }
    let unbound: Vec<usize> = (0..mentions.len()).filter(|index| !bound[*index]).collect();
    if !unbound.is_empty() && unbound.len() == free.len() {
        for (index, digits) in unbound.into_iter().zip(free) {
            mentions[index].handle = digits.to_owned();
        }
    } else {
        for index in unbound {
            mentions[index].handle.clear();
        }
    }
}

/// What a mention is shown as, without the `@`.
pub fn mention_name(mention: &Mention) -> &str {
    if mention.me {
        return YOU;
    }
    mention
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(UNKNOWN_PERSON)
}

/// `text` with each mention shown by name: "@Ana" instead of
/// "@584245550199". Only what the mentions' handles name is replaced.
pub fn with_mention_names(text: &str, mentions: &[Mention]) -> String {
    if mentions.is_empty() || !text.contains('@') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for token in tokens(text) {
        let digits = digits_of(text, &token);
        let Some(mention) = mentions.iter().find(|mention| mention.handle == digits) else {
            continue;
        };
        out.push_str(&text[at..token.start]);
        out.push('@');
        out.push_str(mention_name(mention));
        at = token.end;
    }
    out.push_str(&text[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::ContactId;

    fn mention(id: &str, handle: &str) -> Mention {
        Mention {
            id: ContactId::new(id),
            handle: handle.to_owned(),
            name: None,
            me: false,
        }
    }

    #[test]
    fn a_mention_is_found_by_any_id_of_the_person() {
        // The provider lists the number; the text carries the digits of
        // the hidden-number id, which the address book connects.
        let text = "@200055501000001 are you coming?";
        let mut mentions = vec![mention("+584245550199", "584245550199")];
        bind(
            text,
            &mut mentions,
            &[vec!["584245550199".into(), "200055501000001".into()]],
        );
        assert_eq!(mentions[0].handle, "200055501000001");
        mentions[0].name = Some("Ana Rojas".into());
        assert_eq!(
            with_mention_names(text, &mentions),
            "@Ana Rojas are you coming?"
        );
    }

    #[test]
    fn the_handle_the_provider_gave_wins_when_the_text_has_it() {
        let text = "@584245550199 and @200055501000001";
        let mut mentions = vec![
            mention("+584245550199", "584245550199"),
            mention("lid:200055501000001", "200055501000001"),
        ];
        bind(text, &mut mentions, &[Vec::new(), Vec::new()]);
        assert_eq!(mentions[0].handle, "584245550199");
        assert_eq!(mentions[1].handle, "200055501000001");
    }

    #[test]
    fn one_unknown_stretch_for_one_unknown_mention_is_that_mention() {
        // Nothing connects the number to the digits in the text, but
        // there is one of each.
        let text = "ping @200055501000001";
        let mut mentions = vec![mention("+584245550199", "584245550199")];
        bind(text, &mut mentions, &[vec!["584245550199".into()]]);
        assert_eq!(mentions[0].handle, "200055501000001");

        // Two of one and one of the other: nothing is guessed.
        let text = "ping @200055501000001 and @99887766554";
        let mut mentions = vec![mention("+584245550199", "584245550199")];
        bind(text, &mut mentions, &[Vec::new()]);
        assert_eq!(mentions[0].handle, "");
        assert_eq!(with_mention_names(text, &mentions), text);
    }

    #[test]
    fn several_unknown_mentions_take_the_stretches_in_order() {
        let text = "@1111111 then @2222222, again @1111111";
        let mut mentions = vec![mention("+58424", "58424"), mention("+58412", "58412")];
        bind(text, &mut mentions, &[Vec::new(), Vec::new()]);
        assert_eq!(mentions[0].handle, "1111111");
        assert_eq!(mentions[1].handle, "2222222");
    }

    #[test]
    fn text_that_only_looks_like_a_mention_stays_as_typed() {
        // No mention listed: nothing is touched, whatever the text.
        let text = "call @584245550199 or mail a@5551234";
        assert_eq!(with_mention_names(text, &[]), text);

        // One listed and found: the other stretch is not a mention.
        let mut mentions = vec![mention("+584245550199", "584245550199")];
        mentions[0].name = Some("Ana".into());
        let text = "@584245550199 said @123456789 is a code, x@584245550199 an address";
        bind(text, &mut mentions, &[Vec::new()]);
        assert_eq!(
            with_mention_names(text, &mentions),
            "@Ana said @123456789 is a code, x@584245550199 an address"
        );
        // A longer number that starts the same is another number.
        assert_eq!(
            with_mention_names("@5842455501999", &mentions),
            "@5842455501999"
        );
        assert_eq!(
            with_mention_names("@584245550199abc", &mentions),
            "@584245550199abc"
        );
    }

    #[test]
    fn a_mention_with_no_name_is_someone_and_the_account_is_you() {
        let text = "@200055501000001 and @56955501234";
        let mut mentions = vec![
            mention("lid:200055501000001", "200055501000001"),
            mention("+56955501234", "56955501234"),
        ];
        mentions[1].me = true;
        mentions[1].name = Some("Victor".into());
        bind(text, &mut mentions, &[Vec::new(), Vec::new()]);
        assert_eq!(with_mention_names(text, &mentions), "@someone and @You");
    }

    #[test]
    fn short_stretches_are_never_guessed() {
        let text = "room @12";
        let mut mentions = vec![mention("+584245550199", "584245550199")];
        bind(text, &mut mentions, &[Vec::new()]);
        assert_eq!(mentions[0].handle, "");
    }
}
