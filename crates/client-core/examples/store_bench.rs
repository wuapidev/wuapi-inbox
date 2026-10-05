//! How long the local store takes over a large history: the chat list, the
//! newest window of a conversation and full-text search, which are the
//! queries the window runs when it starts, opens a chat and searches.
//!
//! ```sh
//! cargo run --release -p client-core --example store_bench -- 100000 target/tmp/store-bench
//! ```
//!
//! The first argument is the number of messages (default 100000) and the
//! second a directory for the database (default `target/tmp/store-bench`,
//! emptied first). The database is a file encrypted with SQLCipher, as in
//! the application. The messages are made up: words drawn from 5,000
//! invented ones, the common ones far more often than the rare ones,
//! spread over 200 chats. The numbers of `docs/media/measurements.md` come
//! from this, through `docs/media/measure.sh`.

use client_core::provider::{
    Account, AccountId, Chat, ChatId, ChatKind, ConnectionState, ContactId, DeliveryStatus,
    Direction, Message, MessageContent, MessageId, Timestamp,
};
use client_core::{Store, StoreKey};
use std::time::{Duration, Instant};

const CHATS: usize = 200;
const WORDS: usize = 5_000;
/// What the window asks for: see `MESSAGE_WINDOW` and `MAX_HITS` in the
/// application's shell.
const WINDOW: usize = 120;
const HITS: usize = 40;
/// How many times each query is run, unless that takes longer than
/// `PATIENCE`: then as many times as fit, and at least three.
const RUNS: usize = 30;
const PATIENCE: Duration = Duration::from_secs(5);

/// A small generator with a fixed seed: the same history every time.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() % limit as u64) as usize
    }

    /// A word index, low ones far more often than high ones.
    fn word(&mut self) -> usize {
        let unit = (self.next() % 1_000_000) as f64 / 1_000_000.;
        ((unit * unit * unit) * WORDS as f64) as usize
    }
}

/// The invented word at `index`: three syllables, so that two letters are
/// the start of many words and a whole word the start of few.
fn word(index: usize) -> String {
    const CONSONANTS: &[u8] = b"bcdfglmnprstv";
    const VOWELS: &[u8] = b"aeiou";
    let syllables = CONSONANTS.len() * VOWELS.len();
    let mut word = String::new();
    let mut rest = index;
    for _ in 0..3 {
        let syllable = rest % syllables;
        rest /= syllables;
        word.push(CONSONANTS[syllable / VOWELS.len()] as char);
        word.push(VOWELS[syllable % VOWELS.len()] as char);
    }
    word
}

fn median_and_range(mut samples: Vec<Duration>) -> String {
    samples.sort();
    let micros = |time: Duration| time.as_secs_f64() * 1_000.;
    format!(
        "median {:.3} ms, range {:.3} to {:.3} ms, {} runs",
        micros(samples[samples.len() / 2]),
        micros(samples[0]),
        micros(samples[samples.len() - 1]),
        samples.len()
    )
}

fn main() {
    let mut args = std::env::args().skip(1);
    let total: usize = args
        .next()
        .map(|count| count.parse().expect("the number of messages"))
        .unwrap_or(100_000);
    let dir = std::path::PathBuf::from(
        args.next()
            .unwrap_or_else(|| "target/tmp/store-bench".to_owned()),
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the directory for the database");
    let path = dir.join("bench.db");

    let store = Store::open(&path, Some(&StoreKey::from_bytes([7; 32]))).expect("the store opens");
    let account = AccountId::new("acc");
    store
        .upsert_accounts(
            "bench",
            &[Account {
                id: account.clone(),
                display_name: "Bench".into(),
                phone: None,
                self_contact: Some(ContactId::new("me")),
                connection: ConnectionState::Connected,
                settings: Default::default(),
            }],
        )
        .expect("the account is stored");
    let chats: Vec<ChatId> = (0..CHATS)
        .map(|index| ChatId::new(format!("chat{index}")))
        .collect();
    for (index, id) in chats.iter().enumerate() {
        store
            .upsert_chat(
                &Chat {
                    id: id.clone(),
                    account_id: account.clone(),
                    kind: ChatKind::Direct,
                    title: format!("Chat {index}"),
                    avatar: None,
                    unread_count: 0,
                    pinned: false,
                    muted: false,
                    archived: false,
                    last_message: None,
                    unknown: Default::default(),
                    picture_id: None,
                    pinned_at: None,
                },
                false,
            )
            .expect("the chat is stored");
    }

    let mut random = Random(0x5eed_1234_abcd_ef01);
    let filling = Instant::now();
    let mut written = 0;
    while written < total {
        let batch: Vec<Message> = (written..total.min(written + 1_000))
            .map(|index| {
                let length = 4 + random.below(13);
                let body: Vec<String> = (0..length).map(|_| word(random.word())).collect();
                let mine = random.below(2) == 0;
                Message {
                    id: MessageId::new(format!("m{index}")),
                    client_id: None,
                    account_id: account.clone(),
                    chat_id: chats[random.below(CHATS)].clone(),
                    sender: ContactId::new(if mine { "me" } else { "them" }),
                    sender_name: None,
                    direction: if mine {
                        Direction::Outgoing
                    } else {
                        Direction::Incoming
                    },
                    timestamp: Timestamp::from_millis(1_700_000_000_000 + index as i64 * 60_000),
                    content: MessageContent::text(body.join(" ")),
                    reply_to: None,
                    status: DeliveryStatus::Delivered,
                    edited: false,
                    deleted: false,
                    extras: Default::default(),
                }
            })
            .collect();
        written += batch.len();
        store.upsert_messages(&batch).expect("messages are stored");
    }
    let filled = filling.elapsed();
    let size = std::fs::metadata(&path).map_or(0, |file| file.len());
    println!(
        "history: {total} messages in {CHATS} chats, written in {:.1} s; database file {:.1} MB (encrypted)",
        filled.as_secs_f64(),
        size as f64 / 1_000_000.
    );

    let timed = |work: &mut dyn FnMut(usize) -> usize| {
        let mut found = 0;
        let begun = Instant::now();
        let mut samples = Vec::new();
        for run in 0..RUNS {
            if run >= 3 && begun.elapsed() > PATIENCE {
                break;
            }
            let start = Instant::now();
            found = work(run);
            samples.push(start.elapsed());
        }
        (median_and_range(samples), found)
    };

    let (time, found) = timed(&mut |_| store.chats(&account, None).expect("chats").len());
    println!("chat list ({found} chats): {time}");

    let (time, found) = timed(&mut |run| {
        store
            .messages(&account, &chats[run % CHATS], WINDOW)
            .expect("messages")
            .len()
    });
    println!("open a chat (newest {found} messages of one chat): {time}");

    // The most common word, a rare one, two words together, and two
    // letters: what the search runs after the second keystroke.
    let common = word(0);
    let rare = word(WORDS - 1);
    let pair = format!("{} {}", word(3), word(40));
    let queries = [
        ("the most common word", common.as_str()),
        ("a rare word", rare.as_str()),
        ("two words", pair.as_str()),
        ("two letters, as typed", &common[..2]),
    ];
    for (what, query) in queries {
        let (time, found) = timed(&mut |_| {
            store
                .search_messages(&account, query, HITS)
                .expect("search")
                .len()
        });
        println!("search, {what} (`{query}`, {found} results shown of at most {HITS}): {time}");
    }
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
