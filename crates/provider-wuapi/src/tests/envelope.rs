//! Reading the envelope of a stream event: strict and lenient decoding,
//! the `evt_` id, the types the client ignores and the recent-id window.

use super::{ACCOUNT, GROUP};
use crate::envelope::{decode, Body, Dirty, EvtLru, Mapped, Mapper};
use crate::events::{PollState, Shared};
use crate::follow::Now;
use crate::sse::{SseItem, SseParser};
use client_provider::{ConnectionState, DeliveryStatus, ProviderEvent};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use wuapi::types as api;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

fn stream_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream")
}

/// The `data:` lines of a fixture with the name of the SSE event they
/// came under, as the parser reads them.
pub(super) fn frames(name: &str) -> Vec<(String, String)> {
    let bytes = std::fs::read(stream_dir().join(name)).expect("the fixture exists");
    SseParser::new(1024 * 1024)
        .feed(&bytes)
        .into_iter()
        .filter_map(|item| match item {
            SseItem::Event { event, data, .. } => Some((event, data)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_fixture_data_line_decodes_strictly() {
    let mut checked = 0;
    let mut types = std::collections::BTreeSet::new();
    for entry in std::fs::read_dir(stream_dir()).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.ends_with(".sse") || name == "older_backend.sse" || name == "undecodable.sse" {
            continue;
        }
        for (event, data) in frames(&name) {
            if event == "reset" {
                continue;
            }
            let strict: Result<api::Event, _> = serde_json::from_str(&data);
            assert!(strict.is_ok(), "{name}: {event}: {strict:?}");
            let envelope: serde_json::Value = serde_json::from_str(&data).unwrap();
            assert_eq!(
                envelope["type"],
                event.as_str(),
                "{name}: SSE name is the type"
            );
            types.insert(event);
            checked += 1;
        }
    }
    assert!(checked >= 30, "the fixtures hold frames: {checked}");
    // `mixed.sse` has one of every mapped type.
    assert_eq!(types.len(), 23, "{types:?}");
}

#[test]
fn older_backend_decodes_leniently() {
    let frames = frames("older_backend.sse");
    assert_eq!(frames.len(), 2);
    for (event, data) in &frames {
        assert!(
            serde_json::from_str::<api::Event>(data).is_err(),
            "{event}: the SDK alone cannot read it"
        );
    }
    let message = decode(&frames[0].1);
    assert_eq!(message.id.as_deref(), Some("evt_old1"));
    match message.body {
        Body::Message(wire) => {
            assert_eq!(wire.id, "m_old_1");
            assert!(!wire.forwarded_many_times, "absent means not forwarded");
        }
        other => panic!("a message was expected: {other:?}"),
    }
    let account = decode(&frames[1].1);
    match account.body {
        Body::Account(wire) => assert_eq!(wire.id, "k57a8m2x9d3f0q1wjh6ypc4n2d7s0vbr"),
        other => panic!("an account was expected: {other:?}"),
    }
}

#[test]
fn unknown_type_ignored() {
    let mixed = frames("mixed.sse");
    let unknown = mixed
        .iter()
        .find(|(event, _)| event == "widget.exploded")
        .expect("mixed.sse holds an unknown type");
    let decoded = decode(&unknown.1);
    assert_eq!(decoded.id.as_deref(), Some("evt_mix24"));
    assert_eq!(decoded.kind, "widget.exploded");
    assert!(matches!(decoded.body, Body::Ignored), "{:?}", decoded.body);

    let call = mixed
        .iter()
        .find(|(event, _)| event == "call.received")
        .unwrap();
    let decoded = decode(&call.1);
    assert_eq!(decoded.kind, "call.received");
    assert!(matches!(decoded.body, Body::Ignored), "{:?}", decoded.body);
}

#[test]
fn evt_id_read_before_typed_decode() {
    // The payload cannot be read, the id still is.
    let (_, data) = &frames("undecodable.sse")[0];
    let decoded = decode(data);
    assert_eq!(decoded.id.as_deref(), Some("evt_bad1"));
    assert_eq!(decoded.kind, "message.received");
    assert!(
        matches!(decoded.body, Body::Broken { .. }),
        "{:?}",
        decoded.body
    );

    // A body that is JSON but not an envelope.
    let decoded = decode(r#"{"id":"evt_x","type":"message.received","data":5}"#);
    assert_eq!(decoded.id.as_deref(), Some("evt_x"));
    assert!(matches!(decoded.body, Body::Broken { .. }));

    // Not JSON at all: no id to read.
    let decoded = decode("{not json");
    assert_eq!(decoded.id, None);
    assert!(matches!(decoded.body, Body::Broken { .. }));
}

#[test]
fn evt_lru_skips_duplicate() {
    let mut lru = EvtLru::new(4096);
    assert!(!lru.seen("evt_1"), "the first time is new");
    assert!(lru.seen("evt_1"), "the second is a replay");
    assert!(!lru.seen("evt_2"), "another id is new");
}

#[test]
fn evt_lru_evicts_past_4096() {
    let mut lru = EvtLru::new(4096);
    for i in 0..4096 {
        assert!(!lru.seen(&format!("evt_{i}")));
    }
    // A hit makes it the most recent.
    assert!(lru.seen("evt_0"));
    // One more than the window: the least recently used (evt_1) goes.
    assert!(!lru.seen("evt_4096"));
    assert!(lru.seen("evt_0"), "touched, so it stayed");
    assert!(lru.seen("evt_2"));
    assert!(lru.seen("evt_4096"));
    assert!(!lru.seen("evt_1"), "evicted, so it is new again");
}

// ----- the mapper ------------------------------------------------------------

/// 2026-09-24 12:00 UTC: after the fixtures' messages, before their
/// stories expire.
fn now() -> Now {
    Now {
        at: tokio::time::Instant::now(),
        epoch_ms: crate::mapping::timestamp("2026-09-24T12:00:00.000Z")
            .unwrap()
            .as_millis(),
    }
}

struct Rig {
    mapper: Mapper,
    state: Arc<Mutex<PollState>>,
    shared: Arc<Shared>,
}

fn new_rig() -> Rig {
    let state = Arc::new(Mutex::new(PollState::default()));
    let shared = Arc::new(Shared::default());
    Rig {
        mapper: Mapper::new(state.clone(), shared.clone(), 4096),
        state,
        shared,
    }
}

fn status_word(status: &DeliveryStatus) -> &'static str {
    match status {
        DeliveryStatus::Pending => "pending",
        DeliveryStatus::Accepted => "accepted",
        DeliveryStatus::Sent => "sent",
        DeliveryStatus::Delivered => "delivered",
        DeliveryStatus::Read => "read",
        DeliveryStatus::Failed { .. } => "failed",
    }
}

pub(super) fn say(event: &ProviderEvent) -> String {
    match event {
        ProviderEvent::MessageUpserted(m) => format!(
            "message {} {}{}{}",
            m.id,
            status_word(&m.status),
            if m.deleted { " deleted" } else { "" },
            if m.edited { " edited" } else { "" },
        ),
        ProviderEvent::ConnectionChanged { state, .. } => match state {
            ConnectionState::Connected => "connection connected".to_owned(),
            ConnectionState::Disconnected { .. } => "connection disconnected".to_owned(),
            other => format!("connection {other:?}"),
        },
        ProviderEvent::StoryUpserted(story) => format!("story {}", story.id),
        ProviderEvent::StoryRemoved { story_id, .. } => format!("story removed {story_id}"),
        ProviderEvent::StoryViewed {
            story_id, viewer, ..
        } => format!(
            "viewed {story_id} {} {}",
            viewer.contact,
            viewer.reaction.as_deref().unwrap_or("-")
        ),
        ProviderEvent::ContactUpdated(contact) => format!("contact {}", contact.id),
        ProviderEvent::GroupChanged { group_id, .. } => format!("group {group_id}"),
        other => format!("{other:?}"),
    }
}

fn mark(mark: &Dirty) -> String {
    match mark {
        Dirty::Chat { chat, .. } => format!("chat {chat}"),
        Dirty::Account(_) => "account".to_owned(),
    }
}

fn said(mapped: &Mapped) -> (Vec<String>, Vec<String>) {
    (
        mapped.events.iter().map(say).collect(),
        mapped.marks.iter().map(mark).collect(),
    )
}

const STORY: &str = "m17d0a9w2sqc7k3v8x1n5ybr6t4hjp2e";
const MARIA: &str = "+584245550199";

#[test]
fn mapper_table_covers_every_type() {
    let mut rig = new_rig();
    let story_removed = format!("story removed {STORY}");
    let story_viewed = format!("viewed {STORY} {MARIA} -");
    let story_reacted = format!("viewed {STORY} {MARIA} \u{2764}\u{fe0f}");
    let story_upserted = format!("story {STORY}");
    let group = format!("group {GROUP}");
    let chat_maria = format!("chat {MARIA}");
    let chat_group = format!("chat {GROUP}");
    let contact = format!("contact {MARIA}");
    let rows: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        (
            "message.received",
            vec!["message m_mix_recv delivered"],
            vec![&chat_maria],
        ),
        ("message.sent", vec!["message m_mix_sent sent"], vec![]),
        (
            "message.delivered",
            vec!["message m_mix_sent delivered"],
            vec![],
        ),
        ("message.read", vec!["message m_mix_sent read"], vec![]),
        (
            "message.failed",
            vec!["message m_mix_failed failed"],
            vec![],
        ),
        (
            "message.deleted",
            vec!["message m_mix_deleted delivered deleted"],
            vec![],
        ),
        (
            "message.media_downloaded",
            vec!["message m_mix_media delivered"],
            vec![],
        ),
        (
            "message.edited",
            vec!["message m_mix_edited delivered edited"],
            vec![],
        ),
        (
            "account.disconnected",
            vec!["connection disconnected"],
            vec![],
        ),
        ("account.connected", vec!["connection connected"], vec![]),
        ("story.received", vec![&story_upserted], vec![]),
        ("story.viewed", vec![&story_viewed], vec![]),
        ("story.reacted", vec![&story_reacted], vec![]),
        ("story.deleted", vec![&story_removed], vec![]),
        ("contact.updated", vec![&contact], vec![]),
        ("group.updated", vec![&group], vec![&chat_group]),
        ("chat.updated", vec![], vec![&chat_maria]),
        ("contact.picture_updated", vec![], vec![&chat_maria]),
        ("group.joined", vec![], vec![&chat_group]),
        ("history.synced", vec![], vec!["account"]),
        ("poll.voted", vec!["message m_poll delivered"], vec![]),
        // The second vote has no poll message: no read by id.
        ("poll.voted", vec![], vec![]),
        ("call.received", vec![], vec![]),
        ("widget.exploded", vec![], vec![]),
    ];
    let frames = frames("mixed.sse");
    assert_eq!(frames.len(), rows.len(), "one row per frame");
    for ((event, data), (name, events, marks)) in frames.iter().zip(rows) {
        assert_eq!(event, name);
        let mapped = rig.mapper.map(data, now());
        let expected = (
            events.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            marks.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        );
        assert_eq!(said(&mapped), expected, "{name}");
    }
}

#[test]
fn ticks_emit_three_upserts_no_rest() {
    let mut rig = new_rig();
    let mut said_all = Vec::new();
    for (_, data) in frames("ticks.sse") {
        let mapped = rig.mapper.map(&data, now());
        assert!(mapped.marks.is_empty(), "no chat read for a tick");
        said_all.extend(mapped.events.iter().map(say));
    }
    assert_eq!(
        said_all,
        [
            "message m_tick sent",
            "message m_tick delivered",
            "message m_tick read"
        ]
    );
}

#[test]
fn edit_sets_edited_flag() {
    let mut rig = new_rig();
    let (_, data) = frames("mixed.sse")
        .into_iter()
        .find(|(event, _)| event == "message.edited")
        .unwrap();
    let mapped = rig.mapper.map(&data, now());
    assert_eq!(said(&mapped).0, ["message m_mix_edited delivered edited"]);

    // The envelope may leave `editedAt` out: the type says it was edited.
    let mut rig = new_rig();
    let mut value: serde_json::Value = serde_json::from_str(&data).unwrap();
    value["data"]["object"]["editedAt"] = serde_json::Value::Null;
    let mapped = rig.mapper.map(&value.to_string(), now());
    assert_eq!(said(&mapped).0, ["message m_mix_edited delivered edited"]);
}

#[test]
fn replayed_evt_is_mapped_once() {
    let mut rig = new_rig();
    let frames = frames("duplicate.sse");
    assert_eq!(frames.len(), 2);
    assert_eq!(said(&rig.mapper.map(&frames[0].1, now())).0.len(), 1);
    let again = rig.mapper.map(&frames[1].1, now());
    assert!(again.events.is_empty() && again.marks.is_empty());
}

#[test]
fn account_event_names_its_account() {
    let mut rig = new_rig();
    let (_, data) = frames("mixed.sse")
        .into_iter()
        .find(|(event, _)| event == "account.connected")
        .unwrap();
    match rig.mapper.map(&data, now()).events.as_slice() {
        [ProviderEvent::ConnectionChanged { account_id, .. }] => {
            assert_eq!(account_id.as_str(), ACCOUNT)
        }
        other => panic!("{other:?}"),
    }
}

// ----- one version, said once --------------------------------------------------

/// The same envelope under another `evt_` id: not a replay, so only the
/// version dedupe can stop it.
fn retag(data: &str, evt: &str) -> String {
    let mut value: serde_json::Value = serde_json::from_str(data).unwrap();
    value["id"] = evt.into();
    value.to_string()
}

fn tick(index: usize) -> String {
    frames("ticks.sse")[index].1.clone()
}

#[test]
fn pushed_same_version_skipped() {
    let mut rig = new_rig();
    assert_eq!(said(&rig.mapper.map(&tick(0), now())).0.len(), 1);
    let again = rig.mapper.map(&retag(&tick(0), "evt_other"), now());
    assert!(again.events.is_empty(), "{:?}", again.events);
    // A different version of it is news.
    assert_eq!(said(&rig.mapper.map(&tick(1), now())).0.len(), 1);
}

#[test]
fn pushed_older_updated_at_skipped() {
    let mut rig = new_rig();
    // `read` (08:16:05) first, then a replayed `delivered` (08:16:02).
    assert_eq!(
        said(&rig.mapper.map(&tick(2), now())).0,
        ["message m_tick read"]
    );
    let behind = rig.mapper.map(&tick(1), now());
    assert!(behind.events.is_empty(), "{:?}", behind.events);
    // It did not forget what it knew: `read` again is still not news.
    assert!(rig
        .mapper
        .map(&retag(&tick(2), "evt_again"), now())
        .events
        .is_empty());
    // A message the poll has seen is not said by a replay behind it.
    let mut rig = new_rig();
    let wire: api::Message = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(&tick(2)).unwrap()["data"]["object"].clone(),
    )
    .unwrap();
    rig.state.lock().unwrap().observe_messages(&[wire]);
    assert!(rig.mapper.map(&tick(1), now()).events.is_empty());
}

#[test]
fn stream_then_poll_emits_once() {
    let mut rig = new_rig();
    assert_eq!(said(&rig.mapper.map(&tick(1), now())).0.len(), 1);
    let wire: api::Message = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(&tick(1)).unwrap()["data"]["object"].clone(),
    )
    .unwrap();
    let (polled, _) = rig.state.lock().unwrap().observe_messages(&[wire]);
    assert!(polled.is_empty(), "the poll has nothing to add: {polled:?}");
}

#[test]
fn poll_then_stream_emits_once() {
    let mut rig = new_rig();
    let wire: api::Message = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(&tick(1)).unwrap()["data"]["object"].clone(),
    )
    .unwrap();
    let (polled, _) = rig.state.lock().unwrap().observe_messages(&[wire]);
    assert_eq!(polled.len(), 1);
    assert!(rig.mapper.map(&tick(1), now()).events.is_empty());
    // The next version is still news to the stream.
    assert_eq!(said(&rig.mapper.map(&tick(2), now())).0.len(), 1);
}

#[test]
fn follow_list_retired_by_push() {
    let mut rig = new_rig();
    let at = now();
    // The client sent it: the poller would follow it.
    let queued: api::Message = parse_message("m_tick", "queued", "2026-09-24T08:15:40.000Z");
    let close = Now {
        epoch_ms: crate::mapping::timestamp(&queued.created_at)
            .unwrap()
            .as_millis()
            + 5_000,
        ..at
    };
    rig.shared.follow().accept(&queued, close);
    assert!(rig.shared.follow().watches("m_tick"));
    // `delivered` is pushed: still not read, still watched.
    rig.mapper.map(&tick(1), close);
    assert!(rig.shared.follow().watches("m_tick"));
    // `read` is the end of its ticks: nothing left to ask about.
    rig.mapper.map(&tick(2), close);
    assert!(!rig.shared.follow().watches("m_tick"));
}

fn parse_message(id: &str, status: &str, updated_at: &str) -> api::Message {
    let mut value: serde_json::Value = serde_json::from_str(fixture!("message")).unwrap();
    value["id"] = id.into();
    value["status"] = status.into();
    value["updatedAt"] = updated_at.into();
    serde_json::from_value(value).unwrap()
}

// ----- what cannot be read ----------------------------------------------------

#[test]
fn undecodable_message_marks_chat_dirty_no_upsert() {
    let mut rig = new_rig();
    let (_, data) = &frames("undecodable.sse")[0];
    let mapped = rig.mapper.map(data, now());
    assert!(mapped.events.is_empty(), "{:?}", mapped.events);
    assert_eq!(
        mapped.marks,
        [Dirty::Chat {
            account: ACCOUNT.to_owned(),
            chat: "+584241119999".to_owned()
        }]
    );
    // The same envelope again is a replay: the chat is already marked.
    let replay = rig.mapper.map(data, now());
    assert!(replay.marks.is_empty());
    // Only a message's chat is worth a read: a broken story marks nothing.
    let broken = r#"{"id":"evt_s","type":"story.received","data":{"object":{"id":5}}}"#;
    assert!(rig.mapper.map(broken, now()).marks.is_empty());
}

/// Collects what is logged while a closure runs.
struct Capture(Arc<Mutex<String>>);

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        struct Fields<'a>(&'a mut String);
        impl tracing::field::Visit for Fields<'_> {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                let _ = write!(self.0, "{}={value:?} ", field.name());
            }
        }
        let mut line = String::new();
        event.record(&mut Fields(&mut line));
        let mut log = self.0.lock().unwrap();
        log.push_str(&line);
        log.push('\n');
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[test]
fn no_payload_in_log() {
    let log = Arc::new(Mutex::new(String::new()));
    let (_, undecodable) = frames("undecodable.sse").remove(0);
    let story = r#"{"id":"evt_s","type":"story.received","data":{"object":{"id":"secret-story","text":7}}}"#;
    let not_json = r#"{"id":"evt_n","type":"message.sent","data":secret words"#;
    let mut rig = new_rig();
    tracing::subscriber::with_default(Capture(log.clone()), || {
        for data in [undecodable.as_str(), story, not_json] {
            rig.mapper.map(data, now());
        }
    });
    let log = log.lock().unwrap().clone();
    assert!(
        log.contains("message.received"),
        "something was logged: {log:?}"
    );
    for payload in [
        "12345",
        "m_bad_1",
        "+584241119999",
        "evt_bad1",
        "Your order has shipped",
        "secret",
    ] {
        assert!(!log.contains(payload), "{payload} is in the log: {log}");
    }
}

#[test]
fn broken_reasons_name_the_kind_of_fault_and_quote_nothing() {
    let why = |data: &str| match decode(data).body {
        Body::Broken { why, .. } => why,
        other => panic!("{other:?}"),
    };
    // A wrong type, with the value left out.
    let (_, undecodable) = frames("undecodable.sse").remove(0);
    assert_eq!(why(&undecodable), "invalid type, expected a string");
    // A missing field, named as the schema names it.
    let envelope = r#"{"id":"evt_m","type":"account.connected","data":{"object":{"object":"account","id":"x"}}}"#;
    assert!(
        why(envelope).starts_with("missing field `"),
        "{}",
        why(envelope)
    );
    // Not JSON.
    assert_eq!(why("{nope"), "not valid JSON");
}
