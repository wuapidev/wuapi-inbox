//! One person, several ids: a name reads the same wherever the person
//! shows up, whichever id that place knows them by.

use super::*;
use client_provider::{
    Contact, Group, GroupParticipant, GroupRole, Mention, Party, SystemEvent, SystemKind,
};

const ANA: &str = "+584245550199";
const ANA_LID: &str = "lid:200055501000001";
const GROUP: &str = "g1@g.us";

fn store() -> Store {
    let store = Store::open_in_memory().unwrap();
    store
        .upsert_accounts(
            "test",
            &[Account {
                id: account(),
                display_name: "Victor".into(),
                phone: Some("+56955501234".into()),
                self_contact: Some(ContactId::new("+56955501234")),
                connection: ConnectionState::Connected,
                settings: Default::default(),
            }],
        )
        .unwrap();
    let mut group = chat(GROUP, "Lisbon trip");
    group.kind = ChatKind::Group;
    store.upsert_chat(&group, false).unwrap();
    store
}

/// Ana as the address book lists her: by her number, with the id she is
/// mentioned by when her number is hidden.
fn ana() -> Contact {
    let mut contact = Contact::new(account(), ContactId::new(ANA));
    contact.phone = Some(ANA.into());
    contact.saved_name = Some("Ana Rojas".into());
    contact.alt_ids = vec![ContactId::new(ANA_LID)];
    contact
}

fn mention(id: &str) -> Mention {
    let id = ContactId::new(id);
    Mention {
        handle: client_provider::mention_handle(&id),
        id,
        name: None,
        me: false,
    }
}

fn group_text(id: &str, ts: i64, sender: &str, body: &str, mentions: &[&str]) -> Message {
    let mut message = text(id, GROUP, ts, body, Direction::Incoming);
    message.sender = ContactId::new(sender);
    message.extras.mentions = mentions.iter().map(|id| mention(id)).collect();
    message
}

fn read(store: &Store, id: &str) -> Message {
    store
        .message(&account(), &MessageId::new(id))
        .unwrap()
        .unwrap()
}

fn shown(store: &Store, id: &str) -> String {
    message_preview(&read(store, id))
}

#[test]
fn a_mention_by_hidden_number_id_shows_the_saved_name() {
    let store = store();
    store.upsert_contact(&ana()).unwrap();
    // What the API delivers: the id is already the number, the text
    // still carries the digits of the hidden-number id.
    let resolved = group_text("m1", 1, "+5842", "@200055501000001 are you in?", &[ANA]);
    // And a provider that could not turn the id into a number.
    let hidden = group_text("m2", 2, "+5842", "@200055501000001 again", &[ANA_LID]);
    // And an old-style mention by number.
    let plain = group_text("m3", 3, "+5842", "@584245550199 hello", &[ANA]);
    store.upsert_messages(&[resolved, hidden, plain]).unwrap();

    assert_eq!(shown(&store, "m1"), "@Ana Rojas are you in?");
    assert_eq!(shown(&store, "m2"), "@Ana Rojas again");
    assert_eq!(shown(&store, "m3"), "@Ana Rojas hello");
    let named = read(&store, "m1");
    assert_eq!(named.extras.mentions[0].handle, "200055501000001");
    assert_eq!(named.extras.mentions[0].name.as_deref(), Some("Ana Rojas"));
    assert!(!named.extras.mentions[0].me);
}

#[test]
fn a_name_is_taken_in_one_order_everywhere() {
    let store = store();
    let who = |store: &Store| {
        let person = store
            .person(
                &account(),
                Some(&ChatId::new(GROUP)),
                &ContactId::new(ANA_LID),
            )
            .unwrap();
        person.name
    };
    // Nothing known: no name, and never the digits of the id.
    assert_eq!(who(&store), None);
    let unknown = group_text("m0", 1, "+5842", "@200055501000001 ?", &[ANA_LID]);
    store.upsert_message(&unknown).unwrap();
    assert_eq!(shown(&store, "m0"), "@someone ?");

    // The number, once the address book connects the two ids.
    let mut contact = ana();
    contact.saved_name = None;
    store.upsert_contact(&contact).unwrap();
    assert_eq!(
        who(&store),
        Some(("+58 424 555 0199".into(), NameSource::Phone))
    );

    // A username beats the number.
    contact.username = Some("ana.rojas".into());
    store.upsert_contact(&contact).unwrap();
    assert_eq!(
        who(&store),
        Some(("ana.rojas".into(), NameSource::Username))
    );

    // The name on her own messages beats the username.
    let mut hers = group_text("m1", 2, ANA_LID, "hi", &[]);
    hers.sender_name = Some("Ana ✨".into());
    store.upsert_message(&hers).unwrap();
    assert_eq!(who(&store), Some(("Ana ✨".into(), NameSource::Profile)));

    // What the group calls her beats that.
    store
        .put_group(
            &Group {
                id: ChatId::new(GROUP),
                account_id: account(),
                subject: "Lisbon trip".into(),
                description: None,
                owner: None,
                created_at: None,
                community: false,
                announce: false,
                locked: false,
                join_approval: None,
                members_can_add: None,
                participants: vec![GroupParticipant {
                    contact: ContactId::new(ANA_LID),
                    name: Some("Ana (flat 3)".into()),
                    role: GroupRole::Member,
                }],
                subgroups: Vec::new(),
            },
            Timestamp::from_millis(5),
        )
        .unwrap();
    assert_eq!(
        who(&store),
        Some(("Ana (flat 3)".into(), NameSource::Group))
    );
    // In another chat the group's name for her does not apply.
    let elsewhere = store
        .person(&account(), None, &ContactId::new(ANA_LID))
        .unwrap();
    assert_eq!(elsewhere.shown(), Some("Ana ✨"));

    // The address book beats everything.
    store.upsert_contact(&ana()).unwrap();
    assert_eq!(who(&store), Some(("Ana Rojas".into(), NameSource::Saved)));
    assert_eq!(shown(&store, "m0"), "@Ana Rojas ?");
    // The participant list names her by it too, though it lists her by
    // the hidden-number id.
    let listed = store
        .group_participants(&account(), &ChatId::new(GROUP), None, 10)
        .unwrap();
    assert_eq!(listed[0].name, "Ana Rojas");
    assert!(listed[0].saved);
    assert_eq!(listed[0].phone.as_deref(), Some(ANA));
}

#[test]
fn a_name_that_arrives_later_is_on_the_messages_already_there() {
    let store = store();
    let message = group_text("m1", 1, ANA_LID, "@200055501000001 it is me", &[ANA]);
    store.upsert_message(&message).unwrap();
    // The one stretch for the one mention is hers even before anything
    // connects the ids; all that is known is the number the API listed.
    assert_eq!(shown(&store, "m1"), "@+58 424 555 0199 it is me");
    assert_eq!(read(&store, "m1").sender_name, None);

    // The address book arrives: the listener is told, and the same
    // stored message reads by name, mention and sender both.
    let mut listener = store.subscribe();
    store
        .upsert_listed_contacts(&account(), &[ana()], Timestamp::from_millis(9))
        .unwrap();
    assert!(matches!(
        listener.try_next(),
        Some(StoreChange::Contacts { .. })
    ));
    assert_eq!(shown(&store, "m1"), "@Ana Rojas it is me");
    assert_eq!(read(&store, "m1").sender_name.as_deref(), Some("Ana Rojas"));

    // Somebody the address book does not have sends a message: their
    // earlier mention takes the name it came with.
    let stranger = group_text(
        "m2",
        2,
        ANA,
        "@99887766554433 look",
        &["lid:99887766554433"],
    );
    store.upsert_message(&stranger).unwrap();
    assert_eq!(shown(&store, "m2"), "@someone look");
    let mut theirs = group_text("m3", 3, "lid:99887766554433", "here", &[]);
    theirs.sender_name = Some("Luis".into());
    store.upsert_message(&theirs).unwrap();
    assert_eq!(shown(&store, "m2"), "@Luis look");
}

#[test]
fn the_account_itself_is_you() {
    let store = store();
    let message = group_text(
        "m1",
        1,
        "+5842",
        "@56955501234 can you check?",
        &["+56955501234"],
    );
    store.upsert_message(&message).unwrap();
    let read = read(&store, "m1");
    assert!(read.extras.mentions[0].me);
    assert_eq!(message_preview(&read), "@You can you check?");
}

#[test]
fn text_that_only_looks_like_a_mention_is_left_alone() {
    let store = store();
    store.upsert_contact(&ana()).unwrap();
    // No mention listed: the number in the text is text, even though
    // the address book knows whose it is.
    let typed = group_text("m1", 1, "+5842", "call @584245550199 now", &[]);
    // One listed: the other stretch stays.
    let mixed = group_text(
        "m2",
        2,
        "+5842",
        "@200055501000001 the code is @123456789",
        &[ANA],
    );
    store.upsert_messages(&[typed, mixed]).unwrap();
    assert_eq!(shown(&store, "m1"), "call @584245550199 now");
    assert_eq!(shown(&store, "m2"), "@Ana Rojas the code is @123456789");
}

#[test]
fn the_chat_list_and_a_quote_name_people_the_same_way() {
    let store = store();
    store.upsert_contact(&ana()).unwrap();
    // The group's newest message: from Ana by her hidden-number id, under
    // the name she gave herself, mentioning the account.
    let mut newest = group_text(
        "m1",
        10,
        ANA_LID,
        "@56955501234 dinner at 8?",
        &["+56955501234"],
    );
    newest.sender_name = Some("ana ✨".into());
    store.upsert_message(&newest).unwrap();
    let row = store
        .chat(&account(), &ChatId::new(GROUP))
        .unwrap()
        .unwrap();
    let preview = row.last_message.unwrap();
    assert_eq!(preview.sender_name.as_deref(), Some("Ana Rojas"));
    assert_eq!(preview.text, "@You dinner at 8?");

    // A reply quoting it: the quote says who and what in the same words.
    let mut reply = text("r1", GROUP, 20, "yes", Direction::Outgoing);
    reply.reply_to = Some(ReplyRef {
        message_id: MessageId::new("m1"),
        sender_name: None,
        preview: None,
    });
    store.upsert_message(&reply).unwrap();
    let stored = store.messages(&account(), &ChatId::new(GROUP), 10).unwrap();
    let quote = stored[1].message.reply_to.clone().unwrap();
    assert_eq!(quote.sender_name.as_deref(), Some("Ana Rojas"));
    assert_eq!(quote.preview.as_deref(), Some("@You dinner at 8?"));
    assert_eq!(stored[0].message.sender_name.as_deref(), Some("Ana Rojas"));

    // A notice about her, by the hidden-number id.
    let mut notice = text("n1", GROUP, 30, "", Direction::Incoming);
    notice.content = MessageContent::System(SystemEvent {
        kind: SystemKind::Joined,
        actor: None,
        targets: vec![Party {
            id: ContactId::new(ANA_LID),
            name: None,
        }],
        detail: None,
    });
    store.upsert_message(&notice).unwrap();
    let row = store
        .chat(&account(), &ChatId::new(GROUP))
        .unwrap()
        .unwrap();
    assert!(
        row.last_message.unwrap().text.contains("Ana Rojas"),
        "the notice names her as the address book does"
    );

    // Search finds the message and names its sender likewise.
    let hits = store.search_messages(&account(), "dinner", 10).unwrap();
    assert_eq!(hits[0].message.sender_name.as_deref(), Some("Ana Rojas"));
}

#[test]
fn ids_are_kept_together_as_the_address_book_last_said() {
    let store = store();
    store.upsert_contact(&ana()).unwrap();
    let ids = |id: &str| {
        let mut ids: Vec<String> = store
            .person(&account(), None, &ContactId::new(id))
            .unwrap()
            .ids
            .iter()
            .map(|id| id.to_string())
            .collect();
        ids.sort();
        ids
    };
    assert_eq!(ids(ANA), [ANA, ANA_LID]);
    assert_eq!(ids(ANA_LID), [ANA, ANA_LID]);

    // The hidden-number id turns out to be somebody else's: it leaves
    // Ana, and nothing of hers follows it.
    let mut luis = Contact::new(account(), ContactId::new("+584125556677"));
    luis.saved_name = Some("Luis".into());
    luis.alt_ids = vec![ContactId::new(ANA_LID)];
    store.upsert_contact(&luis).unwrap();
    assert_eq!(ids(ANA), [ANA]);
    assert_eq!(ids(ANA_LID), ["+584125556677", ANA_LID]);
    let person = store
        .person(&account(), None, &ContactId::new(ANA_LID))
        .unwrap();
    assert_eq!(person.shown(), Some("Luis"));

    // Another account's people are its own.
    let other = store
        .person(&AccountId::new("other"), None, &ContactId::new(ANA_LID))
        .unwrap();
    assert_eq!(other.shown(), None);
    assert_eq!(other.ids.len(), 1);
}
