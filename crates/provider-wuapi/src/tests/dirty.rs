//! The set of chats waiting to be read: windows, limits, collapsing and
//! what each kind of failure does.

use crate::config::StreamTuning;
use crate::envelope::{Dirty, DirtyChats, Outcome, Read};
use std::time::Duration;
use tokio::time::Instant;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn chat(account: &str, chat: &str) -> Dirty {
    Dirty::Chat {
        account: account.to_owned(),
        chat: chat.to_owned(),
    }
}

fn read(account: &str, chat: &str) -> Read {
    Read::Chat {
        account: account.to_owned(),
        chat: chat.to_owned(),
    }
}

fn page(account: &str) -> Read {
    Read::Page {
        account: account.to_owned(),
    }
}

/// The defaults, with the limits that are not the point of a test out of
/// the way.
fn roomy() -> StreamTuning {
    StreamTuning {
        dirty_concurrency: 1000,
        dirty_per_minute: 10_000,
        ..StreamTuning::default()
    }
}

#[test]
fn dirty_coalesces_in_window() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    // Five marks of one chat inside a window, and one of another.
    for at in [0, 100, 200, 300, 400] {
        dirty.mark(chat("a", "x"), t0 + ms(at));
    }
    dirty.mark(chat("a", "y"), t0 + ms(450));
    assert_eq!(dirty.due(t0 + ms(499)), vec![], "the window is still open");
    assert_eq!(
        dirty.due(t0 + ms(500)),
        vec![read("a", "x"), read("a", "y")],
        "each chat once, together, when the window closes"
    );
    assert_eq!(dirty.due(t0 + ms(5000)), vec![], "and nothing more");
    assert_eq!(dirty.next_wake(t0 + ms(5000)), None);
}

#[test]
fn dirty_mark_during_read_remarks() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    dirty.mark(chat("a", "x"), t0);
    let x = read("a", "x");
    assert_eq!(dirty.due(t0 + ms(500)), vec![x.clone()]);
    // News arrives while the read is out: it is not read twice at once.
    dirty.mark(chat("a", "x"), t0 + ms(600));
    assert_eq!(dirty.due(t0 + ms(1200)), vec![], "one read at a time");
    dirty.finished(&x, Outcome::Done, t0 + ms(1300));
    // The chat is marked again, but one read per chat per 2 s (counted from
    // when the last one started) holds it back.
    assert_eq!(dirty.due(t0 + ms(2499)), vec![]);
    assert_eq!(dirty.due(t0 + ms(2500)), vec![x.clone()]);
    // Without a mark in between, a finished read leaves nothing behind.
    dirty.finished(&x, Outcome::Done, t0 + ms(2600));
    assert_eq!(dirty.due(t0 + ms(60_000)), vec![]);
}

#[test]
fn dirty_limits_4_concurrent_1_per_chat_2s_60_per_min() {
    let t0 = Instant::now();

    // Four at once.
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    for i in 0..6 {
        dirty.mark(chat("a", &format!("c{i}")), t0);
    }
    let first = dirty.due(t0 + ms(500));
    assert_eq!(
        first,
        (0..4)
            .map(|i| read("a", &format!("c{i}")))
            .collect::<Vec<_>>()
    );
    assert_eq!(dirty.due(t0 + ms(600)), vec![], "four are out");
    dirty.finished(&first[0], Outcome::Done, t0 + ms(700));
    assert_eq!(dirty.due(t0 + ms(700)), vec![read("a", "c4")], "one slot");
    dirty.finished(&first[1], Outcome::Done, t0 + ms(800));
    dirty.finished(&first[2], Outcome::Done, t0 + ms(800));
    assert_eq!(dirty.due(t0 + ms(800)), vec![read("a", "c5")]);

    // One per chat per two seconds, from the start of the last.
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    dirty.mark(chat("a", "x"), t0);
    let x = read("a", "x");
    assert_eq!(dirty.due(t0 + ms(500)), vec![x.clone()]);
    dirty.finished(&x, Outcome::Done, t0 + ms(520));
    dirty.mark(chat("a", "x"), t0 + ms(530));
    assert_eq!(dirty.due(t0 + ms(1100)), vec![], "window over, gap not");
    assert_eq!(dirty.next_wake(t0 + ms(1100)), Some(t0 + ms(2500)));
    assert_eq!(dirty.due(t0 + ms(2500)), vec![x]);

    // Sixty a minute, counted over a rolling minute.
    let mut dirty = DirtyChats::new(&StreamTuning {
        dirty_concurrency: 1000,
        ..StreamTuning::default()
    });
    for i in 0..70 {
        dirty.mark(chat("a", &format!("c{i}")), t0);
    }
    assert_eq!(dirty.due(t0 + ms(500)).len(), 60);
    assert_eq!(dirty.due(t0 + ms(30_000)), vec![]);
    assert_eq!(dirty.next_wake(t0 + ms(30_000)), Some(t0 + ms(60_500)));
    assert_eq!(dirty.due(t0 + ms(60_499)), vec![]);
    assert_eq!(dirty.due(t0 + ms(60_500)).len(), 10);
}

#[test]
fn over_200_pending_collapse_to_account_page() {
    let t0 = Instant::now();
    // 201 chats of two accounts: past the limit, so each account is read
    // as one page of chats.
    let mut dirty = DirtyChats::new(&roomy());
    for i in 0..201 {
        let account = if i < 120 { "a" } else { "b" };
        dirty.mark(chat(account, &format!("c{i}")), t0 + ms(i));
    }
    assert_eq!(dirty.due(t0 + ms(700)), vec![page("a"), page("b")]);
    assert_eq!(dirty.due(t0 + ms(70_000)), vec![]);

    // 200 is still read chat by chat.
    let mut dirty = DirtyChats::new(&roomy());
    for i in 0..200 {
        dirty.mark(chat("a", &format!("c{i}")), t0);
    }
    let due = dirty.due(t0 + ms(500));
    assert_eq!(due.len(), 200);
    assert!(due.iter().all(|read| matches!(read, Read::Chat { .. })));
}

#[test]
fn history_synced_marks_account() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    dirty.mark(Dirty::Account("a".to_owned()), t0);
    dirty.mark(Dirty::Account("a".to_owned()), t0 + ms(100));
    dirty.mark(Dirty::Account("b".to_owned()), t0 + ms(200));
    assert_eq!(dirty.due(t0 + ms(500)), vec![page("a"), page("b")]);
    dirty.finished(&page("a"), Outcome::Done, t0 + ms(600));
    dirty.finished(&page("b"), Outcome::Done, t0 + ms(600));
    assert_eq!(dirty.due(t0 + ms(60_000)), vec![]);
}

#[test]
fn dirty_transient_failure_retries_5s_three_times() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    let x = read("a", "x");
    dirty.mark(chat("a", "x"), t0);
    let mut at = ms(500);
    assert_eq!(dirty.due(t0 + at), vec![x.clone()]);
    // Three retries, five seconds after each failure.
    for _ in 0..3 {
        dirty.finished(&x, Outcome::Transient, t0 + at + ms(100));
        assert_eq!(dirty.due(t0 + at + ms(5099)), vec![], "not yet");
        at += ms(5100);
        assert_eq!(dirty.due(t0 + at), vec![x.clone()], "after 5 s");
    }
    // The fourth failure is the last: the mark is dropped.
    dirty.finished(&x, Outcome::Transient, t0 + at + ms(100));
    assert_eq!(dirty.due(t0 + at + ms(600_000)), vec![]);
    assert_eq!(dirty.next_wake(t0 + at + ms(600_000)), None);
    // A new mark starts over.
    dirty.mark(chat("a", "x"), t0 + at + ms(700_000));
    assert_eq!(dirty.due(t0 + at + ms(700_500)), vec![x]);
}

#[test]
fn dirty_rate_limited_pauses_every_read() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    let x = read("a", "x");
    dirty.mark(chat("a", "x"), t0);
    assert_eq!(dirty.due(t0 + ms(500)), vec![x.clone()]);
    dirty.finished(
        &x,
        Outcome::RateLimited(Duration::from_secs(20)),
        t0 + ms(600),
    );
    // Another chat marked meanwhile waits as well.
    dirty.mark(chat("a", "y"), t0 + ms(700));
    assert_eq!(dirty.due(t0 + ms(19_000)), vec![]);
    assert_eq!(dirty.next_wake(t0 + ms(19_000)), Some(t0 + ms(20_600)));
    assert_eq!(
        dirty.due(t0 + ms(20_600)),
        vec![x, read("a", "y")],
        "the refused read is still owed, and the pause is over"
    );
}

#[test]
fn dirty_not_found_drops_the_mark() {
    let t0 = Instant::now();
    let mut dirty = DirtyChats::new(&StreamTuning::default());
    let x = read("a", "x");
    dirty.mark(chat("a", "x"), t0);
    assert_eq!(dirty.due(t0 + ms(500)), vec![x.clone()]);
    dirty.mark(chat("a", "x"), t0 + ms(600));
    dirty.finished(&x, Outcome::NotFound, t0 + ms(700));
    assert_eq!(dirty.due(t0 + ms(60_000)), vec![], "gone, mark or no mark");
}
