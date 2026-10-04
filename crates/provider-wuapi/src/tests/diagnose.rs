//! The stream report, against a stand-in server on a real socket.

use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
use crate::provider::WuapiProvider;
use crate::tests::envelope::frames;
use crate::tests::fake_stream::{write, FakeStream, Script, Step};
use crate::tests::KEY;
use std::time::Duration;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// A provider on the stand-in, listening for 400 ms instead of 20 s.
fn provider(base: &str) -> WuapiProvider {
    let config = WuapiConfig {
        base_url: base.to_owned(),
        live: LiveTransport::Auto,
        tuning: StreamTuning {
            diagnose_window: ms(400),
            ..StreamTuning::default()
        },
        ..WuapiConfig::new("test-agent/1.0")
    };
    WuapiProvider::new(config, ApiKey::new(KEY)).unwrap()
}

fn ticks() -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream/ticks.sse"),
    )
    .unwrap()
}

#[tokio::test]
async fn diagnose_stream_healthy() {
    // `retry:`, a ping with a cursor, then the three ticks of one message.
    let fake = FakeStream::start(vec![Script::stream(vec![
        write("retry: 3000\n\n"),
        Step::Sleep(ms(30)),
        write(": ping\nid: c1.abc\n\n"),
        write(&ticks()),
        Step::Hang,
    ])])
    .await;
    let lines = provider(&fake.uri()).diagnose_stream().await.unwrap();
    assert!(lines[0].starts_with("sdk wuapi "), "{lines:?}");
    assert_eq!(lines[1], format!("api {}", fake.uri()));
    assert_eq!(lines[2], format!("stream {}/v1/events/stream", fake.uri()));
    assert_eq!(lines[3], "live auto");
    assert!(
        lines[4].starts_with("connect status=200 content_type=text/event-stream took_ms="),
        "{lines:?}"
    );
    assert_eq!(lines[5], "retry 3000");
    assert!(lines[6].starts_with("ping within_ms="), "{lines:?}");
    assert_eq!(lines[7], "cursor seen=true");
    assert_eq!(lines[8], "events 3 window_ms=400");
    assert_eq!(
        &lines[9..],
        [
            "event message.delivered x1",
            "event message.read x1",
            "event message.sent x1"
        ]
    );
    // It asked once, without a cursor.
    let heads = fake.heads();
    assert_eq!(heads.len(), 1);
    assert!(!heads[0].to_lowercase().contains("last-event-id"));
}

#[tokio::test]
async fn diagnose_stream_no_ping() {
    let fake = FakeStream::start(vec![Script::stream(vec![Step::Hang])]).await;
    let lines = provider(&fake.uri()).diagnose_stream().await.unwrap();
    assert!(lines.contains(&"ping none".to_owned()), "{lines:?}");
    assert!(lines.contains(&"cursor seen=false".to_owned()), "{lines:?}");
    assert!(
        lines.contains(&"events 0 window_ms=400".to_owned()),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|line| line.starts_with("retry ")));
}

#[tokio::test]
async fn diagnose_stream_refusal_429_shows_retry_after() {
    let fake = FakeStream::start(vec![Script::answer(
        429,
        "application/json",
        r#"{"code":"stream_connection_limit","message":"This organization has reached its limit."}"#,
    )
    .header("Retry-After", "30")])
    .await;
    let lines = provider(&fake.uri()).diagnose_stream().await.unwrap();
    assert!(
        lines
            .contains(&"connect status=429 code=stream_connection_limit retry_after=30".to_owned()),
        "{lines:?}"
    );
    // A refusal is the whole story: nothing was listened to.
    assert!(
        !lines.iter().any(|line| line.starts_with("events ")),
        "{lines:?}"
    );
}

#[tokio::test]
async fn diagnose_stream_without_an_answer_is_transient() {
    // Nothing listens there.
    let lines = provider("http://127.0.0.1:9")
        .diagnose_stream()
        .await
        .unwrap();
    assert!(
        lines.contains(&"connect status=none code=transient".to_owned()),
        "{lines:?}"
    );
}

#[tokio::test]
async fn diagnose_stream_privacy_no_key_id_cursor_payload() {
    let all: String = ["live.sse", "duplicate.sse", "mixed.sse"]
        .iter()
        .map(|name| {
            std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/stream")
                    .join(name),
            )
            .unwrap()
        })
        .collect();
    let fake = FakeStream::start(vec![
        Script::stream(vec![
            write(&all),
            write(": ping\nid: c1.secretcursor\n\n"),
            Step::Hang,
        ]),
        Script::answer(
            429,
            "application/json",
            r#"{"code":"rate_limited","message":"Words of the API about alice@example.com."}"#,
        ),
    ])
    .await;
    let provider = provider(&fake.uri());
    let mut report = provider.diagnose_stream().await.unwrap().join("\n");
    report.push('\n');
    report.push_str(&provider.diagnose_stream().await.unwrap().join("\n"));
    assert!(report.contains("event message.received"), "{report}");
    // The envelopes carry text, names, numbers and ids.
    let mut private = vec![
        KEY,
        "wu_live",
        "evt_",
        "c1.",
        "secretcursor",
        "alice@example.com",
        "Words of the API",
        "Authorization",
        "Bearer",
    ];
    for (_, data) in frames("mixed.sse") {
        let value: serde_json::Value = serde_json::from_str(&data).unwrap();
        if let Some(text) = value["data"]["object"]["text"].as_str() {
            private.push(Box::leak(text.to_owned().into_boxed_str()));
        }
    }
    for word in private {
        assert!(!report.contains(word), "{word} leaked: {report}");
    }
}
