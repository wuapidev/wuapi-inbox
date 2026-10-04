//! The sticker and GIF library: what is kept and how, what is sent and
//! how, and what the phone's favorites do to it.

use super::*;
use crate::animation::fixtures::{animated_gif, animated_webp, still_webp};
use crate::{
    content_id, LibraryItem, LibraryKind, LibrarySource, NewLibraryItem, LIBRARY_BUDGET,
    RECENT_LIMIT,
};
use async_trait::async_trait;
use client_provider::{
    Capabilities, Cursor, EventStream, MediaData, MediaRef, Page, ProviderResult, SendReceipt,
};

fn ts(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

/// A file of `size` bytes that is its own id (the tests care about
/// bytes, not pictures).
fn blob(tag: u8, size: usize) -> Vec<u8> {
    let mut bytes = vec![tag; size];
    bytes[0] = tag;
    bytes
}

fn new_item(kind: LibraryKind, bytes: Vec<u8>) -> NewLibraryItem {
    NewLibraryItem {
        kind,
        mime: if kind == LibraryKind::Sticker {
            "image/webp".into()
        } else {
            "video/mp4".into()
        },
        animated: kind == LibraryKind::Gif,
        size: Some((512, 512)),
        source: LibrarySource::Imported,
        name: None,
        pack: None,
        thumb: Some((vec![1, 2, 3], "image/png".into())),
        bytes,
    }
}

fn add(store: &Store, kind: LibraryKind, bytes: Vec<u8>, now: i64) -> LibraryItem {
    let id = content_id(&bytes);
    store
        .library_add(new_item(kind, bytes), &id, ts(now), LIBRARY_BUDGET)
        .unwrap()
        .0
}

fn mp4(tag: u8) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftypisom");
    bytes.extend_from_slice(&[tag; 64]);
    bytes
}

fn ids(items: &[LibraryItem]) -> Vec<String> {
    items.iter().map(|item| item.id.clone()).collect()
}

// ----- keeping ----------------------------------------------------------------

#[test]
fn the_same_file_is_one_item_however_it_came() {
    let store = Store::open_in_memory().unwrap();
    let bytes = blob(1, 100);
    let id = content_id(&bytes);
    let (first, created) = store
        .library_add(
            new_item(LibraryKind::Sticker, bytes.clone()),
            &id,
            ts(10),
            LIBRARY_BUDGET,
        )
        .unwrap();
    assert!(created);
    assert_eq!(first.id, id);
    assert!(!first.favorite && first.last_used.is_none());
    store.library_set_favorite(&id, true).unwrap();

    // Saved again from a chat, with a name this time: the same row, still
    // starred, and it gained the name it lacked.
    let mut again = new_item(LibraryKind::Sticker, bytes);
    again.source = LibrarySource::Received;
    again.name = Some("hello".into());
    let (second, created) = store
        .library_add(again, &id, ts(20), LIBRARY_BUDGET)
        .unwrap();
    assert!(!created);
    assert!(second.favorite);
    assert_eq!(second.name.as_deref(), Some("hello"));
    assert_eq!(
        second.source,
        LibrarySource::Imported,
        "where it first came from"
    );
    assert_eq!(second.added_at, ts(10));
    assert_eq!(store.library_items(LibraryKind::Sticker).unwrap().len(), 1);
    assert_eq!(store.library_stats().unwrap().items, 1);
    assert_eq!(
        store.library_file(&id).unwrap().unwrap(),
        (blob(1, 100), "image/webp".to_owned())
    );
    assert_eq!(
        store.library_thumb(&id).unwrap().unwrap().1,
        "image/png".to_owned()
    );
}

#[test]
fn stickers_and_gifs_are_listed_apart() {
    let store = Store::open_in_memory().unwrap();
    let sticker = add(&store, LibraryKind::Sticker, blob(1, 50), 1);
    let gif = add(&store, LibraryKind::Gif, mp4(2), 2);
    assert_eq!(
        ids(&store.library_items(LibraryKind::Sticker).unwrap()),
        vec![sticker.id]
    );
    assert_eq!(
        ids(&store.library_items(LibraryKind::Gif).unwrap()),
        vec![gif.id]
    );
}

#[test]
fn favorites_keep_the_order_the_user_gave_them() {
    let store = Store::open_in_memory().unwrap();
    let a = add(&store, LibraryKind::Sticker, blob(1, 50), 1);
    let b = add(&store, LibraryKind::Sticker, blob(2, 50), 2);
    let c = add(&store, LibraryKind::Sticker, blob(3, 50), 3);
    for item in [&b, &c, &a] {
        assert!(store.library_set_favorite(&item.id, true).unwrap());
    }
    assert!(!store.library_set_favorite(&a.id, true).unwrap(), "already");
    let order = |store: &Store| ids(&store.library_favorites(LibraryKind::Sticker).unwrap());
    assert_eq!(
        order(&store),
        vec![b.id.clone(), c.id.clone(), a.id.clone()]
    );

    // The last one goes first.
    assert!(store.library_move_favorite(&a.id, 0).unwrap());
    assert_eq!(
        order(&store),
        vec![a.id.clone(), b.id.clone(), c.id.clone()]
    );
    // Past the end is the end; nowhere new is no move.
    assert!(store.library_move_favorite(&a.id, 99).unwrap());
    assert_eq!(
        order(&store),
        vec![b.id.clone(), c.id.clone(), a.id.clone()]
    );
    assert!(!store.library_move_favorite(&a.id, 2).unwrap());

    // Unstarred, it leaves the section and a new star goes last.
    assert!(store.library_set_favorite(&b.id, false).unwrap());
    assert_eq!(order(&store), vec![c.id.clone(), a.id.clone()]);
    store.library_set_favorite(&b.id, true).unwrap();
    assert_eq!(order(&store), vec![c.id, a.id, b.id]);
}

#[test]
fn recent_ones_are_the_last_used_first_and_bounded() {
    let store = Store::open_in_memory().unwrap();
    let items: Vec<LibraryItem> = (0..5)
        .map(|n| add(&store, LibraryKind::Sticker, blob(n, 50), i64::from(n)))
        .collect();
    assert!(
        store
            .library_recent(LibraryKind::Sticker, 10)
            .unwrap()
            .is_empty(),
        "nothing was used yet"
    );
    store.library_touch(&items[1].id, ts(100)).unwrap();
    store.library_touch(&items[3].id, ts(200)).unwrap();
    store.library_touch(&items[0].id, ts(300)).unwrap();
    assert_eq!(
        ids(&store.library_recent(LibraryKind::Sticker, 10).unwrap()),
        vec![
            items[0].id.clone(),
            items[3].id.clone(),
            items[1].id.clone()
        ]
    );
    // Bounded.
    assert_eq!(
        store.library_recent(LibraryKind::Sticker, 2).unwrap().len(),
        2
    );
    // Used again, it goes first.
    store.library_touch(&items[1].id, ts(400)).unwrap();
    assert_eq!(
        store.library_recent(LibraryKind::Sticker, 1).unwrap()[0].id,
        items[1].id
    );
    const { assert!(RECENT_LIMIT >= 12 && RECENT_LIMIT <= 100) };
}

#[test]
fn past_its_budget_what_was_used_longest_ago_goes_and_never_a_favorite() {
    let store = Store::open_in_memory().unwrap();
    let budget = 1000;
    let put = |tag: u8, now: i64| {
        let bytes = blob(tag, 300);
        let id = content_id(&bytes);
        store
            .library_add(new_item(LibraryKind::Sticker, bytes), &id, ts(now), budget)
            .unwrap()
            .0
    };
    let oldest = put(1, 1);
    let favorite = put(2, 2);
    store.library_set_favorite(&favorite.id, true).unwrap();
    let used = put(3, 3);
    // Used after the next one came in: it is not the oldest.
    store.library_touch(&oldest.id, ts(50)).unwrap();
    assert_eq!(store.library_stats().unwrap().bytes, 900);

    // 1200 bytes: one has to go. The favorite is older than `used`, but
    // never goes; `used` is the least recently used of the rest.
    let newest = put(4, 60);
    let left = ids(&store.library_items(LibraryKind::Sticker).unwrap());
    assert!(left.contains(&favorite.id), "a favorite is never let go");
    assert!(left.contains(&newest.id), "what just came is kept");
    assert!(left.contains(&oldest.id), "it was used last");
    assert!(!left.contains(&used.id));
    assert_eq!(store.library_stats().unwrap().bytes, 900);

    // Only favorites over the budget: nothing else is left to let go, and
    // nothing starred is touched.
    for item in store.library_items(LibraryKind::Sticker).unwrap() {
        store.library_set_favorite(&item.id, true).unwrap();
    }
    assert_eq!(store.library_evict(10, None).unwrap(), 0);
    assert_eq!(store.library_stats().unwrap().items, 3);
}

#[test]
fn clearing_keeps_the_favorites_when_asked_and_removes_everything_otherwise() {
    let store = Store::open_in_memory().unwrap();
    let starred = add(&store, LibraryKind::Sticker, blob(1, 50), 1);
    let plain = add(&store, LibraryKind::Sticker, blob(2, 50), 2);
    store.library_set_favorite(&starred.id, true).unwrap();
    assert_eq!(store.library_clear(true).unwrap(), 1);
    assert!(store.library_item(&plain.id).unwrap().is_none());
    assert!(store.library_item(&starred.id).unwrap().is_some());
    assert_eq!(store.library_clear(false).unwrap(), 1);
    assert_eq!(store.library_stats().unwrap(), Default::default());
}

#[test]
fn imports_are_grouped_in_packs() {
    let store = Store::open_in_memory().unwrap();
    let pack = store.library_pack("p1", "Cats", ts(5)).unwrap();
    assert_eq!(pack.name, "Cats");
    let mut item = new_item(LibraryKind::Sticker, blob(1, 50));
    item.pack = Some(pack.id.clone());
    let id = content_id(&item.bytes);
    store.library_add(item, &id, ts(6), LIBRARY_BUDGET).unwrap();
    assert_eq!(
        store.library_item(&id).unwrap().unwrap().pack.as_deref(),
        Some("p1")
    );
    // The same name again is the same pack.
    store.library_pack("p1", "Dogs", ts(9)).unwrap();
    assert_eq!(store.library_packs().unwrap().len(), 1);
    assert_eq!(store.library_packs().unwrap()[0].name, "Cats");
}

#[test]
fn the_library_is_in_the_database_file_and_goes_with_it() {
    // One file holds it: a store opened again finds it, and removing the
    // file (what signing out does) leaves nothing of it anywhere.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("inbox.db");
    let key = StoreKey::from_bytes([42; 32]);
    let id = {
        let store = Store::open(&path, Some(&key)).unwrap();
        add(&store, LibraryKind::Sticker, blob(9, 64), 1).id
    };
    let store = Store::open(&path, Some(&key)).unwrap();
    assert!(store.library_item(&id).unwrap().is_some());
    drop(store);
    let on_disk = std::fs::read(&path).unwrap();
    assert!(
        !on_disk
            .windows(64)
            .any(|window| window == blob(9, 64).as_slice()),
        "the file is encrypted: the sticker is not in it in clear"
    );
    std::fs::remove_file(&path).unwrap();
    let fresh = Store::open(&path, Some(&key)).unwrap();
    assert_eq!(fresh.library_stats().unwrap(), Default::default());
}

// ----- the engine: keeping and sending ----------------------------------------------

fn media_of(
    store: &Store,
    account: &AccountId,
    chat: &ChatId,
    id: &ClientMessageId,
) -> client_provider::Media {
    let bubble = stored_media(store, account, chat, id);
    match bubble.message.content {
        MessageContent::Media(media) => media,
        other => panic!("a media bubble, not {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_sticker_is_sent_from_the_library_and_shows_at_once() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let webp = still_webp(64, 64);
    let (item, _) = engine
        .library_save_sticker(webp.clone(), "image/webp", LibrarySource::Received, false)
        .unwrap();
    assert_eq!(item.size, Some((64, 64)));
    assert!(item.has_thumb);

    let client_id = engine
        .send_library_item(&account, &chat, &item.id, None)
        .unwrap();
    // Before any network: pending, a sticker, with the bytes queued.
    assert_eq!(mock.upload_calls(), 0);
    let media = media_of(&store, &account, &chat, &client_id);
    assert_eq!(media.kind, MediaKind::Sticker);
    assert_eq!(media.caption, None);
    assert!(!media.gif);
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Pending
    );
    assert_eq!(*store.outbox_file(&client_id).unwrap().unwrap().bytes, webp);
    // It is the first of the recent ones now.
    assert_eq!(
        store.library_recent(LibraryKind::Sticker, 5).unwrap()[0].id,
        item.id
    );

    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.uploaded(), vec![(webp, "image/webp".to_owned())]);
    let sent = mock.sent().pop().unwrap();
    assert_eq!(sent.client_id, client_id);
    assert!(matches!(
        sent.content,
        OutgoingContent::Media {
            kind: MediaKind::Sticker,
            gif: false,
            caption: None,
            ..
        }
    ));
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Sent
    );
}

#[tokio::test(start_paused = true)]
async fn a_sticker_that_loses_its_connection_is_retried_under_the_same_id_and_sent_once() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let (item, _) = engine
        .library_save_sticker(
            still_webp(32, 32),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    let client_id = engine
        .send_library_item(&account, &chat, &item.id, None)
        .unwrap();

    // The upload drops; then the upload works and the send drops.
    mock.fail_next_uploads([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));
    assert_eq!(
        stored_media(&store, &account, &chat, &client_id)
            .message
            .status,
        DeliveryStatus::Pending,
        "a drop is not a failure the user sees"
    );
    let at = |hours: i64| Timestamp::from_millis(Timestamp::now().as_millis() + hours * 3_600_000);
    let config = SyncConfig::default().outbox;
    mock.fail_next_sends([ProviderError::Transient("eof".into())]);
    let pass = crate::run_outbox_pass(&store, &mock, &config, at(1))
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    let pass = crate::run_outbox_pass(&store, &mock, &config, at(2))
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.uploaded_count(), 1, "one upload under one key");
    let asked = mock.sent();
    assert!(asked.len() >= 2, "the send was repeated");
    assert!(
        asked.iter().all(|message| message.client_id == client_id),
        "with the same client id every time"
    );
    let thread: Vec<_> = store
        .messages(&account, &chat, 500)
        .unwrap()
        .into_iter()
        .filter(|m| m.message.client_id.as_ref() == Some(&client_id))
        .collect();
    assert_eq!(thread.len(), 1, "one bubble");
    assert_eq!(thread[0].message.status, DeliveryStatus::Sent);
}

#[tokio::test(start_paused = true)]
async fn a_sticker_sent_while_replying_answers_that_message() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let quoted = store
        .messages(&account, &chat, 5)
        .unwrap()
        .remove(0)
        .message
        .id;
    let (item, _) = engine
        .library_save_sticker(still_webp(32, 32), "image/webp", LibrarySource::Sent, false)
        .unwrap();
    let client_id = engine
        .send_library_item(&account, &chat, &item.id, Some(quoted.clone()))
        .unwrap();
    engine.flush_outbox().await.unwrap();
    let sent = mock.sent().pop().unwrap();
    assert_eq!(sent.client_id, client_id);
    assert_eq!(sent.reply_to, Some(quoted));
}

#[tokio::test(start_paused = true)]
async fn a_saved_gif_goes_as_a_video_flagged_to_play_as_one() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();
    let item = engine
        .library_import_gif(mp4(7), Some("wave".into()), LibrarySource::Imported)
        .unwrap();
    assert_eq!(item.mime, "video/mp4");
    assert!(item.animated && !item.has_thumb, "no decoder: no still");

    let client_id = engine
        .send_library_item(&account, &chat, &item.id, None)
        .unwrap();
    let media = media_of(&store, &account, &chat, &client_id);
    assert_eq!(media.kind, MediaKind::Video);
    assert!(media.gif, "the bubble says GIF from the start");
    engine.flush_outbox().await.unwrap();
    let sent = mock.sent().pop().unwrap();
    assert!(matches!(
        sent.content,
        OutgoingContent::Media { kind: MediaKind::Video, gif: true, ref mime_type, .. }
            if mime_type.as_deref() == Some("video/mp4")
    ));
    assert_eq!(mock.uploaded()[0].1, "video/mp4");
}

#[tokio::test(start_paused = true)]
async fn a_gif_file_goes_as_the_image_it_is_because_the_api_converts_nothing() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let item = engine
        .library_import_gif(animated_gif(40, 30, 3, 80), None, LibrarySource::Imported)
        .unwrap();
    assert_eq!(item.mime, "image/gif");
    assert_eq!(item.size, Some((40, 30)));
    assert!(item.animated && item.has_thumb);
    engine
        .send_library_item(&account, &chat, &item.id, None)
        .unwrap();
    engine.flush_outbox().await.unwrap();
    let sent = mock.sent().pop().unwrap();
    assert!(matches!(
        sent.content,
        OutgoingContent::Media {
            kind: MediaKind::Image,
            gif: false,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn a_provider_that_takes_no_files_refuses_before_anything_is_queued() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let (item, _) = engine
        .library_save_sticker(still_webp(32, 32), "image/webp", LibrarySource::Sent, false)
        .unwrap();
    mock.set_upload_limit(10);
    assert!(matches!(
        engine.send_library_item(&account, &chat, &item.id, None),
        Err(crate::SendMediaError::TooLarge { .. })
    ));
    assert!(matches!(
        engine.send_library_item(&account, &chat, "no such item", None),
        Err(crate::SendMediaError::Empty)
    ));
    assert_eq!(engine.store().outbox_pending().unwrap().len(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_import_is_a_512_sticker_with_a_still_for_the_picker() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let png = {
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            300,
            200,
            image::Rgba([10, 200, 90, 255]),
        ))
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
        out
    };
    let (item, notes) = engine
        .library_import_sticker(&png, Some("green".into()), None)
        .unwrap();
    assert_eq!(item.size, Some((512, 512)));
    assert_eq!(item.mime, "image/webp");
    assert_eq!(item.source, LibrarySource::Imported);
    assert_eq!(item.name.as_deref(), Some("green"));
    assert!(item.has_thumb);
    assert!(notes.is_empty());
    let (bytes, _) = engine.store().library_file(&item.id).unwrap().unwrap();
    assert_eq!(content_id(&bytes), item.id);
    assert!(matches!(
        engine.library_import_sticker(b"plain text", None, None),
        Err(crate::LibraryError::Import(crate::ImportError::NotAnImage))
    ));
    let animated = animated_webp(64, 64, &[80, 80]);
    let (moving, _) = engine
        .library_import_sticker(&animated, None, None)
        .unwrap();
    assert!(moving.animated);
}

// ----- favorites on the phone -------------------------------------------------------

async fn synced(engine: &SyncEngine, account: &AccountId) -> usize {
    engine.reconcile_favorites(account).await.unwrap()
}

#[tokio::test(start_paused = true)]
async fn what_is_starred_on_the_phone_comes_into_the_library_as_a_favorite() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let one = still_webp(40, 40);
    let two = animated_webp(40, 40, &[60, 60]);
    mock.star_sticker_on_phone(&account, "r1", one.clone(), "image/webp");
    mock.star_sticker_on_phone(&account, "r2", two.clone(), "image/webp");

    assert_eq!(synced(&engine, &account).await, 2);
    let favorites = store.library_favorites(LibraryKind::Sticker).unwrap();
    assert_eq!(favorites.len(), 2);
    assert!(favorites
        .iter()
        .all(|item| item.source == LibrarySource::Phone));
    assert_eq!(favorites[0].id, content_id(&one));
    assert!(favorites[1].animated);
    assert_eq!(
        store.library_file(&content_id(&two)).unwrap().unwrap().0,
        two
    );
    // Nothing of it is sent back to the phone, and a second run is quiet.
    assert_eq!(mock.sticker_calls(), vec!["list".to_owned()]);
    assert_eq!(synced(&engine, &account).await, 0);
    assert_eq!(mock.favorite_sticker_ids(&account), vec!["r1", "r2"]);
}

#[tokio::test(start_paused = true)]
async fn a_star_put_here_reaches_the_phone_and_a_star_taken_off_there_leaves_here() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let webp = still_webp(48, 48);
    let (item, _) = engine
        .library_save_sticker(webp.clone(), "image/webp", LibrarySource::Received, false)
        .unwrap();

    // Starred here: the phone learns it.
    engine.set_library_favorite(&item.id, true).unwrap();
    // Run what the star started, and then ask once more by hand.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    synced(&engine, &account).await;
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id.clone()]);

    // Unstarred on the phone: it is not a favorite here any more, and it
    // is still in the library.
    mock.unstar_sticker_on_phone(&account, &item.id);
    assert_eq!(synced(&engine, &account).await, 1);
    let kept = store.library_item(&item.id).unwrap().unwrap();
    assert!(!kept.favorite);
    // And it is not pushed back by the next run.
    synced(&engine, &account).await;
    assert!(mock.favorite_sticker_ids(&account).is_empty());

    // Starred again here, then unstarred here: the phone follows both.
    store.library_set_favorite(&item.id, true).unwrap();
    synced(&engine, &account).await;
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id.clone()]);
    store.library_set_favorite(&item.id, false).unwrap();
    synced(&engine, &account).await;
    assert!(mock.favorite_sticker_ids(&account).is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_favorite_that_could_not_be_pushed_is_pushed_at_the_next_run() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let (item, _) = engine
        .library_save_sticker(
            still_webp(48, 48),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    engine.store().library_set_favorite(&item.id, true).unwrap();
    // The list goes through, and the add drops.
    mock.fail_sticker_calls_after(1, [ProviderError::Transient("eof".into())]);
    let dropped = engine.reconcile_favorites(&account).await;
    assert!(matches!(dropped, Err(ref error) if error.is_transient()));
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert!(
        engine.store().library_remote(&account).unwrap().is_empty(),
        "an unanswered push is not remembered as done"
    );
    // The star is still here, and the next run does it, once.
    synced(&engine, &account).await;
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id.clone()]);
    synced(&engine, &account).await;
    let adds = mock
        .sticker_calls()
        .into_iter()
        .filter(|call| call.starts_with("add"))
        .count();
    assert_eq!(adds, 2, "the failed one and the one that went through");
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id]);
}

#[tokio::test(start_paused = true)]
async fn a_provider_without_favorites_is_never_asked() {
    // The trait's default answers "unsupported"; the engine does not ask
    // when the capability is off.
    struct Plain(MockProvider);
    #[async_trait]
    impl Provider for Plain {
        fn id(&self) -> &'static str {
            "plain"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                sticker_favorites: false,
                ..self.0.capabilities()
            }
        }
        async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
            self.0.list_accounts().await
        }
        async fn list_chats(&self, a: &AccountId, c: Option<Cursor>) -> ProviderResult<Page<Chat>> {
            self.0.list_chats(a, c).await
        }
        async fn fetch_messages(
            &self,
            a: &AccountId,
            c: &ChatId,
            k: Option<Cursor>,
            n: u32,
        ) -> ProviderResult<Page<Message>> {
            self.0.fetch_messages(a, c, k, n).await
        }
        async fn send(&self, m: OutgoingMessage) -> ProviderResult<SendReceipt> {
            self.0.send(m).await
        }
        async fn mark_read(
            &self,
            a: &AccountId,
            c: &ChatId,
            u: Option<&MessageId>,
        ) -> ProviderResult<()> {
            self.0.mark_read(a, c, u).await
        }
        async fn download_media(&self, a: &AccountId, m: &MediaRef) -> ProviderResult<MediaData> {
            self.0.download_media(a, m).await
        }
        async fn subscribe(&self) -> ProviderResult<EventStream> {
            self.0.subscribe().await
        }
    }
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Plain(mock.clone())),
        SyncConfig::default(),
        Handle::current(),
    );
    engine.refresh().await.unwrap();
    let account = engine.store().accounts().unwrap().remove(0).id;
    let (item, _) = engine
        .library_save_sticker(
            still_webp(32, 32),
            "image/webp",
            LibrarySource::Received,
            true,
        )
        .unwrap();
    engine.want_favorite_stickers(&account);
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert!(mock.sticker_calls().is_empty(), "never asked");
    // The star is local, and stays.
    assert!(
        engine
            .store()
            .library_item(&item.id)
            .unwrap()
            .unwrap()
            .favorite
    );
    assert!(matches!(
        Plain(mock).list_favorite_stickers(&account).await,
        Err(ProviderError::Unsupported(_))
    ));
}

// ----- upgrading ----------------------------------------------------------------

/// The schema the owner's database is at today (v8, sends measured) has no
/// library; v9 adds its tables and changes nothing that exists.
#[test]
fn a_database_at_schema_v8_is_brought_to_the_library_schema() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v8.db");
    {
        let mut raw = rusqlite::Connection::open(&path).unwrap();
        crate::migrations::migrate_to(&mut raw, 8).unwrap();
        // The library's tables are not there yet.
        let tables: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name LIKE 'library_%'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        raw.execute(
            "INSERT INTO messages (account_id, chat_id, id, sender, outgoing, ts, body, content, status)
             VALUES ('acc', 'chat', 'old', 'them', 0, 1, 'from before', ?1, 'delivered')",
            [serde_json::to_string(&MessageContent::text("from before")).unwrap()],
        )
        .unwrap();
    }
    let store = Store::open(&path, None).unwrap();
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    const { assert!(SCHEMA_VERSION >= 9) };

    // What was there is read, and the library works.
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    assert_eq!(store.messages(&account(), &chat_id(), 10).unwrap().len(), 1);
    assert_eq!(store.library_stats().unwrap(), Default::default());
    let item = add(&store, LibraryKind::Sticker, blob(5, 80), 1);
    store.library_set_favorite(&item.id, true).unwrap();
    store
        .library_set_remote(&account(), &item.id, "remote-1")
        .unwrap();
    assert_eq!(
        store.library_remote(&account()).unwrap(),
        vec![(item.id.clone(), "remote-1".to_owned())]
    );

    // Opening again migrates nothing and loses nothing.
    drop(store);
    let store = Store::open(&path, None).unwrap();
    assert!(store.library_item(&item.id).unwrap().unwrap().favorite);
    assert_eq!(store.messages(&account(), &chat_id(), 10).unwrap().len(), 1);
}

// ----- taking a favorite out of the library ----------------------------------------

/// A sticker starred here and known to the phone, with its star there.
async fn starred_on_both(
    mock: &MockProvider,
    engine: &SyncEngine,
    account: &AccountId,
    side: u32,
) -> LibraryItem {
    let (item, _) = engine
        .library_save_sticker(
            still_webp(side, side),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    engine.store().library_set_favorite(&item.id, true).unwrap();
    synced(engine, account).await;
    assert!(mock.favorite_sticker_ids(account).contains(&item.id));
    item
}

#[tokio::test(start_paused = true)]
async fn a_favorite_taken_out_of_the_library_loses_its_star_on_the_phone_and_stays_out() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let item = starred_on_both(&mock, &engine, &account, 48).await;

    assert!(store.library_remove(&item.id).unwrap());
    synced(&engine, &account).await;
    assert!(
        mock.favorite_sticker_ids(&account).is_empty(),
        "the phone is told the star is gone"
    );
    assert!(store.library_item(&item.id).unwrap().is_none());
    // And the next runs do not bring it back.
    assert_eq!(synced(&engine, &account).await, 0);
    assert!(store.library_item(&item.id).unwrap().is_none());
    assert!(store.library_remote(&account).unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_engine_takes_an_item_out_and_tells_the_phone_by_itself() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    // One that came from the phone, under the phone's own id.
    let bytes = still_webp(40, 40);
    mock.star_sticker_on_phone(&account, "r1", bytes.clone(), "image/webp");
    assert_eq!(synced(&engine, &account).await, 1);
    let id = content_id(&bytes);

    assert!(engine.remove_library_item(&id).unwrap());
    for _ in 0..20 {
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert!(engine.store().library_item(&id).unwrap().is_none());
    // Taking out what is not there says so.
    assert!(!engine.remove_library_item(&id).unwrap());
}

#[tokio::test(start_paused = true)]
async fn a_removal_the_phone_did_not_hear_is_told_again_and_never_undone() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let item = starred_on_both(&mock, &engine, &account, 48).await;

    store.library_remove(&item.id).unwrap();
    // The list goes through, and the removal drops.
    mock.fail_sticker_calls_after(1, [ProviderError::Transient("eof".into())]);
    let dropped = engine.reconcile_favorites(&account).await;
    assert!(matches!(dropped, Err(ref error) if error.is_transient()));
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id.clone()]);
    assert!(
        store.library_item(&item.id).unwrap().is_none(),
        "what the user took out is not pulled back"
    );
    synced(&engine, &account).await;
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert!(store.library_item(&item.id).unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn an_unstarred_sticker_let_go_for_room_still_loses_its_star_on_the_phone() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let item = starred_on_both(&mock, &engine, &account, 48).await;

    // Unstarred here while the phone could not be reached.
    store.library_set_favorite(&item.id, false).unwrap();
    mock.fail_sticker_calls_after(1, [ProviderError::Transient("eof".into())]);
    assert!(engine.reconcile_favorites(&account).await.is_err());
    assert_eq!(mock.favorite_sticker_ids(&account), vec![item.id.clone()]);
    // Then the library let it go to make room.
    assert_eq!(store.library_evict(0, None).unwrap(), 1);
    assert!(store.library_item(&item.id).unwrap().is_none());

    synced(&engine, &account).await;
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert_eq!(synced(&engine, &account).await, 0);
    assert!(
        store.library_item(&item.id).unwrap().is_none(),
        "it does not come back starred"
    );
}

// ----- a backend with ids of its own, and one that is not there yet ------------------

/// A backend that names its favorites itself (not by the hash of the
/// file): what was starred here is known by the id it was answered
/// under, so it stays one favorite and stays starred, and a star taken
/// off here is taken off there under that id.
#[tokio::test(start_paused = true)]
async fn a_favorite_the_backend_lists_under_its_own_id_stays_one_and_stays_starred() {
    let mock = MockProvider::quiet();
    mock.use_own_sticker_ids();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let (item, _) = engine
        .library_save_sticker(
            still_webp(48, 48),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    store.library_set_favorite(&item.id, true).unwrap();
    let remote = format!("fav-{}", item.id);

    synced(&engine, &account).await;
    assert_eq!(mock.favorite_sticker_ids(&account), vec![remote.clone()]);
    assert_eq!(
        store.library_remote(&account).unwrap(),
        [(item.id.clone(), remote.clone())]
    );
    // The next runs find it listed under that id and have nothing to do:
    // not fetched as a new one, not unstarred as one that went.
    assert_eq!(synced(&engine, &account).await, 0);
    assert_eq!(synced(&engine, &account).await, 0);
    let favorites = store.library_favorites(LibraryKind::Sticker).unwrap();
    assert_eq!(ids(&favorites), vec![item.id.clone()]);
    assert_eq!(mock.media_calls(), 0, "its own file was not fetched back");
    let calls = mock.sticker_calls();
    assert_eq!(
        calls.iter().filter(|call| call.starts_with("add")).count(),
        1
    );
    assert!(!calls.iter().any(|call| call.starts_with("remove")));

    // Unstarred here: the phone is told, by the backend's id.
    store.library_set_favorite(&item.id, false).unwrap();
    synced(&engine, &account).await;
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert!(mock.sticker_calls().contains(&format!("remove {remote}")));
    assert!(store.library_remote(&account).unwrap().is_empty());
}

/// A sticker that was seen in a message of the account names that
/// message when it is starred on the phone, so the provider can star it
/// without the file travelling; one that came from nowhere (an import)
/// goes by its file, and so does one seen on another account.
#[tokio::test(start_paused = true)]
async fn a_sticker_seen_in_a_message_names_it_when_it_is_starred_on_the_phone() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let store = engine.store().clone();

    // A sticker of a conversation, kept from its message.
    let url = "https://wa/sticker-file";
    let mut media = Media::new(MediaKind::Sticker);
    media.source = Some(client_provider::MediaRef::new(url));
    media.mime_type = Some("image/webp".into());
    let mut message = text("m_sticker", chat.as_str(), 5, "", Direction::Incoming);
    message.account_id = account.clone();
    message.content = MessageContent::Media(media);
    store.upsert_message(&message).unwrap();
    let (seen, _) = engine
        .library_save_sticker(
            still_webp(48, 48),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    engine
        .library_seen_in(&seen.id, &account, &client_provider::MediaRef::new(url))
        .unwrap();
    let kept = store.library_item(&seen.id).unwrap().unwrap();
    assert_eq!(kept.origin, Some((account.clone(), url.to_owned())));
    assert_eq!(kept.origin_message, Some(MessageId::new("m_sticker")));

    // One the user imported, and one seen on another number.
    let (imported, _) = engine
        .library_save_sticker(
            still_webp(40, 40),
            "image/webp",
            LibrarySource::Imported,
            false,
        )
        .unwrap();
    let (foreign, _) = engine
        .library_save_sticker(
            still_webp(44, 44),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    store
        .library_set_origin(
            &foreign.id,
            &AccountId::new("another-number"),
            "https://wa/elsewhere",
            Some(&MessageId::new("m_elsewhere")),
        )
        .unwrap();
    // A message that only this computer has is no message to name.
    assert_eq!(
        store
            .message_with_media(&account, "https://wa/nowhere")
            .unwrap(),
        None
    );

    for item in [&seen, &imported, &foreign] {
        store.library_set_favorite(&item.id, true).unwrap();
    }
    synced(&engine, &account).await;
    let named = mock.sticker_messages_named();
    let adds: Vec<String> = mock
        .sticker_calls()
        .into_iter()
        .filter(|call| call.starts_with("add"))
        .collect();
    assert_eq!(adds.len(), 3);
    let by_item = |id: &str| {
        let at = adds
            .iter()
            .position(|call| call == &format!("add {id}"))
            .unwrap();
        named[at].clone()
    };
    assert_eq!(by_item(&seen.id), Some(MessageId::new("m_sticker")));
    assert_eq!(by_item(&imported.id), None);
    assert_eq!(by_item(&foreign.id), None, "another number's message");
}

/// The provider has favorite stickers and this number does not have them
/// yet (the backend turns them on one number at a time): that is not an
/// error. Favorites starred here stay, nothing is retried in a loop, the
/// view is told so it can say "not available yet for this number", and
/// when the number has them the stars go over by themselves.
#[tokio::test(start_paused = true)]
async fn favorites_that_are_not_there_yet_for_a_number_are_not_an_error() {
    use client_provider::Feature;
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, _) = first_chat(&engine).await;
    let store = engine.store().clone();
    let (item, _) = engine
        .library_save_sticker(
            still_webp(48, 48),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    let (second, _) = engine
        .library_save_sticker(
            still_webp(40, 40),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    assert!(engine.capabilities().sticker_favorites);
    assert!(engine.capabilities_for(&account).sticker_favorites);
    mock.set_unavailable(Some(&account), Feature::StickerFavorites, true);

    let mut changes = store.subscribe();
    // Starring starts a run in the background, as it always does.
    engine.set_library_favorite(&item.id, true).unwrap();
    store.library_set_favorite(&second.id, true).unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    // The run ended on "not available", said as that and nothing else.
    let outcome = engine.reconcile_favorites(&account).await;
    assert!(
        matches!(outcome, Err(ProviderError::Unsupported(_))),
        "{outcome:?}"
    );
    assert!(engine.feature_unavailable(&account, Feature::StickerFavorites));
    assert!(!engine.capabilities_for(&account).sticker_favorites);
    assert!(
        !engine.feature_unavailable(&AccountId::new("another"), Feature::StickerFavorites),
        "it is this number that does not have them"
    );
    // The stars made here are untouched, and nothing was written there.
    let favorites = store.library_favorites(LibraryKind::Sticker).unwrap();
    assert_eq!(favorites.len(), 2);
    assert!(mock.favorite_sticker_ids(&account).is_empty());
    assert!(store.library_remote(&account).unwrap().is_empty());
    // It was not shown as a problem.
    let mut problems = 0;
    while let Some(change) = changes.try_next() {
        if matches!(change, StoreChange::Problem { .. }) {
            problems += 1;
        }
    }
    assert_eq!(problems, 0);
    // And nothing spins: a long while later, no run was started by itself.
    let lists = |mock: &MockProvider| {
        mock.sticker_calls()
            .iter()
            .filter(|call| *call == "list")
            .count()
    };
    let before = lists(&mock);
    tokio::time::sleep(Duration::from_secs(3_600)).await;
    assert_eq!(lists(&mock), before);

    // The number has them now: the next look brings both stars over.
    mock.set_unavailable(Some(&account), Feature::StickerFavorites, false);
    synced(&engine, &account).await;
    assert_eq!(mock.favorite_sticker_ids(&account).len(), 2);
    assert!(!engine.feature_unavailable(&account, Feature::StickerFavorites));
}

/// The view is told when what the phone's favorites can be had changes,
/// so that its line is right without anybody asking.
#[tokio::test(start_paused = true)]
async fn the_view_hears_when_the_phones_favorites_stop_being_available() {
    use client_provider::Feature;

    /// A mock that finds out it has no favorites when it is first asked
    /// to change them, as a backend does.
    struct LearnsLate(MockProvider, AccountId);
    #[async_trait]
    impl Provider for LearnsLate {
        fn id(&self) -> &'static str {
            "learns-late"
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        fn unavailable(&self, account: &AccountId, feature: Feature) -> bool {
            self.0.unavailable(account, feature)
        }
        async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
            self.0.list_accounts().await
        }
        async fn list_chats(&self, a: &AccountId, c: Option<Cursor>) -> ProviderResult<Page<Chat>> {
            self.0.list_chats(a, c).await
        }
        async fn fetch_messages(
            &self,
            a: &AccountId,
            c: &ChatId,
            cursor: Option<Cursor>,
            limit: u32,
        ) -> ProviderResult<Page<Message>> {
            self.0.fetch_messages(a, c, cursor, limit).await
        }
        async fn send(&self, m: OutgoingMessage) -> ProviderResult<SendReceipt> {
            self.0.send(m).await
        }
        async fn mark_read(
            &self,
            a: &AccountId,
            c: &ChatId,
            up_to: Option<&MessageId>,
        ) -> ProviderResult<()> {
            self.0.mark_read(a, c, up_to).await
        }
        async fn download_media(
            &self,
            a: &AccountId,
            m: &client_provider::MediaRef,
        ) -> ProviderResult<client_provider::MediaData> {
            self.0.download_media(a, m).await
        }
        async fn subscribe(&self) -> ProviderResult<client_provider::EventStream> {
            self.0.subscribe().await
        }
        async fn list_favorite_stickers(
            &self,
            a: &AccountId,
        ) -> ProviderResult<Vec<client_provider::FavoriteSticker>> {
            self.0.list_favorite_stickers(a).await
        }
        async fn add_favorite_sticker(
            &self,
            a: &AccountId,
            _sticker: client_provider::StickerFile,
        ) -> ProviderResult<String> {
            self.0
                .set_unavailable(Some(&self.1), Feature::StickerFavorites, true);
            let _ = a;
            Err(ProviderError::Unsupported("favorite stickers"))
        }
    }

    let mock = MockProvider::quiet();
    let plain = engine_for(&mock);
    let (account, _) = first_chat(&plain).await;
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(LearnsLate(mock.clone(), account.clone())),
        SyncConfig::default(),
        Handle::current(),
    );
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (item, _) = engine
        .library_save_sticker(
            still_webp(48, 48),
            "image/webp",
            LibrarySource::Received,
            false,
        )
        .unwrap();
    let mut changes = store.subscribe();
    while changes.try_next().is_some() {}
    assert!(!engine.feature_unavailable(&account, Feature::StickerFavorites));
    engine.set_library_favorite(&item.id, true).unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(engine.feature_unavailable(&account, Feature::StickerFavorites));
    let mut told = 0;
    while let Some(change) = changes.try_next() {
        if change == StoreChange::Library {
            told += 1;
        }
    }
    assert!(
        told >= 2,
        "the star, and then that the phone cannot have it"
    );
    assert!(store.library_item(&item.id).unwrap().unwrap().favorite);
}
