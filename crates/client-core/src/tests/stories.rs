//! Stories: the store's tables and rules (expiry at 24 hours with a
//! controlled clock, what was seen, the order of the lists, wiping), and
//! the engine's reads, posts, receipts, replies, mutes and audience,
//! against the mock provider.

use super::{chat, engine_for, seed_account, text};
use crate::imaging::tests::png;
use crate::*;
use async_trait::async_trait;
use client_provider::{
    AccountId, Capabilities, ChatId, ClientMessageId, Contact, ContactId, Cursor, DeliveryStatus,
    Direction, EventStream, MediaData, MediaKind, MediaRef, Message, MessageId, NewStory,
    NewStoryContent, OutgoingMessage, Page, Provider, ProviderError, ProviderEvent, ProviderResult,
    SendReceipt, Story, StoryAudience, StoryBody, StoryFont, StoryPrivacy, StoryStyle, StoryViewer,
    Timestamp, STORY_LIFETIME,
};
use provider_mock::{MockConfig, MockProvider};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Handle;

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const T0: i64 = 1_800_000_000_000;

fn at(offset_ms: i64) -> Timestamp {
    Timestamp::from_millis(T0 + offset_ms)
}

fn account() -> AccountId {
    AccountId::new("acc")
}

fn story(id: &str, author: &str, posted: i64, words: &str) -> Story {
    Story {
        id: MessageId::new(id),
        client_id: None,
        account_id: account(),
        author: ContactId::new(author),
        author_name: Some(format!("name of {author}")),
        mine: false,
        posted_at: at(posted),
        expires_at: None,
        body: StoryBody::Text {
            text: words.into(),
            style: StoryStyle {
                background: 0x112233,
                font: StoryFont(7),
            },
        },
        mentions: Vec::new(),
        viewed: false,
        view_count: None,
    }
}

fn image_story(id: &str, author: &str, posted: i64, url: &str) -> Story {
    let mut media = client_provider::Media::new(MediaKind::Image);
    media.source = Some(MediaRef::new(url));
    Story {
        body: StoryBody::Media(media),
        ..story(id, author, posted, "")
    }
}

fn store() -> Store {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    // `acc` is not one of the mock's: the account the store tests use.
    store
}

fn names(authors: &[StoryAuthor]) -> Vec<String> {
    authors.iter().map(|author| author.key.clone()).collect()
}

// ----- the store ----------------------------------------------------------

#[test]
fn stories_are_listed_by_author_with_the_name_every_view_uses() {
    let store = store();
    let mut ana = Contact::new(account(), ContactId::new("+5841"));
    ana.saved_name = Some("Ana (saved)".into());
    store.upsert_contact(&ana).unwrap();
    store
        .upsert_story(&story("s1", "+5841", -3 * HOUR, "one"), at(0))
        .unwrap();
    store
        .upsert_story(&story("s2", "+5841", -HOUR, "two"), at(0))
        .unwrap();
    store
        .upsert_story(&story("s3", "lid:9", -2 * HOUR, "three"), at(0))
        .unwrap();

    let feed = store.story_feed(&account(), at(0)).unwrap();
    assert_eq!(feed.recent.len(), 2);
    let ana = &feed.recent[0];
    // Newest author first, and her stories oldest first.
    assert_eq!(ana.name.as_deref(), Some("Ana (saved)"));
    let ids: Vec<_> = ana
        .stories
        .iter()
        .map(|item| item.story.id.as_str())
        .collect();
    assert_eq!(ids, ["s1", "s2"]);
    assert_eq!(ana.unviewed(), 2);
    assert_eq!(ana.start_index(), 0);
    // One who is not in the address book: the name their story came with.
    assert_eq!(feed.recent[1].name.as_deref(), Some("name of lid:9"));
    assert_eq!(feed.unviewed(), 3);
}

#[test]
fn recent_viewed_and_muted_are_ordered_and_a_view_moves_an_author() {
    let store = store();
    for (id, author, posted) in [
        ("a1", "+1", -5 * HOUR),
        ("b1", "+2", -HOUR),
        ("c1", "+3", -2 * HOUR),
        ("d1", "+4", -30 * MINUTE),
        ("e1", "+5", -3 * HOUR),
    ] {
        store
            .upsert_story(&story(id, author, posted, id), at(0))
            .unwrap();
    }
    // +5 is muted; +3's story was seen on another device.
    store
        .set_story_muted(&account(), &ContactId::new("+5"), true)
        .unwrap();
    let mut seen = story("c1", "+3", -2 * HOUR, "c1");
    seen.viewed = true;
    store.upsert_story(&seen, at(0)).unwrap();

    let feed = store.story_feed(&account(), at(0)).unwrap();
    assert_eq!(names(&feed.recent), ["+4", "+2", "+1"], "newest first");
    assert_eq!(names(&feed.viewed), ["+3"]);
    assert_eq!(names(&feed.muted), ["+5"]);
    // Muted authors do not count as news.
    assert_eq!(feed.unviewed(), 3);
    assert!(feed.muted[0].muted);

    // Seeing +1's only story moves them to Viewed, behind the newer ones.
    assert!(store
        .mark_story_viewed(&account(), &MessageId::new("a1"), at(1))
        .unwrap());
    let feed = store.story_feed(&account(), at(0)).unwrap();
    assert_eq!(names(&feed.recent), ["+4", "+2"]);
    assert_eq!(names(&feed.viewed), ["+3", "+1"]);

    // Unmuting brings the author back where their stories belong.
    store
        .set_story_muted(&account(), &ContactId::new("+5"), false)
        .unwrap();
    let feed = store.story_feed(&account(), at(0)).unwrap();
    assert!(feed.muted.is_empty());
    assert_eq!(names(&feed.recent), ["+4", "+2", "+5"]);
}

#[test]
fn a_story_is_seen_once_and_listing_it_again_does_not_unsee_it() {
    let store = store();
    let s = story("s1", "+1", -HOUR, "x");
    store.upsert_story(&s, at(0)).unwrap();
    assert!(!store.story(&account(), &s.id).unwrap().unwrap().viewed);
    assert!(store.mark_story_viewed(&account(), &s.id, at(5)).unwrap());
    assert!(!store.mark_story_viewed(&account(), &s.id, at(9)).unwrap());
    // The provider lists it again, as new as ever: still seen, and the
    // time it was shown is the first.
    store.upsert_story(&s, at(10)).unwrap();
    let kept = store.story(&account(), &s.id).unwrap().unwrap();
    assert!(kept.viewed);
    assert_eq!(kept.viewed_at, Some(at(5)));
    // Listing, as the engine does, changes nothing either.
    store
        .sync_stories(&account(), std::slice::from_ref(&s), at(11))
        .unwrap();
    assert!(store.story(&account(), &s.id).unwrap().unwrap().viewed);
}

#[test]
fn the_ring_counts_stories_and_marks_the_ones_seen() {
    let store = store();
    for (id, posted) in [("s1", -3 * HOUR), ("s2", -2 * HOUR), ("s3", -HOUR)] {
        store
            .upsert_story(&story(id, "+1", posted, id), at(0))
            .unwrap();
    }
    store
        .mark_story_viewed(&account(), &MessageId::new("s1"), at(1))
        .unwrap();
    let rings = store.story_rings(&account(), at(0)).unwrap();
    assert_eq!(
        rings.get(&ContactId::new("+1")),
        Some(&StoryRing {
            total: 3,
            unviewed: 2
        })
    );
    assert_eq!(rings.get(&ContactId::new("+2")), None);
    // All seen: a ring with nothing new in it.
    for id in ["s2", "s3"] {
        store
            .mark_story_viewed(&account(), &MessageId::new(id), at(2))
            .unwrap();
    }
    let ring = store.story_rings(&account(), at(0)).unwrap()[&ContactId::new("+1")];
    assert_eq!((ring.total, ring.unviewed), (3, 0));
}

#[test]
fn a_story_expires_a_day_after_it_was_posted_and_takes_its_media_with_it() {
    let store = store();
    let posted = image_story("s1", "+1", 0, "https://media.example/story");
    store.upsert_story(&posted, at(MINUTE)).unwrap();
    // Cached for it: its thumbnail and its file.
    let cached = CachedMedia {
        bytes: vec![1, 2, 3],
        mime: Some("image/png".into()),
        size: Some((1, 1)),
    };
    for key in [
        thumbnail_key("https://media.example/story"),
        file_key("https://media.example/story"),
    ] {
        store.put_media(&key, &cached, at(MINUTE), 1 << 20).unwrap();
    }
    store
        .put_media(
            "thumb:https://other.example/keep",
            &cached,
            at(MINUTE),
            1 << 20,
        )
        .unwrap();
    store
        .put_story_viewers(
            &account(),
            &MessageId::new("s1"),
            &[StoryViewer {
                contact: ContactId::new("+7"),
                name: None,
                viewed_at: at(2 * MINUTE),
                reaction: None,
            }],
        )
        .unwrap();

    let day = STORY_LIFETIME.as_millis() as i64;
    assert_eq!(store.next_story_expiry().unwrap(), Some(at(day)));
    // One minute before: still there, still listed.
    assert_eq!(store.expire_stories(at(day - MINUTE)).unwrap(), 0);
    assert_eq!(
        store
            .story_feed(&account(), at(day - MINUTE))
            .unwrap()
            .recent
            .len(),
        1
    );
    // At 24 hours the feed no longer lists it, even before the cleanup...
    assert!(store
        .story_feed(&account(), at(day))
        .unwrap()
        .recent
        .is_empty());
    // ...and the cleanup takes the row, the viewers and the cached media.
    assert_eq!(store.expire_stories(at(day)).unwrap(), 1);
    assert!(store.story(&account(), &posted.id).unwrap().is_none());
    assert!(store
        .story_viewers(&account(), &posted.id)
        .unwrap()
        .is_empty());
    for key in [
        thumbnail_key("https://media.example/story"),
        file_key("https://media.example/story"),
    ] {
        assert!(store.media(&key, at(day)).unwrap().is_none(), "{key}");
    }
    assert!(store
        .media("thumb:https://other.example/keep", at(day))
        .unwrap()
        .is_some());
    assert_eq!(store.next_story_expiry().unwrap(), None);
}

#[test]
fn a_story_that_has_already_expired_is_not_kept() {
    let store = store();
    let old = story("old", "+1", -25 * HOUR, "gone");
    assert!(!store.upsert_story(&old, at(0)).unwrap());
    assert!(store.story(&account(), &old.id).unwrap().is_none());
    // A listing that still carries one drops it too.
    let fresh = story("fresh", "+1", -HOUR, "here");
    assert_eq!(
        store
            .sync_stories(&account(), &[old, fresh], at(0))
            .unwrap(),
        1
    );
    assert_eq!(store.story_feed(&account(), at(0)).unwrap().recent.len(), 1);
}

#[test]
fn a_listing_is_the_whole_truth_and_storage_is_bounded() {
    let store = store();
    store
        .sync_stories(
            &account(),
            &[story("a", "+1", -HOUR, "a"), story("b", "+2", -HOUR, "b")],
            at(0),
        )
        .unwrap();
    // "a" was taken down: the next listing does not have it.
    store
        .sync_stories(&account(), &[story("b", "+2", -HOUR, "b")], at(MINUTE))
        .unwrap();
    assert!(store
        .story(&account(), &MessageId::new("a"))
        .unwrap()
        .is_none());
    assert!(store
        .story(&account(), &MessageId::new("b"))
        .unwrap()
        .is_some());

    // The oldest go when there are more than the store keeps.
    let many: Vec<Story> = (0..STORIES_KEPT + 20)
        .map(|n| {
            story(
                &format!("m{n}"),
                &format!("+{}", n % 50),
                -(n as i64) * 1000,
                "x",
            )
        })
        .collect();
    store.sync_stories(&account(), &many, at(0)).unwrap();
    let held: usize = store
        .story_feed(&account(), at(0))
        .unwrap()
        .recent
        .iter()
        .map(|author| author.stories.len())
        .sum();
    assert_eq!(held, STORIES_KEPT);
    assert!(store
        .story(&account(), &MessageId::new("m0"))
        .unwrap()
        .is_some());
    assert!(store
        .story(
            &account(),
            &MessageId::new(format!("m{}", STORIES_KEPT + 19))
        )
        .unwrap()
        .is_none());
}

#[test]
fn a_provider_that_cannot_read_a_text_story_back_does_not_wipe_its_style() {
    let store = store();
    let mut mine = story("m1", "me", -MINUTE, "hello");
    mine.mine = true;
    store.upsert_story(&mine, at(0)).unwrap();
    // The provider lists it again with the defaults.
    let mut plain = mine.clone();
    plain.body = StoryBody::Text {
        text: "hello".into(),
        style: StoryStyle::default(),
    };
    store.upsert_story(&plain, at(1)).unwrap();
    let kept = store.story(&account(), &mine.id).unwrap().unwrap();
    assert_eq!(kept.story.body, mine.body);
    // A story whose words changed is the provider's to say.
    let mut edited = plain.clone();
    edited.body = StoryBody::Text {
        text: "other words".into(),
        style: StoryStyle::default(),
    };
    store.upsert_story(&edited, at(2)).unwrap();
    let now = store.story(&account(), &mine.id).unwrap().unwrap();
    assert_eq!(now.story.body, edited.body);
}

#[test]
fn removing_an_account_wipes_its_stories_and_everything_kept_for_them() {
    let store = store();
    let s = story("s1", "+1", -HOUR, "x");
    store.upsert_story(&s, at(0)).unwrap();
    store
        .set_story_muted(&account(), &ContactId::new("+1"), true)
        .unwrap();
    store
        .put_story_privacy(&account(), &StoryPrivacy::default(), at(0))
        .unwrap();
    store
        .owe_story_receipt(&account(), &s.id, &s.author, at(0))
        .unwrap();
    store
        .enqueue_story_post(
            &NewStory {
                client_id: ClientMessageId::new("p1"),
                account_id: account(),
                content: NewStoryContent::Text {
                    text: "x".into(),
                    style: StoryStyle::default(),
                },
            },
            None,
            at(0),
            Duration::from_secs(3600),
        )
        .unwrap();
    store.remove_account(&account()).unwrap();
    seed_account(&store);
    let feed = store.story_feed(&account(), at(0)).unwrap();
    assert_eq!(feed, StoryFeed::default());
    assert!(store.cached_story_privacy(&account()).unwrap().is_none());
    assert!(store.story_receipts_owed().unwrap().is_empty());
    assert!(store.story_posts_pending().unwrap().is_empty());
    assert!(!store
        .story_muted(&account(), &ContactId::new("+1"))
        .unwrap());
}

// ----- the engine ---------------------------------------------------------

fn personal() -> AccountId {
    AccountId::new("acc_personal")
}

async fn engine_with_stories(mock: &MockProvider) -> (SyncEngine, AccountId) {
    let engine = engine_for(mock);
    engine.refresh().await.unwrap();
    engine.sync_stories(&personal()).await.unwrap();
    (engine, personal())
}

#[tokio::test]
async fn the_engine_copies_the_providers_stories_and_leaves_them_unseen() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let feed = engine.story_feed(&account).unwrap();
    // The mock's seed: authors in each list, one muted, one seen elsewhere.
    assert!(feed.recent.len() >= 3, "{feed:?}");
    assert!(!feed.viewed.is_empty());
    assert_eq!(feed.muted.len(), 1);
    assert!(!feed.mine.is_empty(), "the account's own stories are there");
    // Everything was only listed: nothing counts as seen, nothing was
    // told to anybody.
    assert!(feed
        .recent
        .iter()
        .all(|author| author.stories.iter().all(|item| item.viewed_at.is_none())));
    assert!(mock.story_views().is_empty());
    // The one that is about to expire is there, and the expired are not.
    let soon = Timestamp::from_millis(Timestamp::now().as_millis() + 11 * MINUTE);
    let still: usize = engine
        .store()
        .story_feed(&account, soon)
        .unwrap()
        .recent
        .iter()
        .map(|author| author.stories.len())
        .sum();
    let now: usize = feed.recent.iter().map(|author| author.stories.len()).sum();
    assert_eq!(still, now - 1, "one expires within eleven minutes");
}

#[tokio::test]
async fn a_view_receipt_goes_out_when_the_story_was_shown_and_only_then() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    let author = item.story.author.clone();

    // Listed, prefetched, downloaded: no receipt is owed.
    engine
        .flush_story_receipts_at(Timestamp::now())
        .await
        .unwrap();
    assert!(mock.story_views().is_empty());

    assert!(engine.story_shown(&account, &item.story.id).unwrap());
    let pass = engine
        .flush_story_receipts_at(Timestamp::now())
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(mock.story_views(), vec![(item.story.id.clone(), author)]);
    // Shown again: the same view, no second receipt.
    assert!(!engine.story_shown(&account, &item.story.id).unwrap());
    engine
        .flush_story_receipts_at(Timestamp::now())
        .await
        .unwrap();
    assert_eq!(mock.story_views().len(), 1);
    assert!(engine.store().story_receipts_owed().unwrap().is_empty());
}

#[tokio::test]
async fn with_receipts_off_viewing_is_local_only() {
    for turn_off in 0..3 {
        let mock = MockProvider::quiet();
        let (engine, account) = engine_with_stories(&mock).await;
        let feed = engine.story_feed(&account).unwrap();
        let item = feed.recent[0].stories[0].clone();
        match turn_off {
            // "Send read receipts" is off: the stories follow it.
            0 => engine.set_read_receipts(false),
            // The stories' own switch is off.
            1 => engine.set_story_receipts(false),
            // Turned off after it was seen, before the receipt left.
            _ => {}
        }
        engine.story_shown(&account, &item.story.id).unwrap();
        if turn_off == 2 {
            engine.set_read_receipts(false);
        }
        engine
            .flush_story_receipts_at(Timestamp::now())
            .await
            .unwrap();
        assert!(mock.story_views().is_empty(), "case {turn_off}");
        assert!(engine.store().story_receipts_owed().unwrap().is_empty());
        // It still counts as seen here.
        assert!(
            engine
                .store()
                .story(&account, &item.story.id)
                .unwrap()
                .unwrap()
                .viewed
        );
    }
}

#[tokio::test]
async fn your_own_stories_owe_nobody_a_receipt() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let mine = engine.story_feed(&account).unwrap().mine[0].clone();
    engine.story_shown(&account, &mine.story.id).unwrap();
    engine
        .flush_story_receipts_at(Timestamp::now())
        .await
        .unwrap();
    assert!(mock.story_views().is_empty());
}

#[tokio::test]
async fn a_receipt_that_met_a_dropped_connection_is_sent_once_when_it_comes_back() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    engine.story_shown(&account, &item.story.id).unwrap();
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    let now = Timestamp::now();
    let first = engine.flush_story_receipts_at(now).await.unwrap();
    assert_eq!((first.sent, first.retried), (0, 1));
    assert!(mock.story_views().is_empty());
    // Not due yet: nothing is tried.
    assert_eq!(
        engine.flush_story_receipts_at(now).await.unwrap(),
        ReceiptPass::default()
    );
    let later = Timestamp::from_millis(now.as_millis() + 5 * MINUTE);
    let second = engine.flush_story_receipts_at(later).await.unwrap();
    assert_eq!(second.sent, 1);
    assert_eq!(mock.story_views().len(), 1);
}

#[tokio::test]
async fn a_text_story_shows_under_my_status_at_once_and_is_posted_once() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let before = engine.story_feed(&account).unwrap().mine.len();
    let style = StoryStyle {
        background: 0xAA5500,
        font: StoryFont(2),
    };
    let client_id = engine
        .post_story_text(&account, "  Hello, world  ", style)
        .unwrap();

    // At once, before any network: pending, with its words and style.
    let feed = engine.story_feed(&account).unwrap();
    assert_eq!(feed.mine.len(), before + 1);
    let pending = feed.mine.last().unwrap();
    assert_eq!(pending.post, Some(StoryPostState::Queued));
    assert_eq!(pending.story.client_id.as_ref(), Some(&client_id));
    assert_eq!(
        pending.story.body,
        StoryBody::Text {
            text: "Hello, world".into(),
            style
        }
    );
    assert!(mock.posted_stories().is_empty());

    // The connection drops: the story stays pending, no failure shown.
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    let now = Timestamp::now();
    let pass = engine.flush_story_posts_at(now).await.unwrap();
    assert_eq!((pass.posted, pass.retried), (0, 1));
    assert_eq!(
        engine
            .story_feed(&account)
            .unwrap()
            .mine
            .last()
            .unwrap()
            .post,
        Some(StoryPostState::Queued)
    );

    // Back: posted, with the same client id, and now a story like any.
    let later = Timestamp::from_millis(now.as_millis() + MINUTE);
    let pass = engine.flush_story_posts_at(later).await.unwrap();
    assert_eq!(pass.posted, 1);
    assert_eq!(mock.posted_stories().len(), 1);
    let feed = engine.story_feed(&account).unwrap();
    assert_eq!(feed.mine.len(), before + 1, "one story, not two");
    let posted = feed.mine.last().unwrap();
    assert_eq!(posted.post, None);
    assert_eq!(posted.story.body, pending.story.body);
    assert!(engine.store().story_posts_pending().unwrap().is_empty());

    // The same request repeated (an answer that was lost) is one story.
    let again = mock
        .post_story(NewStory {
            client_id: client_id.clone(),
            account_id: account.clone(),
            content: NewStoryContent::Text {
                text: "Hello, world".into(),
                style,
            },
        })
        .await
        .unwrap();
    assert_eq!(again.id, posted.story.id);
    assert_eq!(mock.posted_stories().len(), 1);
}

#[tokio::test]
async fn a_text_story_is_checked_before_it_is_queued() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let style = StoryStyle::default();
    assert!(matches!(
        engine.post_story_text(&account, "   ", style),
        Err(StoryPostError::Blank)
    ));
    assert!(matches!(
        engine.post_story_text(&account, &"x".repeat(4097), style),
        Err(StoryPostError::TooLong)
    ));
    assert!(engine
        .post_story_text(&account, &"x".repeat(4096), style)
        .is_ok());
}

#[tokio::test]
async fn a_picture_story_is_uploaded_then_posted_and_shows_from_its_own_bytes() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let file = NewMedia {
        kind: MediaKind::Image,
        bytes: png(320, 640),
        mime_type: "image/png".into(),
        file_name: Some("pic.png".into()),
        caption: Some("The view".into()),
        reply_to: None,
        mentions: Vec::new(),
    };
    let prepared = engine.prepare_story_media(file).unwrap();
    let client_id = engine.post_story_prepared(&account, prepared).unwrap();

    // Pending at once, and its small copy is in the cache.
    let feed = engine.story_feed(&account).unwrap();
    let pending = feed.mine.last().unwrap();
    assert_eq!(pending.post, Some(StoryPostState::Queued));
    let local = outbox_local(&client_id);
    assert!(engine
        .store()
        .media_size(&thumbnail_key(&local))
        .unwrap()
        .is_some());

    // The upload drops once: still pending, and the next pass uploads
    // again under the same key; then the post goes.
    mock.fail_next_uploads([ProviderError::Transient("eof".into())]);
    let now = Timestamp::now();
    let first = engine.flush_story_posts_at(now).await.unwrap();
    assert_eq!(first.retried, 1);
    assert_eq!(mock.uploaded_count(), 0);
    let later = Timestamp::from_millis(now.as_millis() + MINUTE);
    let second = engine.flush_story_posts_at(later).await.unwrap();
    assert_eq!(second.posted, 1);
    assert_eq!(mock.uploaded_count(), 1);
    let posted = mock.posted_stories();
    assert_eq!(posted.len(), 1);
    match &posted[0].body {
        StoryBody::Media(media) => assert_eq!(media.caption.as_deref(), Some("The view")),
        other => panic!("{other:?}"),
    }
    // The file's copy moved to the cache, under the client's own key.
    assert!(engine
        .store()
        .media(&file_key(&local), later)
        .unwrap()
        .is_some());
    assert!(engine.store().story_posts_pending().unwrap().is_empty());
}

fn outbox_local(client_id: &ClientMessageId) -> String {
    format!("local:{client_id}")
}

#[tokio::test]
async fn only_a_picture_or_a_video_can_be_a_story_file() {
    let mock = MockProvider::quiet();
    let (engine, _) = engine_with_stories(&mock).await;
    let file = |kind, bytes: Vec<u8>| NewMedia {
        kind,
        bytes,
        mime_type: "application/octet-stream".into(),
        file_name: None,
        caption: None,
        reply_to: None,
        mentions: Vec::new(),
    };
    assert!(matches!(
        engine.prepare_story_media(file(MediaKind::Document, vec![1])),
        Err(StoryPostError::NotAStoryFile)
    ));
    assert!(matches!(
        engine.prepare_story_media(file(MediaKind::Image, Vec::new())),
        Err(StoryPostError::Empty)
    ));
    mock.set_upload_limit(10);
    assert!(matches!(
        engine.prepare_story_media(file(MediaKind::Video, vec![0; 11])),
        Err(StoryPostError::TooLarge { .. })
    ));
}

#[tokio::test]
async fn a_story_the_provider_refuses_fails_visibly_and_can_be_tried_again() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let client_id = engine
        .post_story_text(&account, "will be refused", StoryStyle::default())
        .unwrap();
    mock.fail_next_story_calls([ProviderError::Rejected {
        code: "no_recipients".into(),
        message: "The story has no recipients.".into(),
    }]);
    let now = Timestamp::now();
    assert_eq!(engine.flush_story_posts_at(now).await.unwrap().failed, 1);
    let feed = engine.story_feed(&account).unwrap();
    assert_eq!(
        feed.mine.last().unwrap().post,
        Some(StoryPostState::Failed(
            "The story has no recipients.".into()
        ))
    );
    // Nothing is retried by itself.
    assert_eq!(
        engine.flush_story_posts_at(now).await.unwrap(),
        StoryPass::default()
    );
    // The user tries again: same story, same key, posted.
    assert!(engine.retry_story_post(&client_id).unwrap());
    let retried = engine.flush_story_posts_at(Timestamp::now()).await.unwrap();
    assert_eq!(retried.posted, 1);
    assert_eq!(mock.posted_stories().len(), 1);
}

#[tokio::test]
async fn a_story_that_cannot_be_sent_in_time_fails_instead_of_going_out_late() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    engine
        .post_story_text(&account, "stale", StoryStyle::default())
        .unwrap();
    let much_later = Timestamp::from_millis(Timestamp::now().as_millis() + 13 * HOUR);
    let pass = engine.flush_story_posts_at(much_later).await.unwrap();
    assert_eq!((pass.posted, pass.failed), (0, 1));
    assert!(mock.posted_stories().is_empty());
    // A failed post can be given up.
    let failed = engine.store().story_posts_failed(&account).unwrap();
    assert_eq!(failed.len(), 1);
    assert!(engine.discard_story_post(&failed[0].0).unwrap());
    assert!(engine
        .story_feed(&account)
        .unwrap()
        .mine
        .iter()
        .all(|i| i.post.is_none()));
}

#[tokio::test]
async fn a_reply_is_a_message_to_the_author_that_says_which_story_it_answers() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    let client_id = engine
        .reply_to_story(&account, &item.story.id, "  Nice one!  ", Vec::new())
        .unwrap();

    // In the author's chat at once, pending, marked as a story reply.
    let chat = ChatId::new(item.story.author.as_str());
    let shown = engine.store().messages(&account, &chat, 5).unwrap();
    let pending = shown
        .iter()
        .find(|stored| stored.message.client_id.as_ref() == Some(&client_id))
        .expect("the reply is in the chat");
    let marked = pending.message.extras.story_reply.clone().unwrap();
    assert_eq!(marked.story, item.story.id);
    assert!(!marked.of_mine);

    // The outbox hands it to the provider's call for replies to stories.
    engine.flush_outbox().await.unwrap();
    let replies = mock.story_replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, item.story.id);
    assert_eq!(
        replies[0].1.content,
        client_provider::OutgoingContent::Text {
            body: "Nice one!".into()
        }
    );
    assert_eq!(mock.send_calls(), 1, "one message, not two");
    // The provider's own copy of it comes back without the mark: the
    // mark stays.
    let sent = engine
        .store()
        .messages(&account, &chat, 5)
        .unwrap()
        .into_iter()
        .find(|stored| stored.message.client_id.as_ref() == Some(&client_id))
        .unwrap();
    assert_eq!(
        sent.message.extras.story_reply.unwrap().story,
        item.story.id
    );
    assert_ne!(sent.message.status, DeliveryStatus::Pending);
}

#[tokio::test]
async fn a_reply_that_meets_a_drop_is_sent_once() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    engine
        .reply_to_story(&account, &item.story.id, "hello", Vec::new())
        .unwrap();
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    // Due again after the backoff.
    tokio::time::sleep(Duration::from_millis(1)).await;
    let engine_config = SyncConfig::default();
    let _ = engine_config;
    let mut sent = 0;
    for _ in 0..3 {
        let later = Timestamp::from_millis(Timestamp::now().as_millis() + 10 * MINUTE);
        let pass = run_outbox_pass(
            engine.store(),
            mock_dyn(&mock).as_ref(),
            &OutboxConfig::default(),
            later,
        )
        .await
        .unwrap();
        sent += pass.sent;
    }
    assert_eq!(sent, 1);
    assert_eq!(mock.story_replies().len(), 1);
    assert_eq!(mock.send_calls(), 1);
}

fn mock_dyn(mock: &MockProvider) -> Arc<dyn Provider> {
    Arc::new(mock.clone())
}

#[tokio::test]
async fn a_reaction_to_a_story_goes_through_the_provider_s_call_for_it() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    engine
        .react_to_story(&account, &item.story.id, "🔥")
        .unwrap();
    engine.flush_outbox().await.unwrap();
    let reactions = mock.story_reactions();
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].0, item.story.id);
    assert!(matches!(
        &reactions[0].1.content,
        client_provider::OutgoingContent::Reaction { emoji, .. } if emoji == "🔥"
    ));
}

#[tokio::test]
async fn a_reply_to_a_story_that_is_gone_is_not_queued() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let result = engine.reply_to_story(&account, &MessageId::new("nope"), "x", Vec::new());
    assert!(matches!(result, Err(StoryReplyError::Gone)));
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    assert!(matches!(
        engine.reply_to_story(&account, &item.story.id, "   ", Vec::new()),
        Err(StoryReplyError::Blank)
    ));
}

#[tokio::test(start_paused = true)]
async fn muting_shows_at_once_and_the_provider_hears_of_it_through_drops() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let author = engine.story_feed(&account).unwrap().recent[0]
        .author
        .clone();
    mock.fail_next_story_calls([
        ProviderError::Transient("eof".into()),
        ProviderError::RateLimited { retry_after: None },
    ]);
    engine.set_story_muted(&account, &author, true);
    // At once: the author is under Muted.
    let feed = engine.story_feed(&account).unwrap();
    assert!(feed.muted.iter().any(|muted| muted.author == author));
    assert!(!mock.muted_authors(&account).contains(&author));
    super::settle().await;
    assert!(mock.muted_authors(&account).contains(&author));
    // And back.
    engine.set_story_muted(&account, &author, false);
    super::settle().await;
    assert!(!mock.muted_authors(&account).contains(&author));
    assert!(engine
        .story_feed(&account)
        .unwrap()
        .muted
        .iter()
        .all(|muted| muted.author != author));
}

#[tokio::test(start_paused = true)]
async fn the_audience_is_read_and_changed_and_a_refusal_puts_it_back() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    engine.want_story_privacy(&account);
    super::settle().await;
    let read = engine.story_privacy_cached(&account).unwrap().unwrap();
    assert_eq!(read, mock.privacy_of(&account));
    assert_eq!(read.audience, StoryAudience::ContactsExcept);

    let only = StoryPrivacy {
        audience: StoryAudience::OnlyShareWith,
        only: vec![ContactId::new("contact:ana")],
        ..read.clone()
    };
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    engine.set_story_privacy(&account, only.clone());
    // At once.
    assert_eq!(
        engine.story_privacy_cached(&account).unwrap(),
        Some(only.clone())
    );
    super::settle().await;
    assert_eq!(mock.privacy_of(&account), only);

    // A refusal puts the old one back and says why.
    let mut changes = engine.store().subscribe();
    mock.fail_next_story_calls([ProviderError::Rejected {
        code: "invalid_request".into(),
        message: "Not like that.".into(),
    }]);
    let everyone = StoryPrivacy {
        audience: StoryAudience::Contacts,
        ..only.clone()
    };
    engine.set_story_privacy(&account, everyone);
    super::settle().await;
    assert_eq!(engine.story_privacy_cached(&account).unwrap(), Some(only));
    let mut problem = None;
    while let Some(change) = changes.try_next() {
        if let StoreChange::Problem { message } = change {
            problem = Some(message);
        }
    }
    assert!(problem.unwrap().contains("Not like that."));
}

#[tokio::test(start_paused = true)]
async fn taking_a_story_down_is_local_at_once_and_a_refusal_brings_it_back() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let mine = engine.story_feed(&account).unwrap().mine;
    let (first, second) = (mine[0].story.id.clone(), mine[1].story.id.clone());
    engine.delete_story(&account, &first);
    assert!(engine.store().story(&account, &first).unwrap().is_none());
    super::settle().await;
    assert_eq!(mock.deleted_stories(), vec![first.clone()]);

    mock.fail_next_story_calls([ProviderError::Rejected {
        code: "forbidden".into(),
        message: "Not yours.".into(),
    }]);
    engine.delete_story(&account, &second);
    assert!(engine.store().story(&account, &second).unwrap().is_none());
    super::settle().await;
    assert!(
        engine.store().story(&account, &second).unwrap().is_some(),
        "put back when the provider refuses"
    );
}

#[tokio::test]
async fn events_add_remove_and_count_stories() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let before = engine.story_feed(&account).unwrap();
    let author = ContactId::new("contact:newcomer");
    let posted = mock.contact_posts_story(
        &account,
        &author,
        "Newcomer",
        StoryBody::Text {
            text: "first!".into(),
            style: StoryStyle::default(),
        },
    );
    engine
        .apply_event(ProviderEvent::StoryUpserted(posted.clone()))
        .unwrap();
    let feed = engine.story_feed(&account).unwrap();
    assert_eq!(feed.recent.len(), before.recent.len() + 1);
    assert_eq!(feed.recent[0].name.as_deref(), Some("Newcomer"));

    engine
        .apply_event(ProviderEvent::StoryRemoved {
            account_id: account.clone(),
            story_id: posted.id.clone(),
        })
        .unwrap();
    assert_eq!(
        engine.story_feed(&account).unwrap().recent.len(),
        before.recent.len()
    );

    // Who saw one of mine.
    let mine = before.mine[0].story.id.clone();
    let viewer = StoryViewer {
        contact: ContactId::new("contact:ana"),
        name: Some("Ana".into()),
        viewed_at: Timestamp::now(),
        reaction: Some("❤️".into()),
    };
    engine
        .apply_event(ProviderEvent::StoryViewed {
            account_id: account.clone(),
            story_id: mine.clone(),
            viewer: viewer.clone(),
        })
        .unwrap();
    let viewers = engine.store().story_viewers(&account, &mine).unwrap();
    assert!(viewers
        .iter()
        .any(|known| known.contact == viewer.contact && known.reaction.as_deref() == Some("❤️")));
    let count = engine
        .store()
        .story(&account, &mine)
        .unwrap()
        .unwrap()
        .story
        .view_count;
    assert_eq!(count, Some(viewers.len() as u32));

    // Muted on another device.
    engine
        .apply_event(ProviderEvent::StoryMuteChanged {
            account_id: account.clone(),
            contact: before.recent[0].author.clone(),
            muted: true,
        })
        .unwrap();
    assert!(engine
        .story_feed(&account)
        .unwrap()
        .muted
        .iter()
        .any(|muted| muted.author == before.recent[0].author));
}

#[tokio::test(start_paused = true)]
async fn the_viewers_of_an_own_story_are_asked_for_and_kept() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let mine = engine.story_feed(&account).unwrap().mine[0]
        .story
        .id
        .clone();
    engine.want_story_viewers(&account, &mine);
    super::settle().await;
    let viewers = engine.store().story_viewers(&account, &mine).unwrap();
    assert_eq!(viewers.len(), 5);
    // Newest first, with the reactions they sent.
    assert!(viewers
        .windows(2)
        .all(|pair| pair[0].viewed_at >= pair[1].viewed_at));
    assert!(viewers.iter().any(|viewer| viewer.reaction.is_some()));
}

#[tokio::test]
async fn a_story_that_expires_while_the_app_runs_is_cleaned_up_by_the_timer() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let account = personal();
    // A story with 300 ms to live: the worker sleeps until its deadline
    // and then lets it go, with nobody asking.
    let soon = Story {
        expires_at: Some(Timestamp::from_millis(Timestamp::now().as_millis() + 300)),
        ..story_expiring_in(&account, 3600)
    };
    engine
        .store()
        .upsert_story(&soon, Timestamp::now())
        .unwrap();
    engine.start();
    assert!(engine.store().story(&account, &soon.id).unwrap().is_some());
    let mut gone = false;
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if engine.store().story(&account, &soon.id).unwrap().is_none() {
            gone = true;
            break;
        }
    }
    assert!(gone, "the timer never cleaned it up");
    engine.shutdown();
}

fn story_expiring_in(account: &AccountId, seconds: i64) -> Story {
    let now = Timestamp::now().as_millis();
    Story {
        id: MessageId::new(format!("exp{seconds}")),
        client_id: None,
        account_id: account.clone(),
        author: ContactId::new("contact:someone"),
        author_name: Some("Someone".into()),
        mine: false,
        posted_at: Timestamp::from_millis(now - HOUR),
        expires_at: Some(Timestamp::from_millis(now + seconds * 1000)),
        body: StoryBody::Text {
            text: "soon".into(),
            style: StoryStyle::default(),
        },
        mentions: Vec::new(),
        viewed: false,
        view_count: None,
    }
}

#[tokio::test]
async fn startup_cleans_up_the_stories_that_expired_while_the_app_was_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stories.db");
    let mock = MockProvider::quiet();
    {
        let store = Store::open(&path, None).unwrap();
        seed_account(&store);
        // Written by an earlier run, a day and a bit ago.
        let day = STORY_LIFETIME.as_millis() as i64;
        let old = Timestamp::from_millis(Timestamp::now().as_millis() - day - HOUR);
        store
            .upsert_story(
                &Story {
                    posted_at: old,
                    ..story("old", "+1", 0, "x")
                },
                old,
            )
            .unwrap();
        store
            .put_media(
                &thumbnail_key("https://media.example/old"),
                &CachedMedia {
                    bytes: vec![1],
                    mime: None,
                    size: None,
                },
                old,
                1 << 20,
            )
            .unwrap();
    }
    let store = Arc::new(Store::open(&path, None).unwrap());
    let engine = SyncEngine::new(
        store.clone(),
        Arc::new(mock),
        SyncConfig::default(),
        Handle::current(),
    );
    engine.start();
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if store
            .story(&account(), &MessageId::new("old"))
            .unwrap()
            .is_none()
        {
            break;
        }
    }
    assert!(store
        .story(&account(), &MessageId::new("old"))
        .unwrap()
        .is_none());
    engine.shutdown();
}

// ----- a provider without stories --------------------------------------------

struct Plain;

#[async_trait]
impl Provider for Plain {
    fn id(&self) -> &'static str {
        "plain"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::none()
    }
    async fn list_accounts(&self) -> ProviderResult<Vec<client_provider::Account>> {
        Ok(Vec::new())
    }
    async fn list_chats(
        &self,
        _: &AccountId,
        _: Option<Cursor>,
    ) -> ProviderResult<Page<client_provider::Chat>> {
        Ok(Page::last(Vec::new()))
    }
    async fn fetch_messages(
        &self,
        _: &AccountId,
        _: &ChatId,
        _: Option<Cursor>,
        _: u32,
    ) -> ProviderResult<Page<Message>> {
        Ok(Page::last(Vec::new()))
    }
    async fn send(&self, _: OutgoingMessage) -> ProviderResult<SendReceipt> {
        Err(ProviderError::Unsupported("sending"))
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

#[tokio::test]
async fn a_provider_without_stories_says_so_instead_of_failing() {
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(Plain),
        SyncConfig::default(),
        Handle::current(),
    );
    let account = account();
    assert!(!engine.capabilities().story_post);
    assert!(!engine.stories_from_contacts());
    assert!(matches!(
        engine.post_story_text(&account, "hi", StoryStyle::default()),
        Err(StoryPostError::NotAvailable)
    ));
    assert!(matches!(
        engine.reply_to_story(&account, &MessageId::new("s"), "hi", Vec::new()),
        Err(StoryReplyError::NotAvailable)
    ));
    assert!(matches!(
        engine.react_to_story(&account, &MessageId::new("s"), "👍"),
        Err(StoryReplyError::NotAvailable)
    ));
    // Nothing is asked of it, and nothing is owed.
    assert_eq!(engine.sync_stories(&account).await.unwrap(), 0);
    engine.want_stories(&account, Duration::ZERO);
    assert_eq!(
        engine.flush_story_posts_at(Timestamp::now()).await.unwrap(),
        StoryPass::default()
    );
    // Calling it directly answers `Unsupported`, as the trait promises.
    let direct = Plain.list_stories(&account).await;
    assert!(matches!(direct, Err(ProviderError::Unsupported(_))));
    // Local things still work: muting is the user's, kept here.
    engine.set_story_muted(&account, &ContactId::new("+1"), true);
    assert!(engine
        .store()
        .story_muted(&account, &ContactId::new("+1"))
        .unwrap());
}

#[tokio::test]
async fn the_mocks_clients_see_the_same_world_whoever_asks() {
    // The seed is deterministic and the mock's clock is real: what is
    // listed twice is the same set.
    let mock = MockProvider::new(MockConfig::quiet());
    let first = mock.list_stories(&personal()).await.unwrap();
    let second = mock.list_stories(&personal()).await.unwrap();
    assert_eq!(first, second);
    assert!(first.iter().any(|story| story.mine));
    assert!(first.iter().any(|story| story.viewed && !story.mine));
}

// ----- answers to a story that went meanwhile ---------------------------------

#[tokio::test]
async fn an_answer_to_a_story_that_went_while_it_waited_fails_and_is_never_a_plain_message() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    let reply = engine
        .reply_to_story(&account, &item.story.id, "hello", Vec::new())
        .unwrap();
    engine
        .react_to_story(&account, &item.story.id, "🔥")
        .unwrap();
    // The connection is down: they wait, and nothing says so.
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));

    // Meanwhile the story goes (it expired, or its author took it down).
    assert!(engine
        .store()
        .remove_story(&account, &item.story.id)
        .unwrap());
    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 10 * MINUTE);
    let pass = run_outbox_pass(
        engine.store(),
        mock_dyn(&mock).as_ref(),
        &OutboxConfig::default(),
        later,
    )
    .await
    .unwrap();
    assert_eq!((pass.sent, pass.failed), (0, 2));
    assert_eq!(
        mock.send_calls(),
        0,
        "never an ordinary message that quotes a story nobody has"
    );
    assert!(mock.story_replies().is_empty() && mock.story_reactions().is_empty());
    assert!(engine.store().outbox_pending().unwrap().is_empty());
    // The reply says why, in its chat.
    let chat = ChatId::new(item.story.author.as_str());
    let shown = engine
        .store()
        .messages(&account, &chat, 5)
        .unwrap()
        .into_iter()
        .find(|stored| stored.message.client_id.as_ref() == Some(&reply))
        .expect("the reply is still in the chat");
    assert_eq!(
        shown.message.status,
        DeliveryStatus::Failed {
            reason: "This status is no longer there.".into()
        }
    );
}

#[tokio::test]
async fn a_queued_reply_that_is_edited_still_answers_its_story() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let item = engine.story_feed(&account).unwrap().recent[0].stories[0].clone();
    let reply = engine
        .reply_to_story(&account, &item.story.id, "helo", Vec::new())
        .unwrap();
    assert!(engine.store().outbox_edit_text(&reply, "hello").unwrap());
    let queued = engine.store().outbox_pending().unwrap();
    assert_eq!(queued[0].story.as_ref(), Some(&item.story.id));
    engine.flush_outbox().await.unwrap();
    let replies = mock.story_replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(
        replies[0].1.content,
        client_provider::OutgoingContent::Text {
            body: "hello".into()
        }
    );
}

// ----- taking down against what is on its way ---------------------------------

#[tokio::test(start_paused = true)]
async fn a_listing_that_was_on_its_way_when_a_story_was_taken_down_does_not_bring_it_back() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let first = engine.story_feed(&account).unwrap().mine[0]
        .story
        .id
        .clone();
    mock.set_story_latency(Duration::from_secs(1));
    // The number is back and its stories are asked for; the answer, taken
    // now, is on its way.
    let listing = tokio::spawn({
        let (engine, account) = (engine.clone(), account.clone());
        async move { engine.sync_stories(&account).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    engine.delete_story(&account, &first);
    listing.await.unwrap().unwrap();
    assert!(
        engine.store().story(&account, &first).unwrap().is_none(),
        "the list was taken before the story was taken down"
    );
    super::settle().await;
    assert_eq!(mock.deleted_stories(), vec![first.clone()]);
    mock.set_story_latency(Duration::ZERO);
    engine.sync_stories(&account).await.unwrap();
    assert!(engine.store().story(&account, &first).unwrap().is_none());
}

#[tokio::test(start_paused = true)]
async fn a_story_taken_down_during_a_drop_stays_down_and_a_refusal_still_brings_it_back() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let mine = engine.story_feed(&account).unwrap().mine;
    let (first, second) = (mine[0].story.id.clone(), mine[1].story.id.clone());
    // The provider has not heard of it yet: it still lists the story.
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    engine.delete_story(&account, &first);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(mock.deleted_stories().is_empty());
    engine.sync_stories(&account).await.unwrap();
    assert!(
        engine.store().story(&account, &first).unwrap().is_none(),
        "what the user took down is not put back by a list"
    );
    super::settle().await;
    assert_eq!(mock.deleted_stories(), vec![first.clone()]);

    // A refusal puts it back, and the next list leaves it there.
    mock.fail_next_story_calls([ProviderError::Rejected {
        code: "forbidden".into(),
        message: "Not yours.".into(),
    }]);
    engine.delete_story(&account, &second);
    super::settle().await;
    assert!(engine.store().story(&account, &second).unwrap().is_some());
    engine.sync_stories(&account).await.unwrap();
    assert!(engine.store().story(&account, &second).unwrap().is_some());
}

#[tokio::test(start_paused = true)]
async fn a_post_given_up_while_it_was_on_its_way_is_taken_down_and_never_shown() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let before = engine.story_feed(&account).unwrap().mine.len();
    let client_id = engine
        .post_story_text(&account, "oops", StoryStyle::default())
        .unwrap();
    mock.set_story_latency(Duration::from_secs(1));
    let posting = tokio::spawn({
        let engine = engine.clone();
        async move { engine.flush_story_posts_at(Timestamp::now()).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    // The provider has it; its answer is on its way. The user gives up.
    assert_eq!(mock.posted_stories().len(), 1);
    assert!(engine.discard_story_post(&client_id).unwrap());
    let pass = posting.await.unwrap().unwrap();
    assert_eq!(pass.posted, 0);
    assert_eq!(engine.story_feed(&account).unwrap().mine.len(), before);
    // And it does not stay up for everybody else.
    super::settle().await;
    let posted = mock.posted_stories();
    assert_eq!(mock.deleted_stories(), vec![posted[0].id.clone()]);
    mock.set_story_latency(Duration::ZERO);
    engine.sync_stories(&account).await.unwrap();
    assert_eq!(engine.story_feed(&account).unwrap().mine.len(), before);
}

#[tokio::test(start_paused = true)]
async fn a_post_given_up_after_its_answer_was_lost_is_taken_down_when_it_is_listed() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let before = engine.story_feed(&account).unwrap().mine.len();
    let client_id = engine
        .post_story_text(&account, "oops", StoryStyle::default())
        .unwrap();
    // The request reached the provider and its answer never came.
    let posted = mock
        .post_story(NewStory {
            client_id: client_id.clone(),
            account_id: account.clone(),
            content: NewStoryContent::Text {
                text: "oops".into(),
                style: StoryStyle::default(),
            },
        })
        .await
        .unwrap();
    assert!(engine.discard_story_post(&client_id).unwrap());
    engine.sync_stories(&account).await.unwrap();
    assert_eq!(engine.story_feed(&account).unwrap().mine.len(), before);
    super::settle().await;
    assert_eq!(mock.deleted_stories(), vec![posted.id]);
}

// ----- passing a story on --------------------------------------------------

/// Every story the engine has for the account, own ones included.
fn every_story(engine: &SyncEngine, account: &AccountId) -> Vec<Story> {
    let feed = engine.story_feed(account).unwrap();
    feed.recent
        .iter()
        .chain(&feed.viewed)
        .chain(&feed.muted)
        .flat_map(|author| author.stories.iter())
        .chain(&feed.mine)
        .map(|item| item.story.clone())
        .collect()
}

fn a_text_and_a_picture(engine: &SyncEngine, account: &AccountId) -> (Story, Story) {
    let all = every_story(engine, account);
    let words = all
        .iter()
        .find(|story| story.body.media_kind().is_none())
        .expect("a text story")
        .clone();
    let picture = all
        .iter()
        .find(|story| {
            matches!(&story.body, client_provider::StoryBody::Media(media)
                if media.kind == MediaKind::Image && media.source.is_some())
        })
        .expect("a picture story")
        .clone();
    (words, picture)
}

#[tokio::test]
async fn a_story_is_passed_on_as_a_message_is_a_text_again_a_picture_by_naming_it() {
    let mock = MockProvider::quiet();
    let (engine, account) = engine_with_stories(&mock).await;
    let (words, picture) = a_text_and_a_picture(&engine, &account);
    let to = engine.store().chats(&account, None).unwrap()[0].id.clone();
    assert_eq!(
        engine.story_forward_refusal(&account, &words.id).unwrap(),
        None
    );
    assert_eq!(
        engine.story_forward_refusal(&account, &picture.id).unwrap(),
        None
    );

    // The words go as a text with the mark.
    engine.forward_story(&account, &words.id, &to).unwrap();
    engine.flush_outbox().await.unwrap();
    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].forwarded);
    assert_eq!(
        sent[0].content,
        client_provider::OutgoingContent::Text {
            body: words.body.words().unwrap().to_owned()
        }
    );
    assert!(mock.forward_calls().is_empty());

    // The picture goes by naming the story: no file passes through here,
    // and it is shown in the chat at once, with the mark.
    let client_id = engine.forward_story(&account, &picture.id, &to).unwrap();
    let pending = engine
        .store()
        .messages(&account, &to, 50)
        .unwrap()
        .into_iter()
        .find(|stored| stored.message.client_id.as_ref() == Some(&client_id))
        .expect("the copy is in the chat");
    assert!(pending.message.extras.forwarded);
    assert!(matches!(
        pending.message.content,
        client_provider::MessageContent::Media(_)
    ));
    // A drop on the way: asked again under the same id, one copy.
    mock.fail_next_forwards([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried), (0, 1));
    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 10 * MINUTE);
    let pass = run_outbox_pass(engine.store(), &mock, &SyncConfig::default().outbox, later)
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    let calls = mock.forward_calls();
    assert_eq!(calls.len(), 2);
    assert!(calls
        .iter()
        .all(|item| item.message == picture.id && item.to == to && item.client_id == client_id));
    assert_eq!(mock.forwarded_copies(), 1);
    assert_eq!(mock.sent().len(), 1, "nothing was sent as a file");
    // Neither was handed to the calls that answer a story.
    assert!(mock.story_replies().is_empty());

    // A story that is gone is refused, in words.
    let gone = MessageId::new("no-such-story");
    assert_eq!(
        engine.story_forward_refusal(&account, &gone).unwrap(),
        Some(crate::ForwardRefusal::Deleted)
    );
    assert!(matches!(
        engine.forward_story(&account, &gone, &to),
        Err(crate::ForwardError::Refused(crate::ForwardRefusal::Deleted))
    ));
}

#[tokio::test]
async fn without_forwarding_by_name_only_the_words_of_a_text_story_are_passed_on() {
    let mock = MockProvider::quiet();
    mock.set_forward_available(false);
    let (engine, account) = engine_with_stories(&mock).await;
    let (words, picture) = a_text_and_a_picture(&engine, &account);
    let to = engine.store().chats(&account, None).unwrap()[0].id.clone();
    assert_eq!(
        engine.story_forward_refusal(&account, &words.id).unwrap(),
        None
    );
    let refusal = engine
        .story_forward_refusal(&account, &picture.id)
        .unwrap()
        .unwrap();
    assert_eq!(refusal, crate::ForwardRefusal::NotAvailableYet);
    assert_eq!(refusal.reason(), "Not available yet from this provider");
    assert!(matches!(
        engine.forward_story(&account, &picture.id, &to),
        Err(crate::ForwardError::Refused(
            crate::ForwardRefusal::NotAvailableYet
        ))
    ));
    engine.forward_story(&account, &words.id, &to).unwrap();
    engine.flush_outbox().await.unwrap();
    assert_eq!(mock.sent().len(), 1);
    assert!(mock.forward_calls().is_empty());
}

// ----- a message that answers a story, as a provider tells of it ----------------------

fn reply_mark(message: &Message) -> Option<client_provider::StoryReplyRef> {
    message.extras.story_reply.clone()
}

fn stored(store: &Store, id: &str) -> Message {
    store
        .message(&account(), &MessageId::new(id))
        .unwrap()
        .unwrap()
}

/// A provider that says a message answers a story names the story and
/// nothing of what it showed. For somebody's answer to a story of the
/// account's, the story held here says the rest; the mark this client
/// wrote for its own answer says more than the provider's and is kept;
/// and without either, the bare mark is what there is.
#[test]
fn a_bare_story_mark_from_the_provider_is_filled_in_from_what_is_held_here() {
    use client_provider::{StoryReplyKind, StoryReplyRef};
    let store = store();
    store.upsert_chat(&chat("them", "Ana"), false).unwrap();
    let mut mine = image_story("s_mine", "me", 0, "https://wa/story.jpg");
    mine.mine = true;
    if let StoryBody::Media(media) = &mut mine.body {
        media.caption = Some("The lake at dawn".into());
    }
    store.upsert_story(&mine, at(1)).unwrap();
    let bare = |story: &str, of_mine: bool| StoryReplyRef {
        story: MessageId::new(story),
        kind: StoryReplyKind::Text,
        preview: None,
        of_mine,
    };

    // Somebody answered the account's story: the provider only names it.
    let mut answer = text(
        "m_answer",
        "them",
        T0 + 10,
        "Beautiful!",
        Direction::Incoming,
    );
    answer.extras.story_reply = Some(bare("s_mine", true));
    store.upsert_message(&answer).unwrap();
    let mark = reply_mark(&stored(&store, "m_answer")).unwrap();
    assert_eq!(mark.kind, StoryReplyKind::Image);
    assert_eq!(mark.preview.as_deref(), Some("The lake at dawn"));
    assert!(mark.of_mine);

    // The story goes after its day; the mark stays as it was filled in,
    // however often the provider repeats the message.
    store
        .remove_story(&account(), &MessageId::new("s_mine"))
        .unwrap();
    store.upsert_message(&answer).unwrap();
    assert_eq!(reply_mark(&stored(&store, "m_answer")), Some(mark));

    // The account's own answer: the client wrote the mark when it was
    // queued, and the provider's copy does not take it away.
    let written = StoryReplyRef {
        story: MessageId::new("s_theirs"),
        kind: StoryReplyKind::Video,
        preview: Some("At the beach".into()),
        of_mine: false,
    };
    let mut own = text("m_own", "them", T0 + 20, "Nice", Direction::Outgoing);
    own.extras.story_reply = Some(written.clone());
    store.upsert_message(&own).unwrap();
    let mut copy = own.clone();
    copy.extras.story_reply = Some(bare("s_theirs", false));
    copy.status = DeliveryStatus::Delivered;
    store.upsert_message(&copy).unwrap();
    assert_eq!(reply_mark(&stored(&store, "m_own")), Some(written));

    // A story nobody holds here: the bare mark, which reads "Status".
    let mut unknown = text("m_unknown", "them", T0 + 30, "?", Direction::Incoming);
    unknown.extras.story_reply = Some(bare("s_never_seen", true));
    store.upsert_message(&unknown).unwrap();
    assert_eq!(
        reply_mark(&stored(&store, "m_unknown")),
        Some(bare("s_never_seen", true))
    );
}

// ----- contacts' stories that are not there yet ----------------------------------------

/// The provider has contacts' stories and its backend does not, yet. The
/// account's own stories still list, the view is told, and what needs the
/// missing part is not offered for that number.
#[tokio::test(start_paused = true)]
async fn stories_that_are_missing_for_now_take_nothing_else_with_them() {
    use client_provider::Feature;
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        Handle::current(),
    );
    engine.refresh().await.unwrap();
    let account = engine.store().accounts().unwrap().remove(0).id;
    let caps = engine.capabilities_for(&account);
    assert!(caps.story_contacts && caps.story_view && caps.story_viewers);
    assert!(caps.story_reply && caps.story_react);

    mock.set_unavailable(None, Feature::Stories, true);
    assert!(engine.feature_unavailable(&account, Feature::Stories));
    let caps = engine.capabilities_for(&account);
    assert!(!caps.story_contacts && !caps.story_view && !caps.story_viewers);
    assert!(!caps.story_reply && !caps.story_react);
    // What does not depend on it is as it was.
    assert!(caps.story_list && caps.story_post && caps.story_delete && caps.story_privacy);
    assert_eq!(
        Capabilities {
            story_contacts: true,
            story_view: true,
            story_viewers: true,
            story_reply: true,
            story_react: true,
            ..caps
        },
        engine.capabilities()
    );
    // Listing still works.
    engine.sync_stories(&account).await.unwrap();
}

/// Going to Status asks for what is up now: a listing older than a few
/// seconds is asked for again, a provider that remembered stories as
/// missing is told to ask its backend again, and the view can say
/// whether the list is on its way, here, or did not come.
#[tokio::test(start_paused = true)]
async fn looking_at_status_asks_again_and_says_how_the_listing_is_doing() {
    use client_provider::Feature;
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let account = personal();
    let lists = |mock: &MockProvider| {
        mock.story_calls()
            .iter()
            .filter(|call| **call == "list")
            .count()
    };
    assert_eq!(engine.story_listing(&account), StoryListing::default());

    // The first look: asked, and said to be on its way until it answers.
    mock.set_story_latency(Duration::from_secs(2));
    engine.look_at_stories(&account, STORIES_OPENED);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(engine.story_listing(&account).loading);
    assert!(!engine.story_listing(&account).listed);
    tokio::time::sleep(Duration::from_secs(3)).await;
    let listing = engine.story_listing(&account);
    assert!(listing.listed && !listing.loading && !listing.failed);
    assert_eq!(lists(&mock), 1);
    assert!(!engine.story_feed(&account).unwrap().recent.is_empty());
    mock.set_story_latency(Duration::ZERO);

    // A second look right away asks nothing; "Check again" does,
    // however lately the list was read.
    engine.look_at_stories(&account, STORIES_OPENED);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(lists(&mock), 1);
    engine.look_at_stories(&account, Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(lists(&mock), 2);

    // A listing that does not come is said, and the next look asks again
    // without waiting for anything.
    mock.fail_next_story_calls([ProviderError::Transient("eof".into())]);
    engine.look_at_stories(&account, Duration::ZERO);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let listing = engine.story_listing(&account);
    assert!(listing.failed && listing.listed && !listing.loading);
    engine.look_at_stories(&account, STORIES_OPENED);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!engine.story_listing(&account).failed);

    // Missing for now: a look tells the provider to ask its backend
    // again, at once, however lately the list was read.
    mock.set_unavailable(None, Feature::Stories, true);
    let before = lists(&mock);
    engine.look_at_stories(&account, STORIES_OPENED);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(mock.story_calls().contains(&"recheck"));
    assert_eq!(
        lists(&mock),
        before + 1,
        "asked although it was just listed"
    );
}
