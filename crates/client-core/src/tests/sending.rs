//! From Enter to the ticks: what the bubble of a message sent from here
//! shows at each step, that it stays one bubble whichever way the
//! provider's copy arrives, and that the times are written down.

use super::*;
use async_trait::async_trait;
use client_provider::{
    Capabilities, Cursor, EventStream, MediaData, MediaRef, Page, ProviderResult, SendReceipt,
};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::sync::{Notify, Semaphore};

/// A provider that only takes messages, and answers each send with what
/// the test queued. With a gate, a send waits there after it was counted,
/// like a request whose answer is slow to come.
#[derive(Default)]
struct Queueing {
    answers: Mutex<VecDeque<ProviderResult<SendReceipt>>>,
    calls: AtomicUsize,
    gate: Option<Arc<Semaphore>>,
    entered: Arc<Notify>,
}

impl Queueing {
    fn answering(answers: impl IntoIterator<Item = ProviderResult<SendReceipt>>) -> Self {
        Self {
            answers: Mutex::new(answers.into_iter().collect()),
            ..Default::default()
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

fn receipt(id: &str, status: DeliveryStatus) -> ProviderResult<SendReceipt> {
    Ok(SendReceipt {
        message_id: MessageId::new(id),
        status,
        timestamp: None,
    })
}

#[async_trait]
impl Provider for Queueing {
    fn id(&self) -> &'static str {
        "queueing"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::none()
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        Ok(Vec::new())
    }

    async fn list_chats(&self, _: &AccountId, _: Option<Cursor>) -> ProviderResult<Page<Chat>> {
        Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        })
    }

    async fn fetch_messages(
        &self,
        _: &AccountId,
        _: &ChatId,
        _: Option<Cursor>,
        _: u32,
    ) -> ProviderResult<Page<Message>> {
        Ok(Page {
            items: Vec::new(),
            next_cursor: None,
        })
    }

    async fn send(&self, _: OutgoingMessage) -> ProviderResult<SendReceipt> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            self.entered.notify_one();
            gate.acquire().await.expect("the gate stays").forget();
        }
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("the test queued an answer for every send")
    }

    async fn mark_read(
        &self,
        _: &AccountId,
        _: &ChatId,
        _: Option<&MessageId>,
    ) -> ProviderResult<()> {
        Ok(())
    }

    async fn download_media(&self, _: &AccountId, _: &MediaRef) -> ProviderResult<MediaData> {
        Err(ProviderError::Unsupported("media"))
    }

    async fn subscribe(&self) -> ProviderResult<EventStream> {
        Err(ProviderError::Unsupported("events"))
    }
}

fn seeded() -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    store
}

/// The bubbles of the chat: (id, status).
fn bubbles(store: &Store) -> Vec<(String, DeliveryStatus)> {
    store
        .messages(&account(), &chat_id(), 100)
        .unwrap()
        .into_iter()
        .map(|m| (m.message.id.to_string(), m.message.status))
        .collect()
}

/// The provider's copy of a message sent from here, as an event or a page
/// of history brings it.
fn copy(id: &str, client_id: &str, body: &str, status: DeliveryStatus) -> Message {
    Message {
        client_id: Some(ClientMessageId::new(client_id)),
        status,
        ..text(
            id,
            "chat",
            Timestamp::now().as_millis(),
            body,
            Direction::Outgoing,
        )
    }
}

/// Everything announced so far, and whether the open chat was told.
fn told_the_chat(listener: &mut ChangeListener) -> bool {
    let mut told = false;
    while let Some(change) = listener.try_next() {
        told |= change
            == StoreChange::Messages {
                account_id: account(),
                chat_id: chat_id(),
            };
    }
    told
}

#[tokio::test]
async fn a_message_the_provider_queued_leaves_the_clock_at_once_and_follows_its_ticks() {
    let store = seeded();
    let provider = Queueing::answering([receipt("m1", DeliveryStatus::Accepted)]);
    let config = OutboxConfig::default();
    let t0 = Timestamp::now();
    store
        .enqueue(&outgoing("c1", "Esta bien."), t0, HOUR)
        .unwrap();
    assert_eq!(
        bubbles(&store),
        [("local:c1".to_owned(), DeliveryStatus::Pending)],
        "the clock: it has not left this computer"
    );
    let mut listener = store.subscribe();

    // The provider answers that it has the message, queued.
    run_outbox_pass(&store, &provider, &config, t0)
        .await
        .unwrap();
    assert_eq!(
        bubbles(&store),
        [("m1".to_owned(), DeliveryStatus::Accepted)],
        "the same row, under the provider's id, no longer pending"
    );
    assert!(told_the_chat(&mut listener), "the view hears of it");

    // Each later status is a change of the status alone, and each one is
    // announced to whoever shows the chat.
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(Queueing::default()),
        SyncConfig::default(),
        Handle::current(),
    );
    for status in [
        DeliveryStatus::Sent,
        DeliveryStatus::Delivered,
        DeliveryStatus::Read,
    ] {
        engine
            .apply_event(ProviderEvent::MessageStatusChanged {
                account_id: account(),
                chat_id: chat_id(),
                message_id: MessageId::new("m1"),
                status: status.clone(),
            })
            .unwrap();
        assert_eq!(bubbles(&store), [("m1".to_owned(), status.clone())]);
        assert!(told_the_chat(&mut listener), "{status:?} is announced");
    }
    // The same status again, or an older one, changes and announces nothing.
    for status in [DeliveryStatus::Read, DeliveryStatus::Accepted] {
        engine
            .apply_event(ProviderEvent::MessageUpserted(copy(
                "m1",
                "c1",
                "Esta bien.",
                status,
            )))
            .unwrap();
    }
    assert_eq!(bubbles(&store), [("m1".to_owned(), DeliveryStatus::Read)]);

    // And the times of it all were written down.
    let timing = store.send_timings(10).unwrap().remove(0);
    assert_eq!(timing.client_id, ClientMessageId::new("c1"));
    assert_eq!(timing.kind, "text");
    assert_eq!(timing.queued_at, t0);
    assert_eq!(timing.attempts, 1);
    assert!(timing.post_started_at.is_some() && timing.post.is_some());
    let moments = [
        timing.accepted_at,
        timing.first_change_at,
        timing.sent_at,
        timing.delivered_at,
        timing.read_at,
    ];
    assert!(moments.iter().all(Option::is_some), "{timing:?}");
    assert!(moments.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(timing.failed_at, None);
    let line = timing.line();
    assert!(line.contains("kind=text") && line.contains("attempts=1"));
    assert!(!line.contains("Esta bien"), "no text in the report: {line}");
}

#[tokio::test]
async fn a_status_that_comes_with_the_whole_message_is_announced_too() {
    let store = seeded();
    let provider = Queueing::answering([receipt("m1", DeliveryStatus::Accepted)]);
    let t0 = Timestamp::now();
    store.enqueue(&outgoing("c1", "hola"), t0, HOUR).unwrap();
    run_outbox_pass(&store, &provider, &OutboxConfig::default(), t0)
        .await
        .unwrap();
    let mut listener = store.subscribe();

    // A poll reports the message again, with nothing new but its status.
    let mut reported = copy("m1", "c1", "hola", DeliveryStatus::Delivered);
    reported.timestamp = store.messages(&account(), &chat_id(), 1).unwrap()[0]
        .message
        .timestamp;
    assert_eq!(store.upsert_message(&reported).unwrap(), Upsert::Updated);
    assert!(told_the_chat(&mut listener));
    assert_eq!(
        bubbles(&store),
        [("m1".to_owned(), DeliveryStatus::Delivered)]
    );
    assert_eq!(store.upsert_message(&reported).unwrap(), Upsert::Unchanged);
    assert!(
        !told_the_chat(&mut listener),
        "nothing changed, nothing said"
    );
    let timing = store.send_timings(1).unwrap().remove(0);
    assert!(timing.sent_at.is_some() && timing.delivered_at.is_some());
    assert_eq!(timing.read_at, None);
}

#[tokio::test]
async fn the_providers_copy_seen_before_its_answer_is_the_same_bubble_and_ends_the_retries() {
    let store = seeded();
    // The request times out here; the provider took it all the same.
    let provider = Queueing::answering([
        Err(ProviderError::Transient("the request timed out".into())),
        receipt("m2", DeliveryStatus::Accepted),
    ]);
    let config = OutboxConfig::default();
    let t0 = Timestamp::now();
    store.enqueue(&outgoing("c1", "first"), t0, HOUR).unwrap();
    let later = Timestamp::from_millis(t0.as_millis() + 1);
    store
        .enqueue(&outgoing("c2", "second"), later, HOUR)
        .unwrap();
    let pass = run_outbox_pass(&store, &provider, &config, later)
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    assert_eq!(provider.calls(), 1, "the second waits behind the first");
    assert_eq!(store.outbox_pending().unwrap().len(), 2);

    // A poll sees the provider's copy before any retry was made.
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(Queueing::default()),
        SyncConfig::default(),
        Handle::current(),
    );
    engine
        .apply_event(ProviderEvent::MessageUpserted(copy(
            "m1",
            "c1",
            "first",
            DeliveryStatus::Sent,
        )))
        .unwrap();
    let shown = bubbles(&store);
    assert_eq!(shown.len(), 2, "one bubble each, not three: {shown:?}");
    assert!(shown.contains(&("m1".to_owned(), DeliveryStatus::Sent)));
    assert!(shown.contains(&("local:c2".to_owned(), DeliveryStatus::Pending)));
    let waiting = store.outbox_pending().unwrap();
    assert_eq!(waiting.len(), 1, "nothing is left to send of the first");
    assert_eq!(waiting[0].message.client_id, ClientMessageId::new("c2"));

    // So the next pass does not wait out the first one's backoff: the
    // second goes now, and the first is not asked about again.
    let pass = run_outbox_pass(&store, &provider, &config, later)
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried), (1, 0));
    assert_eq!(provider.calls(), 2);
    let shown = bubbles(&store);
    assert_eq!(shown.len(), 2);
    assert!(shown.contains(&("m2".to_owned(), DeliveryStatus::Accepted)));
    let first = store
        .send_timings(10)
        .unwrap()
        .into_iter()
        .find(|timing| timing.client_id.as_str() == "c1")
        .unwrap();
    assert!(first.accepted_at.is_some() && first.sent_at.is_some());
    assert_eq!(first.post, None, "no request of it was ever answered");
}

#[tokio::test]
async fn an_answer_that_arrives_after_the_providers_copy_changes_nothing() {
    let store = seeded();
    let gate = Arc::new(Semaphore::new(0));
    let provider = Arc::new(Queueing {
        // What a repeat of an accepted request should never answer, and
        // must not be believed if it does.
        answers: Mutex::new(
            [Err(ProviderError::Rejected {
                code: "account_not_ready".into(),
                message: "The account is not connected.".into(),
            })]
            .into(),
        ),
        gate: Some(gate.clone()),
        ..Default::default()
    });
    let t0 = Timestamp::now();
    store.enqueue(&outgoing("c1", "slow"), t0, HOUR).unwrap();

    let entered = provider.entered.clone();
    let pass = {
        let (store, provider) = (store.clone(), provider.clone());
        tokio::spawn(async move {
            run_outbox_pass(&store, provider.as_ref(), &OutboxConfig::default(), t0).await
        })
    };
    // The request is on its way, and the provider's copy overtakes it.
    entered.notified().await;
    store
        .upsert_message(&copy("m1", "c1", "slow", DeliveryStatus::Delivered))
        .unwrap();
    assert_eq!(
        bubbles(&store),
        [("m1".to_owned(), DeliveryStatus::Delivered)]
    );
    gate.add_permits(1);
    pass.await.unwrap().unwrap();

    assert_eq!(
        bubbles(&store),
        [("m1".to_owned(), DeliveryStatus::Delivered)],
        "a message the provider has is not failed by a late answer"
    );
    assert!(store.outbox_pending().unwrap().is_empty());
}

#[tokio::test]
async fn a_slow_answer_keeps_the_clock_and_the_repeat_resolves_the_same_row() {
    let store = seeded();
    let provider = Queueing::answering([
        Err(ProviderError::Transient("the request timed out".into())),
        // The repeat carries the same client id, so the provider answers
        // with the message it already has, as it stands by then.
        receipt("m1", DeliveryStatus::Sent),
    ]);
    let config = OutboxConfig::default();
    let t0 = Timestamp::now();
    store.enqueue(&outgoing("c1", "hola"), t0, HOUR).unwrap();

    run_outbox_pass(&store, &provider, &config, t0)
        .await
        .unwrap();
    assert_eq!(
        bubbles(&store),
        [("local:c1".to_owned(), DeliveryStatus::Pending)],
        "not known to have arrived: still the clock"
    );
    // Not before its backoff, which is short.
    run_outbox_pass(&store, &provider, &config, t0)
        .await
        .unwrap();
    assert_eq!(provider.calls(), 1);
    let retry = Timestamp::from_millis(t0.as_millis() + config.backoff(1).as_millis() as i64);
    assert!(config.backoff(1) <= Duration::from_secs(2));
    run_outbox_pass(&store, &provider, &config, retry)
        .await
        .unwrap();
    assert_eq!(provider.calls(), 2);
    assert_eq!(bubbles(&store), [("m1".to_owned(), DeliveryStatus::Sent)]);
    let timing = store.send_timings(1).unwrap().remove(0);
    assert_eq!(timing.attempts, 2);
}

#[tokio::test]
async fn a_message_the_provider_refuses_shows_failed_with_its_reason() {
    let store = seeded();
    let provider = Queueing::answering([Err(ProviderError::Rejected {
        code: "not_on_whatsapp".into(),
        message: "The recipient has no WhatsApp.".into(),
    })]);
    let t0 = Timestamp::now();
    store.enqueue(&outgoing("c1", "hola"), t0, HOUR).unwrap();
    run_outbox_pass(&store, &provider, &OutboxConfig::default(), t0)
        .await
        .unwrap();
    let shown = bubbles(&store);
    assert!(
        matches!(&shown[0].1, DeliveryStatus::Failed { reason } if reason.contains("no WhatsApp")),
        "{shown:?}"
    );
    // A message the provider had queued can fail later, too.
    let other = seeded();
    let provider = Queueing::answering([receipt("m1", DeliveryStatus::Accepted)]);
    other.enqueue(&outgoing("c1", "hola"), t0, HOUR).unwrap();
    run_outbox_pass(&other, &provider, &OutboxConfig::default(), t0)
        .await
        .unwrap();
    let failed = DeliveryStatus::Failed {
        reason: "The account went offline.".into(),
    };
    assert!(other
        .set_message_status(&account(), &MessageId::new("m1"), &failed)
        .unwrap());
    assert_eq!(bubbles(&other), [("m1".to_owned(), failed)]);
    assert!(other.send_timings(1).unwrap()[0].failed_at.is_some());
}

#[tokio::test]
async fn ticks_that_moved_while_nobody_listened_are_read_again_on_a_refresh() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(2).id;
    // A conversation that was read here, with a message sent from here.
    engine.fetch_latest(&account, &chat).await.unwrap();
    engine
        .send_text(&account, &chat, "are you there?", None)
        .unwrap();
    engine.flush_outbox().await.unwrap();
    let sent = store
        .messages(&account, &chat, 500)
        .unwrap()
        .into_iter()
        .rev()
        .find(|m| m.message.content == MessageContent::text("are you there?"))
        .unwrap()
        .message;
    assert_eq!(sent.status, DeliveryStatus::Sent);
    // Messages arrive after it, so it is not the chat's last message...
    for (n, at) in [1_000, 2_000].into_iter().enumerate() {
        mock.push_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(format!("reply-{n}")),
            client_id: None,
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + at),
            content: MessageContent::text("yes"),
            direction: Direction::Incoming,
            ..sent.clone()
        }));
    }
    engine.fetch_latest(&account, &chat).await.unwrap();
    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    let calls = mock.history_calls();
    // ...and then it is read, while this client was not listening. The
    // chat list has nothing new to say about that chat.
    mock.push_event(ProviderEvent::MessageStatusChanged {
        account_id: account.clone(),
        chat_id: chat.clone(),
        message_id: sent.id.clone(),
        status: DeliveryStatus::Read,
    });
    assert_eq!(
        store.message(&account, &sent.id).unwrap().unwrap().status,
        DeliveryStatus::Sent
    );

    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    assert_eq!(
        store.message(&account, &sent.id).unwrap().unwrap().status,
        DeliveryStatus::Read
    );
    assert!(
        mock.history_calls() > calls,
        "its newest page was read again"
    );
    // Nothing is left to wait for in that chat, so the next refresh does
    // not read it again.
    let calls = mock.history_calls();
    engine.refresh().await.unwrap();
    engine.preload().await.unwrap();
    assert_eq!(mock.history_calls(), calls);
}
