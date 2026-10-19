//! The provider over a session that is not WhatsApp: what the client
//! relies on, and what this provider does with what it hears. Nothing here
//! touches the network or needs a phone.

use crate::archive::Archive;
use crate::convert;
use crate::mapping::{option_hash, MediaPointer};
use crate::session::{
    CallSignal, HistoryChat, Identity, Inbound, Incoming, OpenOptions, ReceiptLevel, Session,
    SessionError, SessionResult, Sessions, Sink, Target,
};
use crate::NativeProvider;
use async_trait::async_trait;
use client_provider::{
    AccountId, CallState, ChatChange, ChatId, ClientMessageId, ConnectionState, DeliveryStatus,
    Direction, ForwardItem, HistoryImport, LinkStep, MediaKind, MediaUpload, MessageContent,
    MessageId, NewAccount, OutgoingContent, OutgoingMessage, PresenceState, Provider,
    ProviderError, ProviderEvent, SystemKind,
};
use futures::StreamExt as _;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;
use whatsapp_rust::buffa::MessageField;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::Jid;

const ANA: &str = "584245550199@s.whatsapp.net";
const ANA_ID: &str = "+584245550199";
const ME: &str = "584125550100@s.whatsapp.net";

/// A read receipt: the chat, the sender it names, the messages.
type ReadCall = (String, Option<String>, Vec<String>);

/// What went over the pretend wire.
#[derive(Default)]
struct Wire {
    sent: Mutex<Vec<(String, String, wa::Message)>>,
    read: Mutex<Vec<ReadCall>>,
    /// How many sends go out and lose their answer.
    lost_answers: AtomicUsize,
    /// What the phone hands over when asked for older messages.
    older: Mutex<Option<Inbound>>,
    history_asked: AtomicUsize,
    sink: Mutex<Option<(i32, Sink)>>,
    removed: Mutex<Vec<i32>>,
    ended_calls: Mutex<Vec<String>>,
}

struct FakeSession(Arc<Wire>);

#[async_trait]
impl Session for FakeSession {
    fn identity(&self) -> Identity {
        Identity {
            pn: Some(ME.parse().unwrap()),
            lid: None,
            push_name: "Me".into(),
        }
    }

    async fn send(&self, to: &Jid, message: wa::Message, id: &str) -> SessionResult<()> {
        if let Some(text) = message.conversation.as_deref() {
            if text == "refuse me" {
                return Err(SessionError::refused(
                    "not_on_whatsapp",
                    "No WhatsApp there.",
                ));
            }
        }
        self.0
            .sent
            .lock()
            .unwrap()
            .push((convert::wire(to), id.to_owned(), message));
        // One fewer answer to lose, if any is left to lose.
        let left = &self.0.lost_answers;
        let mut seen = left.load(Ordering::SeqCst);
        let lose = loop {
            let Some(fewer) = seen.checked_sub(1) else {
                break false;
            };
            match left.compare_exchange(seen, fewer, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break true,
                Err(now) => seen = now,
            }
        };
        if lose {
            return Err(SessionError::NotNow("the answer was lost".into()));
        }
        Ok(())
    }

    async fn mark_read(
        &self,
        chat: &Jid,
        sender: Option<&Jid>,
        ids: &[String],
    ) -> SessionResult<()> {
        self.0.read.lock().unwrap().push((
            convert::wire(chat),
            sender.map(convert::wire),
            ids.to_vec(),
        ));
        Ok(())
    }

    async fn download(&self, pointer: &MediaPointer) -> SessionResult<Vec<u8>> {
        Ok(pointer.direct_path.as_bytes().to_vec())
    }

    async fn upload(&self, bytes: Vec<u8>, class: &str) -> SessionResult<MediaPointer> {
        Ok(MediaPointer {
            direct_path: format!("/up/{}", bytes.len()),
            media_key: vec![1; 32],
            file_sha256: vec![2; 32],
            file_enc_sha256: vec![3; 32],
            file_length: bytes.len() as u64,
            class: class.to_owned(),
            mime_type: None,
            url: Some("https://mmg.whatsapp.net/up".into()),
            key_timestamp: Some(1),
        })
    }

    async fn request_history(&self, _oldest: &Target, _count: u32) -> SessionResult<()> {
        self.0.history_asked.fetch_add(1, Ordering::SeqCst);
        let older = self.0.older.lock().unwrap().take();
        if let (Some(older), Some((device, sink))) = (older, self.0.sink.lock().unwrap().clone()) {
            let _ = sink.send((device, older));
        }
        Ok(())
    }

    async fn pair_code(&self, _phone: &str) -> SessionResult<String> {
        Ok("ABCD1234".into())
    }

    async fn cancel_pair_code(&self) -> SessionResult<()> {
        Ok(())
    }

    async fn reconnect(&self) {}

    async fn logout(&self) -> SessionResult<()> {
        Ok(())
    }

    async fn shutdown(&self) {}

    async fn react(&self, target: &Target, _emoji: &str) -> SessionResult<String> {
        Ok(format!("R-{}", target.id))
    }

    async fn vote(
        &self,
        _target: &Target,
        _poll: &wa::Message,
        _options: &[String],
    ) -> SessionResult<()> {
        Ok(())
    }

    async fn end_call(&self, id: &str) -> SessionResult<()> {
        self.0.ended_calls.lock().unwrap().push(id.to_owned());
        Ok(())
    }

    async fn accept_call(&self, _id: &str) -> SessionResult<()> {
        Ok(())
    }
}

struct FakeSessions {
    wire: Arc<Wire>,
    next: AtomicI32,
}

#[async_trait]
impl Sessions for FakeSessions {
    async fn open(
        &self,
        device: Option<i32>,
        _options: OpenOptions,
        sink: Sink,
    ) -> SessionResult<(i32, Arc<dyn Session>)> {
        let device = device.unwrap_or_else(|| self.next.fetch_add(1, Ordering::SeqCst));
        *self.wire.sink.lock().unwrap() = Some((device, sink));
        Ok((device, Arc::new(FakeSession(self.wire.clone()))))
    }

    async fn remove(&self, device: i32) -> SessionResult<()> {
        self.wire.removed.lock().unwrap().push(device);
        Ok(())
    }
}

struct World {
    provider: NativeProvider,
    wire: Arc<Wire>,
    account: AccountId,
}

impl World {
    /// Hands the provider a piece of news, as the session would.
    fn hear(&self, inbound: Inbound) {
        self.provider.inner.apply(1, inbound);
    }

    fn sent(&self) -> Vec<(String, String, wa::Message)> {
        self.wire.sent.lock().unwrap().clone()
    }
}

fn new_account() -> NewAccount {
    NewAccount {
        name: Some("Work".into()),
        place: None,
        pairing_phone: None,
        history: HistoryImport::Recent,
        request_id: "request-1".into(),
    }
}

fn provider_on(wire: &Arc<Wire>) -> NativeProvider {
    let sessions = Arc::new(FakeSessions {
        wire: wire.clone(),
        next: AtomicI32::new(1),
    });
    NativeProvider::new(sessions, Archive::in_memory().unwrap(), &Handle::current())
}

/// A provider with one number, linked and online.
async fn world() -> World {
    let wire = Arc::new(Wire::default());
    let provider = provider_on(&wire);
    let status = provider.create_account(&new_account()).await.unwrap();
    let world = World {
        account: status.account.id,
        provider,
        wire,
    };
    world.hear(Inbound::Connected(Identity {
        pn: Some(ME.parse().unwrap()),
        lid: None,
        push_name: "Me".into(),
    }));
    world
}

/// The provider's events from now on: a new listener is first told where
/// each number's connection stands, which these tests already know.
async fn listen(world: &World) -> client_provider::EventStream {
    let mut events = world.provider.subscribe().await.unwrap();
    let said = events.next().await;
    assert!(matches!(
        said,
        Some(ProviderEvent::ConnectionChanged { .. })
    ));
    events
}

fn jid(address: &str) -> Jid {
    address.parse().unwrap()
}

fn text_proto(body: &str) -> wa::Message {
    wa::Message {
        conversation: Some(body.into()),
        ..Default::default()
    }
}

fn incoming(chat: &str, id: &str, at_ms: i64, message: wa::Message) -> Incoming {
    Incoming {
        chat: jid(chat),
        chat_alt: None,
        sender: jid(chat),
        sender_alt: None,
        from_me: false,
        id: id.into(),
        push_name: Some("Ana P.".into()),
        timestamp_ms: at_ms,
        message: Arc::new(message),
        status: None,
        starred: false,
    }
}

fn text(world: &World, client_id: &str, body: &str) -> OutgoingMessage {
    OutgoingMessage {
        client_id: ClientMessageId::new(client_id),
        account_id: world.account.clone(),
        chat_id: ChatId::new(ANA_ID),
        content: OutgoingContent::Text { body: body.into() },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    }
}

async fn history(world: &World) -> Vec<client_provider::Message> {
    world
        .provider
        .fetch_messages(&world.account, &ChatId::new(ANA_ID), None, 50)
        .await
        .unwrap()
        .items
}

// ----- linking --------------------------------------------------------------

#[tokio::test]
async fn a_number_is_linked_by_scanning_and_listed_once_it_is() {
    let wire = Arc::new(Wire::default());
    let provider = provider_on(&wire);
    assert!(provider.list_accounts().await.unwrap().is_empty());

    let status = provider.create_account(&new_account()).await.unwrap();
    let account = status.account.id.clone();
    assert_eq!(status.step, LinkStep::Starting);
    assert!(!status.was_linked);
    assert!(status.account.never_linked());
    // The same request again is the same number.
    let again = provider.create_account(&new_account()).await.unwrap();
    assert_eq!(again.account.id, account);
    assert_eq!(provider.list_accounts().await.unwrap().len(), 1);

    provider.inner.apply(1, Inbound::Qr("2@abc,def,ghi".into()));
    let LinkStep::Scan { png } = provider.link_status(&account).await.unwrap().step else {
        panic!("a code to scan");
    };
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']), "a PNG image");

    // A code typed on the phone instead, and the way back to the QR code.
    let typed = provider
        .pairing_code(&account, "+58 412 555 0100")
        .await
        .unwrap();
    assert_eq!(
        typed.step,
        LinkStep::TypeCode {
            code: "ABCD-1234".into(),
            expires_at: None
        }
    );
    provider.inner.apply(1, Inbound::Qr("2@next".into()));
    assert!(matches!(
        provider.link_status(&account).await.unwrap().step,
        LinkStep::TypeCode { .. }
    ));
    let back = provider.scan_instead(&account).await.unwrap();
    assert!(matches!(back.step, LinkStep::Scan { .. }));

    provider.inner.apply(1, Inbound::Paired);
    assert_eq!(
        provider.link_status(&account).await.unwrap().step,
        LinkStep::Finishing
    );
    provider.inner.apply(
        1,
        Inbound::Connected(Identity {
            pn: Some(jid(ME)),
            lid: Some(jid("200055501000001@lid")),
            push_name: "Me".into(),
        }),
    );
    let linked = provider.link_status(&account).await.unwrap();
    assert_eq!(linked.step, LinkStep::Linked);
    assert!(linked.was_linked);
    assert_eq!(linked.account.connection, ConnectionState::Connected);
    assert_eq!(linked.account.phone.as_deref(), Some("+584125550100"));
    assert_eq!(
        linked.account.display_name, "Work",
        "the user's label comes first"
    );
    assert_eq!(
        linked.account.self_contact.as_ref().map(|id| id.as_str()),
        Some("+584125550100")
    );
}

#[tokio::test]
async fn a_session_that_speaks_before_it_is_written_down_is_heard() {
    let wire = Arc::new(Wire::default());
    let provider = provider_on(&wire);
    // The QR code of a session nobody knows the number of yet.
    provider.inner.apply(1, Inbound::Qr("2@early".into()));
    let status = provider.create_account(&new_account()).await.unwrap();
    assert!(matches!(status.step, LinkStep::Scan { .. }));
}

#[tokio::test]
async fn a_number_logged_out_on_the_phone_says_so_and_can_be_linked_again() {
    let world = world().await;
    let mut events = listen(&world).await;
    world.hear(Inbound::LoggedOut(None));
    assert_eq!(
        events.next().await,
        Some(ProviderEvent::ConnectionChanged {
            account_id: world.account.clone(),
            state: ConnectionState::LoggedOut,
        })
    );
    let error = world
        .provider
        .send(text(&world, "k", "hello"))
        .await
        .unwrap_err();
    assert!(!error.is_transient(), "nothing to wait for: {error:?}");

    let status = world
        .provider
        .reconnect_account(&world.account)
        .await
        .unwrap();
    assert_eq!(status.step, LinkStep::Starting);
    assert_eq!(
        *world.wire.removed.lock().unwrap(),
        [1],
        "the dead session's keys go"
    );
    assert!(status.was_linked);
}

#[tokio::test]
async fn deleting_a_number_leaves_nothing_of_it() {
    let world = world().await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A1",
        1_000,
        text_proto("hi"),
    )]));
    world.provider.delete_account(&world.account).await.unwrap();
    assert!(world.provider.list_accounts().await.unwrap().is_empty());
    assert_eq!(*world.wire.removed.lock().unwrap(), [1]);
    // Deleting it again succeeds: the client repeats a lost answer.
    world.provider.delete_account(&world.account).await.unwrap();
}

// ----- the contract ---------------------------------------------------------

#[tokio::test]
async fn the_account_and_its_chat_are_listed_with_a_preview() {
    let world = world().await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A1",
        1_000,
        text_proto("hi"),
    )]));
    let accounts = world.provider.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    let chats = world
        .provider
        .list_chats(&accounts[0].id, None)
        .await
        .unwrap();
    assert_eq!(chats.items.len(), 1);
    assert!(chats.next_cursor.is_none(), "one page is all there is");
    let chat = &chats.items[0];
    assert_eq!(chat.id.as_str(), ANA_ID);
    assert_eq!(chat.title, "Ana P.", "named after who wrote");
    assert_eq!(chat.unread_count, 1);
    assert_eq!(
        chat.last_message.as_ref().map(|message| &message.content),
        Some(&MessageContent::text("hi"))
    );
}

#[tokio::test]
async fn a_send_repeated_is_one_message_with_one_id() {
    let world = world().await;
    let first = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap();
    let again = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap();
    assert_eq!(first.message_id, again.message_id);
    assert_eq!(first.status, DeliveryStatus::Sent);
    assert_eq!(world.sent().len(), 1, "one message on WhatsApp");

    let stored = history(&world).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, first.message_id);
    assert_eq!(stored[0].client_id, Some(ClientMessageId::new("key-1")));
    assert_eq!(stored[0].direction, Direction::Outgoing);
    assert_eq!(stored[0].sender.as_str(), "+584125550100");
}

#[tokio::test]
async fn a_lost_answer_never_sends_a_second_message() {
    let world = world().await;
    world.wire.lost_answers.store(1, Ordering::SeqCst);
    let lost = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap_err();
    assert!(lost.is_transient());
    let receipt = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap();
    // It went over the wire twice, under one id: to WhatsApp that is one
    // message, which is what the id being made from the key is for.
    let sent = world.sent();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, sent[1].1);
    assert_eq!(receipt.message_id.as_str(), sent[0].1);
    assert_eq!(history(&world).await.len(), 1);
}

#[tokio::test]
async fn the_copy_from_another_device_is_the_bubble_already_shown() {
    let world = world().await;
    let receipt = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap();
    // WhatsApp hands the account's other devices' view of it back.
    let mut echo = incoming(ANA, receipt.message_id.as_str(), 2_000, text_proto("hello"));
    echo.from_me = true;
    echo.sender = jid(ME);
    let mut events = listen(&world).await;
    world.hear(Inbound::Messages(vec![echo]));
    let Some(ProviderEvent::MessageUpserted(copy)) = events.next().await else {
        panic!("the copy is announced");
    };
    assert_eq!(copy.client_id, Some(ClientMessageId::new("key-1")));
    assert_eq!(history(&world).await.len(), 1);
}

#[tokio::test]
async fn an_event_reaches_whoever_subscribed() {
    let world = world().await;
    let mut events = listen(&world).await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A1",
        1_000,
        text_proto("hi"),
    )]));
    let mut seen = Vec::new();
    for _ in 0..3 {
        seen.push(events.next().await.unwrap());
    }
    assert!(seen.iter().any(
        |event| matches!(event, ProviderEvent::ContactUpdated(contact)
        if contact.profile_name.as_deref() == Some("Ana P."))
    ));
    assert!(seen.iter().any(
        |event| matches!(event, ProviderEvent::MessageUpserted(message)
        if message.id.as_str() == "A1" && message.direction == Direction::Incoming)
    ));
    assert!(seen
        .iter()
        .any(|event| matches!(event, ProviderEvent::ChatUpdated(chat)
        if chat.unread_count == 1)));
}

// The clock is paused: waiting for a phone that has nothing more takes
// no time here.
#[tokio::test(start_paused = true)]
async fn history_is_newest_first_and_pages_back_to_the_start() {
    let world = world().await;
    let batch = (0..5)
        .map(|n| {
            incoming(
                ANA,
                &format!("A{n}"),
                1_000 + n,
                text_proto(&format!("{n}")),
            )
        })
        .collect();
    world.hear(Inbound::Messages(batch));

    let chat = ChatId::new(ANA_ID);
    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let page = world
            .provider
            .fetch_messages(&world.account, &chat, cursor, 2)
            .await
            .unwrap();
        seen.extend(page.items.iter().map(|message| message.id.to_string()));
        pages += 1;
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
        assert!(pages < 10, "the listing ends");
    }
    assert_eq!(seen, ["A4", "A3", "A2", "A1", "A0"]);
    // Past the oldest message here, the phone was asked once, and had
    // nothing more.
    assert_eq!(world.wire.history_asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn older_messages_come_from_the_phone_when_this_device_runs_out() {
    let world = world().await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A5",
        5_000,
        text_proto("new"),
    )]));
    *world.wire.older.lock().unwrap() = Some(Inbound::History {
        chats: vec![HistoryChat {
            chat: jid(ANA),
            chat_alt: None,
            name: None,
            unread: None,
            marked_unread: false,
            archived: false,
            pinned_at_ms: None,
            muted_until_ms: None,
            messages: vec![incoming(ANA, "A1", 1_000, text_proto("old"))],
        }],
        names: Vec::new(),
        aliases: Vec::new(),
        asked: true,
    });
    let chat = ChatId::new(ANA_ID);
    let first = world
        .provider
        .fetch_messages(&world.account, &chat, None, 10)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 1);
    let second = world
        .provider
        .fetch_messages(&world.account, &chat, first.next_cursor, 10)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 1);
    assert_eq!(second.items[0].id.as_str(), "A1");
    assert_eq!(second.items[0].content, MessageContent::text("old"));
}

#[tokio::test]
async fn what_cannot_be_sent_is_rejected_and_what_must_wait_is_transient() {
    let world = world().await;
    let refused = world
        .provider
        .send(text(&world, "k1", "refuse me"))
        .await
        .unwrap_err();
    assert!(matches!(&refused, ProviderError::Rejected { code, .. } if code == "not_on_whatsapp"));
    let empty = world
        .provider
        .send(text(&world, "k2", "  "))
        .await
        .unwrap_err();
    assert!(matches!(empty, ProviderError::Rejected { .. }));

    world.hear(Inbound::Reconnecting);
    let waiting = world
        .provider
        .send(text(&world, "k3", "later"))
        .await
        .unwrap_err();
    assert!(waiting.is_transient(), "{waiting:?}");
    world.hear(Inbound::Connected(Identity::default()));
    world
        .provider
        .send(text(&world, "k3", "later"))
        .await
        .unwrap();
}

// ----- what is heard --------------------------------------------------------

#[tokio::test]
async fn reading_a_chat_tells_the_sender_once_and_clears_the_count() {
    let world = world().await;
    world.hear(Inbound::Messages(vec![
        incoming(ANA, "A1", 1_000, text_proto("one")),
        incoming(ANA, "A2", 2_000, text_proto("two")),
    ]));
    let chat = ChatId::new(ANA_ID);
    world
        .provider
        .mark_read(&world.account, &chat, None)
        .await
        .unwrap();
    world
        .provider
        .mark_read(&world.account, &chat, None)
        .await
        .unwrap();
    let read = world.wire.read.lock().unwrap().clone();
    assert_eq!(read.len(), 1, "a receipt is not sent twice");
    assert_eq!(read[0].0, ANA);
    assert_eq!(read[0].1, None, "a direct chat names no sender");
    let mut ids = read[0].2.clone();
    ids.sort();
    assert_eq!(ids, ["A1", "A2"]);
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert_eq!(chats.items[0].unread_count, 0);
    assert!(history(&world)
        .await
        .iter()
        .all(|m| m.status == DeliveryStatus::Read));
}

#[tokio::test]
async fn ticks_move_forward_and_never_back() {
    let world = world().await;
    let receipt = world
        .provider
        .send(text(&world, "key-1", "hello"))
        .await
        .unwrap();
    let id = receipt.message_id.to_string();
    let mut events = listen(&world).await;
    let hear = |level| {
        world.hear(Inbound::Receipt {
            chat: jid(ANA),
            from: jid(ANA),
            ids: vec![id.clone()],
            level,
            timestamp_ms: 3_000,
        });
    };
    hear(ReceiptLevel::Read);
    hear(ReceiptLevel::Delivered);
    assert_eq!(
        events.next().await,
        Some(ProviderEvent::MessageStatusChanged {
            account_id: world.account.clone(),
            chat_id: ChatId::new(ANA_ID),
            message_id: receipt.message_id.clone(),
            status: DeliveryStatus::Read,
        })
    );
    assert_eq!(history(&world).await[0].status, DeliveryStatus::Read);
    // Nothing else was announced: the late "delivered" changed nothing.
    world.hear(Inbound::Activity {
        chat: jid(ANA),
        from: jid(ANA),
        state: PresenceState::Typing,
    });
    assert!(matches!(
        events.next().await,
        Some(ProviderEvent::Presence { .. })
    ));
}

#[tokio::test]
async fn a_message_taken_back_or_edited_changes_in_place() {
    use wa::message::protocol_message::Type;
    let world = world().await;
    world.hear(Inbound::Messages(vec![
        incoming(ANA, "A1", 1_000, text_proto("typo")),
        incoming(ANA, "A2", 2_000, text_proto("oops")),
    ]));
    let protocol = |target: &str, kind, edited: Option<wa::Message>| wa::Message {
        protocol_message: MessageField::some(wa::message::ProtocolMessage {
            key: MessageField::some(wa::MessageKey {
                id: Some(target.into()),
                ..Default::default()
            }),
            r#type: Some(kind),
            edited_message: edited.map(MessageField::some).unwrap_or_default(),
            ..Default::default()
        }),
        ..Default::default()
    };
    world.hear(Inbound::Messages(vec![
        incoming(
            ANA,
            "E1",
            3_000,
            protocol("A1", Type::MESSAGE_EDIT, Some(text_proto("fixed"))),
        ),
        incoming(ANA, "E2", 4_000, protocol("A2", Type::REVOKE, None)),
    ]));
    let stored = history(&world).await;
    assert_eq!(
        stored.len(),
        2,
        "an edit and a revoke are not messages of their own"
    );
    let (taken_back, edited) = (&stored[0], &stored[1]);
    assert!(taken_back.deleted && taken_back.id.as_str() == "A2");
    assert_eq!(edited.content, MessageContent::text("fixed"));
    assert!(edited.edited);
}

#[tokio::test]
async fn one_person_under_a_hidden_id_and_a_number_is_one_chat() {
    let world = world().await;
    let hidden = "200055501000009@lid";
    // First by the hidden id alone: that is all this device knows.
    world.hear(Inbound::Messages(vec![incoming(
        hidden,
        "L1",
        1_000,
        text_proto("one"),
    )]));
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert_eq!(chats.items[0].id.as_str(), "lid:200055501000009");

    // Then a message that says both.
    let mut both = incoming(hidden, "L2", 2_000, text_proto("two"));
    both.chat_alt = Some(jid(ANA));
    both.sender_alt = Some(jid(ANA));
    world.hear(Inbound::Messages(vec![both]));
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert_eq!(chats.items.len(), 1);
    assert_eq!(chats.items[0].id.as_str(), ANA_ID);
    assert_eq!(history(&world).await.len(), 2);

    let contacts = world
        .provider
        .list_contacts(&world.account, None)
        .await
        .unwrap();
    let ana = contacts
        .items
        .iter()
        .find(|c| c.id.as_str() == ANA_ID)
        .unwrap();
    assert_eq!(ana.alt_ids[0].as_str(), "lid:200055501000009");
    assert_eq!(ana.phone.as_deref(), Some(ANA_ID));
}

#[tokio::test]
async fn the_phones_history_lists_chats_as_the_phone_has_them() {
    let world = world().await;
    let group = "120363000000000001@g.us";
    let mut from_me = incoming(ANA, "H2", 2_000, text_proto("sure"));
    from_me.from_me = true;
    from_me.status = Some(convert::READ);
    world.hear(Inbound::History {
        chats: vec![
            HistoryChat {
                chat: jid(ANA),
                chat_alt: None,
                name: None,
                unread: Some(3),
                marked_unread: false,
                archived: false,
                pinned_at_ms: Some(0),
                muted_until_ms: Some(-1),
                messages: vec![incoming(ANA, "H1", 1_000, text_proto("lunch?")), from_me],
            },
            HistoryChat {
                chat: jid(group),
                chat_alt: None,
                name: Some("Family".into()),
                unread: Some(0),
                marked_unread: false,
                archived: true,
                pinned_at_ms: None,
                muted_until_ms: None,
                messages: Vec::new(),
            },
            HistoryChat {
                chat: jid("120363111111111111@newsletter"),
                chat_alt: None,
                name: Some("A channel".into()),
                unread: None,
                marked_unread: false,
                archived: false,
                pinned_at_ms: None,
                muted_until_ms: None,
                messages: Vec::new(),
            },
        ],
        names: vec![(jid(ANA), "Ana P.".into())],
        aliases: Vec::new(),
        asked: false,
    });
    world.hear(Inbound::Contact {
        jid: jid(ANA),
        alt: None,
        saved_name: Some("Ana".into()),
        username: None,
    });
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap()
        .items;
    assert_eq!(chats.len(), 2, "a channel is not a conversation");
    let ana = chats
        .iter()
        .find(|chat| chat.id.as_str() == ANA_ID)
        .unwrap();
    assert_eq!(ana.title, "Ana", "the address book's name wins");
    assert_eq!(ana.unread_count, 3);
    assert!(ana.pinned && ana.muted && !ana.archived);
    assert_eq!(
        ana.last_message
            .as_ref()
            .map(|message| message.status.clone()),
        Some(DeliveryStatus::Read)
    );
    let family = chats.iter().find(|chat| chat.id.as_str() == group).unwrap();
    assert_eq!(family.title, "Family");
    assert!(family.archived);
    assert_eq!(family.kind, client_provider::ChatKind::Group);

    // A pin made on the phone later is heard, and said.
    let mut events = listen(&world).await;
    world.hear(Inbound::Pinned {
        chat: jid(group),
        pinned: true,
        at_ms: 9_000,
    });
    let Some(ProviderEvent::ChatUpdated(pinned)) = events.next().await else {
        panic!("the chat is announced");
    };
    assert!(pinned.pinned);
    assert_eq!(pinned.pinned_at.map(|at| at.as_millis()), Some(9_000));
}

#[tokio::test]
async fn a_file_is_uploaded_once_and_sent_by_its_pointer() {
    let world = world().await;
    let upload = || MediaUpload {
        key: "upload-1".into(),
        kind: MediaKind::Image,
        bytes: Arc::new(vec![0; 10]),
        mime_type: "image/jpeg".into(),
        file_name: None,
    };
    let progress: client_provider::UploadProgress = Arc::new(|_| {});
    let media = world
        .provider
        .upload_media(&world.account, upload(), progress.clone())
        .await
        .unwrap();
    let again = world
        .provider
        .upload_media(&world.account, upload(), progress)
        .await
        .unwrap();
    assert_eq!(media, again, "the same key is the same upload");

    let mut message = text(&world, "key-m", "");
    message.content = OutgoingContent::Media {
        kind: MediaKind::Image,
        media: media.clone(),
        mime_type: Some("image/jpeg".into()),
        caption: Some("look".into()),
        file_name: None,
        gif: false,
        preview: None,
    };
    world.provider.send(message).await.unwrap();
    let sent = world.sent();
    let image = sent[0]
        .2
        .image_message
        .as_option()
        .expect("an image went out");
    assert_eq!(image.direct_path.as_deref(), Some("/up/10"));
    assert_eq!(image.caption.as_deref(), Some("look"));

    let stored = history(&world).await;
    let MessageContent::Media(held) = &stored[0].content else {
        panic!("a picture");
    };
    let data = world
        .provider
        .download_media(&world.account, held.source.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(data.bytes, b"/up/10");
    assert_eq!(data.mime_type.as_deref(), Some("image/jpeg"));
}

#[tokio::test]
async fn a_forward_names_the_original_and_refuses_what_cannot_go() {
    let world = world().await;
    let once = wa::Message {
        view_once_message_v2: MessageField::some(wa::message::FutureProofMessage {
            message: MessageField::some(wa::Message {
                image_message: MessageField::some(Default::default()),
                ..Default::default()
            }),
        }),
        ..Default::default()
    };
    world.hear(Inbound::Messages(vec![
        incoming(ANA, "A1", 1_000, text_proto("pass this on")),
        incoming(ANA, "A2", 2_000, once),
    ]));
    let item = |key: &str, message: &str| ForwardItem {
        client_id: ClientMessageId::new(key),
        message: MessageId::new(message),
        to: ChatId::new("+584140000001"),
    };
    let results = world
        .provider
        .forward_messages(
            &world.account,
            &[item("f1", "A1"), item("f2", "A2"), item("f3", "GONE")],
        )
        .await
        .unwrap();
    assert!(results[0].is_ok());
    assert!(
        matches!(&results[1], Err(ProviderError::Rejected { code, .. })
        if code == client_provider::refusal::FORWARD_VIEW_ONCE)
    );
    assert!(matches!(&results[2], Err(ProviderError::Rejected { .. })));

    let sent = world.sent();
    assert_eq!(sent.len(), 1, "a refusal fails its own copy alone");
    assert_eq!(sent[0].0, "584140000001@s.whatsapp.net");
    let context = sent[0]
        .2
        .extended_text_message
        .context_info
        .as_option()
        .unwrap();
    assert_eq!(context.is_forwarded, Some(true));
    // Repeating the same copy is the same message.
    let again = world
        .provider
        .forward_messages(&world.account, &[item("f1", "A1")])
        .await
        .unwrap();
    assert_eq!(
        again[0].as_ref().unwrap().message_id,
        results[0].as_ref().unwrap().message_id
    );
    assert_eq!(world.sent().len(), 1);
}

#[tokio::test]
async fn a_poll_counts_votes_as_they_are_heard_and_the_accounts_own() {
    let world = world().await;
    let mut poll = text(&world, "key-p", "");
    poll.content = OutgoingContent::Poll {
        question: "Where?".into(),
        options: vec!["Beach".into(), "City".into()],
        max_choices: 1,
    };
    let receipt = world.provider.send(poll).await.unwrap();
    world.hear(Inbound::PollVote {
        chat: jid(ANA),
        poll: receipt.message_id.to_string(),
        voter: jid(ANA),
        mine: false,
        hashes: vec![option_hash("Beach")],
        timestamp_ms: 5_000,
    });
    let voted = world
        .provider
        .vote_poll(
            &world.account,
            &ChatId::new(ANA_ID),
            &receipt.message_id,
            &["City".to_owned()],
        )
        .await
        .unwrap()
        .expect("the poll with its tally");
    let MessageContent::Poll(tally) = voted.content else {
        panic!("a poll");
    };
    assert_eq!(
        tally.options.iter().map(|o| o.votes).collect::<Vec<_>>(),
        [1, 1]
    );
    assert_eq!(tally.chosen, Some(vec!["City".to_owned()]));
    assert_eq!(tally.voters, 2);
}

#[tokio::test]
async fn a_chats_state_is_changed_here_and_remembered() {
    // The session of these tests refuses chat changes; this provider
    // reports that as what it is.
    let world = world().await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A1",
        1_000,
        text_proto("hi"),
    )]));
    let error = world
        .provider
        .update_chat(
            &world.account,
            &ChatId::new(ANA_ID),
            ChatChange::Pinned(true),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Unsupported(_)));
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert!(
        !chats.items[0].pinned,
        "a change that was refused is not kept"
    );
}

// ----- stories --------------------------------------------------------------

#[tokio::test]
async fn a_contacts_story_is_listed_seen_once_and_answered_by_quoting_it() {
    let world = world().await;
    let mut story = incoming(
        "status@broadcast",
        "S1",
        client_provider::Timestamp::now().as_millis(),
        text_proto("today"),
    );
    story.sender = jid(ANA);
    let mut events = listen(&world).await;
    world.hear(Inbound::Messages(vec![story]));
    let announced = loop {
        match events.next().await.unwrap() {
            ProviderEvent::StoryUpserted(story) => break story,
            _ => continue,
        }
    };
    assert_eq!(announced.author.as_str(), ANA_ID);
    assert!(!announced.mine && !announced.viewed);

    let stories = world.provider.list_stories(&world.account).await.unwrap();
    assert_eq!(stories.len(), 1);
    assert!(
        world
            .provider
            .list_chats(&world.account, None)
            .await
            .unwrap()
            .items
            .is_empty(),
        "the status list is not a chat"
    );

    world
        .provider
        .view_story(&world.account, &stories[0].id, &stories[0].author)
        .await
        .unwrap();
    let read = world.wire.read.lock().unwrap().clone();
    assert_eq!(
        read[0],
        (
            "status@broadcast".to_owned(),
            Some(ANA.to_owned()),
            vec!["S1".to_owned()]
        )
    );
    assert!(world.provider.list_stories(&world.account).await.unwrap()[0].viewed);

    let reply = text(&world, "key-r", "nice");
    world
        .provider
        .reply_to_story(&stories[0], reply)
        .await
        .unwrap();
    let sent = world.sent();
    assert_eq!(sent[0].0, ANA, "a reply goes to the author's chat");
    let context = sent[0]
        .2
        .extended_text_message
        .context_info
        .as_option()
        .unwrap();
    assert_eq!(context.remote_jid.as_deref(), Some("status@broadcast"));
    assert_eq!(context.stanza_id.as_deref(), Some("S1"));
    let stored = history(&world).await;
    assert_eq!(
        stored[0]
            .extras
            .story_reply
            .as_ref()
            .unwrap()
            .story
            .as_str(),
        "S1"
    );
}

// ----- calls ----------------------------------------------------------------

#[tokio::test]
async fn a_call_nobody_answered_is_a_line_in_the_chat() {
    let world = world().await;
    let mut events = listen(&world).await;
    world.hear(Inbound::Call {
        id: "CALL1".into(),
        peer: jid(ANA),
        signal: CallSignal::Ringing {
            video: false,
            group: false,
        },
        timestamp_ms: 1_000,
    });
    let Some(ProviderEvent::Call(ringing)) = events.next().await else {
        panic!("the call is announced");
    };
    assert_eq!(ringing.chat_id.as_str(), ANA_ID);
    assert_eq!(
        ringing.state,
        CallState::Ringing {
            incoming: true,
            group: false
        }
    );
    world.hear(Inbound::Call {
        id: "CALL1".into(),
        peer: jid(ANA),
        signal: CallSignal::Ended {
            missed: true,
            reason: None,
        },
        timestamp_ms: 2_000,
    });
    let stored = history(&world).await;
    assert!(matches!(&stored[0].content, MessageContent::System(notice)
        if notice.kind == SystemKind::MissedVoiceCall));
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert_eq!(chats.items[0].unread_count, 1);
}

#[tokio::test]
async fn a_call_that_took_place_is_a_line_in_the_chat_that_says_how_it_went() {
    let world = world().await;
    let hear = |id: &str, signal: CallSignal, at: i64| {
        world.hear(Inbound::Call {
            id: id.into(),
            peer: jid(ANA),
            signal,
            timestamp_ms: at,
        });
    };
    let ringing = || CallSignal::Ringing {
        video: true,
        group: false,
    };
    let over = || CallSignal::Ended {
        missed: false,
        reason: None,
    };
    // Answered, and on for a minute and five seconds.
    hear("TALK", ringing(), 1_000);
    hear("TALK", CallSignal::Connected, 5_000);
    hear("TALK", over(), 70_000);
    // Declined: it rang and was ended without being answered.
    hear("NOPE", ringing(), 80_000);
    hear("NOPE", over(), 82_000);
    // Said twice, as events are: one line each.
    hear("NOPE", over(), 83_000);

    let stored = history(&world).await;
    let lines: Vec<(SystemKind, Option<String>)> = stored
        .iter()
        .filter_map(|message| match &message.content {
            MessageContent::System(notice) => Some((notice.kind, notice.detail.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines.contains(&(SystemKind::VideoCall, Some("Incoming · 1:05".into()))));
    assert!(lines.contains(&(SystemKind::VideoCall, Some("Incoming · declined".into()))));
    // A call that took place is nothing to read.
    let chats = world
        .provider
        .list_chats(&world.account, None)
        .await
        .unwrap();
    assert_eq!(chats.items[0].unread_count, 0);
}

#[tokio::test]
async fn declining_a_call_tells_whatsapp_and_ending_one_that_is_over_is_fine() {
    let world = world().await;
    world.hear(Inbound::Call {
        id: "CALL2".into(),
        peer: jid(ANA),
        signal: CallSignal::Ringing {
            video: false,
            group: false,
        },
        timestamp_ms: 1_000,
    });
    world
        .provider
        .accept_call(&world.account, "CALL2")
        .await
        .unwrap();
    world.hear(Inbound::Call {
        id: "CALL2".into(),
        peer: jid(ANA),
        signal: CallSignal::Connected,
        timestamp_ms: 1_500,
    });
    world
        .provider
        .end_call(&world.account, "CALL2")
        .await
        .unwrap();
    assert_eq!(*world.wire.ended_calls.lock().unwrap(), ["CALL2"]);
    world.hear(Inbound::Call {
        id: "CALL2".into(),
        peer: jid(ANA),
        signal: CallSignal::Ended {
            missed: true,
            reason: None,
        },
        timestamp_ms: 3_000,
    });
    // It was answered: not a missed call, whatever the last signal says.
    // It is a call that took place, and says for how long.
    let stored = history(&world).await;
    assert_eq!(stored.len(), 1);
    assert!(matches!(&stored[0].content, MessageContent::System(notice)
        if notice.kind == SystemKind::VoiceCall
            && notice.detail.as_deref() == Some("Incoming · 0:01")));
    world
        .provider
        .end_call(&world.account, "CALL2")
        .await
        .unwrap();
    assert_eq!(world.wire.ended_calls.lock().unwrap().len(), 1);
}

// ----- what only the real pieces can say ------------------------------------

#[tokio::test]
async fn a_vote_cast_on_a_phone_is_opened_with_the_polls_secret() {
    use whatsapp_rust::wacore::poll::encrypt_poll_vote_with_secret;
    let world = world().await;
    let mut poll = text(&world, "key-p", "");
    poll.content = OutgoingContent::Poll {
        question: "Where?".into(),
        options: vec!["Beach".into(), "City".into()],
        max_choices: 1,
    };
    let receipt = world.provider.send(poll).await.unwrap();
    let sent = world.sent();
    let secret = convert::poll_secret(&sent[0].2).expect("a poll carries its secret");

    // Ana's phone encrypts her vote for the poll's maker and herself.
    let (payload, iv) = encrypt_poll_vote_with_secret(
        &[option_hash("Beach")],
        &secret,
        receipt.message_id.as_str(),
        ME,
        ANA,
    )
    .unwrap();
    let vote = wa::Message {
        poll_update_message: MessageField::some(wa::message::PollUpdateMessage {
            poll_creation_message_key: MessageField::some(wa::MessageKey {
                id: Some(receipt.message_id.to_string()),
                ..Default::default()
            }),
            vote: MessageField::some(wa::message::PollEncValue {
                enc_payload: Some(payload),
                enc_iv: Some(iv.to_vec()),
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    world.hear(Inbound::Messages(vec![incoming(ANA, "V1", 9_000, vote)]));

    let stored = history(&world).await;
    assert_eq!(stored.len(), 1, "a vote is not a message of its own");
    let MessageContent::Poll(tally) = &stored[0].content else {
        panic!("a poll");
    };
    assert_eq!(
        tally.options.iter().map(|o| o.votes).collect::<Vec<_>>(),
        [1, 0]
    );
    assert_eq!(tally.voters, 1);
    assert_eq!(tally.chosen, None, "the account itself has not voted");
}

#[tokio::test]
async fn the_sessions_keys_are_never_written_plain() {
    use crate::live::LiveSessions;
    let dir = std::env::temp_dir().join(format!("native-sessions-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sessions.db");
    let key = "ab".repeat(32);
    drop(LiveSessions::open(&path, Some(key.clone())).await.unwrap());

    let head = std::fs::read(&path).unwrap();
    assert!(head.len() > 16);
    assert_ne!(&head[..15], b"SQLite format 3", "the file is encrypted");
    assert!(LiveSessions::open(&path, Some(key)).await.is_ok());
    assert!(
        LiveSessions::open(&path, Some("cd".repeat(32)))
            .await
            .is_err(),
        "another key does not open it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn whoever_starts_listening_is_told_where_each_number_stands() {
    let world = world().await;
    // The number connected before anybody listened.
    let mut events = world.provider.subscribe().await.unwrap();
    assert_eq!(
        events.next().await,
        Some(ProviderEvent::ConnectionChanged {
            account_id: world.account.clone(),
            state: ConnectionState::Connected,
        })
    );
}

#[tokio::test]
async fn a_code_that_could_not_be_asked_yet_is_asked_when_the_request_is_repeated() {
    let wire = Arc::new(Wire::default());
    let provider = provider_on(&wire);
    // Linked by scanning first: the request made a number showing a QR code.
    let scanned = provider.create_account(&new_account()).await.unwrap();
    provider.inner.apply(1, Inbound::Qr("2@abc".into()));
    // The same request again, now with a phone to type the code on.
    let coded = provider
        .create_account(&NewAccount {
            pairing_phone: Some("+58 412 555 0100".into()),
            ..new_account()
        })
        .await
        .unwrap();
    assert_eq!(coded.account.id, scanned.account.id, "one number, not two");
    assert!(matches!(coded.step, LinkStep::TypeCode { .. }));
}

/// The real thing, as far as it goes without a phone: a session is opened
/// against WhatsApp, shows its QR code, and is deleted. Nothing is linked,
/// which is what opening WhatsApp Web and closing it is. It needs the
/// network, so it only runs when asked for:
///
/// ```sh
/// cargo test -p provider-native -- --ignored a_real_session_shows_its_qr_code
/// ```
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "talks to WhatsApp: run it by name"]
async fn a_real_session_shows_its_qr_code() {
    use crate::live::LiveSessions;
    let dir = std::env::temp_dir().join(format!("native-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let key = "5a".repeat(32);
    let sessions = LiveSessions::open(&dir.join("sessions.db"), Some(key.clone()))
        .await
        .unwrap();
    let archive = Archive::open(&dir.join("archive.db"), Some(&key)).unwrap();
    let provider = NativeProvider::new(Arc::new(sessions), archive, &Handle::current());

    let status = provider.create_account(&new_account()).await.unwrap();
    let account = status.account.id.clone();
    let mut step = status.step;
    for _ in 0..80 {
        if matches!(step, LinkStep::Scan { .. } | LinkStep::Stopped { .. }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        step = provider.link_status(&account).await.unwrap().step;
    }
    let LinkStep::Scan { png } = step else {
        panic!("no QR code after forty seconds: {step:?}");
    };
    assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    let shown = dir.join("qr.png");
    std::fs::write(&shown, &png).unwrap();
    println!("the QR code is {} bytes, at {}", png.len(), shown.display());

    provider.delete_account(&account).await.unwrap();
    assert!(provider.list_accounts().await.unwrap().is_empty());
    let _ = std::fs::remove_file(dir.join("sessions.db"));
}

// ----- under the client's own engine ----------------------------------------

/// Waits for something the engine does on its own tasks.
async fn until(what: &str, done: impl Fn() -> bool) {
    for _ in 0..400 {
        if done() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the engine never got to: {what}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_clients_engine_runs_on_this_provider() {
    use client_core::{Store, SyncConfig, SyncEngine};
    let world = world().await;
    world.hear(Inbound::Messages(vec![incoming(
        ANA,
        "A1",
        client_provider::Timestamp::now().as_millis() - 60_000,
        text_proto("hi"),
    )]));
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(world.provider.clone()),
        SyncConfig::default(),
        Handle::current(),
    );
    engine.start();
    let (account, chat) = (world.account.clone(), ChatId::new(ANA_ID));

    // The chat list is the provider's own: the chat is there with its
    // unread message before any history is read.
    until("list the chat", || {
        store.chat(&account, &chat).ok().flatten().is_some()
    })
    .await;
    let listed = store.chat(&account, &chat).unwrap().unwrap();
    assert_eq!(listed.title, "Ana P.");
    assert_eq!(listed.unread_count, 1);

    // A text goes out through the outbox, once, and its bubble takes the
    // provider's id and status.
    let key = engine.send_text(&account, &chat, "hello", None).unwrap();
    until("send the text", || world.sent().len() == 1).await;
    let wire_id = world.sent()[0].1.clone();
    let sent_row = || {
        store
            .messages(&account, &chat, 50)
            .unwrap()
            .into_iter()
            .find(|row| row.message.client_id.as_ref() == Some(&key))
    };
    until("settle the bubble", || {
        sent_row().is_some_and(|row| row.message.status == DeliveryStatus::Sent)
    })
    .await;
    assert_eq!(sent_row().unwrap().message.id.as_str(), wire_id);

    // The copy WhatsApp hands back, and a receipt, land on that bubble.
    let mut echo = incoming(
        ANA,
        &wire_id,
        client_provider::Timestamp::now().as_millis(),
        text_proto("hello"),
    );
    echo.from_me = true;
    echo.sender = jid(ME);
    world.hear(Inbound::Messages(vec![echo]));
    world.hear(Inbound::Receipt {
        chat: jid(ANA),
        from: jid(ANA),
        ids: vec![wire_id.clone()],
        level: ReceiptLevel::Read,
        timestamp_ms: client_provider::Timestamp::now().as_millis(),
    });
    until("move the ticks", || {
        sent_row().is_some_and(|row| row.message.status == DeliveryStatus::Read)
    })
    .await;
    let outgoing = store
        .messages(&account, &chat, 50)
        .unwrap()
        .into_iter()
        .filter(|row| row.message.direction == Direction::Outgoing)
        .count();
    assert_eq!(outgoing, 1, "one bubble, whichever copy came");
    assert_eq!(world.sent().len(), 1, "and one message on WhatsApp");
    engine.shutdown();
}
