//! Apply-time check for the stream link (design 3, Open Questions): the
//! link polls `Response::chunk()` under a `select!` against timers, so a
//! dropped `chunk()` future must lose nothing.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn chunk_future_dropped_under_select_loses_no_data() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    // Many small writes with gaps longer than the timer below, so the
    // `chunk()` future is dropped over and over, often with a part of a
    // frame already in flight.
    let sent: Vec<u8> = (0..60)
        .flat_map(|i| format!("id: c{i}\ndata: {{\"n\":{i},\"s\":\"é✓\"}}\n\n").into_bytes())
        .collect();
    let to_send = sent.clone();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request first: closing with it unread would reset the
        // connection and cut the body short.
        let mut head = [0u8; 1024];
        let read = socket.read(&mut head).await.unwrap();
        assert!(read > 0, "the client sent its request");
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        for piece in to_send.chunks(7) {
            socket.write_all(piece).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        // Closing ends the close-delimited body.
        socket.shutdown().await.unwrap();
    });

    let mut response = reqwest::Client::new()
        .get(format!("http://{address}/"))
        .send()
        .await
        .unwrap();
    let mut got = Vec::new();
    let mut drops = 0u32;
    loop {
        tokio::select! {
            chunk = response.chunk() => match chunk.unwrap() {
                Some(bytes) => got.extend_from_slice(&bytes),
                None => break,
            },
            // Always shorter than the writer's gap: the loser is dropped.
            () = tokio::time::sleep(Duration::from_millis(1)) => drops += 1,
        }
    }
    assert!(drops > 20, "the chunk future was really dropped: {drops}");
    assert_eq!(
        String::from_utf8(got).unwrap(),
        String::from_utf8(sent).unwrap(),
        "every byte arrives, in order"
    );
}

/// Every `updatedAt` in `value`, however deep.
fn updated_ats(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                match inner.as_str() {
                    Some(text) if key == "updatedAt" => out.push(text.to_owned()),
                    _ => updated_ats(inner, out),
                }
            }
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| updated_ats(item, out)),
        _ => {}
    }
}

/// Apply-time check for `observe_pushed` (design 1): a pushed event whose
/// `updatedAt` is older than the one seen is skipped by comparing the two
/// strings. That only works if they order like the instants they name.
#[test]
fn iso_updated_at_orders_lexicographically() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut stamps = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "json") {
            let json = std::fs::read_to_string(&path).unwrap();
            updated_ats(&serde_json::from_str(&json).unwrap(), &mut stamps);
        }
    }
    stamps.sort();
    stamps.dedup();
    assert!(stamps.len() >= 5, "the fixtures hold stamps: {stamps:?}");
    for stamp in &stamps {
        assert_eq!(stamp.len(), 24, "one fixed-width form: {stamp}");
        assert!(stamp.ends_with('Z'), "UTC, not an offset: {stamp}");
    }
    let instant = |s: &String| chrono::DateTime::parse_from_rfc3339(s).unwrap();
    for pair in stamps.windows(2) {
        assert!(
            instant(&pair[0]) < instant(&pair[1]),
            "{} sorts before {} and is also earlier",
            pair[0],
            pair[1]
        );
    }
}
