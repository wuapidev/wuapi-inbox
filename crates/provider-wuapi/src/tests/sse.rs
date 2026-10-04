//! The SSE parser: field rules, line endings, chunk boundaries and caps.

use crate::sse::{SseItem, SseParser};
use std::time::Duration;

const CAP: usize = 1024 * 1024;

fn parse(bytes: &[u8]) -> Vec<SseItem> {
    SseParser::new(CAP).feed(bytes)
}

fn event(name: &str, data: &str, id: Option<&str>) -> SseItem {
    SseItem::Event {
        event: name.to_owned(),
        data: data.to_owned(),
        id: id.map(str::to_owned),
    }
}

#[test]
fn line_endings_lf_cr_crlf_equal() {
    let frame =
        |eol: &str| format!("id: c1{eol}event: message.received{eol}data: {{\"a\":1}}{eol}{eol}");
    let expected = vec![event("message.received", "{\"a\":1}", Some("c1"))];
    for eol in ["\n", "\r", "\r\n"] {
        assert_eq!(parse(frame(eol).as_bytes()), expected, "{eol:?}");
    }
}

#[test]
fn crlf_split_across_chunks() {
    let mut parser = SseParser::new(CAP);
    // The CR ends the line; the LF that follows in the next chunk is the
    // same line end, not an empty line that would dispatch `a` alone.
    assert_eq!(parser.feed(b"data: a\r"), vec![]);
    assert_eq!(
        parser.feed(b"\ndata: b\r\n\r"),
        vec![event("message", "a\nb", None)],
        "the lone CR is the blank line"
    );
    assert_eq!(
        parser.feed(b"\n"),
        vec![],
        "and the LF after it belongs to that CR, it is not another blank line"
    );
    assert_eq!(
        parser.feed(b"data: c\n\n"),
        vec![event("message", "c", None)]
    );
}

#[test]
fn bom_dropped_once() {
    assert_eq!(
        parse("\u{feff}data: a\n\n".as_bytes()),
        vec![event("message", "a", None)]
    );
    // Only at the start of the stream: later it is part of a field name.
    let mut parser = SseParser::new(CAP);
    assert_eq!(parser.feed(b"data: a\n\n").len(), 1);
    assert_eq!(parser.feed("\u{feff}data: b\n\n".as_bytes()), vec![]);
    // A BOM split between chunks is still dropped.
    let mut parser = SseParser::new(CAP);
    assert_eq!(parser.feed(&[0xEF, 0xBB]), vec![]);
    assert_eq!(
        parser.feed(&[0xBF, b'd', b'a', b't', b'a', b':', b'x', b'\n', b'\n']),
        vec![event("message", "x", None)]
    );
}

#[test]
fn multiline_data_joined() {
    assert_eq!(
        parse(b"data: a\ndata: b\n\n"),
        vec![event("message", "a\nb", None)]
    );
    assert_eq!(
        parse(b"data: a\ndata:\ndata: b\n\n"),
        vec![event("message", "a\n\nb", None)],
        "an empty data line is an empty line of data"
    );
    assert_eq!(
        parse(b"data:\n\n"),
        vec![event("message", "", None)],
        "a frame whose only data line is empty still dispatches"
    );
}

#[test]
fn one_leading_space_dropped() {
    assert_eq!(
        parse(b"data:  two\n\n"),
        vec![event("message", " two", None)]
    );
    assert_eq!(
        parse(b"data:none\n\n"),
        vec![event("message", "none", None)]
    );
    assert_eq!(
        parse(b"id: c 1\ndata: x\n\n"),
        vec![event("message", "x", Some("c 1"))]
    );
}

#[test]
fn default_event_is_message() {
    assert_eq!(parse(b"data: 1\n\n"), vec![event("message", "1", None)]);
    assert_eq!(
        parse(b"event:\ndata: 1\n\n"),
        vec![event("message", "1", None)],
        "an empty name is no name"
    );
    assert_eq!(
        parse(b"event: x\ndata: 1\n\ndata: 2\n\n"),
        vec![event("x", "1", None), event("message", "2", None)],
        "the name belongs to its frame only"
    );
}

#[test]
fn comment_item() {
    assert_eq!(parse(b": ping\n"), vec![SseItem::Comment]);
    assert_eq!(
        parse(b": ping\n\n"),
        vec![SseItem::Comment],
        "a block of only a comment dispatches nothing"
    );
    assert_eq!(
        parse(b": a\n: b\n"),
        vec![SseItem::Comment, SseItem::Comment]
    );
}

#[test]
fn retry_digits_only() {
    assert_eq!(
        parse(b"retry: 3000\n"),
        vec![SseItem::Retry(Duration::from_millis(3000))]
    );
    assert_eq!(
        parse(b"retry:0\n"),
        vec![SseItem::Retry(Duration::ZERO)],
        "zero is a number"
    );
    for bad in [
        "retry: 3s",
        "retry: -5",
        "retry:",
        "retry: 1.5",
        "retry: 99999999999999999999999",
    ] {
        assert_eq!(parse(format!("{bad}\n").as_bytes()), vec![], "{bad}");
    }
}

#[test]
fn id_with_nul_ignored() {
    assert_eq!(
        parse(b"id: a\0b\ndata: x\n\n"),
        vec![event("message", "x", None)]
    );
    assert_eq!(
        parse(b"id: ok\nid: a\0b\ndata: x\n\n"),
        vec![event("message", "x", Some("ok"))],
        "the bad line does not replace a good id"
    );
}

#[test]
fn empty_id_means_no_cursor() {
    // `Some("")` and `Cursor("")` both tell the link to clear its cursor.
    assert_eq!(
        parse(b"id:\ndata: x\n\n"),
        vec![event("message", "x", Some(""))]
    );
    assert_eq!(parse(b"id:\n\n"), vec![SseItem::Cursor(String::new())]);
    assert_eq!(
        parse(b"id: c1\nid:\ndata: x\n\n"),
        vec![event("message", "x", Some(""))],
        "the last id of the block wins"
    );
}

#[test]
fn ping_with_id_is_cursor_not_event() {
    assert_eq!(
        parse(b": ping\nid: C9\n\n"),
        vec![SseItem::Comment, SseItem::Cursor("C9".to_owned())]
    );
    assert_eq!(
        parse(b": ping\nid: C9\n\ndata: x\nid: C10\n\n"),
        vec![
            SseItem::Comment,
            SseItem::Cursor("C9".to_owned()),
            event("message", "x", Some("C10"))
        ],
        "a block with data is an event, whatever came before it"
    );
}

#[test]
fn unknown_field_and_no_colon_ignored() {
    assert_eq!(
        parse(b"foo: bar\ndata\nid\nretry\nevent\ndata: x\n\n"),
        vec![event("message", "x", None)],
        "a line without a colon is ignored, even one named like a field"
    );
    assert_eq!(parse(b"foo: 1\n\n"), vec![]);
}

#[test]
fn oversized_frame_dropped_next_delivered() {
    let valid = event("message", "after", Some("c2"));
    // One line over the cap, in two chunks; its id is still honoured.
    let mut parser = SseParser::new(32);
    let mut items = parser.feed(b"id: c1\ndata: ");
    items.extend(parser.feed(&[b'x'; 40]));
    items.extend(parser.feed(b"\n\ndata: after\nid: c2\n\n"));
    assert_eq!(
        items,
        vec![
            SseItem::Oversized,
            SseItem::Cursor("c1".to_owned()),
            valid.clone()
        ]
    );
    // Many small lines whose sum is over the cap.
    let mut bytes = Vec::new();
    for _ in 0..10 {
        bytes.extend_from_slice(b"data: 123456\n");
    }
    bytes.extend_from_slice(b"\ndata: after\nid: c2\n\n");
    assert_eq!(
        SseParser::new(32).feed(&bytes),
        vec![SseItem::Oversized, valid.clone()]
    );
    // A frame exactly at the cap is delivered.
    assert_eq!(
        SseParser::new(32).feed(b"data: 123456789012345678901234\n\n"),
        vec![event("message", "123456789012345678901234", None)]
    );
}

#[test]
fn eof_mid_frame_discards() {
    let mut parser = SseParser::new(CAP);
    // A whole frame, then one that is cut off: only the first is dispatched.
    let items = parser.feed(b"data: whole\n\nid: c2\ndata: part");
    assert_eq!(items, vec![event("message", "whole", None)]);
    // The connection closes here and the parser is dropped with the rest.
    // The next connection gets a fresh one, which knows nothing of it.
    assert_eq!(
        SseParser::new(CAP).feed(b"\n\n"),
        vec![],
        "the cut frame left nothing behind"
    );
}

#[test]
fn every_split_point_is_identical() {
    let input = "\u{feff}retry: 3000\r\n: ping\r\nid: c1\r\nevent: message.received\r\n\
                 data: {\"t\":\"é✓ \u{1f600}\"}\r\ndata: second\r\n\r\n\
                 : ping\rid: c2\r\r\
                 id: c3\ndata: tail\n\n"
        .as_bytes();
    let whole = parse(input);
    assert_eq!(
        whole,
        vec![
            SseItem::Retry(Duration::from_millis(3000)),
            SseItem::Comment,
            event(
                "message.received",
                "{\"t\":\"é✓ \u{1f600}\"}\nsecond",
                Some("c1")
            ),
            SseItem::Comment,
            SseItem::Cursor("c2".to_owned()),
            event("message", "tail", Some("c3")),
        ]
    );
    for at in 0..=input.len() {
        let mut parser = SseParser::new(CAP);
        let mut items = parser.feed(&input[..at]);
        items.extend(parser.feed(&input[at..]));
        assert_eq!(items, whole, "split at {at}");
    }
    let mut parser = SseParser::new(CAP);
    let by_byte: Vec<SseItem> = input
        .iter()
        .flat_map(|byte| parser.feed(&[*byte]))
        .collect();
    assert_eq!(by_byte, whole, "one byte at a time");
}
