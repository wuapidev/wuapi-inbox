//! Tests of the store, the outbox and the sync engine against the mock
//! provider.

use crate::*;
use client_provider::{
    Account, AccountId, Chat, ChatChange, ChatId, ChatKind, ClientMessageId, ConnectionState,
    ContactId, DeliveryStatus, Direction, Media, MediaKind, Message, MessageContent, MessageId,
    OutgoingContent, OutgoingMessage, PresenceState, Provider, ProviderError, ProviderEvent,
    ReplyRef, Timestamp,
};
use provider_mock::{MockConfig, MockProvider};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;

/// The key of the test databases on disk: they are encrypted, as the
/// application's are.
fn test_key() -> StoreKey {
    StoreKey::from_bytes([42; 32])
}

fn account() -> AccountId {
    AccountId::new("acc")
}

fn chat_id() -> ChatId {
    ChatId::new("chat")
}

fn seed_account(store: &Store) {
    store
        .upsert_accounts(
            "test",
            &[Account {
                id: account(),
                display_name: "Test".into(),
                phone: None,
                self_contact: Some(ContactId::new("me")),
                connection: ConnectionState::Connected,
                settings: Default::default(),
            }],
        )
        .unwrap();
}

fn chat(id: &str, title: &str) -> Chat {
    Chat {
        id: ChatId::new(id),
        account_id: account(),
        kind: ChatKind::Direct,
        title: title.into(),
        avatar: None,
        unread_count: 0,
        pinned: false,
        muted: false,
        archived: false,
        last_message: None,
        unknown: Default::default(),
        picture_id: None,
        pinned_at: None,
    }
}

fn text(id: &str, chat: &str, ts: i64, body: &str, direction: Direction) -> Message {
    Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: account(),
        chat_id: ChatId::new(chat),
        sender: ContactId::new(match direction {
            Direction::Incoming => "them",
            Direction::Outgoing => "me",
        }),
        sender_name: None,
        direction,
        timestamp: Timestamp::from_millis(ts),
        content: MessageContent::text(body),
        reply_to: None,
        status: match direction {
            Direction::Incoming => DeliveryStatus::Delivered,
            Direction::Outgoing => DeliveryStatus::Sent,
        },
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

fn reaction(id: &str, target: &str, sender: &str, ts: i64, emoji: &str) -> Message {
    let mut m = text(id, "chat", ts, "", Direction::Incoming);
    m.sender = ContactId::new(sender);
    m.content = MessageContent::Reaction {
        target: MessageId::new(target),
        emoji: emoji.into(),
    };
    m
}

fn outgoing(client_id: &str, body: &str) -> OutgoingMessage {
    OutgoingMessage {
        client_id: ClientMessageId::new(client_id),
        account_id: account(),
        chat_id: chat_id(),
        content: OutgoingContent::Text { body: body.into() },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    }
}

const HOUR: Duration = Duration::from_secs(3600);

// ----- store ------------------------------------------------------------

#[test]
fn migrations_run_once_and_data_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.db");
    {
        let store = Store::open(&path, Some(&test_key())).unwrap();
        seed_account(&store);
        store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
        store
            .upsert_message(&text("m1", "chat", 10, "hola", Direction::Incoming))
            .unwrap();
    }
    let store = Store::open(&path, Some(&test_key())).unwrap();
    assert_eq!(store.accounts().unwrap().len(), 1);
    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].message.content, MessageContent::text("hola"));
}

#[test]
fn chat_list_is_ordered_by_pin_then_activity_and_shows_previews() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store.upsert_chat(&chat("a", "Ana"), false).unwrap();
    store.upsert_chat(&chat("b", "Bruno"), false).unwrap();
    let mut pinned = chat("c", "Carla");
    pinned.pinned = true;
    pinned.unread_count = 3;
    store.upsert_chat(&pinned, false).unwrap();

    store
        .upsert_messages(&[
            text("m1", "a", 100, "older", Direction::Incoming),
            text("m2", "b", 300, "newest", Direction::Outgoing),
            text("m3", "c", 50, "pinned but old", Direction::Incoming),
            text("m4", "a", 200, "newer", Direction::Incoming),
        ])
        .unwrap();

    let chats = store.chats(&account(), None).unwrap();
    let order: Vec<_> = chats.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(order, ["Carla", "Bruno", "Ana"]);
    assert_eq!(chats[0].unread_count, 3);
    let preview = chats[2].last_message.as_ref().unwrap();
    assert_eq!(preview.text, "newer");
    assert!(chats[1].last_message.as_ref().unwrap().outgoing);

    let filtered = store.chats(&account(), Some("bru")).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].title, "Bruno");
    assert!(store.chats(&account(), Some("100%")).unwrap().is_empty());
}

/// Pinned chats are in the order they were pinned, the last one first, as
/// WhatsApp lists them: by the time the provider gives, or the moment the
/// pin was made here. A pin without a known time comes after those with
/// one, and how lately a pinned chat was written in does not matter.
#[test]
fn pinned_chats_are_in_the_order_they_were_pinned() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let pinned = |id: &str, title: &str, at: Option<i64>| {
        let mut chat = chat(id, title);
        chat.pinned = true;
        chat.pinned_at = at.map(Timestamp::from_millis);
        chat
    };
    store
        .upsert_chat(&pinned("a", "Ana", Some(1_000)), false)
        .unwrap();
    store
        .upsert_chat(&pinned("b", "Bruno", Some(3_000)), false)
        .unwrap();
    store
        .upsert_chat(&pinned("c", "Carla", None), false)
        .unwrap();
    store
        .upsert_chat(&pinned("d", "Dora", Some(2_000)), false)
        .unwrap();
    store.upsert_chat(&chat("e", "Eva"), false).unwrap();
    store
        .upsert_messages(&[
            // The oldest pin has the newest message, and Eva a newer one.
            text("m1", "a", 900, "x", Direction::Incoming),
            text("m2", "c", 800, "x", Direction::Incoming),
            text("m3", "e", 950, "x", Direction::Incoming),
        ])
        .unwrap();
    let order = |store: &Store| -> Vec<String> {
        store
            .chats(&account(), None)
            .unwrap()
            .iter()
            .map(|chat| chat.title.clone())
            .collect()
    };
    assert_eq!(order(&store), ["Bruno", "Dora", "Ana", "Carla", "Eva"]);

    // A pin made here is the newest pin. The provider's next list knows
    // the chat is pinned and not when: the time made here is kept.
    store
        .apply_chat_change(&account(), &ChatId::new("e"), ChatChange::Pinned(true))
        .unwrap();
    assert_eq!(order(&store), ["Eva", "Bruno", "Dora", "Ana", "Carla"]);
    store.upsert_chat(&pinned("e", "Eva", None), false).unwrap();
    assert_eq!(order(&store), ["Eva", "Bruno", "Dora", "Ana", "Carla"]);
    // Pinning what is pinned is not a new pin.
    store
        .apply_chat_change(&account(), &ChatId::new("a"), ChatChange::Pinned(true))
        .unwrap();
    assert_eq!(order(&store), ["Eva", "Bruno", "Dora", "Ana", "Carla"]);
    // A list with nothing to say about pins changes nothing.
    let mut blind = chat("b", "Bruno");
    blind.unknown.pinned = true;
    store.upsert_chat(&blind, false).unwrap();
    assert_eq!(order(&store), ["Eva", "Bruno", "Dora", "Ana", "Carla"]);
    // The provider's time wins when it has one: Ana was pinned again on
    // the phone.
    store
        .upsert_chat(&pinned("a", "Ana", Some(9_000_000_000_000)), false)
        .unwrap();
    assert_eq!(order(&store), ["Ana", "Eva", "Bruno", "Dora", "Carla"]);
    // Unpinned, here or there: back among the rest, and the time is gone.
    store
        .apply_chat_change(&account(), &ChatId::new("a"), ChatChange::Pinned(false))
        .unwrap();
    store.upsert_chat(&chat("b", "Bruno"), false).unwrap();
    assert_eq!(order(&store), ["Eva", "Dora", "Carla", "Ana", "Bruno"]);
    store
        .upsert_chat(&pinned("b", "Bruno", None), false)
        .unwrap();
    assert_eq!(
        order(&store),
        ["Eva", "Dora", "Carla", "Bruno", "Ana"],
        "pinned again without a time: after those that have one, by activity"
    );
}

#[test]
fn messages_come_back_oldest_first_and_limited_to_the_newest() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    let batch: Vec<_> = (0..20)
        .map(|i| {
            text(
                &format!("m{i}"),
                "chat",
                i,
                &format!("n{i}"),
                Direction::Incoming,
            )
        })
        .collect();
    assert_eq!(store.upsert_messages(&batch).unwrap(), 20);
    // Storing the same page again changes nothing.
    assert_eq!(store.upsert_messages(&batch).unwrap(), 0);

    let newest = store.messages(&account(), &chat_id(), 5).unwrap();
    let ids: Vec<_> = newest.iter().map(|m| m.message.id.as_str()).collect();
    assert_eq!(ids, ["m15", "m16", "m17", "m18", "m19"]);
    assert_eq!(store.message_count(&account(), &chat_id()).unwrap(), 20);
}

#[test]
fn delivery_status_never_moves_backwards() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store
        .upsert_message(&text("m1", "chat", 1, "hi", Direction::Outgoing))
        .unwrap();
    let id = MessageId::new("m1");

    assert!(store
        .set_message_status(&account(), &id, &DeliveryStatus::Read)
        .unwrap());
    // A late "delivered" must not undo "read".
    assert!(!store
        .set_message_status(&account(), &id, &DeliveryStatus::Delivered)
        .unwrap());
    // Nor may a stale copy of the message from a history page.
    store
        .upsert_message(&text("m1", "chat", 1, "hi", Direction::Outgoing))
        .unwrap();
    let stored = store.message(&account(), &id).unwrap().unwrap();
    assert_eq!(stored.status, DeliveryStatus::Read);
}

#[test]
fn reactions_fold_into_their_target() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store
        .upsert_messages(&[
            text("m1", "chat", 1, "great news", Direction::Outgoing),
            reaction("r1", "m1", "ana", 10, "👍"),
            reaction("r2", "m1", "bruno", 11, "👍"),
            reaction("r3", "m1", "carla", 12, "❤️"),
            // Ana changes her mind; the older reaction is replaced.
            reaction("r4", "m1", "ana", 20, "❤️"),
            // An out-of-order replay of her first reaction is ignored.
            reaction("r1", "m1", "ana", 10, "👍"),
            // Bruno removes his.
            reaction("r5", "m1", "bruno", 30, ""),
        ])
        .unwrap();

    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    assert_eq!(messages.len(), 1, "reactions are not bubbles");
    assert_eq!(
        messages[0].reactions,
        vec![ReactionSummary {
            emoji: "❤️".into(),
            count: 2,
            from_me: false,
            // Who they are, in the order they reacted.
            by: vec![
                Reactor {
                    sender: ContactId::new("carla"),
                    from_me: false
                },
                Reactor {
                    sender: ContactId::new("ana"),
                    from_me: false
                },
            ],
        }]
    );
    // The chat preview is still the text message, not a reaction.
    let chat = store.chat(&account(), &chat_id()).unwrap().unwrap();
    assert_eq!(chat.last_message.unwrap().text, "great news");
}

#[test]
fn reactions_say_who_reacted_and_which_is_the_accounts_own() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let mut mine = reaction("r2", "m1", "me", 11, "👍");
    mine.direction = Direction::Outgoing;
    store
        .upsert_messages(&[
            text("m1", "chat", 1, "great news", Direction::Incoming),
            text("m2", "chat", 2, "and more", Direction::Incoming),
            reaction("r1", "m1", "ana", 10, "👍"),
            mine,
            reaction("r3", "m1", "carla", 12, "❤️"),
            reaction("r4", "m2", "bruno", 13, "😂"),
        ])
        .unwrap();

    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    let who = |message: &StoredMessage| -> Vec<(String, Vec<(String, bool)>)> {
        message
            .reactions
            .iter()
            .map(|reaction| {
                assert_eq!(reaction.count as usize, reaction.by.len());
                assert_eq!(reaction.from_me, reaction.by.iter().any(|by| by.from_me));
                (
                    reaction.emoji.clone(),
                    reaction
                        .by
                        .iter()
                        .map(|by| (by.sender.to_string(), by.from_me))
                        .collect(),
                )
            })
            .collect()
    };
    // Most used first; within an emoji, the first to react first.
    assert_eq!(
        who(&messages[0]),
        vec![
            (
                "👍".to_owned(),
                vec![("ana".to_owned(), false), ("me".to_owned(), true)]
            ),
            ("❤️".to_owned(), vec![("carla".to_owned(), false)]),
        ]
    );
    // Each message has its own.
    assert_eq!(
        who(&messages[1]),
        vec![("😂".to_owned(), vec![("bruno".to_owned(), false)])]
    );
}

#[test]
fn replies_are_resolved_from_the_local_copy() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let mut quoted = text("m1", "chat", 1, "", Direction::Incoming);
    quoted.sender_name = Some("Ana".into());
    quoted.content = MessageContent::Media(Media {
        caption: Some("the beach".into()),
        ..Media::new(MediaKind::Image)
    });
    let mut reply = text("m2", "chat", 2, "lovely", Direction::Outgoing);
    reply.reply_to = Some(ReplyRef {
        message_id: MessageId::new("m1"),
        sender_name: None,
        preview: None,
    });
    store.upsert_messages(&[quoted, reply]).unwrap();

    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    let quote = messages[1].message.reply_to.as_ref().unwrap();
    assert_eq!(quote.preview.as_deref(), Some("Photo · the beach"));
    assert_eq!(quote.sender_name.as_deref(), Some("Ana"));
}

#[test]
fn full_text_search_ignores_case_and_accents_and_tolerates_syntax() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    store.upsert_chat(&chat("other", "Bruno"), false).unwrap();
    let mut photo = text("m3", "other", 3, "", Direction::Incoming);
    photo.content = MessageContent::Media(Media {
        caption: Some("Camión nuevo".into()),
        ..Media::new(MediaKind::Image)
    });
    store
        .upsert_messages(&[
            text(
                "m1",
                "chat",
                1,
                "El camión llega mañana",
                Direction::Incoming,
            ),
            text("m2", "chat", 2, "nothing to see", Direction::Outgoing),
            photo,
        ])
        .unwrap();

    let hits = store.search_messages(&account(), "CAMION", 10).unwrap();
    let ids: Vec<_> = hits.iter().map(|h| h.message.id.as_str()).collect();
    assert_eq!(ids, ["m3", "m1"], "newest first, captions included");
    assert_eq!(hits[0].chat_title, "Bruno");

    // Prefix matching, every word required.
    assert_eq!(
        store
            .search_messages(&account(), "cam lleg", 10)
            .unwrap()
            .len(),
        1
    );
    // FTS operators typed by the user are data, not syntax.
    assert!(store
        .search_messages(&account(), "\"camión\" AND (", 10)
        .is_ok());
    assert!(store
        .search_messages(&account(), "   ", 10)
        .unwrap()
        .is_empty());

    // Edits are re-indexed.
    store
        .upsert_message(&text(
            "m2",
            "chat",
            2,
            "the truck arrived",
            Direction::Outgoing,
        ))
        .unwrap();
    assert_eq!(
        store
            .search_messages(&account(), "truck", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(store
        .search_messages(&account(), "nothing", 10)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn writes_announce_what_changed() {
    let store = Store::open_in_memory().unwrap();
    let mut listener = store.subscribe();
    seed_account(&store);
    assert_eq!(listener.next().await, Some(StoreChange::Accounts));

    store
        .upsert_message(&text("m1", "chat", 1, "hi", Direction::Incoming))
        .unwrap();
    assert_eq!(
        listener.next().await,
        Some(StoreChange::Messages {
            account_id: account(),
            chat_id: chat_id()
        })
    );
    assert_eq!(
        listener.next().await,
        Some(StoreChange::Chats {
            account_id: account()
        })
    );
    // Nothing changed, nothing announced.
    store
        .upsert_message(&text("m1", "chat", 1, "hi", Direction::Incoming))
        .unwrap();
    assert_eq!(listener.try_next(), None);
}

// ----- outbox -----------------------------------------------------------

/// A store with the mock's first account and chat, plus the ids to use.
async fn mock_store(mock: &MockProvider) -> (Arc<Store>, AccountId, ChatId) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let accounts = mock.list_accounts().await.unwrap();
    store.upsert_accounts("mock", &accounts).unwrap();
    let account = accounts[0].id.clone();
    let chat = mock
        .list_chats(&account, None)
        .await
        .unwrap()
        .items
        .remove(0);
    store.upsert_chat(&chat, false).unwrap();
    (store, account, chat.id)
}

fn outgoing_to(account: &AccountId, chat: &ChatId, client_id: &str, body: &str) -> OutgoingMessage {
    OutgoingMessage {
        client_id: ClientMessageId::new(client_id),
        account_id: account.clone(),
        chat_id: chat.clone(),
        content: OutgoingContent::Text { body: body.into() },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    }
}

fn status_of(store: &Store, account: &AccountId, chat: &ChatId, client_id: &str) -> DeliveryStatus {
    store
        .messages(account, chat, 500)
        .unwrap()
        .into_iter()
        .find(|m| m.message.client_id.as_ref().map(|c| c.as_str()) == Some(client_id))
        .expect("the message is in the store")
        .message
        .status
}

#[test]
fn queueing_shows_a_pending_message_and_is_idempotent() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let now = Timestamp::from_millis(1_000);
    store.enqueue(&outgoing("c1", "hello"), now, HOUR).unwrap();
    store.enqueue(&outgoing("c1", "hello"), now, HOUR).unwrap();

    assert_eq!(store.outbox_pending().unwrap().len(), 1);
    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    assert_eq!(messages.len(), 1);
    let message = &messages[0].message;
    assert_eq!(message.status, DeliveryStatus::Pending);
    assert_eq!(message.direction, Direction::Outgoing);
    assert_eq!(message.sender, ContactId::new("me"));
    assert_eq!(message.id, local_message_id(&ClientMessageId::new("c1")));
}

#[test]
fn the_outbox_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.db");
    {
        let store = Store::open(&path, Some(&test_key())).unwrap();
        seed_account(&store);
        store
            .enqueue(&outgoing("c1", "hello"), Timestamp::from_millis(1), HOUR)
            .unwrap();
    }
    let store = Store::open(&path, Some(&test_key())).unwrap();
    store.outbox_recover().unwrap();
    let pending = store.outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message, outgoing("c1", "hello"));
}

#[tokio::test]
async fn a_transient_failure_stays_pending_and_is_retried_with_the_same_id() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let config = OutboxConfig::default();
    let t0 = Timestamp::from_millis(1_000_000);
    store
        .enqueue(&outgoing_to(&account, &chat, "c1", "hello"), t0, HOUR)
        .unwrap();

    mock.fail_next_sends([
        ProviderError::Transient("connection reset".into()),
        ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(7)),
        },
    ]);

    // Attempt 1: the network drops.
    let pass = run_outbox_pass(&store, &mock, &config, t0).await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Pending
    );
    let entry = store.outbox_pending().unwrap().remove(0);
    assert_eq!(entry.attempts, 1);
    assert_eq!(entry.next_attempt_at.as_millis(), t0.as_millis() + 2_000);

    // Not due yet: nothing is attempted.
    let pass = run_outbox_pass(&store, &mock, &config, t0).await.unwrap();
    assert_eq!(pass, OutboxPass::default());
    assert_eq!(mock.send_calls(), 1);

    // Attempt 2: rate limited, the provider's delay is honoured.
    let t1 = Timestamp::from_millis(t0.as_millis() + 2_000);
    run_outbox_pass(&store, &mock, &config, t1).await.unwrap();
    let entry = store.outbox_pending().unwrap().remove(0);
    assert_eq!(entry.next_attempt_at.as_millis(), t1.as_millis() + 7_000);
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Pending
    );

    // Attempt 3 goes through.
    let t2 = Timestamp::from_millis(t1.as_millis() + 7_000);
    let pass = run_outbox_pass(&store, &mock, &config, t2).await.unwrap();
    assert_eq!(pass.sent, 1);
    assert!(store.outbox_pending().unwrap().is_empty());
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Sent
    );
    assert_eq!(mock.send_calls(), 3);
    assert_eq!(mock.delivered_count(), 1);
}

#[tokio::test]
async fn a_lost_answer_never_sends_twice() {
    // The provider takes longer than the send timeout: the message reaches
    // WhatsApp but the client never hears back, so it tries again.
    let mock = MockProvider::new(MockConfig {
        send_latency: Duration::from_millis(80),
        ..MockConfig::quiet()
    });
    let (store, account, chat) = mock_store(&mock).await;
    let impatient = OutboxConfig {
        send_timeout: Duration::from_millis(10),
        ..OutboxConfig::default()
    };
    let t0 = Timestamp::from_millis(1_000_000);
    store
        .enqueue(&outgoing_to(&account, &chat, "c1", "hello"), t0, HOUR)
        .unwrap();

    let pass = run_outbox_pass(&store, &mock, &impatient, t0)
        .await
        .unwrap();
    assert_eq!(pass.retried, 1, "a timeout is transient");
    assert_eq!(mock.delivered_count(), 1, "the provider did send it");
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Pending
    );

    // A crash here would leave the row mid-flight; recovery requeues it.
    store.outbox_recover().unwrap();

    let patient = OutboxConfig::default();
    let t1 = Timestamp::from_millis(t0.as_millis() + 60_000);
    let first = run_outbox_pass(&store, &mock, &patient, t1).await.unwrap();
    assert_eq!(first.sent, 1);
    // And a duplicate submission of the same message is harmless too.
    let duplicate = mock
        .send(outgoing_to(&account, &chat, "c1", "hello"))
        .await
        .unwrap();

    assert_eq!(mock.delivered_count(), 1, "exactly one message exists");
    let copies: Vec<_> = store
        .messages(&account, &chat, 500)
        .unwrap()
        .into_iter()
        .filter(|m| m.message.content == MessageContent::text("hello"))
        .collect();
    assert_eq!(copies.len(), 1, "exactly one bubble exists");
    assert_eq!(copies[0].message.id, duplicate.message_id);
    assert_eq!(copies[0].message.status, DeliveryStatus::Sent);
}

#[tokio::test]
async fn only_terminal_errors_are_shown_as_failed() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let config = OutboxConfig::default();
    let t0 = Timestamp::from_millis(1_000_000);
    store
        .enqueue(&outgoing_to(&account, &chat, "c1", "hello"), t0, HOUR)
        .unwrap();
    mock.fail_next_sends([ProviderError::Rejected {
        code: "not_on_whatsapp".into(),
        message: "The recipient is not on WhatsApp".into(),
    }]);

    let pass = run_outbox_pass(&store, &mock, &config, t0).await.unwrap();
    assert_eq!(pass.failed, 1);
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Failed {
            reason: "The recipient is not on WhatsApp".into()
        }
    );
    // A failed message is not retried behind the user's back.
    assert!(store.outbox_pending().unwrap().is_empty());
    run_outbox_pass(&store, &mock, &config, t0).await.unwrap();
    assert_eq!(mock.send_calls(), 1);
}

#[tokio::test]
async fn a_message_that_cannot_be_sent_in_time_fails() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let config = OutboxConfig::default();
    let t0 = Timestamp::from_millis(1_000_000);
    store
        .enqueue(&outgoing_to(&account, &chat, "c1", "hello"), t0, HOUR)
        .unwrap();
    mock.fail_next_sends([ProviderError::Transient("offline".into())]);
    run_outbox_pass(&store, &mock, &config, t0).await.unwrap();

    let much_later = Timestamp::from_millis(t0.as_millis() + HOUR.as_millis() as i64 + 1);
    let pass = run_outbox_pass(&store, &mock, &config, much_later)
        .await
        .unwrap();
    assert_eq!(pass.failed, 1);
    assert!(matches!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Failed { .. }
    ));
    assert_eq!(mock.delivered_count(), 0);
}

#[tokio::test]
async fn messages_of_a_chat_keep_their_order_across_retries() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let config = OutboxConfig::default();
    let t0 = Timestamp::from_millis(1_000_000);
    for (i, body) in ["one", "two", "three"].iter().enumerate() {
        store
            .enqueue(
                &outgoing_to(&account, &chat, &format!("c{i}"), body),
                Timestamp::from_millis(t0.as_millis() + i as i64),
                HOUR,
            )
            .unwrap();
    }
    mock.fail_next_sends([ProviderError::Transient("eof".into())]);

    // "one" fails: "two" and "three" must wait behind it.
    let pass = run_outbox_pass(
        &store,
        &mock,
        &config,
        Timestamp::from_millis(t0.as_millis() + 10),
    )
    .await
    .unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    assert_eq!(mock.send_calls(), 1);

    let later = Timestamp::from_millis(t0.as_millis() + 5_000);
    let pass = run_outbox_pass(&store, &mock, &config, later)
        .await
        .unwrap();
    assert_eq!(pass.sent, 3);
    let sent: Vec<_> = mock
        .fetch_messages(&account, &chat, None, 3)
        .await
        .unwrap()
        .items
        .into_iter()
        .rev()
        .map(|m| m.content)
        .collect();
    assert_eq!(
        sent,
        ["one", "two", "three"].map(MessageContent::text).to_vec()
    );
}

#[tokio::test]
async fn reconnecting_flushes_what_was_waiting() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        Handle::current(),
    );
    mock.fail_next_sends([ProviderError::Transient("account reconnecting".into())]);
    engine.send_text(&account, &chat, "hello", None).unwrap();
    assert_eq!(engine.flush_outbox().await.unwrap().retried, 1);
    // Still backing off: a pass now does nothing.
    assert_eq!(engine.flush_outbox().await.unwrap(), OutboxPass::default());

    engine
        .apply_event(ProviderEvent::ConnectionChanged {
            account_id: account.clone(),
            state: ConnectionState::Connected,
        })
        .unwrap();
    assert_eq!(engine.flush_outbox().await.unwrap().sent, 1);
    assert_eq!(mock.delivered_count(), 1);
}

// ----- sync -------------------------------------------------------------

fn engine_for(mock: &MockProvider) -> SyncEngine {
    SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        Handle::current(),
    )
}

async fn wait_for_account_sync(condition: impl Fn() -> bool) {
    for _ in 0..500 {
        if condition() {
            return;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        condition(),
        "account sync did not finish while the stream stayed open"
    );
}

#[tokio::test(start_paused = true)]
async fn an_external_link_appears_without_restarting_an_empty_client() {
    use client_provider::{HistoryImport, NewAccount};
    let mock = MockProvider::quiet();
    for account in mock.list_accounts().await.unwrap() {
        mock.delete_account(&account.id).await.unwrap();
    }
    let engine = engine_for(&mock);
    let store = engine.store();
    let before = mock.account_calls();
    engine.start();
    wait_for_account_sync(|| mock.account_calls() > before).await;
    assert!(store.accounts().unwrap().is_empty());

    // The dashboard creates and links a number; the desktop receives only
    // its live connection event, not the full result of the web request.
    let linked = mock
        .create_account(&NewAccount {
            name: Some("Web support".into()),
            place: None,
            pairing_phone: Some("+15550009999".into()),
            history: HistoryImport::Recent,
            request_id: "web-link".into(),
        })
        .await
        .unwrap()
        .account
        .id;
    mock.complete_link(&linked);
    let expected = mock.link_status(&linked).await.unwrap().account;
    let chat = mock.import_history(&linked, "+15550001111", &["Already on the phone"]);
    let before = mock.account_calls();
    for _ in 0..10 {
        mock.push_event(ProviderEvent::ConnectionChanged {
            account_id: linked.clone(),
            state: ConnectionState::Connected,
        });
    }
    wait_for_account_sync(|| store.chat(&linked, &chat).unwrap().is_some()).await;
    assert_eq!(store.accounts().unwrap(), vec![expected]);
    assert_eq!(
        mock.account_calls(),
        before + 1,
        "duplicate events share one refresh"
    );
    engine.shutdown();
}

#[tokio::test(start_paused = true)]
async fn an_external_reconnection_refreshes_metadata_and_missing_chats() {
    use client_provider::AccountChange;
    let mock = MockProvider::quiet();
    let account = mock.list_accounts().await.unwrap()[0].id.clone();
    mock.push_event(ProviderEvent::ConnectionChanged {
        account_id: account.clone(),
        state: ConnectionState::Disconnected { reason: None },
    });
    let engine = engine_for(&mock);
    let store = engine.store();
    engine.start();
    wait_for_account_sync(|| !store.accounts().unwrap().is_empty()).await;
    mock.update_account(&account, AccountChange::Rename("Renamed on the web".into()))
        .await
        .unwrap();
    let chat = mock.import_history(&account, "+15550008888", &["Imported while disconnected"]);
    assert!(store.chat(&account, &chat).unwrap().is_none());
    let before = mock.account_calls();
    mock.push_event(ProviderEvent::ConnectionChanged {
        account_id: account.clone(),
        state: ConnectionState::Connected,
    });
    wait_for_account_sync(|| store.chat(&account, &chat).unwrap().is_some()).await;
    let stored = store
        .accounts()
        .unwrap()
        .into_iter()
        .find(|a| a.id == account)
        .unwrap();
    assert_eq!(stored, mock.account(&account).unwrap());
    assert_eq!(mock.account_calls(), before + 1);
    engine.shutdown();
}

#[tokio::test(start_paused = true)]
async fn external_account_refresh_retries_without_another_connection_event() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.start();
    wait_for_account_sync(|| !engine.store().accounts().unwrap().is_empty()).await;
    let account = engine.store().accounts().unwrap()[0].id.clone();
    mock.push_event(ProviderEvent::ConnectionChanged {
        account_id: account.clone(),
        state: ConnectionState::Disconnected { reason: None },
    });
    wait_for_account_sync(|| {
        !engine.store().accounts().unwrap()[0]
            .connection
            .is_connected()
    })
    .await;
    let chat = mock.import_history(&account, "+15550007777", &["After a failed refresh"]);
    let before = mock.account_calls();
    mock.fail_next_account_lists([ProviderError::Transient("offline".into())]);
    mock.push_event(ProviderEvent::ConnectionChanged {
        account_id: account.clone(),
        state: ConnectionState::Connected,
    });
    wait_for_account_sync(|| mock.account_calls() > before).await;
    tokio::time::advance(Duration::from_secs(2)).await;
    wait_for_account_sync(|| engine.store().chat(&account, &chat).unwrap().is_some()).await;
    assert_eq!(mock.account_calls(), before + 2);
    engine.shutdown();
}

#[tokio::test]
async fn refresh_copies_the_provider_into_the_store() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store();

    let accounts = store.accounts().unwrap();
    assert_eq!(accounts.len(), 2);
    let chats = store.chats(&accounts[0].id, None).unwrap();
    assert!(chats.len() >= 24);
    assert!(chats.iter().all(|c| c.last_message.is_some()));
    assert!(chats[0].pinned, "pinned chats come first");

    // The list is complete, previews included, without one request for
    // history: that waits for a chat to be opened.
    assert_eq!(mock.history_calls(), 0);
    for chat in &chats {
        let state = store.history_state(&chat.account_id, &chat.id).unwrap();
        assert!(!state.loaded);
        assert_eq!(store.message_count(&chat.account_id, &chat.id).unwrap(), 1);
    }

    // The provider's chat order (by activity) matches the store's, pins aside.
    let provider_first = mock.list_chats(&accounts[0].id, None).await.unwrap().items;
    let unpinned: Vec<_> = chats.iter().filter(|c| !c.pinned).map(|c| &c.id).collect();
    let expected: Vec<_> = provider_first
        .iter()
        .filter(|c| !c.pinned)
        .map(|c| &c.id)
        .collect();
    assert_eq!(unpinned[..5], expected[..5]);

    // A second refresh finds nothing new.
    let mut listener = store.subscribe();
    engine.refresh().await.unwrap();
    let mut message_changes = 0;
    while let Some(change) = listener.try_next() {
        if matches!(change, StoreChange::Messages { .. }) {
            message_changes += 1;
        }
    }
    assert_eq!(message_changes, 0);
}

#[tokio::test]
async fn older_history_is_paged_in_on_demand() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store();
    let account = store.accounts().unwrap().remove(0).id;
    // The seeded long thread.
    let long = mock
        .list_chats(&account, None)
        .await
        .unwrap()
        .items
        .remove(0)
        .id;

    let mut pages = 0;
    while engine.fetch_older(&account, &long).await.unwrap() {
        pages += 1;
        assert!(pages < 100, "paging terminates");
    }
    assert!(pages >= 12);
    assert!(store.history_state(&account, &long).unwrap().complete);

    // Everything the provider has is now local: bubbles plus reactions.
    let mut provider_total = 0;
    let mut provider_bubbles = 0;
    let mut cursor = None;
    loop {
        let page = mock
            .fetch_messages(&account, &long, cursor, 200)
            .await
            .unwrap();
        provider_total += page.items.len();
        provider_bubbles += page
            .items
            .iter()
            .filter(|m| !matches!(m.content, MessageContent::Reaction { .. }))
            .count();
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert!(provider_total > provider_bubbles);
    assert_eq!(
        store.message_count(&account, &long).unwrap(),
        provider_bubbles
    );
    let messages = store.messages(&account, &long, 10_000).unwrap();
    assert!(messages.iter().any(|m| !m.reactions.is_empty()));
    assert!(messages.iter().any(|m| m.message.reply_to.is_some()));
    assert!(messages
        .windows(2)
        .all(|w| w[0].message.timestamp <= w[1].message.timestamp));
}

#[tokio::test]
async fn live_events_update_the_store_and_notify() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(3);
    let unread_before = chat.unread_count;
    let mut listener = store.subscribe();

    // Typing shows up, without touching the database.
    engine
        .apply_event(ProviderEvent::Presence {
            account_id: account.clone(),
            chat_id: chat.id.clone(),
            contact_id: ContactId::new("them"),
            state: PresenceState::Typing,
        })
        .unwrap();
    assert_eq!(
        engine.presence(&account, &chat.id),
        Some(PresenceState::Typing)
    );
    assert_eq!(
        listener.try_next(),
        Some(StoreChange::Presence {
            account_id: account.clone(),
            chat_id: chat.id.clone()
        })
    );

    // A message arrives: stored, chat moved to the top, typing cleared.
    let mut incoming = text(
        "live-1",
        chat.id.as_str(),
        0,
        "are you there?",
        Direction::Incoming,
    );
    incoming.account_id = account.clone();
    incoming.sender = ContactId::new("them");
    incoming.timestamp = Timestamp::now();
    let mut updated_chat = mock
        .list_chats(&account, None)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|c| c.id == chat.id)
        .unwrap();
    updated_chat.unread_count = unread_before + 1;
    updated_chat.last_message = Some(incoming.clone());
    engine
        .apply_event(ProviderEvent::MessageUpserted(incoming))
        .unwrap();
    engine
        .apply_event(ProviderEvent::ChatUpdated(updated_chat))
        .unwrap();

    assert_eq!(engine.presence(&account, &chat.id), None);
    let chats = store.chats(&account, None).unwrap();
    let top_unpinned = chats.iter().find(|c| !c.pinned).unwrap();
    assert_eq!(top_unpinned.id, chat.id);
    assert_eq!(top_unpinned.unread_count, unread_before + 1);
    assert_eq!(
        top_unpinned.last_message.as_ref().unwrap().text,
        "are you there?"
    );
    let mut saw_message_change = false;
    while let Some(change) = listener.try_next() {
        saw_message_change |=
            matches!(change, StoreChange::Messages { chat_id, .. } if chat_id == chat.id);
    }
    assert!(saw_message_change);

    // Reading clears the badge at once, before the provider hears of it.
    engine.mark_read(&account, &chat.id);
    assert_eq!(
        store
            .chat(&account, &chat.id)
            .unwrap()
            .unwrap()
            .unread_count,
        0
    );
}

#[tokio::test]
async fn an_event_that_overtakes_the_receipt_does_not_duplicate_the_message() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    let config = OutboxConfig::default();
    let t0 = Timestamp::now();
    store
        .enqueue(&outgoing_to(&account, &chat, "c1", "hello"), t0, HOUR)
        .unwrap();

    // The provider's copy arrives through the event stream first.
    let receipt = mock
        .send(outgoing_to(&account, &chat, "c1", "hello"))
        .await
        .unwrap();
    let mut echoed = mock
        .fetch_messages(&account, &chat, None, 1)
        .await
        .unwrap()
        .items
        .remove(0);
    echoed.status = DeliveryStatus::Delivered;
    store.upsert_message(&echoed).unwrap();

    // Then the outbox gets its (idempotent) receipt.
    run_outbox_pass(&store, &mock, &config, t0).await.unwrap();

    let copies: Vec<_> = store
        .messages(&account, &chat, 500)
        .unwrap()
        .into_iter()
        .filter(|m| m.message.content == MessageContent::text("hello"))
        .collect();
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].message.id, receipt.message_id);
    assert_eq!(
        copies[0].message.status,
        DeliveryStatus::Delivered,
        "the receipt's older status did not undo the event's"
    );
}

#[tokio::test]
async fn end_to_end_a_typed_message_goes_out_and_gets_read() {
    let mock = MockProvider::new(MockConfig {
        send_latency: Duration::from_millis(30),
        simulate_replies: true,
        time_scale: 0.01,
        ..MockConfig::quiet()
    });
    let engine = engine_for(&mock);
    let store = engine.store().clone();
    let mut listener = store.subscribe();
    engine.start();

    // Wait for the first refresh.
    let account = loop {
        listener.next().await.unwrap();
        if let Some(account) = store.accounts().unwrap().first() {
            if !store.chats(&account.id, None).unwrap().is_empty() {
                break account.id.clone();
            }
        }
    };
    let chat = store.chats(&account, None).unwrap().remove(0).id;

    let client_id = engine
        .send_text(&account, &chat, "see you at 8", None)
        .unwrap();
    // Visible at once, pending, before any network round trip.
    assert_eq!(
        status_of(&store, &account, &chat, client_id.as_str()),
        DeliveryStatus::Pending
    );

    // The mock accepts it, delivers it, reads it and answers.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut seen = vec![DeliveryStatus::Pending];
    loop {
        let status = status_of(&store, &account, &chat, client_id.as_str());
        if seen.last() != Some(&status) {
            seen.push(status.clone());
        }
        let newest = store
            .messages(&account, &chat, 1)
            .unwrap()
            .remove(0)
            .message;
        if status == DeliveryStatus::Read && newest.direction == Direction::Incoming {
            break;
        }
        tokio::time::timeout_at(deadline, listener.next())
            .await
            .expect("the message is read and answered in time");
    }
    assert!(
        seen.windows(2).all(|w| w[0].rank() < w[1].rank()),
        "{seen:?}"
    );
    assert_eq!(mock.delivered_count(), 1);
    engine.shutdown();
}

// ----- chat state ---------------------------------------------------------

/// Lets the engine's background tasks run, and a paused clock skip their
/// waits.
async fn settle() {
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_chat_change_shows_at_once_and_survives_a_bad_connection() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|c| !c.pinned)
        .unwrap()
        .id;

    // The connection drops twice. The user sees the pin regardless.
    mock.fail_next_chat_updates([
        ProviderError::Transient("eof".into()),
        ProviderError::RateLimited { retry_after: None },
    ]);
    engine.update_chat(&account, &chat, ChatChange::Pinned(true));
    assert!(store.chat(&account, &chat).unwrap().unwrap().pinned);
    assert!(!mock.chat(&account, &chat).unwrap().pinned);
    assert_eq!(
        store.chats(&account, None).unwrap()[0].id,
        chat,
        "a pinned chat goes to the top"
    );

    settle().await;
    assert!(mock.chat(&account, &chat).unwrap().pinned, "it got through");
    assert!(store.chat(&account, &chat).unwrap().unwrap().pinned);

    // Archiving takes the chat out of the list and into the archive.
    engine.update_chat(&account, &chat, ChatChange::Archived(true));
    assert!(store
        .chats(&account, None)
        .unwrap()
        .iter()
        .all(|c| c.id != chat));
    assert_eq!(store.archived_chats(&account).unwrap()[0].id, chat);

    // A mark as unread is one unread message, as far as anyone knows.
    engine.update_chat(&account, &chat, ChatChange::MarkedUnread);
    assert_eq!(
        store.chat(&account, &chat).unwrap().unwrap().unread_count,
        1
    );
}

#[tokio::test(start_paused = true)]
async fn a_chat_change_the_provider_refuses_is_put_back() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0);

    mock.fail_next_chat_updates([ProviderError::Rejected {
        code: "forbidden".into(),
        message: "no".into(),
    }]);
    engine.update_chat(&account, &chat.id, ChatChange::Muted(!chat.muted));
    assert_eq!(
        store.chat(&account, &chat.id).unwrap().unwrap().muted,
        !chat.muted
    );
    settle().await;
    assert_eq!(
        store.chat(&account, &chat.id).unwrap().unwrap().muted,
        chat.muted,
        "the refusal is final: the old state is back"
    );

    // The user changes their mind while the first change is still being
    // retried: the newer one stands, whatever happens to the older.
    mock.fail_next_chat_updates([ProviderError::Transient("eof".into())]);
    engine.update_chat(&account, &chat.id, ChatChange::Pinned(true));
    engine.update_chat(&account, &chat.id, ChatChange::Pinned(false));
    settle().await;
    assert!(!store.chat(&account, &chat.id).unwrap().unwrap().pinned);
    assert!(!mock.chat(&account, &chat.id).unwrap().pinned);
}

#[tokio::test]
async fn starting_a_chat_with_a_number_creates_it_once() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;

    let chat = engine
        .start_chat(&account, "+58 424-555 0199")
        .await
        .unwrap();
    assert_eq!(chat.as_str(), "+584245550199");
    let stored = store.chat(&account, &chat).unwrap().unwrap();
    assert_eq!(stored.kind, ChatKind::Direct);

    // Starting it again finds it, and keeps what the store knows.
    engine.update_chat(&account, &chat, ChatChange::Pinned(true));
    let again = engine.start_chat(&account, "+584245550199").await.unwrap();
    assert_eq!(again, chat);
    assert!(store.chat(&account, &chat).unwrap().unwrap().pinned);

    // What is not a number is refused, and nothing is created.
    let error = engine.start_chat(&account, "hello").await.unwrap_err();
    assert!(!error.is_transient());

    // The new chat can be written to like any other.
    engine.send_text(&account, &chat, "hi", None).unwrap();
    assert_eq!(engine.flush_outbox().await.unwrap().sent, 1);
}

// ----- encryption ---------------------------------------------------------

/// True when `needle` can be read in any of the database's files.
fn readable_on_disk(path: &std::path::Path, needle: &str) -> bool {
    ["", "-wal", "-shm"].iter().any(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        std::fs::read(name).is_ok_and(|bytes| {
            bytes
                .windows(needle.len())
                .any(|window| window == needle.as_bytes())
        })
    })
}

const SECRET: &str = "meet me behind the lighthouse";

fn fill_with_a_secret(store: &Store) {
    seed_account(store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    store
        .upsert_message(&text("m1", "chat", 1_000, SECRET, Direction::Incoming))
        .unwrap();
    store
        .enqueue(
            &outgoing("c1", "on my way"),
            Timestamp::from_millis(2_000),
            Duration::from_secs(3600),
        )
        .unwrap();
}

fn assert_filled(store: &Store) {
    let messages = store.messages(&account(), &chat_id(), 10).unwrap();
    assert_eq!(messages.len(), 2, "the message and the queued one");
    // Full-text search works on the encrypted file.
    let hits = store.search_messages(&account(), "lighthouse", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message.id.as_str(), "m1");
    // The outbox came along: what was waiting still is.
    assert_eq!(store.outbox_pending().unwrap().len(), 1);
}

#[test]
fn an_encrypted_database_opens_only_with_its_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    assert_eq!(file_state(&path).unwrap(), FileState::Missing);
    fill_with_a_secret(&Store::open(&path, Some(&test_key())).unwrap());

    // Nothing of it is readable in the file, its WAL included.
    assert_eq!(file_state(&path).unwrap(), FileState::Encrypted);
    assert!(!readable_on_disk(&path, SECRET));
    assert!(!readable_on_disk(&path, "lighthouse"));
    assert!(!readable_on_disk(&path, "SQLite format 3"));

    // The same key opens it again, with everything in it.
    let again = Store::open(&path, Some(&test_key())).unwrap();
    assert_filled(&again);
    drop(again);

    // Another key, or none, fails cleanly, and harms nothing.
    let wrong = StoreKey::from_bytes([7; 32]);
    assert!(matches!(
        Store::open(&path, Some(&wrong)),
        Err(StoreError::WrongKey)
    ));
    assert!(matches!(
        Store::open(&path, None),
        Err(StoreError::WrongKey)
    ));
    assert!(!Store::key_opens(&path, &wrong));
    assert!(Store::key_opens(&path, &test_key()));
    assert_filled(&Store::open(&path, Some(&test_key())).unwrap());
}

#[test]
fn an_encrypted_database_still_writes_ahead_and_reads_beside_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let store = Store::open(&path, Some(&test_key())).unwrap();
    fill_with_a_secret(&store);
    // WAL: the log is next to the file, and encrypted like it.
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    assert!(
        std::path::Path::new(&wal).exists(),
        "write-ahead logging is on"
    );
    assert!(!readable_on_disk(&path, SECRET));
    // The reader connection sees what the writer wrote.
    assert_filled(&store);
}

#[test]
fn a_plain_database_is_encrypted_in_place_with_everything_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    // What a version before encryption left on disk.
    fill_with_a_secret(&Store::open(&path, None).unwrap());
    assert_eq!(file_state(&path).unwrap(), FileState::Plain);
    assert!(readable_on_disk(&path, SECRET), "plain, as it used to be");

    Store::encrypt_plain(&path, &test_key()).unwrap();

    assert_eq!(file_state(&path).unwrap(), FileState::Encrypted);
    assert!(!readable_on_disk(&path, SECRET));
    assert!(!readable_on_disk(&path, "lighthouse"));
    let files: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != "store.db")
        .collect();
    assert!(
        files.is_empty(),
        "no plain copy or journal is left: {files:?}"
    );

    // Messages, the search index and the outbox all made it, and the
    // schema version with them: opening does not migrate from scratch.
    let store = Store::open(&path, Some(&test_key())).unwrap();
    assert_filled(&store);
    // And it is a working database: new messages are stored and found.
    store
        .upsert_message(&text(
            "m2",
            "chat",
            3_000,
            "by the harbour",
            Direction::Incoming,
        ))
        .unwrap();
    assert_eq!(
        store
            .search_messages(&account(), "harbour", 10)
            .unwrap()
            .len(),
        1
    );
    assert!(!readable_on_disk(&path, "harbour"));
}

#[test]
fn a_migration_that_fails_leaves_the_plain_database_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    fill_with_a_secret(&Store::open(&path, None).unwrap());

    // The place the encrypted copy is written to cannot be written to.
    let mut blocked = path.as_os_str().to_owned();
    blocked.push(".encrypting");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(std::path::Path::new(&blocked).join("keep"), b"x").unwrap();

    assert!(Store::encrypt_plain(&path, &test_key()).is_err());
    assert_eq!(file_state(&path).unwrap(), FileState::Plain);
    assert_filled(&Store::open(&path, None).unwrap());

    // What is not a plain database is refused, not mangled.
    let other = dir.path().join("encrypted.db");
    fill_with_a_secret(&Store::open(&other, Some(&test_key())).unwrap());
    let before = std::fs::read(&other).unwrap();
    assert!(Store::encrypt_plain(&other, &StoreKey::from_bytes([1; 32])).is_err());
    assert_eq!(std::fs::read(&other).unwrap(), before);
}

#[test]
fn a_key_survives_the_keychain_as_text_and_never_prints() {
    let key = StoreKey::from_bytes(*b"0123456789abcdef0123456789abcdef");
    assert_eq!(StoreKey::from_hex(&key.to_hex()), Some(key.clone()));
    assert_eq!(key.to_hex().len(), 64);
    assert_eq!(StoreKey::from_hex("abc"), None);
    assert_eq!(StoreKey::from_hex(&"zz".repeat(32)), None);
    let printed = format!("{key:?}");
    assert!(
        !printed.contains("3031") && !printed.contains("0123"),
        "{printed}"
    );
}

// ----- history in the background -------------------------------------------

fn engine_with(mock: &MockProvider, history: HistoryMode) -> SyncEngine {
    SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig {
            history,
            ..SyncConfig::default()
        },
        Handle::current(),
    )
}

/// (chats with their newest page, chats with all their history).
fn loaded(store: &Store) -> (usize, usize) {
    let mut counts = (0, 0);
    for account in store.accounts().unwrap() {
        for chat in store.chats(&account.id, None).unwrap() {
            let state = store.history_state(&account.id, &chat.id).unwrap();
            counts.0 += usize::from(state.loaded);
            counts.1 += usize::from(state.complete);
        }
    }
    counts
}

fn chat_count(store: &Store) -> usize {
    store
        .accounts()
        .unwrap()
        .iter()
        .map(|account| store.chats(&account.id, None).unwrap().len())
        .sum()
}

#[tokio::test(start_paused = true)]
async fn history_is_loaded_as_far_as_the_mode_says() {
    // When a chat is opened: nothing ahead of time.
    let mock = MockProvider::quiet();
    let engine = engine_with(&mock, HistoryMode::OnOpen);
    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    assert_eq!(mock.history_calls(), 0);
    assert_eq!(loaded(engine.store()), (0, 0));

    // Recent chats: the newest page of the five most recent of each of the
    // two accounts, one request each.
    let mock = MockProvider::quiet();
    let engine = engine_with(&mock, HistoryMode::Recent(5));
    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    assert_eq!(mock.history_calls(), 10);
    assert_eq!(loaded(engine.store()).0, 10);
    // They are the most recent ones.
    let store = engine.store();
    let account = store.accounts().unwrap().remove(0).id;
    let mut chats = store.chats(&account, None).unwrap();
    chats.sort_by_key(|c| std::cmp::Reverse(c.last_message.as_ref().map(|m| m.timestamp)));
    for (index, chat) in chats.iter().enumerate() {
        let state = store.history_state(&account, &chat.id).unwrap();
        assert_eq!(state.loaded, index < 5, "chat {index}");
    }
    // A second pass has nothing left to ask.
    engine.preload().await.unwrap();
    assert_eq!(mock.history_calls(), 10);

    // Changing the mode takes effect on the next pass.
    engine.set_history(HistoryMode::Everything);
    engine.preload().await.unwrap();
    let chats = chat_count(store);
    assert_eq!(loaded(store), (chats, chats), "every page of every chat");
}

#[tokio::test(start_paused = true)]
async fn preloading_waits_out_a_rate_limit_and_stops_at_a_dropped_connection() {
    let mock = MockProvider::quiet();
    let engine = engine_with(&mock, HistoryMode::Recent(3));
    engine.refresh().await.unwrap();

    // Told to slow down: the request is repeated after the wait it was
    // given, and the pass completes.
    mock.fail_next_history([ProviderError::RateLimited {
        retry_after: Some(Duration::from_secs(7)),
    }]);
    let started = tokio::time::Instant::now();
    engine.preload().await.unwrap();
    assert!(started.elapsed() >= Duration::from_secs(7), "it waited");
    assert_eq!(mock.history_calls(), 7, "six chats, one request repeated");
    assert_eq!(loaded(engine.store()).0, 6);

    // The connection drops: the pass ends there, as a failure to retry.
    engine.set_history(HistoryMode::Recent(6));
    mock.fail_next_history([ProviderError::Transient("eof".into())]);
    let error = engine.preload().await.unwrap_err();
    assert!(error.is_transient());
    let done = loaded(engine.store()).0;
    assert!((6..12).contains(&done), "what was fetched is kept: {done}");
    // Back online: it picks up what is missing, and only that.
    let before = mock.history_calls();
    engine.preload().await.unwrap();
    assert_eq!(loaded(engine.store()).0, 12);
    assert_eq!(mock.history_calls() - before, 12 - done);
}

#[tokio::test(start_paused = true)]
async fn preloading_stays_under_the_rate_limit() {
    let mock = MockProvider::quiet();
    let engine = engine_with(&mock, HistoryMode::Everything);
    engine.refresh().await.unwrap();
    let started = tokio::time::Instant::now();
    engine.preload().await.unwrap();
    let calls = mock.history_calls() as f64;
    let minutes = started.elapsed().as_secs_f64() / 60.;
    assert!(calls > 60., "a real amount of work: {calls}");
    // 250 ms apart is 240 a minute at most; the API allows 600.
    assert!(
        calls / minutes <= 245.,
        "{calls} requests in {minutes:.2} minutes"
    );
}

#[tokio::test(start_paused = true)]
async fn a_chat_read_before_gets_its_hole_filled_whatever_the_mode() {
    let mock = MockProvider::quiet();
    let engine = engine_with(&mock, HistoryMode::OnOpen);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(3);
    // The user opens a chat: its history loads, by itself.
    engine.fetch_latest(&account, &chat.id).await.unwrap();
    assert_eq!(mock.history_calls(), 1);

    // While the client was away, messages arrived in it.
    let template = store
        .messages(&account, &chat.id, 1)
        .unwrap()
        .remove(0)
        .message;
    for n in 0..3 {
        mock.push_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(format!("away-{n}")),
            client_id: None,
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 1_000 + n),
            content: MessageContent::text(format!("while you were away {n}")),
            direction: Direction::Incoming,
            ..template.clone()
        }));
    }
    // The refresh brings the last one with the chat list; the other two
    // would be a hole in a conversation the user has already read.
    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    assert_eq!(mock.history_calls(), 2, "one request, for that chat only");
    let texts: Vec<_> = store
        .messages(&account, &chat.id, 3)
        .unwrap()
        .into_iter()
        .map(|m| m.message.id.to_string())
        .collect();
    assert_eq!(texts, ["away-0", "away-1", "away-2"]);
}

// ----- credentials that are no longer good ---------------------------------

fn unauthorized() -> ProviderError {
    ProviderError::Unauthorized("the API key was revoked".into())
}

#[tokio::test(start_paused = true)]
async fn a_revoked_key_stops_the_engine_instead_of_being_retried() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let mut auth = engine.auth_lost();
    assert_eq!(*auth.borrow(), None);

    mock.fail_next_account_lists([unauthorized()]);
    engine.start();
    auth.changed().await.unwrap();
    assert_eq!(auth.borrow().as_deref(), Some("the API key was revoked"));
    assert!(engine.is_auth_lost());

    // Asked once. Not again, however long it is left running.
    let asked = mock.account_calls();
    assert_eq!(asked, 1);
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert_eq!(mock.account_calls(), asked);
    assert_eq!(mock.history_calls(), 0);
    engine.shutdown();
}

#[tokio::test(start_paused = true)]
async fn a_dropped_connection_is_never_mistaken_for_a_revoked_key() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    mock.fail_next_account_lists([
        ProviderError::Transient("could not connect".into()),
        ProviderError::Transient("HTTP 503".into()),
        ProviderError::RateLimited { retry_after: None },
        ProviderError::Transient("the request timed out".into()),
    ]);
    engine.start();
    tokio::time::sleep(Duration::from_secs(120)).await;

    // It kept trying, got through, and nobody was signed out.
    assert!(!engine.is_auth_lost());
    assert_eq!(*engine.auth_lost().borrow(), None);
    assert!(mock.account_calls() >= 5);
    assert_eq!(engine.store().accounts().unwrap().len(), 2);
    engine.shutdown();
}

#[tokio::test(start_paused = true)]
async fn a_revoked_key_keeps_queued_messages_for_the_next_sign_in() {
    let mock = MockProvider::quiet();
    let (store, account, chat) = mock_store(&mock).await;
    store
        .enqueue(
            &outgoing_to(&account, &chat, "c1", "hello"),
            Timestamp::now(),
            Duration::from_secs(3600),
        )
        .unwrap();
    mock.fail_next_sends([unauthorized()]);
    let pass = run_outbox_pass(&store, &mock, &OutboxConfig::default(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.failed), (0, 0));
    assert_eq!(
        pass.unauthorized.as_deref(),
        Some("the API key was revoked")
    );
    // Still waiting, not failed in front of the user.
    assert_eq!(store.outbox_pending().unwrap().len(), 1);
    assert_eq!(
        status_of(&store, &account, &chat, "c1"),
        DeliveryStatus::Pending
    );
    // With good credentials again, it goes out.
    let pass = run_outbox_pass(&store, &mock, &OutboxConfig::default(), Timestamp::now())
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
}

// ----- pictures -------------------------------------------------------------

use crate::imaging::tests::png;

/// A used engine on the mock, its first account and three of its chats.
async fn engine_with_chats(mock: &MockProvider) -> (SyncEngine, AccountId, Vec<ChatId>) {
    let engine = engine_for(mock);
    engine.refresh().await.unwrap();
    let account = engine.store().accounts().unwrap().remove(0).id;
    let chats = engine
        .store()
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .take(3)
        .map(|chat| chat.id)
        .collect();
    (engine, account, chats)
}

/// Makes a stored picture look as if it was checked this long ago.
fn age_avatar(store: &Store, account: &AccountId, chat: &ChatId, hours: i64) {
    let then = Timestamp::from_millis(Timestamp::now().as_millis() - hours * 3_600_000);
    store.touch_avatar(account, chat, then).unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_picture_is_fetched_once_kept_small_and_reused() {
    let mock = MockProvider::quiet();
    let (engine, account, chats) = engine_with_chats(&mock).await;
    let store = engine.store().clone();
    mock.set_avatar(&account, &chats[0], Some(("pic-1", png(640, 640))));

    let mut changes = store.subscribe();
    let next = engine.load_avatar(&account, &chats[0]).await;
    assert_eq!(next, AVATAR_TTL, "good for a day");
    assert_eq!(mock.avatar_calls(), 1);
    let stored = store.avatar(&account, &chats[0]).unwrap().unwrap();
    assert_eq!(stored.picture_id.as_deref(), Some("pic-1"));
    // Kept at avatar size, whatever was uploaded.
    let image = image::load_from_memory(stored.image.as_deref().unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (96, 96));
    assert!(stored.image.unwrap().len() < 20_000);
    // The view is told.
    let mut told = false;
    while let Some(change) = changes.try_next() {
        told |= change
            == StoreChange::Avatar {
                account_id: account.clone(),
                subject: chats[0].clone(),
            };
    }
    assert!(told);

    // Asked for again and again, as rows are drawn: the store has it.
    for _ in 0..5 {
        engine.load_avatar(&account, &chats[0]).await;
        engine.want_avatar(&account, &chats[0]);
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(mock.avatar_calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_picture_is_downloaded_again_only_when_its_id_changes() {
    let mock = MockProvider::quiet();
    let (engine, account, chats) = engine_with_chats(&mock).await;
    let store = engine.store().clone();
    mock.set_avatar(&account, &chats[0], Some(("pic-1", png(200, 200))));
    engine.load_avatar(&account, &chats[0]).await;
    let first = store.avatar(&account, &chats[0]).unwrap().unwrap();

    // A day later the provider is asked, with the id the client has: it is
    // the same picture, so nothing is downloaded or rewritten.
    age_avatar(&store, &account, &chats[0], 25);
    engine.load_avatar(&account, &chats[0]).await;
    assert_eq!(mock.avatar_calls(), 2);
    let same = store.avatar(&account, &chats[0]).unwrap().unwrap();
    assert_eq!(same.image, first.image);
    assert!(same.checked_at > Timestamp::from_millis(Timestamp::now().as_millis() - 60_000));

    // The contact changes their picture: a new id, a new image.
    mock.set_avatar(&account, &chats[0], Some(("pic-2", png(300, 150))));
    age_avatar(&store, &account, &chats[0], 25);
    engine.load_avatar(&account, &chats[0]).await;
    let changed = store.avatar(&account, &chats[0]).unwrap().unwrap();
    assert_eq!(changed.picture_id.as_deref(), Some("pic-2"));
    assert_ne!(changed.image, first.image);

    // And then removes it.
    mock.set_avatar(&account, &chats[0], None);
    age_avatar(&store, &account, &chats[0], 25);
    engine.load_avatar(&account, &chats[0]).await;
    let gone = store.avatar(&account, &chats[0]).unwrap().unwrap();
    assert_eq!((gone.picture_id, gone.image), (None, None));
}

#[tokio::test(start_paused = true)]
async fn a_chat_list_that_names_the_picture_decides_when_it_is_fetched() {
    // A provider whose chat list carries each chat's picture id: the
    // picture is asked for when that id is not the one held, and never
    // while it is, however old the copy.
    let mock = MockProvider::quiet();
    mock.list_picture_ids(true);
    let (engine, account, chats) = engine_with_chats(&mock).await;
    let store = engine.store().clone();
    let chat = chats[0].clone();
    mock.set_avatar(&account, &chat, Some(("pic-1", png(200, 200))));
    engine.refresh().await.unwrap();
    assert_eq!(
        store.chat_picture_id(&account, &chat).unwrap().as_deref(),
        Some("pic-1")
    );

    engine.load_avatar(&account, &chat).await;
    assert_eq!(mock.avatar_calls(), 1);
    // Days pass: the id is the same, so there is nothing to ask.
    age_avatar(&store, &account, &chat, 24 * 30);
    for _ in 0..3 {
        assert_eq!(engine.load_avatar(&account, &chat).await, AVATAR_TTL);
        engine.refresh().await.unwrap();
        engine.want_avatar(&account, &chat);
    }
    settle().await;
    assert_eq!(mock.avatar_calls(), 1, "an unchanged id costs no request");

    // The picture changes: the next refresh brings another id, and the
    // picture is fetched at once, although it was checked a moment ago.
    engine.want_avatar(&account, &chat);
    mock.set_avatar(&account, &chat, Some(("pic-2", png(300, 150))));
    engine.refresh().await.unwrap();
    engine.want_avatar(&account, &chat);
    settle().await;
    assert_eq!(mock.avatar_calls(), 2);
    assert_eq!(
        store
            .avatar(&account, &chat)
            .unwrap()
            .unwrap()
            .picture_id
            .as_deref(),
        Some("pic-2")
    );

    // The picture is removed: the copy goes without asking anyone, and
    // the view is told.
    let mut changes = store.subscribe();
    mock.set_avatar(&account, &chat, None);
    engine.refresh().await.unwrap();
    let gone = store.avatar(&account, &chat).unwrap().unwrap();
    assert_eq!((gone.picture_id, gone.image), (None, None));
    let mut told = false;
    while let Some(change) = changes.try_next() {
        told |= change
            == StoreChange::Avatar {
                account_id: account.clone(),
                subject: chat.clone(),
            };
    }
    assert!(told);
    engine.want_avatar(&account, &chat);
    settle().await;
    assert_eq!(mock.avatar_calls(), 2, "no id: checked once a day, not now");
}

#[tokio::test(start_paused = true)]
async fn no_picture_is_remembered_too() {
    let mock = MockProvider::quiet();
    let (engine, account, chats) = engine_with_chats(&mock).await;
    // No picture set: hidden by privacy, none at all, or a group the
    // provider cannot show. The answer is kept, not asked for per frame.
    assert_eq!(engine.load_avatar(&account, &chats[1]).await, AVATAR_TTL);
    let stored = engine.store().avatar(&account, &chats[1]).unwrap().unwrap();
    assert_eq!((stored.picture_id, stored.image), (None, None));
    engine.load_avatar(&account, &chats[1]).await;
    assert_eq!(mock.avatar_calls(), 1);

    // Something that is not an image is no picture either.
    mock.set_avatar(&account, &chats[2], Some(("pic-x", b"<html>".to_vec())));
    engine.load_avatar(&account, &chats[2]).await;
    let stored = engine.store().avatar(&account, &chats[2]).unwrap().unwrap();
    assert_eq!(stored.image, None);
}

#[tokio::test(start_paused = true)]
async fn pictures_wait_out_a_rate_limit_and_an_outage() {
    let mock = MockProvider::quiet();
    let (engine, account, chats) = engine_with_chats(&mock).await;
    let store = engine.store().clone();
    mock.set_avatar(&account, &chats[0], Some(("pic-1", png(100, 100))));

    // 429 with Retry-After: waited out, then fetched.
    mock.fail_next_avatars([ProviderError::RateLimited {
        retry_after: Some(Duration::from_secs(9)),
    }]);
    let started = tokio::time::Instant::now();
    assert_eq!(engine.load_avatar(&account, &chats[0]).await, AVATAR_TTL);
    assert!(started.elapsed() >= Duration::from_secs(9));
    assert_eq!(mock.avatar_calls(), 2);
    assert!(store
        .avatar(&account, &chats[0])
        .unwrap()
        .unwrap()
        .image
        .is_some());

    // A dropped connection: nothing is stored (not even "no picture"),
    // and it is tried again a minute later, not a day later.
    mock.fail_next_avatars([ProviderError::Transient("eof".into())]);
    let again = engine.load_avatar(&account, &chats[1]).await;
    assert!(again <= Duration::from_secs(60));
    assert_eq!(store.avatar(&account, &chats[1]).unwrap(), None);

    // While the number is offline the provider is not asked at all.
    store
        .set_connection(&account, &ConnectionState::Reconnecting)
        .unwrap();
    let calls = mock.avatar_calls();
    assert!(engine.load_avatar(&account, &chats[2]).await <= Duration::from_secs(60));
    assert_eq!(mock.avatar_calls(), calls);
}

// ----- media ----------------------------------------------------------------

/// Waits until the engine's background download has settled `key`.
async fn settled(engine: &SyncEngine, key: &str) -> MediaState {
    for _ in 0..500 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let cached = engine.store().media_size(key).unwrap().is_some()
            || engine
                .store()
                .media(key, Timestamp::now())
                .unwrap()
                .is_some();
        let state = engine.media_state(key);
        if cached || matches!(state, MediaState::Unavailable(_) | MediaState::Expired(_)) {
            return state;
        }
    }
    panic!("`{key}` never settled");
}

#[tokio::test]
async fn an_image_becomes_a_thumbnail_once() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let url = "https://media.example/photo";
    mock.set_media(url, png(1600, 800), "image/png");
    let key = thumbnail_key(url);

    engine.want_thumbnail(&account, url);
    assert_eq!(engine.media_state(&key), MediaState::Loading);
    settled(&engine, &key).await;
    // The size is known without reading the bytes: it is what the row
    // is laid out with.
    assert_eq!(engine.store().media_size(&key).unwrap(), Some((720, 360)));
    let cached = engine
        .store()
        .media(&key, Timestamp::now())
        .unwrap()
        .unwrap();
    assert_eq!(cached.mime.as_deref(), Some("image/jpeg"));
    let image = image::load_from_memory(&cached.bytes).unwrap();
    assert_eq!((image.width(), image.height()), (720, 360));

    // Wanted again by every frame that draws the row: not downloaded again.
    for _ in 0..5 {
        engine.want_thumbnail(&account, url);
    }
    settled(&engine, &key).await;
    assert_eq!(mock.media_calls(), 1);
}

#[tokio::test]
async fn an_animated_sticker_is_kept_as_it_came_next_to_its_first_frame() {
    use crate::animation::fixtures::{animated_gif, animated_webp};
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let store = engine.store().clone();
    let cached = |key: &str| store.media(key, Timestamp::now()).unwrap();

    // One download gives the first frame and the animation.
    let sticker = "https://media.example/sticker.webp";
    let original = animated_webp(96, 96, &[60, 60, 60]);
    mock.set_media(sticker, original.clone(), "image/webp");
    engine.want_thumbnail(&account, sticker);
    settled(&engine, &thumbnail_key(sticker)).await;
    settled(&engine, &animation_key(sticker)).await;
    let first = cached(&thumbnail_key(sticker)).unwrap();
    assert_eq!(
        first.mime.as_deref(),
        Some("image/png"),
        "a still, with its transparency"
    );
    let kept = cached(&animation_key(sticker)).unwrap();
    assert_eq!(kept.bytes, original, "the animated bytes, untouched");
    assert_eq!(kept.mime.as_deref(), Some("image/webp"));
    assert_eq!(mock.media_calls(), 1);

    // A picture that does not move says so once, with an empty entry.
    let photo = "https://media.example/photo.png";
    mock.set_media(photo, png(64, 64), "image/png");
    engine.want_thumbnail(&account, photo);
    settled(&engine, &animation_key(photo)).await;
    let still = cached(&animation_key(photo)).unwrap();
    assert!(still.bytes.is_empty() && still.mime.is_none());

    // A sticker whose first frame was cached by an earlier version: the
    // animation is asked for by itself, once, and the frame is left as
    // it is.
    let old = "https://media.example/old.gif";
    mock.set_media(old, animated_gif(40, 40, 3, 80), "image/gif");
    let frame = CachedMedia {
        bytes: vec![1, 2, 3],
        mime: Some("image/png".into()),
        size: Some((40, 40)),
    };
    store
        .put_media(&thumbnail_key(old), &frame, Timestamp::now(), u64::MAX)
        .unwrap();
    assert!(cached(&animation_key(old)).is_none());
    let calls = mock.media_calls();
    for _ in 0..5 {
        engine.want_animation(&account, old);
    }
    settled(&engine, &animation_key(old)).await;
    assert_eq!(
        cached(&animation_key(old)).unwrap().mime.as_deref(),
        Some("image/gif")
    );
    assert_eq!(cached(&thumbnail_key(old)).unwrap().bytes, vec![1, 2, 3]);
    engine.want_animation(&account, old);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(mock.media_calls(), calls + 1, "asked once");

    // An animation too large to keep is a still here.
    let huge = "https://media.example/huge.gif";
    let mut bytes = animated_gif(16, 16, 2, 80);
    bytes.resize(ANIMATION_KEEP_LIMIT + 1, 0);
    mock.set_media(huge, bytes, "image/gif");
    engine.want_thumbnail(&account, huge);
    settled(&engine, &animation_key(huge)).await;
    assert!(cached(&animation_key(huge)).unwrap().bytes.is_empty());
    assert!(cached(&thumbnail_key(huge)).is_some());
}

#[tokio::test]
async fn the_demo_sticker_is_a_real_animation() {
    let mock = MockProvider::quiet();
    let (_, account, _) = engine_with_chats(&mock).await;
    let file = mock
        .download_media(
            &account,
            &client_provider::MediaRef::new(provider_mock::SHOWCASE_STICKER),
        )
        .await
        .unwrap();
    assert_eq!(animated_format(&file.bytes), Some(AnimatedFormat::Gif));
    let animation = animation_frames(&file.bytes, &AnimationLimits::default())
        .unwrap()
        .expect("it moves");
    assert_eq!(animation.frames.len(), 8);
    assert!(!animation.truncated);
    assert_eq!(animation.frames[0].image.dimensions(), (160, 160));
    assert_eq!(animation.duration(), Duration::from_millis(720));
    // Its ground is transparent and its frames differ.
    assert_eq!(animation.frames[0].image.get_pixel(0, 0).0[3], 0);
    assert_eq!(animation.frames[0].image.get_pixel(80, 80).0[3], 255);
    assert_ne!(animation.frames[0].image, animation.frames[4].image);
    // And its first frame is a picture like any other.
    assert!(thumbnail(&file.bytes, 160).is_ok());
}

#[tokio::test]
async fn what_is_not_a_reasonable_image_is_not_loaded_by_itself() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;

    // Not an image, by its content type.
    let document = "https://media.example/contract";
    mock.set_media(document, b"%PDF-1.7".to_vec(), "application/pdf");
    // An image, it says, but not one that decodes.
    let fake = "https://media.example/fake";
    mock.set_media(fake, b"GIF89a-not-really".to_vec(), "image/gif");
    // Larger than what is fetched without asking.
    let huge = "https://media.example/huge";
    mock.set_media(huge, vec![0; AUTO_MEDIA_LIMIT as usize + 1], "image/png");

    for url in [document, fake, huge] {
        let key = thumbnail_key(url);
        engine.want_thumbnail(&account, url);
        let state = settled(&engine, &key).await;
        assert!(
            matches!(state, MediaState::Unavailable(_)),
            "{url}: {state:?}"
        );
        assert_eq!(engine.store().media_size(&key).unwrap(), None);
    }
    // A refusal is final for the session: no retry per frame.
    let calls = mock.media_calls();
    engine.want_thumbnail(&account, document);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(mock.media_calls(), calls);
    assert_eq!(engine.store().media_total().unwrap(), 0, "nothing was kept");

    // Asked for by the user, the document is fetched whole, as it is.
    engine.want_file(&account, document);
    settled(&engine, &file_key(document)).await;
    let file = engine
        .store()
        .media(&file_key(document), Timestamp::now())
        .unwrap()
        .unwrap();
    assert_eq!(file.bytes, b"%PDF-1.7");
    assert_eq!(file.mime.as_deref(), Some("application/pdf"));
}

#[tokio::test]
async fn downloads_nobody_waits_for_are_dropped() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    // More requests than the queue holds, as when scrolling fast: the
    // oldest are forgotten, and closing the chat forgets the rest.
    for n in 0..60 {
        let url = format!("https://media.example/{n}");
        mock.set_media(&url, png(8, 8), "image/png");
        engine.want_thumbnail(&account, &url);
    }
    engine.cancel_media();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        mock.media_calls() <= 3 + 3,
        "only what was in flight: {}",
        mock.media_calls()
    );
    // A forgotten request can be made again.
    let url = "https://media.example/0";
    engine.want_thumbnail(&account, url);
    settled(&engine, &thumbnail_key(url)).await;
    assert!(engine
        .store()
        .media_size(&thumbnail_key(url))
        .unwrap()
        .is_some());

    // After shutdown nothing new starts.
    engine.shutdown();
    let calls = mock.media_calls();
    engine.want_thumbnail(&account, "https://media.example/59");
    engine.want_avatar(&account, &ChatId::new("anyone"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(mock.media_calls(), calls);
}

#[test]
fn the_media_cache_stays_within_its_budget() {
    let store = Store::open_in_memory().unwrap();
    let entry = |fill: u8| CachedMedia {
        bytes: vec![fill; 400],
        mime: Some("image/jpeg".into()),
        size: Some((10, 10)),
    };
    let at = Timestamp::from_millis;
    assert_eq!(store.put_media("a", &entry(1), at(1), 1000).unwrap(), 0);
    assert_eq!(store.put_media("b", &entry(2), at(2), 1000).unwrap(), 0);
    // `a` is looked at again: it is now the most recently used.
    assert!(store.media("a", at(3)).unwrap().is_some());
    // A third does not fit: the one used longest ago goes, which is `b`.
    assert_eq!(store.put_media("c", &entry(3), at(4), 1000).unwrap(), 1);
    assert!(store.media("b", at(5)).unwrap().is_none());
    assert!(store.media("a", at(5)).unwrap().is_some());
    assert!(store.media("c", at(5)).unwrap().is_some());
    assert_eq!(store.media_total().unwrap(), 800);
    // Something larger than the whole budget still gets in, alone.
    let large = CachedMedia {
        bytes: vec![9; 5000],
        mime: None,
        size: None,
    };
    assert_eq!(store.put_media("d", &large, at(6), 1000).unwrap(), 2);
    assert_eq!(store.media_total().unwrap(), 5000);
}

#[test]
fn an_unbounded_budget_drops_nothing() {
    let store = Store::open_in_memory().unwrap();
    let now = Timestamp::now();
    assert_eq!(store.put_media("a", &cached(1), now, u64::MAX).unwrap(), 0);
    assert_eq!(store.put_media("b", &cached(2), now, u64::MAX).unwrap(), 0);
    assert_eq!(cached_bytes(&store, "a"), Some(vec![1; 40]));
}

/// A voice note whose file is at `source`.
fn voice_note(id: &str, source: &str) -> Message {
    let mut message = text(id, "chat", 1, "", Direction::Incoming);
    message.content = MessageContent::Media(Media {
        source: Some(client_provider::MediaRef::new(source)),
        ..Media::new(MediaKind::Voice)
    });
    message
}

fn cached(fill: u8) -> CachedMedia {
    CachedMedia {
        bytes: vec![fill; 40],
        mime: Some("audio/ogg".into()),
        size: None,
    }
}

fn cached_bytes(store: &Store, key: &str) -> Option<Vec<u8>> {
    store
        .media(key, Timestamp::now())
        .unwrap()
        .map(|cached| cached.bytes)
}

#[test]
fn what_is_cached_follows_a_message_to_its_new_reference() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    // Fetched while the file was still on WhatsApp: the provider named
    // the message.
    let old = "provider-media:9000:m1";
    store.upsert_message(&voice_note("m1", old)).unwrap();
    let now = Timestamp::now();
    for (key, fill) in [
        (file_key(old), 1),
        (thumbnail_key(old), 2),
        (animation_key(old), 3),
        // A kind the store knows nothing of: the application's own.
        (format!("wave:{old}"), 4),
    ] {
        store.put_media(&key, &cached(fill), now, u64::MAX).unwrap();
    }
    let total = store.media_total().unwrap();

    // The provider now stores the file, and names it by its address.
    let new = "https://media.example/m1.ogg";
    assert_eq!(
        store.upsert_message(&voice_note("m1", new)).unwrap(),
        Upsert::Updated
    );

    assert_eq!(cached_bytes(&store, &file_key(new)), Some(vec![1; 40]));
    assert_eq!(cached_bytes(&store, &thumbnail_key(new)), Some(vec![2; 40]));
    assert_eq!(cached_bytes(&store, &animation_key(new)), Some(vec![3; 40]));
    assert_eq!(
        cached_bytes(&store, &format!("wave:{new}")),
        Some(vec![4; 40])
    );
    for key in [
        file_key(old),
        thumbnail_key(old),
        animation_key(old),
        format!("wave:{old}"),
    ] {
        assert_eq!(cached_bytes(&store, &key), None, "{key}");
    }
    assert_eq!(store.media_total().unwrap(), total, "moved, not copied");
}

#[test]
fn what_is_cached_under_the_new_reference_is_kept() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let (old, new) = ("provider-media:9000:m1", "https://media.example/m1.ogg");
    store.upsert_message(&voice_note("m1", old)).unwrap();
    let now = Timestamp::now();
    store
        .put_media(&file_key(old), &cached(1), now, u64::MAX)
        .unwrap();
    store
        .put_media(&file_key(new), &cached(2), now, u64::MAX)
        .unwrap();

    store.upsert_message(&voice_note("m1", new)).unwrap();

    assert_eq!(cached_bytes(&store, &file_key(new)), Some(vec![2; 40]));
    assert_eq!(cached_bytes(&store, &file_key(old)), None);
}

#[test]
fn a_reference_another_message_uses_keeps_what_is_cached() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    // The same file in two messages: a forwarded copy.
    let (old, new) = (
        "https://media.example/shared.ogg",
        "https://media.example/m1.ogg",
    );
    store
        .upsert_messages(&[voice_note("m1", old), voice_note("m2", old)])
        .unwrap();
    store
        .put_media(&file_key(old), &cached(1), Timestamp::now(), u64::MAX)
        .unwrap();

    store.upsert_message(&voice_note("m1", new)).unwrap();

    // The other message still finds it, and this one does not fetch it
    // again.
    assert_eq!(cached_bytes(&store, &file_key(old)), Some(vec![1; 40]));
    assert_eq!(cached_bytes(&store, &file_key(new)), Some(vec![1; 40]));
}

#[test]
fn pictures_and_media_are_encrypted_with_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let marker = b"PICTURE-BYTES-IN-CLEAR-0123456789";
    {
        let store = Store::open(&path, Some(&test_key())).unwrap();
        store
            .put_avatar(
                &account(),
                &chat_id(),
                Some(("pic-1", marker)),
                Timestamp::now(),
            )
            .unwrap();
        let media = CachedMedia {
            bytes: marker.repeat(50),
            mime: Some("image/jpeg".into()),
            size: Some((4, 4)),
        };
        store
            .put_media(
                "thumb:https://media.example/secret-photo",
                &media,
                Timestamp::now(),
                u64::MAX,
            )
            .unwrap();
    }
    let marker = std::str::from_utf8(marker).unwrap();
    assert!(!readable_on_disk(&path, marker));
    assert!(!readable_on_disk(&path, "secret-photo"), "nor the URLs");
    // They are in the database, and nowhere else: one file and its journal.
    let others: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with("store.db"))
        .collect();
    assert!(others.is_empty(), "{others:?}");
    let store = Store::open(&path, Some(&test_key())).unwrap();
    assert_eq!(
        store
            .avatar(&account(), &chat_id())
            .unwrap()
            .unwrap()
            .image
            .as_deref(),
        Some(marker.as_bytes())
    );
}

#[tokio::test]
async fn the_demo_photos_load_like_any_other() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let url = "mock://image/7/320x240";
    engine.want_thumbnail(&account, url);
    settled(&engine, &thumbnail_key(url)).await;
    assert_eq!(
        engine.store().media_size(&thumbnail_key(url)).unwrap(),
        Some((320, 240))
    );
}

// ----- a pin that sticks ----------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_pin_survives_a_refresh_from_a_provider_that_has_not_observed_it() {
    // The owner's report: "it does not let me pin chats". The provider
    // accepted the pin, but its chat list says nothing of pins it has not
    // seen WhatsApp report (wuapi: `pinned: null`), and the next refresh
    // wrote that nothing over the pin.
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    mock.observe_no_chat_state(true);
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .rfind(|c| !c.pinned)
        .unwrap()
        .id;

    engine.update_chat(&account, &chat, ChatChange::Pinned(true));
    settle().await;
    assert!(
        mock.chat(&account, &chat).unwrap().pinned,
        "the provider took it"
    );

    // The chat list is read again, as it is every few seconds.
    engine.refresh().await.unwrap();
    let after = store.chat(&account, &chat).unwrap().unwrap();
    assert!(after.pinned, "the pin is still there");
    // And above every chat that is not pinned.
    let list = store.chats(&account, None).unwrap();
    let place = list.iter().position(|c| c.id == chat).unwrap();
    assert!(
        list[..=place].iter().all(|c| c.pinned),
        "in the pinned block"
    );
}

#[tokio::test(start_paused = true)]
async fn the_users_change_wins_until_the_provider_agrees_refuses_or_moves_on() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .rfind(|c| !c.pinned)
        .unwrap()
        .id;
    let pinned = |store: &Store| store.chat(&account, &chat).unwrap().unwrap().pinned;

    // While the change is still on its way (the connection is failing), a
    // refresh that says "not pinned" does not undo it.
    mock.fail_next_chat_updates([ProviderError::Transient("eof".into())]);
    engine.update_chat(&account, &chat, ChatChange::Pinned(true));
    tokio::task::yield_now().await;
    engine.refresh().await.unwrap();
    assert!(pinned(&store), "pending: the user's value stands");
    settle().await;
    assert!(mock.chat(&account, &chat).unwrap().pinned);

    // The provider now reports the pin itself: confirmed, nothing pending.
    engine.refresh().await.unwrap();
    assert!(pinned(&store));

    // Later the chat is unpinned on the phone. The provider says so, and
    // that is the truth: the old local change does not mask it.
    mock.push_event(ProviderEvent::ChatUpdated(Chat {
        pinned: false,
        ..mock.chat(&account, &chat).unwrap()
    }));
    tokio::time::sleep(Duration::from_secs(180)).await;
    engine.refresh().await.unwrap();
    assert!(!mock.chat(&account, &chat).unwrap().pinned);
    assert!(!pinned(&store), "the phone's change arrived");

    // A change the provider refuses is put back, and the user is told.
    let mut changes = store.subscribe();
    mock.fail_next_chat_updates([ProviderError::Rejected {
        code: "whatsapp_error".into(),
        message: "WhatsApp allows three pinned chats".into(),
    }]);
    engine.update_chat(&account, &chat, ChatChange::Pinned(true));
    settle().await;
    assert!(!pinned(&store));
    let mut said = None;
    while let Some(change) = changes.try_next() {
        if let StoreChange::Problem { message } = change {
            said = Some(message);
        }
    }
    let said = said.expect("a refusal is said, not swallowed");
    assert!(
        said.contains("could not be pinned") && said.contains("three pinned chats"),
        "{said}"
    );
    // And a refresh after the refusal does not bring the pin back.
    engine.refresh().await.unwrap();
    assert!(!pinned(&store));
}

#[test]
fn what_a_provider_does_not_know_is_left_as_it_is() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let mut known = chat("a", "Ana");
    known.pinned = true;
    known.muted = true;
    store.upsert_chat(&known, false).unwrap();

    // A later report that knows nothing about pins and mutes.
    let mut blind = chat("a", "Ana Maria");
    blind.unknown = client_provider::ChatUnknown {
        pinned: true,
        muted: true,
        archived: true,
        picture: false,
        unread: false,
    };
    store.upsert_chat(&blind, false).unwrap();
    let stored = store.chat(&account(), &ChatId::new("a")).unwrap().unwrap();
    assert_eq!(stored.title, "Ana Maria", "what it does know is taken");
    assert!(stored.pinned && stored.muted && !stored.archived);

    // One that does know is believed.
    store.upsert_chat(&chat("a", "Ana Maria"), false).unwrap();
    let stored = store.chat(&account(), &ChatId::new("a")).unwrap().unwrap();
    assert!(!stored.pinned && !stored.muted);

    // A chat seen for the first time with nothing known is not pinned.
    store
        .upsert_chat(
            &Chat {
                id: ChatId::new("b"),
                ..blind
            },
            false,
        )
        .unwrap();
    assert!(
        !store
            .chat(&account(), &ChatId::new("b"))
            .unwrap()
            .unwrap()
            .pinned
    );
}

#[test]
fn a_chat_without_messages_sorts_after_the_others() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    // A group nobody has written in yet, listed first by the provider.
    store
        .upsert_chat(&chat("empty", "Marketing"), false)
        .unwrap();
    store.upsert_chat(&chat("a", "Ana"), false).unwrap();
    store
        .upsert_message(&text("m1", "a", 1_000, "hi", Direction::Incoming))
        .unwrap();
    let list = store.chats(&account(), None).unwrap();
    let ids: Vec<_> = list.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["a", "empty"]);
    assert!(list[1].last_message.is_none());
}

// ----- numbers --------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_number_is_linked_once_and_the_store_follows_it() {
    use client_provider::{HistoryImport, LinkStep, NewAccount};
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    assert_eq!(store.accounts().unwrap().len(), 2);

    let places = engine.link_places().await.unwrap();
    let new = NewAccount {
        name: Some("Support".into()),
        place: places.first().cloned(),
        pairing_phone: None,
        history: HistoryImport::Recent,
        request_id: "attempt-1".into(),
    };
    // The answer was lost and the request sent again: one number.
    let first = engine.create_account(&new).await.unwrap();
    let again = engine.create_account(&new).await.unwrap();
    assert_eq!(first.account.id, again.account.id);
    assert_eq!(first.step, LinkStep::Starting);
    let stored = store.accounts().unwrap();
    assert_eq!(stored.len(), 3, "one number, however often it was asked");
    // After the others, with what the provider said of it.
    assert_eq!(stored[2].id, first.account.id);
    assert_eq!(stored[2].display_name, "Support");
    assert_eq!(
        stored[2].settings.history_import,
        Some(HistoryImport::Recent)
    );
    assert!(!stored[2].connection.is_connected());

    let id = first.account.id.clone();
    let status = engine.link_status(&id).await.unwrap();
    assert!(matches!(status.step, LinkStep::Scan { .. }));
    mock.complete_link(&id);
    let status = engine.link_status(&id).await.unwrap();
    assert_eq!(status.step, LinkStep::Linked);
    settle().await;
    let stored = store.accounts().unwrap();
    assert!(stored[2].connection.is_connected());
    assert!(stored[2].phone.is_some());
    // The first two kept their places.
    assert_eq!(stored[0].id.as_str(), "acc_personal");
}

#[tokio::test(start_paused = true)]
async fn a_number_is_renamed_logged_out_and_deleted_with_what_was_stored_for_it() {
    use client_provider::{AccountChange, ConnectionState, HistoryImport};
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let accounts = store.accounts().unwrap();
    let (first, second) = (accounts[0].id.clone(), accounts[1].id.clone());

    let renamed = engine
        .update_account(&first, AccountChange::Rename("Ventas".into()))
        .await
        .unwrap();
    assert_eq!(renamed.display_name, "Ventas");
    engine
        .update_account(&first, AccountChange::History(HistoryImport::Recent))
        .await
        .unwrap();
    let stored = store.accounts().unwrap();
    assert_eq!(stored[0].display_name, "Ventas");
    assert_eq!(
        stored[0].settings.history_import,
        Some(HistoryImport::Recent)
    );
    // A refusal changes nothing here.
    let error = engine
        .update_account(&first, AccountChange::Rename("  ".into()))
        .await
        .unwrap_err();
    assert!(!error.is_transient());
    assert_eq!(store.accounts().unwrap()[0].display_name, "Ventas");

    // Logged out: the number and its chats stay.
    let chats_before = store.chats(&first, None).unwrap().len();
    assert!(chats_before > 0);
    engine.unlink_account(&first).await.unwrap();
    assert_eq!(
        store.accounts().unwrap()[0].connection,
        ConnectionState::LoggedOut
    );
    assert_eq!(store.chats(&first, None).unwrap().len(), chats_before);

    // Deleted: everything stored for it goes, and nothing of the other.
    let other_chats = store.chats(&second, None).unwrap().len();
    engine.delete_account(&first).await.unwrap();
    let stored = store.accounts().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, second);
    assert!(store.chats(&first, None).unwrap().is_empty());
    assert_eq!(store.chats(&second, None).unwrap().len(), other_chats);
    assert_eq!(mock.deleted_accounts(), vec![first]);
}

#[tokio::test(start_paused = true)]
async fn history_the_phone_hands_over_after_a_link_is_picked_up() {
    use client_provider::{HistoryImport, LinkStep, NewAccount};
    // The owner's report: "it did not sync my previous chats". The phone
    // sends its history during the minutes after the link, without an
    // event for any of it, so the chat list has to be read again.
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let new = NewAccount {
        name: None,
        place: None,
        pairing_phone: None,
        history: HistoryImport::Recent,
        request_id: "attempt-1".into(),
    };
    let account = engine.create_account(&new).await.unwrap().account.id;
    mock.complete_link(&account);
    assert_eq!(
        engine.link_status(&account).await.unwrap().step,
        LinkStep::Linked
    );
    let pause = || tokio::time::sleep(Duration::from_secs(1));
    pause().await;
    assert!(store.chats(&account, None).unwrap().is_empty());

    // A minute later the phone has handed over two chats.
    let old = mock.import_history(&account, "+584140000001", &["first", "second"]);
    mock.import_history(&account, "+584140000002", &["hello"]);
    tokio::time::sleep(Duration::from_secs(30)).await;
    let chats = store.chats(&account, None).unwrap();
    assert_eq!(
        chats.len(),
        2,
        "no event announced them; the list was read again"
    );

    // A chat that was read to its beginning, and then gets older messages
    // underneath: it is read again.
    engine.fetch_latest(&account, &old).await.unwrap();
    while engine.fetch_older(&account, &old).await.unwrap() {}
    assert!(store.history_state(&account, &old).unwrap().complete);
    assert_eq!(store.messages(&account, &old, 50).unwrap().len(), 2);
    mock.import_history(&account, "+584140000001", &["before everything"]);
    tokio::time::sleep(Duration::from_secs(90)).await;
    assert!(!store.history_state(&account, &old).unwrap().complete);
    engine.fetch_latest(&account, &old).await.unwrap();
    while engine.fetch_older(&account, &old).await.unwrap() {}
    assert_eq!(store.messages(&account, &old, 50).unwrap().len(), 3);

    // It ends: nothing is read forever.
    let calls = mock.account_calls();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    let after = mock.account_calls();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    assert!(after > calls, "the later rounds ran");
    assert_eq!(mock.account_calls(), after, "and then stopped");
}

#[tokio::test(start_paused = true)]
async fn a_number_that_does_not_import_history_is_read_once_after_linking() {
    use client_provider::{HistoryImport, NewAccount};
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let new = NewAccount {
        name: None,
        place: None,
        pairing_phone: None,
        history: HistoryImport::Off,
        request_id: "attempt-1".into(),
    };
    let account = engine.create_account(&new).await.unwrap().account.id;
    mock.complete_link(&account);
    engine.link_status(&account).await.unwrap();
    settle().await;
    let calls = mock.account_calls();
    tokio::time::sleep(Duration::from_secs(3600)).await;
    settle().await;
    assert_eq!(mock.account_calls(), calls);
    // Asking again about a number that is already linked starts nothing.
    engine.link_status(&account).await.unwrap();
    settle().await;
    assert_eq!(mock.account_calls(), calls);
}

// ----- contacts -------------------------------------------------------------

fn book(account: &AccountId, count: usize) -> Vec<client_provider::Contact> {
    (0..count)
        .map(|index| {
            let phone = format!("+5841400000{index:02}");
            let mut contact =
                client_provider::Contact::new(account.clone(), ContactId::new(phone.clone()));
            contact.phone = Some(phone);
            contact.saved_name = Some(format!("Contact {index:02}"));
            contact
        })
        .collect()
}

#[tokio::test(start_paused = true)]
async fn the_address_book_is_copied_page_by_page_and_goes_on_where_it_stopped() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    // Twelve contacts: three pages of the mock's five.
    mock.set_contacts(&account, book(&account, 12));
    assert!(!engine.contacts_synced(&account));

    engine.sync_contacts(&account).await.unwrap();
    assert_eq!(store.contact_count(&account).unwrap(), 12);
    assert!(engine.contacts_synced(&account));
    assert_eq!(
        mock.contact_cursors(),
        vec![None, Some("5".into()), Some("10".into())]
    );

    // A second pass, interrupted: what it read stays, and the next call
    // goes on from the page that failed instead of from the top.
    let mut changed = book(&account, 12);
    changed[0].saved_name = Some("Aaron".into());
    changed.remove(11);
    mock.set_contacts(&account, changed);
    mock.fail_contact_pages_after(1, [ProviderError::Transient("eof".into())]);
    let before = mock.contact_calls();
    let error = engine.sync_contacts(&account).await.unwrap_err();
    assert!(error.is_transient());
    assert_eq!(
        mock.contact_calls(),
        before + 2,
        "it stopped at the failure"
    );
    assert_eq!(
        store.contact_count(&account).unwrap(),
        12,
        "nothing is removed by a pass that did not finish"
    );
    engine.sync_contacts(&account).await.unwrap();
    let cursors = mock.contact_cursors();
    assert_eq!(
        cursors[cursors.len() - 4..],
        [None, Some("5".into()), Some("5".into()), Some("10".into())],
        "the failed page again, then the rest"
    );
    // The pass finished: the renamed contact is renamed, and the one
    // deleted on the phone is gone.
    assert_eq!(store.contact_count(&account).unwrap(), 11);
    let first = store.contacts(&account, None, 50).unwrap().remove(0);
    assert_eq!(first.display_name(), "Aaron");
}

#[tokio::test(start_paused = true)]
async fn contacts_are_found_by_name_username_and_number_and_events_only_add() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let mut people = book(&account, 3);
    people[0].saved_name = Some("María José".into());
    people[1].saved_name = None;
    people[1].business_name = Some("Panadería 100%".into());
    people[2].saved_name = Some("Zoe".into());
    people[2].username = Some("zoe_dev".into());
    mock.set_contacts(&account, people);
    engine.sync_contacts(&account).await.unwrap();

    let names = |query: Option<&str>| -> Vec<String> {
        store
            .contacts(&account, query, 50)
            .unwrap()
            .iter()
            .map(client_provider::Contact::display_name)
            .collect()
    };
    assert_eq!(names(None), ["María José", "Panadería 100%", "Zoe"]);
    assert_eq!(names(Some("jos")), ["María José"]);
    assert_eq!(
        names(Some("ZOE_")),
        ["Zoe"],
        "by username, the underscore is itself"
    );
    assert_eq!(
        names(Some("100%")),
        ["Panadería 100%"],
        "the percent is itself"
    );
    assert_eq!(
        names(Some("+58 414 000 0002")),
        ["Zoe"],
        "by number, however typed"
    );
    assert_eq!(names(Some("0000001")), ["Panadería 100%"]);
    assert!(names(Some("nobody")).is_empty());
    assert_eq!(store.contacts(&account, None, 2).unwrap().len(), 2);

    // An event knows less than the list: it adds, and removes nothing.
    let mut about = client_provider::Contact::new(account.clone(), ContactId::new("+584140000002"));
    about.about = Some("Available".into());
    engine
        .apply_event(ProviderEvent::ContactUpdated(about))
        .unwrap();
    let zoe = store
        .contact(&account, &ContactId::new("+584140000002"))
        .unwrap()
        .unwrap();
    assert_eq!(zoe.saved_name.as_deref(), Some("Zoe"));
    assert_eq!(zoe.about.as_deref(), Some("Available"));
    assert_eq!(names(Some("zoe")), ["Zoe"], "still found under its name");
    // And a contact known only from an event survives a pass of the list.
    let stranger = client_provider::Contact {
        profile_name: Some("Stranger".into()),
        ..client_provider::Contact::new(account.clone(), ContactId::new("+584149999999"))
    };
    engine
        .apply_event(ProviderEvent::ContactUpdated(stranger))
        .unwrap();
    engine.sync_contacts(&account).await.unwrap();
    assert_eq!(names(Some("strang")), ["Stranger"]);
    assert_eq!(
        store
            .contact(&account, &ContactId::new("+584140000002"))
            .unwrap()
            .unwrap()
            .about
            .as_deref(),
        Some("Available"),
        "the list does not carry About, so it does not erase it"
    );
}

#[tokio::test(start_paused = true)]
async fn a_typed_number_is_checked_before_its_chat_opens() {
    use crate::NewChat;
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chats = store.chats(&account, None).unwrap().len();

    mock.without_whatsapp("+584140000099");
    let answer = engine
        .start_chat_checked(&account, "+58 414 000 0099")
        .await
        .unwrap();
    assert_eq!(answer, NewChat::NotOnWhatsApp("+584140000099".into()));
    assert_eq!(
        store.chats(&account, None).unwrap().len(),
        chats,
        "no chat was made"
    );

    let answer = engine
        .start_chat_checked(&account, "+58 414 000 0098")
        .await
        .unwrap();
    assert_eq!(answer, NewChat::Open(ChatId::new("+584140000098")));
    assert_eq!(store.chats(&account, None).unwrap().len(), chats + 1);
    // Not a number at all: said, and final.
    let error = engine
        .start_chat_checked(&account, "maria")
        .await
        .unwrap_err();
    assert!(!error.is_transient());

    // A contact of the address book opens without asking anyone, once.
    let calls = mock.check_calls();
    let mut contact =
        client_provider::Contact::new(account.clone(), ContactId::new("+584140000050"));
    contact.saved_name = Some("Ana".into());
    let chat = engine.chat_with_contact(&contact).unwrap();
    assert_eq!(engine.chat_with_contact(&contact).unwrap(), chat);
    assert_eq!(mock.check_calls(), calls);
    let listed = store.chats(&account, None).unwrap();
    assert_eq!(listed.len(), chats + 2);
    assert!(listed.iter().any(|c| c.id == chat && c.title == "Ana"));
}

#[tokio::test(start_paused = true)]
async fn a_sender_goes_by_the_name_the_address_book_gives() {
    let mock = MockProvider::quiet();
    let (store, account, _) = mock_store(&mock).await;
    let group = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Group)
        .expect("the demo data has a group");
    let stored = store.messages(&account, &group.id, 200).unwrap();
    let from_someone = stored
        .iter()
        .find(|m| m.message.direction == Direction::Incoming && m.message.sender_name.is_some())
        .expect("someone wrote in the group")
        .clone();
    let profile_name = from_someone.message.sender_name.clone().unwrap();

    // The account saved that person under another name.
    let mut contact =
        client_provider::Contact::new(account.clone(), from_someone.message.sender.clone());
    contact.saved_name = Some("Saved Name".into());
    store
        .upsert_listed_contacts(&account, &[contact], Timestamp::now())
        .unwrap();
    let after = store.messages(&account, &group.id, 200).unwrap();
    let same = after
        .iter()
        .find(|m| m.message.id == from_someone.message.id)
        .unwrap();
    assert_eq!(same.message.sender_name.as_deref(), Some("Saved Name"));

    // Removed from the address book: the name the message came with.
    store
        .prune_contacts(&account, Timestamp::from_millis(i64::MAX))
        .unwrap();
    let after = store.messages(&account, &group.id, 200).unwrap();
    let same = after
        .iter()
        .find(|m| m.message.id == from_someone.message.id)
        .unwrap();
    assert_eq!(
        same.message.sender_name.as_deref(),
        Some(profile_name.as_str())
    );
}

// ----- unread counts --------------------------------------------------------

fn incoming(account: &AccountId, chat: &ChatId, id: &str) -> Message {
    Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: account.clone(),
        chat_id: chat.clone(),
        sender: ContactId::new(chat.as_str()),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: Timestamp::now(),
        content: MessageContent::Text {
            body: "hello".into(),
        },
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

fn unread_of(store: &Store, account: &AccountId, chat: &ChatId) -> u32 {
    store.chat(account, chat).unwrap().unwrap().unread_count
}

#[tokio::test(start_paused = true)]
async fn the_providers_unread_count_is_shown_and_nothing_here_zeroes_it() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap()[3].id.clone();
    engine.mark_read(&account, &chat);
    settle().await;
    assert_eq!(unread_of(&store, &account, &chat), 0);

    // The provider counts: its number is the number, through an event
    // and through a refresh alike.
    mock.set_unread(&account, &chat, 4);
    engine
        .apply_event(ProviderEvent::ChatUpdated(
            mock.chat(&account, &chat).unwrap(),
        ))
        .unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 4);
    engine.refresh().await.unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 4);
    // A message arriving is not counted twice: the provider counts.
    engine
        .apply_event(ProviderEvent::MessageUpserted(incoming(
            &account, &chat, "n1",
        )))
        .unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 4);
    // Reading clears it here at once and tells the provider.
    engine.mark_read(&account, &chat);
    assert_eq!(unread_of(&store, &account, &chat), 0);
    settle().await;
    assert_eq!(mock.chat(&account, &chat).unwrap().unread_count, 0);
    assert_eq!(mock.receipts_sent().last(), Some(&chat));
}

#[tokio::test(start_paused = true)]
async fn a_count_the_provider_does_not_know_is_kept_here() {
    // The owner's report: "unread counts are missing". A provider's chat
    // list can have no count for a chat (wuapi: a chat from before it
    // counted, or one holding only imported history). That used to be
    // read as zero and written over whatever was known.
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap()[3].id.clone();
    engine.mark_read(&account, &chat);
    settle().await;
    mock.unread_unknown(&chat);
    engine.refresh().await.unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 0);

    // Messages arrive: nobody else counts them, so the engine does.
    for id in ["u1", "u2"] {
        engine
            .apply_event(ProviderEvent::MessageUpserted(incoming(
                &account, &chat, id,
            )))
            .unwrap();
    }
    assert_eq!(unread_of(&store, &account, &chat), 2);
    // The same message again (a poll that overlaps) is not counted again.
    engine
        .apply_event(ProviderEvent::MessageUpserted(incoming(
            &account, &chat, "u2",
        )))
        .unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 2);
    // A refresh, and the chat arriving as an event, leave the count alone.
    engine.refresh().await.unwrap();
    engine
        .apply_event(ProviderEvent::ChatUpdated({
            let mut blind = mock.chat(&account, &chat).unwrap();
            blind.unread_count = 0;
            blind.unknown.unread = true;
            blind
        }))
        .unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 2);
    // The user's own message does not count.
    let mut own = incoming(&account, &chat, "u3");
    own.direction = Direction::Outgoing;
    engine
        .apply_event(ProviderEvent::MessageUpserted(own))
        .unwrap();
    assert_eq!(unread_of(&store, &account, &chat), 2);
    // Opening the chat clears it.
    engine.mark_read(&account, &chat);
    assert_eq!(unread_of(&store, &account, &chat), 0);
}

#[tokio::test(start_paused = true)]
async fn with_read_receipts_off_a_chat_is_marked_read_quietly() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.unread_count > 0)
        .expect("the demo data has an unread chat")
        .id;
    assert!(engine.read_receipts(), "on unless turned off");
    engine.set_read_receipts(false);
    engine.mark_read(&account, &chat);
    assert_eq!(
        unread_of(&store, &account, &chat),
        0,
        "cleared here at once"
    );
    settle().await;
    // The provider cleared its count too, and sent no receipt.
    assert_eq!(mock.marked_read(), vec![chat.clone()]);
    assert!(mock.receipts_sent().is_empty());
    assert_eq!(mock.chat(&account, &chat).unwrap().unread_count, 0);
}

#[tokio::test(start_paused = true)]
async fn a_number_deleted_elsewhere_goes_from_here_on_the_next_refresh() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let accounts = store.accounts().unwrap();
    let (gone, stays) = (accounts[0].id.clone(), accounts[1].id.clone());
    assert!(!store.chats(&gone, None).unwrap().is_empty());
    let kept = store.chats(&stays, None).unwrap().len();

    // Deleted from the dashboard, say: not through this client.
    mock.delete_account(&gone).await.unwrap();
    // A listing that fails removes nothing.
    mock.fail_next_account_lists([ProviderError::Transient("eof".into())]);
    assert!(engine.refresh().await.is_err());
    assert_eq!(store.accounts().unwrap().len(), 2);

    engine.refresh().await.unwrap();
    let left = store.accounts().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, stays);
    assert!(
        store.chats(&gone, None).unwrap().is_empty(),
        "its chats went with it"
    );
    assert_eq!(store.chats(&stays, None).unwrap().len(), kept);
}

// ----- files on their way out -----------------------------------------------

fn document(bytes: usize) -> crate::NewMedia {
    crate::NewMedia {
        kind: MediaKind::Document,
        bytes: vec![7; bytes],
        mime_type: "application/pdf".into(),
        file_name: Some("report.pdf".into()),
        caption: Some("the report".into()),
        reply_to: None,
        mentions: Vec::new(),
    }
}

fn stored_media(
    store: &Store,
    account: &AccountId,
    chat: &ChatId,
    client_id: &ClientMessageId,
) -> StoredMessage {
    store
        .messages(account, chat, 200)
        .unwrap()
        .into_iter()
        .find(|m| m.message.client_id.as_ref() == Some(client_id))
        .expect("the bubble is there")
}

async fn first_chat(engine: &SyncEngine) -> (AccountId, ChatId) {
    engine.refresh().await.unwrap();
    let store = engine.store();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0).id;
    (account, chat)
}

#[tokio::test(start_paused = true)]
async fn a_file_is_copied_uploaded_and_sent_with_what_the_upload_answered() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();

    let client_id = engine.send_media(&account, &chat, document(4096)).unwrap();
    // Before any network: the copy is in the store and the bubble shows.
    assert_eq!(mock.upload_calls(), 0);
    assert_eq!(
        store.outbox_file(&client_id).unwrap().unwrap().bytes.len(),
        4096
    );
    let bubble = stored_media(&store, &account, &chat, &client_id);
    assert_eq!(bubble.message.status, DeliveryStatus::Pending);
    let MessageContent::Media(media) = &bubble.message.content else {
        panic!("a media bubble");
    };
    assert_eq!(media.source, Some(crate::local_media_ref(&client_id)));
    assert_eq!(media.size_bytes, Some(4096));
    assert_eq!(media.file_name.as_deref(), Some("report.pdf"));
    assert_eq!(media.caption.as_deref(), Some("the report"));

    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!((mock.upload_calls(), mock.uploaded_count()), (1, 1));
    // The provider got the uploaded reference, not the local one.
    let sent = mock
        .fetch_messages(&account, &chat, None, 1)
        .await
        .unwrap()
        .items
        .remove(0);
    let MessageContent::Media(sent) = sent.content else {
        panic!("the provider has a media message");
    };
    assert!(sent.source.unwrap().as_str().starts_with("mock://upload/"));
    assert_eq!(sent.size_bytes, Some(4096), "the bytes that were queued");
    // The queue's copy is gone; the bubble still finds the file.
    assert!(store.outbox_file(&client_id).unwrap().is_none());
    let bubble = stored_media(&store, &account, &chat, &client_id);
    assert_eq!(bubble.message.status, DeliveryStatus::Sent);
    engine.want_file(&account, crate::local_media_ref(&client_id).as_str());
    let key = crate::file_key(crate::local_media_ref(&client_id).as_str());
    let cached = store
        .media(&key, Timestamp::now())
        .unwrap()
        .expect("still here");
    assert_eq!(cached.bytes.len(), 4096);
    assert_eq!(
        mock.media_calls(),
        0,
        "no provider was asked for our own file"
    );
}

#[tokio::test(start_paused = true)]
async fn an_upload_that_fails_for_now_is_repeated_under_the_same_key_and_never_doubles() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let client_id = engine.send_media(&account, &chat, document(1000)).unwrap();

    // The connection drops during the upload, twice.
    mock.fail_next_uploads([
        ProviderError::Transient("eof".into()),
        ProviderError::RateLimited { retry_after: None },
    ]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Pending,
        "nothing the user needs to know"
    );
    // Not before its backoff.
    assert_eq!(
        engine.flush_outbox().await.unwrap(),
        crate::OutboxPass::default()
    );
    tokio::time::sleep(Duration::from_secs(600)).await;
    let at = |hours: i64| Timestamp::from_millis(Timestamp::now().as_millis() + hours * 3_600_000);
    let config = SyncConfig::default().outbox;
    let pass = run_outbox_pass(&store, &mock, &config, at(1))
        .await
        .unwrap();
    assert_eq!(pass.retried, 1);
    // The upload works; then the send drops, and is repeated too.
    mock.fail_next_sends([ProviderError::Transient("eof".into())]);
    let pass = run_outbox_pass(&store, &mock, &config, at(2))
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    assert_eq!(mock.uploaded_count(), 1);
    let pass = run_outbox_pass(&store, &mock, &config, at(3))
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    // One file, one message, whatever it took: the upload was not done
    // again for the send's retry.
    assert_eq!(mock.uploaded_count(), 1);
    assert_eq!(mock.upload_calls(), 3);
    assert_eq!(mock.delivered_count(), 1);
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Sent
    );
}

#[tokio::test(start_paused = true)]
async fn a_queued_file_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.db");
    let mock = MockProvider::quiet();
    let (account, chat, client_id) = {
        let engine = SyncEngine::new(
            Arc::new(Store::open(&path, Some(&test_key())).unwrap()),
            Arc::new(mock.clone()),
            SyncConfig::default(),
            Handle::current(),
        );
        let (account, chat) = first_chat(&engine).await;
        // Queued while the provider is unreachable, then the app closes.
        mock.fail_next_uploads([ProviderError::Transient("offline".into())]);
        let client_id = engine.send_media(&account, &chat, document(2048)).unwrap();
        engine.flush_outbox().await.unwrap();
        (account, chat, client_id)
    };
    assert_eq!(mock.uploaded_count(), 0);

    let store = Arc::new(Store::open(&path, Some(&test_key())).unwrap());
    store.outbox_recover().unwrap();
    assert_eq!(
        store.outbox_file(&client_id).unwrap().unwrap().bytes.len(),
        2048
    );
    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 3_600_000);
    let pass = run_outbox_pass(&store, &mock, &SyncConfig::default().outbox, later)
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.uploaded_count(), 1);
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Sent
    );
}

#[tokio::test(start_paused = true)]
async fn only_a_refusal_fails_a_file_and_too_large_is_refused_before_anything() {
    let mock = MockProvider::quiet();
    mock.set_upload_limit(10_000);
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let before = store.messages(&account, &chat, 500).unwrap().len();

    // Over the provider's limit: not queued, not stored, not uploaded.
    let error = engine
        .send_media(&account, &chat, document(10_001))
        .unwrap_err();
    assert!(matches!(
        error,
        crate::SendMediaError::TooLarge {
            size: 10_001,
            limit: 10_000
        }
    ));
    assert!(matches!(
        engine.send_media(&account, &chat, document(0)),
        Err(crate::SendMediaError::Empty)
    ));
    assert_eq!(store.messages(&account, &chat, 500).unwrap().len(), before);
    assert_eq!(mock.upload_calls(), 0);

    // The provider refuses the type: failed, with its words, once.
    let client_id = engine.send_media(&account, &chat, document(100)).unwrap();
    mock.fail_next_uploads([ProviderError::Rejected {
        code: "unsupported_type".into(),
        message: "This kind of file cannot be sent.".into(),
    }]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 0, 1));
    let bubble = stored_media(&store, &account, &chat, &client_id);
    assert_eq!(
        bubble.message.status,
        DeliveryStatus::Failed {
            reason: "This kind of file cannot be sent.".into()
        }
    );
    assert!(
        store.outbox_file(&client_id).unwrap().is_none(),
        "the copy is let go"
    );
    assert_eq!(
        engine.flush_outbox().await.unwrap(),
        crate::OutboxPass::default()
    );
    assert_eq!(mock.upload_calls(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_text_typed_after_a_file_does_not_overtake_it() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let other = store.chats(&account, None).unwrap().remove(1).id;

    let file = engine.send_media(&account, &chat, document(500)).unwrap();
    let text = engine
        .send_text(&account, &chat, "see the file above", None)
        .unwrap();
    let elsewhere = engine
        .send_text(&account, &other, "meanwhile", None)
        .unwrap();

    // The upload fails for now: the text of the same chat waits behind
    // the file; another chat is not held up.
    mock.fail_next_uploads([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried), (1, 1));
    let status = |chat: &ChatId, id: &ClientMessageId| {
        stored_media(&store, &account, chat, id).message.status
    };
    assert_eq!(status(&other, &elsewhere), DeliveryStatus::Sent);
    assert_eq!(status(&chat, &file), DeliveryStatus::Pending);
    assert_eq!(status(&chat, &text), DeliveryStatus::Pending);

    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 3_600_000);
    let pass = run_outbox_pass(&store, &mock, &SyncConfig::default().outbox, later)
        .await
        .unwrap();
    assert_eq!(pass.sent, 2);
    // In the provider's history: the file, then the text.
    let newest = mock
        .fetch_messages(&account, &chat, None, 2)
        .await
        .unwrap()
        .items;
    assert!(matches!(newest[0].content, MessageContent::Text { .. }));
    assert!(matches!(newest[1].content, MessageContent::Media(_)));
}

#[tokio::test(start_paused = true)]
async fn a_queued_file_can_be_taken_back_until_it_is_sent() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let before = store.messages(&account, &chat, 500).unwrap().len();

    let client_id = engine.send_media(&account, &chat, document(300)).unwrap();
    let text = engine.send_text(&account, &chat, "after it", None).unwrap();
    mock.fail_next_uploads([ProviderError::Transient("eof".into())]);
    engine.flush_outbox().await.unwrap();
    assert!(engine.cancel_send(&client_id).unwrap());
    // Its bubble, its copy and its place in the queue are gone.
    assert_eq!(
        store.messages(&account, &chat, 500).unwrap().len(),
        before + 1
    );
    assert!(store.outbox_file(&client_id).unwrap().is_none());
    assert!(
        !engine.cancel_send(&client_id).unwrap(),
        "nothing left to cancel"
    );
    // What was waiting behind it goes out; the file never does.
    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 3_600_000);
    let pass = run_outbox_pass(&store, &mock, &SyncConfig::default().outbox, later)
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.uploaded_count(), 0);
    assert_eq!(
        stored_media(&store, &account, &chat, &text).message.status,
        DeliveryStatus::Sent
    );
    // A message that has gone cannot be taken back.
    assert!(!engine.cancel_send(&text).unwrap());
}

#[tokio::test(start_paused = true)]
async fn an_upload_the_provider_let_go_is_made_again() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let client_id = engine.send_media(&account, &chat, document(700)).unwrap();
    // Uploaded, but the send does not get through for a long time, and
    // the provider's copy of the upload expires meanwhile.
    mock.fail_next_sends([ProviderError::Transient("eof".into())]);
    engine.flush_outbox().await.unwrap();
    assert_eq!(mock.uploaded_count(), 1);
    mock.expire_uploads();

    let config = SyncConfig::default().outbox;
    let at = |hours: i64| Timestamp::from_millis(Timestamp::now().as_millis() + hours * 3_600_000);
    let pass = run_outbox_pass(&store, &mock, &config, at(1))
        .await
        .unwrap();
    assert_eq!(
        (pass.sent, pass.retried, pass.failed),
        (0, 1, 0),
        "not a failure"
    );
    let pass = run_outbox_pass(&store, &mock, &config, at(2))
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.upload_calls(), 2, "uploaded again, under a new key");
    assert_eq!(mock.delivered_count(), 1);
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Sent
    );
}

#[tokio::test(start_paused = true)]
async fn an_image_shows_from_the_copy_in_hand_and_progress_is_reported() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let image = crate::NewMedia {
        kind: MediaKind::Image,
        bytes: png(800, 400),
        mime_type: "image/png".into(),
        file_name: Some("shot.png".into()),
        caption: None,
        reply_to: None,
        mentions: Vec::new(),
    };
    let client_id = engine.send_media(&account, &chat, image).unwrap();
    // The thumbnail is there before any network, with the picture's shape.
    let local = crate::local_media_ref(&client_id);
    let size = store
        .media_size(&thumbnail_key(local.as_str()))
        .unwrap()
        .unwrap();
    assert_eq!(size.0, size.1 * 2);
    let MessageContent::Media(media) = stored_media(&store, &account, &chat, &client_id)
        .message
        .content
    else {
        panic!("a media bubble");
    };
    assert_eq!(
        media.width.zip(media.height).map(|(w, h)| w == h * 2),
        Some(true)
    );
    assert_eq!(engine.upload_progress(&client_id), None, "not started");

    // While it uploads, the engine says how far it is.
    mock.set_upload_latency(Duration::from_secs(5));
    let flushing = {
        let engine = engine.clone();
        tokio::spawn(async move { engine.flush_outbox().await })
    };
    tokio::time::sleep(Duration::from_secs(1)).await;
    let (sent, total) = engine.upload_progress(&client_id).expect("uploading");
    assert!(sent > 0 && sent < total, "{sent} of {total}");
    assert_eq!(flushing.await.unwrap().unwrap().sent, 1);
    assert_eq!(engine.upload_progress(&client_id), None, "done");
    assert_eq!(mock.media_calls(), 0);
}

mod actions;
mod chat_stickers;
mod communities;
mod forward;
mod library;
mod message_types;
mod people;
mod sending;
mod social;
mod stories;
mod upgrade;

// ----- downloads the user watches -------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_download_reports_how_far_it_is_and_can_be_stopped() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let url = "https://media.example/slow.png";
    let bytes = png(320, 200);
    let size = bytes.len() as u64;
    mock.set_media(url, bytes, "image/png");
    mock.set_media_latency(Duration::from_secs(10));
    let key = thumbnail_key(url);

    engine.want_thumbnail(&account, url);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(engine.media_state(&key), MediaState::Loading);
    assert_eq!(engine.media_progress(&key), Some(size / 2));

    // Stopped where it stands: nothing is kept, and nothing arrives late.
    engine.cancel_media_key(&key);
    assert_eq!(engine.media_state(&key), MediaState::Idle);
    assert_eq!(engine.media_progress(&key), None);
    tokio::time::sleep(Duration::from_secs(30)).await;
    assert!(engine.store().media_size(&key).unwrap().is_none());
    assert_eq!(engine.media_state(&key), MediaState::Idle);
    // Stopping something that is not loading does nothing.
    engine.cancel_media_key(&key);

    // Asked for again: it starts over and arrives.
    mock.set_media_latency(Duration::ZERO);
    engine.want_thumbnail(&account, url);
    assert_eq!(settled(&engine, &key).await, MediaState::Idle);
    assert!(engine.store().media_size(&key).unwrap().is_some());
    assert_eq!(engine.media_progress(&key), None, "nothing is left over");
}

#[tokio::test(start_paused = true)]
async fn a_file_whatsapp_no_longer_has_is_gone_and_a_refusal_can_be_tried_again() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let url = "https://media.example/expired.png";
    mock.set_media(url, png(64, 64), "image/png");
    let key = thumbnail_key(url);
    let rejected = |code: &str, message: &str| ProviderError::Rejected {
        code: code.into(),
        message: message.into(),
    };

    // Refused for now: said, and asking again works.
    mock.fail_next_media([rejected("forbidden", "Not now.")]);
    engine.want_thumbnail(&account, url);
    assert_eq!(
        settled(&engine, &key).await,
        MediaState::Unavailable("Not now.".into())
    );
    engine.retry_media(&key);
    // Expired on WhatsApp: gone, and "retry" does not bring it back.
    mock.fail_next_media([rejected(
        "media_expired",
        "WhatsApp no longer has this file.",
    )]);
    engine.want_thumbnail(&account, url);
    let state = settled(&engine, &key).await;
    assert_eq!(
        state,
        MediaState::Expired("WhatsApp no longer has this file.".into())
    );
    let calls = mock.media_calls();
    engine.retry_media(&key);
    engine.want_thumbnail(&account, url);
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(mock.media_calls(), calls, "not asked for again");
    assert!(matches!(engine.media_state(&key), MediaState::Expired(_)));
}

#[tokio::test(start_paused = true)]
async fn a_picture_the_user_asks_for_may_be_larger_than_one_fetched_unasked() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = engine_with_chats(&mock).await;
    let url = "https://media.example/poster.png";
    // A real picture, padded past the limit for what is fetched unasked.
    let mut bytes = png(64, 64);
    bytes.resize(AUTO_MEDIA_LIMIT as usize + 1, 0);
    mock.set_media(url, bytes, "image/png");
    let key = thumbnail_key(url);
    engine.want_thumbnail(&account, url);
    assert!(matches!(
        settled(&engine, &key).await,
        MediaState::Unavailable(_)
    ));
    // On the user's click the larger limit applies.
    engine.retry_media(&key);
    engine.want_thumbnail_asked(&account, url);
    let state = settled(&engine, &key).await;
    assert!(
        engine.store().media_size(&key).unwrap().is_some(),
        "fetched on request: {state:?}"
    );
}

#[test]
fn search_stays_quick_over_a_long_history() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    // One word in every message and one in every hundredth.
    let batch: Vec<_> = (0..20_000)
        .map(|i| {
            let body = if i % 100 == 0 {
                format!("common rare number{i}")
            } else {
                format!("common number{i}")
            };
            text(&format!("m{i}"), "chat", i, &body, Direction::Incoming)
        })
        .collect();
    store.upsert_messages(&batch).unwrap();

    let start = std::time::Instant::now();
    let common = store.search_messages(&account(), "comm", 40).unwrap();
    let rare = store.search_messages(&account(), "rare", 40).unwrap();
    let took = start.elapsed();
    assert_eq!(common.len(), 40);
    assert_eq!(common[0].message.id.as_str(), "m19999", "newest first");
    assert_eq!(rare.len(), 40);
    // Tens of milliseconds when the index is asked once; several seconds
    // when it is asked once per message.
    assert!(
        took < std::time::Duration::from_secs(1),
        "two searches took {took:?}"
    );
}
