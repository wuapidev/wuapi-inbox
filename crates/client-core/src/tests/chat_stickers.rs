//! The stickers the account already has in its chats, listed in the
//! library: what it sent (from here or from its phone) and what it
//! received, from the messages and the media cache in the store.

use super::*;
use crate::animation::fixtures::{animated_webp, still_webp};
use crate::{
    content_id, thumbnail_key, LibraryKind, LibrarySource, HEARD_LIMIT, LIBRARY_BUDGET,
    RECENT_LIMIT,
};

/// A sticker message of the account's chat, sent by it or received.
fn sticker_message(id: &str, url: &str, ts: i64, direction: Direction) -> Message {
    let mut message = text(id, "chat", ts, "", direction);
    let mut media = Media::new(MediaKind::Sticker);
    media.source = Some(client_provider::MediaRef::new(url));
    media.mime_type = Some("image/webp".into());
    message.content = MessageContent::Media(media);
    message
}

/// The thumbnail the engine keeps of a still sticker: a PNG, with its
/// answer that it does not move. `side` tells one picture from another.
fn cache_still(store: &Store, url: &str, side: u32) {
    let thumb = CachedMedia {
        bytes: crate::imaging::tests::png(side, side),
        mime: Some("image/png".into()),
        size: Some((side, side)),
    };
    let still = CachedMedia {
        bytes: Vec::new(),
        mime: None,
        size: None,
    };
    let now = Timestamp::now();
    store
        .put_media(&thumbnail_key(url), &thumb, now, i64::MAX as u64)
        .unwrap();
    store
        .put_media(&animation_key(url), &still, now, i64::MAX as u64)
        .unwrap();
}

fn store_with_chat() -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    store
}

fn engine_over(store: Arc<Store>, mock: &MockProvider) -> SyncEngine {
    SyncEngine::new(
        store,
        Arc::new(mock.clone()),
        SyncConfig::default(),
        Handle::current(),
    )
}

/// Runs the indexer until it has nothing more to do.
fn index_all(engine: &SyncEngine) {
    for _ in 0..200 {
        if !engine.index_stickers().unwrap() {
            return;
        }
    }
    panic!("the indexer never finished");
}

fn ids(items: &[LibraryItem]) -> Vec<String> {
    items.iter().map(|item| item.id.clone()).collect()
}

#[tokio::test]
async fn sent_and_received_stickers_of_the_chats_are_listed_newest_first_and_once() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    // Sent: 1 (long ago), 2 (from the phone, in a chat), 3 (newest).
    // Received: 4, 5 (the same picture as 4, under another reference), 6.
    let plan = [
        ("m1", "https://wa/s1", 10, 20, Direction::Outgoing),
        ("m2", "https://wa/s2", 20, 21, Direction::Outgoing),
        ("m3", "https://wa/s3", 30, 22, Direction::Outgoing),
        ("m4", "https://wa/s4", 15, 30, Direction::Incoming),
        ("m5", "https://wa/s5", 40, 30, Direction::Incoming),
        ("m6", "https://wa/s6", 25, 31, Direction::Incoming),
    ];
    for (id, url, ts, side, direction) in plan {
        store
            .upsert_message(&sticker_message(id, url, ts, direction))
            .unwrap();
        cache_still(&store, url, side);
    }
    // Words in the same chat are not stickers.
    store
        .upsert_message(&text("t1", "chat", 50, "hola", Direction::Incoming))
        .unwrap();

    index_all(&engine);

    let recent = store.library_recent(LibraryKind::Sticker, 36).unwrap();
    assert_eq!(recent.len(), 3, "what the account sent");
    // Newest first, whichever picture it is.
    let sent_at: Vec<i64> = recent
        .iter()
        .map(|item| item.sent_at.unwrap().as_millis())
        .collect();
    assert_eq!(sent_at, [30, 20, 10]);
    assert!(recent.iter().all(|item| item.indexed
        && item.source == LibrarySource::Sent
        && !item.pending()
        && item.has_thumb));

    let heard = store.library_heard(10).unwrap();
    assert_eq!(heard.len(), 2, "m4 and m5 are one picture");
    assert_eq!(
        heard[0].heard_at.unwrap().as_millis(),
        40,
        "the newest copy"
    );
    assert_eq!(heard[1].heard_at.unwrap().as_millis(), 25);
    // The file is the one that is sent: a WebP.
    let (bytes, mime) = store.library_file(&heard[0].id).unwrap().unwrap();
    assert_eq!(mime, "image/webp");
    assert!(matches!(
        crate::sniff(&bytes),
        crate::FileKind::Webp { animated: false }
    ));
    assert_eq!(heard[0].id, content_id(&bytes));
    // Kept stickers (the user's) are not counted among these.
    assert_eq!(store.library_stats().unwrap().items, 0);

    // A second pass changes nothing.
    let before = store.library_heard(10).unwrap();
    index_all(&engine);
    assert_eq!(store.library_heard(10).unwrap(), before);
    assert_eq!(
        store.library_recent(LibraryKind::Sticker, 36).unwrap(),
        recent
    );
}

#[tokio::test]
async fn an_animated_sticker_of_a_chat_is_kept_as_it_came() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    let url = "https://wa/moving";
    let original = animated_webp(96, 96, &[60, 60, 60]);
    store
        .upsert_message(&sticker_message("m1", url, 5, Direction::Incoming))
        .unwrap();
    let now = Timestamp::now();
    let frame = CachedMedia {
        bytes: crate::imaging::tests::png(96, 96),
        mime: Some("image/png".into()),
        size: Some((96, 96)),
    };
    store
        .put_media(&thumbnail_key(url), &frame, now, i64::MAX as u64)
        .unwrap();
    let moving = CachedMedia {
        bytes: original.clone(),
        mime: Some("image/webp".into()),
        size: None,
    };
    store
        .put_media(&animation_key(url), &moving, now, i64::MAX as u64)
        .unwrap();
    index_all(&engine);
    let heard = store.library_heard(10).unwrap();
    assert_eq!(heard.len(), 1);
    assert!(heard[0].animated);
    assert_eq!(heard[0].id, content_id(&original));
    assert_eq!(
        store.library_file(&heard[0].id).unwrap().unwrap().0,
        original
    );
}

#[tokio::test]
async fn the_lists_are_bounded_and_keep_the_newest() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    // More of each than the caps, every one a different picture.
    let sent = RECENT_LIMIT + 6;
    let heard = HEARD_LIMIT + 6;
    for n in 0..sent {
        let url = format!("https://wa/out{n}");
        store
            .upsert_message(&sticker_message(
                &format!("o{n}"),
                &url,
                1_000 + n as i64,
                Direction::Outgoing,
            ))
            .unwrap();
        cache_still(&store, &url, 8 + n as u32);
    }
    for n in 0..heard {
        let url = format!("https://wa/in{n}");
        store
            .upsert_message(&sticker_message(
                &format!("i{n}"),
                &url,
                2_000 + n as i64,
                Direction::Incoming,
            ))
            .unwrap();
        cache_still(&store, &url, 200 + n as u32);
    }
    index_all(&engine);

    let recent = store.library_recent(LibraryKind::Sticker, 1000).unwrap();
    assert_eq!(recent.len(), RECENT_LIMIT);
    assert_eq!(
        recent[0].sent_at.unwrap().as_millis(),
        1_000 + sent as i64 - 1
    );
    assert_eq!(
        recent.last().unwrap().sent_at.unwrap().as_millis(),
        1_000 + (sent - RECENT_LIMIT) as i64,
        "the oldest six are not kept"
    );
    let from_chats = store.library_heard(1000).unwrap();
    assert_eq!(from_chats.len(), HEARD_LIMIT);
    assert_eq!(
        from_chats[0].heard_at.unwrap().as_millis(),
        2_000 + heard as i64 - 1
    );
    assert!(
        !engine.index_stickers().unwrap(),
        "with the caps met it has nothing more to look at"
    );
}

#[tokio::test]
async fn a_sticker_that_arrives_or_is_sent_later_is_listed_without_reading_the_rest() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    store
        .upsert_message(&sticker_message(
            "m1",
            "https://wa/a",
            1,
            Direction::Incoming,
        ))
        .unwrap();
    cache_still(&store, "https://wa/a", 20);
    index_all(&engine);
    assert_eq!(store.library_heard(10).unwrap().len(), 1);
    let state = store.library_index_state().unwrap().unwrap();
    assert!(state.done);

    // One arrives and one is sent from the phone: the next run lists both.
    store
        .upsert_message(&sticker_message(
            "m2",
            "https://wa/b",
            2,
            Direction::Incoming,
        ))
        .unwrap();
    store
        .upsert_message(&sticker_message(
            "m3",
            "https://wa/c",
            3,
            Direction::Outgoing,
        ))
        .unwrap();
    cache_still(&store, "https://wa/b", 21);
    cache_still(&store, "https://wa/c", 22);
    assert!(!engine.index_stickers().unwrap());
    assert_eq!(store.library_heard(10).unwrap().len(), 2);
    assert_eq!(
        store
            .library_recent(LibraryKind::Sticker, 10)
            .unwrap()
            .len(),
        1
    );
    let after = store.library_index_state().unwrap().unwrap();
    assert_eq!(after.high, store.newest_message_pk().unwrap());
    assert!(after.high > state.high);

    // One the account sent from here is the library item that went: its
    // message is not listed again as another item.
    let before = store.library_stats().unwrap();
    store
        .upsert_message(&sticker_message(
            "m4",
            &format!("{}client-1", crate::outbox::LOCAL_MEDIA),
            4,
            Direction::Outgoing,
        ))
        .unwrap();
    engine.index_stickers().unwrap();
    assert_eq!(store.library_stats().unwrap(), before);
    assert_eq!(
        store
            .library_recent(LibraryKind::Sticker, 10)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test(start_paused = true)]
async fn the_loop_lists_a_sticker_when_its_message_arrives() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    let task = {
        let engine = engine.clone();
        tokio::spawn(async move { engine.run_library_index().await })
    };
    tokio::time::sleep(Duration::from_secs(2)).await;
    store
        .upsert_message(&sticker_message(
            "m1",
            "https://wa/a",
            1,
            Direction::Incoming,
        ))
        .unwrap();
    cache_still(&store, "https://wa/a", 20);
    store.notify(StoreChange::Messages {
        account_id: account(),
        chat_id: chat_id(),
    });
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if !store.library_heard(10).unwrap().is_empty() {
            break;
        }
    }
    assert_eq!(store.library_heard(10).unwrap().len(), 1);
    task.abort();
}

#[tokio::test]
async fn a_sticker_not_downloaded_yet_is_a_place_that_fills_in_when_it_is_fetched_and_nothing_is_read(
) {
    let mock = MockProvider::quiet();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = engine_over(store.clone(), &mock);
    engine.refresh().await.unwrap();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0);
    let url = "https://wa/not-yet";
    let webp = still_webp(64, 64);
    mock.set_media(url, webp, "image/webp");
    let mut message = sticker_message(
        "m-pending",
        url,
        Timestamp::now().as_millis(),
        Direction::Incoming,
    );
    message.account_id = account.clone();
    message.chat_id = chat.id.clone();
    store.upsert_message(&message).unwrap();
    let unread = store
        .chat(&account, &chat.id)
        .unwrap()
        .unwrap()
        .unread_count;
    let calls = mock.media_calls();

    index_all(&engine);
    // A place: nothing to show or send, and nothing was fetched for it.
    let heard = store.library_heard(10).unwrap();
    assert_eq!(heard.len(), 1);
    assert!(heard[0].pending() && !heard[0].has_thumb);
    assert_eq!(mock.media_calls(), calls);
    assert!(matches!(
        engine.send_library_item(&account, &chat.id, &heard[0].id, None),
        Err(SendMediaError::Empty)
    ));

    // Looked at: the file is fetched once, with the bubble's own queue.
    for _ in 0..3 {
        engine.want_sticker_file(&account, url, false);
    }
    settled(&engine, &animation_key(url)).await;
    assert_eq!(mock.media_calls(), calls + 1);
    index_all(&engine);
    let heard = store.library_heard(10).unwrap();
    assert_eq!(heard.len(), 1, "the place became the item");
    assert!(!heard[0].pending() && heard[0].has_thumb && heard[0].indexed);
    assert_eq!(
        heard[0].id,
        content_id(&store.library_file(&heard[0].id).unwrap().unwrap().0)
    );
    // And can be sent.
    engine
        .send_library_item(&account, &chat.id, &heard[0].id, None)
        .unwrap();

    // Nothing was marked read, here or on the phone.
    assert!(mock.marked_read().is_empty());
    assert_eq!(
        store
            .chat(&account, &chat.id)
            .unwrap()
            .unwrap()
            .unread_count,
        unread
    );
}

#[tokio::test]
async fn a_place_for_a_file_whatsapp_no_longer_has_goes() {
    let mock = MockProvider::quiet();
    let store = Arc::new(Store::open_in_memory().unwrap());
    let engine = engine_over(store.clone(), &mock);
    engine.refresh().await.unwrap();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0);
    let url = "https://wa/gone";
    mock.fail_next_media([ProviderError::Rejected {
        code: "media_expired".into(),
        message: "gone".into(),
    }]);
    let mut message = sticker_message("m-gone", url, 5, Direction::Incoming);
    message.account_id = account.clone();
    message.chat_id = chat.id.clone();
    store.upsert_message(&message).unwrap();
    index_all(&engine);
    assert_eq!(store.library_heard(10).unwrap().len(), 1);
    engine.want_sticker_file(&account, url, false);
    settled(&engine, &thumbnail_key(url)).await;
    index_all(&engine);
    assert!(store.library_heard(10).unwrap().is_empty());
}

#[tokio::test]
async fn a_favorite_is_never_let_go_and_an_unstarred_sticker_of_a_chat_is() {
    let store = store_with_chat();
    let engine = engine_over(store.clone(), &MockProvider::quiet());
    for (n, direction) in [
        Direction::Incoming,
        Direction::Incoming,
        Direction::Outgoing,
    ]
    .into_iter()
    .enumerate()
    {
        let url = format!("https://wa/{n}");
        store
            .upsert_message(&sticker_message(
                &format!("m{n}"),
                &url,
                n as i64 + 1,
                direction,
            ))
            .unwrap();
        cache_still(&store, &url, 20 + n as u32);
    }
    index_all(&engine);
    let heard = store.library_heard(10).unwrap();
    assert_eq!(heard.len(), 2);
    let starred = heard[0].id.clone();
    engine.set_library_favorite(&starred, true).unwrap();

    // A budget of nothing: everything unstarred goes, the star stays.
    store.library_evict(0, None).unwrap();
    let left = store.library_items(LibraryKind::Sticker).unwrap();
    assert_eq!(ids(&left), std::slice::from_ref(&starred));
    assert!(left[0].favorite);
    // The favorite counts as the user's: it is in the stats.
    assert_eq!(store.library_stats().unwrap().favorites, 1);

    // A starred one is not pruned when newer ones push it past the caps.
    for n in 0..(HEARD_LIMIT + 3) {
        let url = format!("https://wa/later{n}");
        store
            .upsert_message(&sticker_message(
                &format!("l{n}"),
                &url,
                1_000 + n as i64,
                Direction::Incoming,
            ))
            .unwrap();
        cache_still(&store, &url, 60 + n as u32);
    }
    index_all(&engine);
    assert!(store.library_item(&starred).unwrap().unwrap().favorite);
    assert_eq!(
        store.library_heard(1000).unwrap().len(),
        HEARD_LIMIT + 1,
        "the cap and the star"
    );
}

#[tokio::test]
async fn a_chat_sticker_that_is_used_is_the_first_recent_one_and_is_kept() {
    let store = store_with_chat();
    let mock = MockProvider::quiet();
    let engine = engine_over(store.clone(), &mock);
    engine.refresh().await.unwrap();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0).id;
    let url = "https://wa/used";
    let mut message = sticker_message("m1", url, 1, Direction::Incoming);
    message.account_id = account.clone();
    message.chat_id = chat.clone();
    store.upsert_message(&message).unwrap();
    cache_still(&store, url, 33);
    index_all(&engine);
    let item = store.library_heard(1).unwrap().remove(0);
    engine
        .send_library_item(&account, &chat, &item.id, None)
        .unwrap();
    let recent = store.library_recent(LibraryKind::Sticker, 5).unwrap();
    assert_eq!(recent[0].id, item.id);
    assert!(recent[0].last_used.is_some());
    // Used, it is not pruned even past the caps.
    store.library_prune_indexed(0, 0).unwrap();
    assert!(store.library_item(&item.id).unwrap().is_some());
}

#[test]
fn what_was_indexed_is_in_the_database_file_and_goes_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let key = StoreKey::from_bytes([42; 32]);
    {
        let store = Store::open(&path, Some(&key)).unwrap();
        seed_account(&store);
        store
            .library_add_seen(
                &SeenSticker {
                    account: account(),
                    message: MessageId::new("m_x"),
                    url: "https://wa/x".into(),
                    outgoing: true,
                    at: Timestamp::from_millis(5),
                },
                None,
                Timestamp::from_millis(6),
            )
            .unwrap();
        store
            .library_set_index_state(IndexState {
                high: 3,
                low: 1,
                done: true,
            })
            .unwrap();
    }
    let store = Store::open(&path, Some(&key)).unwrap();
    assert_eq!(
        store.library_recent(LibraryKind::Sticker, 5).unwrap().len(),
        1
    );
    assert!(store.library_index_state().unwrap().is_some());
    drop(store);
    // Signing out removes the file: no item, no place, no memory of how
    // far the indexer got.
    std::fs::remove_file(&path).unwrap();
    let fresh = Store::open(&path, Some(&key)).unwrap();
    assert!(fresh
        .library_recent(LibraryKind::Sticker, 5)
        .unwrap()
        .is_empty());
    assert!(fresh.library_heard(5).unwrap().is_empty());
    assert!(fresh.library_index_state().unwrap().is_none());
    let _ = LIBRARY_BUDGET;
}
