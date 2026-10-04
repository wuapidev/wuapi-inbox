//! Server-sent events: bytes in, items out. Pure, no I/O, no clock.

use std::time::Duration;

/// What the parser found in the bytes it was fed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SseItem {
    /// A dispatched frame.
    Event {
        event: String,
        data: String,
        id: Option<String>,
    },
    /// A block with an id and no data.
    Cursor(String),
    /// A `retry:` hint.
    Retry(Duration),
    /// A comment line.
    Comment,
    /// A frame over the cap was dropped.
    Oversized,
}

/// The byte order mark some servers put first.
const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// Incremental SSE parser.
///
/// Splits on bytes and decodes one complete line at a time, so a character
/// cut by a chunk boundary needs no care. The result is the same for any
/// split of the input. At the end of the input a half-read frame is simply
/// never dispatched: drop the parser with the connection.
///
/// Two readings of the format are deliberate: an `id` belongs to its block
/// only (it is not carried to the next one), and a line without a colon is
/// ignored, whatever its name.
pub(crate) struct SseParser {
    max_frame: usize,
    /// The first bytes, while they could still be a byte order mark.
    head: Vec<u8>,
    started: bool,
    /// The line being read.
    line: Vec<u8>,
    /// The line outgrew `max_frame`: its bytes are skipped to its end.
    line_overflow: bool,
    /// A CR ended the last line: an LF right after it is the same line end.
    pending_cr: bool,
    // The frame being read.
    event: String,
    data: String,
    has_data: bool,
    id: Option<String>,
    frame_bytes: usize,
    poisoned: bool,
}

impl SseParser {
    /// A parser that drops frames and lines over `max_frame` bytes.
    pub(crate) fn new(max_frame: usize) -> Self {
        Self {
            max_frame,
            head: Vec::new(),
            started: false,
            line: Vec::new(),
            line_overflow: false,
            pending_cr: false,
            event: String::new(),
            data: String::new(),
            has_data: false,
            id: None,
            frame_bytes: 0,
            poisoned: false,
        }
    }

    /// Takes the next bytes and returns what they completed.
    pub(crate) fn feed(&mut self, chunk: &[u8]) -> Vec<SseItem> {
        let mut items = Vec::new();
        for &byte in chunk {
            if !self.started {
                self.head.push(byte);
                if self.head == BOM {
                    self.head.clear();
                    self.started = true;
                } else if !BOM.starts_with(&self.head) {
                    self.started = true;
                    for held in std::mem::take(&mut self.head) {
                        self.byte(held, &mut items);
                    }
                }
                continue;
            }
            self.byte(byte, &mut items);
        }
        items
    }

    fn byte(&mut self, byte: u8, items: &mut Vec<SseItem>) {
        if std::mem::take(&mut self.pending_cr) && byte == b'\n' {
            return;
        }
        match byte {
            b'\n' => self.end_line(items),
            b'\r' => {
                self.pending_cr = true;
                self.end_line(items);
            }
            _ if self.line_overflow => {}
            _ => {
                self.line.push(byte);
                if self.line.len() > self.max_frame {
                    self.line.clear();
                    self.line_overflow = true;
                }
            }
        }
    }

    fn end_line(&mut self, items: &mut Vec<SseItem>) {
        let line = std::mem::take(&mut self.line);
        if std::mem::take(&mut self.line_overflow) {
            self.poison();
            return;
        }
        if line.is_empty() {
            self.dispatch(items);
            return;
        }
        self.frame_bytes += line.len() + 1;
        if self.frame_bytes > self.max_frame {
            self.poison();
        }
        let line = String::from_utf8_lossy(&line);
        if line.starts_with(':') {
            items.push(SseItem::Comment);
            return;
        }
        let Some((name, value)) = line.split_once(':') else {
            return;
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match name {
            "event" if !self.poisoned => self.event = value.to_owned(),
            "data" if !self.poisoned => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.data.push_str(value);
                self.has_data = true;
            }
            // Even a dropped frame has its id honoured, so the cursor
            // moves past it.
            "id" if !value.contains('\0') => self.id = Some(value.to_owned()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                if let Ok(millis) = value.parse() {
                    items.push(SseItem::Retry(Duration::from_millis(millis)));
                }
            }
            _ => {}
        }
    }

    /// The frame is over the cap: its content goes, its id stays.
    fn poison(&mut self) {
        self.poisoned = true;
        self.event.clear();
        self.data.clear();
        self.has_data = false;
    }

    /// A blank line: the block is complete.
    fn dispatch(&mut self, items: &mut Vec<SseItem>) {
        let event = std::mem::take(&mut self.event);
        let data = std::mem::take(&mut self.data);
        let id = self.id.take();
        let has_data = std::mem::take(&mut self.has_data);
        self.frame_bytes = 0;
        if std::mem::take(&mut self.poisoned) {
            items.push(SseItem::Oversized);
        } else if has_data {
            items.push(SseItem::Event {
                event: if event.is_empty() {
                    "message".to_owned()
                } else {
                    event
                },
                data,
                id,
            });
            return;
        }
        if let Some(id) = id {
            items.push(SseItem::Cursor(id));
        }
    }
}
