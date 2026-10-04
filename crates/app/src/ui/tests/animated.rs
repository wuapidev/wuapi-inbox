//! Stickers and GIFs that move, in the real window: a sticker that is on
//! screen asks for its next frame and shows it, and asks for nothing when
//! it is scrolled away, held, or under reduced motion; decoded frames stay
//! within their memory; and a GIF that is a video says so on its tile.

use super::*;
use client_core::fixtures::{animated_gif, animated_webp, still_webp};
use client_provider::{
    ContactId, Media, MediaKind, MediaRef, Message, MessageExtras, MessageId, ProviderEvent,
    Timestamp,
};
use std::cell::Cell;

thread_local! {
    static CLOCK: Cell<i64> = const { Cell::new(0) };
}

/// A chat open with motion on, in an active window.
fn open_moving(cx: &mut TestAppContext) -> Harness {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    cx.update(|cx| settings::update(cx, |settings| settings.motion = MotionChoice::On));
    cx.update_window(harness.window.into(), |_, window, _| {
        window.activate_window()
    })
    .unwrap();
    cx.run_until_parked();
    harness
}

fn push_media(harness: &Harness, cx: &mut TestAppContext, id: &str, media: Media) {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let tick = CLOCK.with(|clock| {
        clock.set(clock.get() + 1);
        clock.get()
    });
    let message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new("them"),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000 + tick),
        content: MessageContent::Media(media),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras::default(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

fn sticker(url: &str, mime: &str) -> Media {
    let mut media = Media::new(MediaKind::Sticker);
    media.mime_type = Some(mime.to_owned());
    media.source = Some(MediaRef::new(url));
    media
}

fn text(harness: &Harness, cx: &mut TestAppContext, id: &str) {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let tick = CLOCK.with(|clock| {
        clock.set(clock.get() + 1);
        clock.get()
    });
    let message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new("them"),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000 + tick),
        content: MessageContent::text("a line\nand another\nand one more to push things up"),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras::default(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

/// Lets downloads and decodes (which run on other threads) finish: until
/// `done` says so.
fn until(harness: &Harness, cx: &mut TestAppContext, what: &str, done: impl Fn(&Shell) -> bool) {
    for _ in 0..400 {
        harness.settle(cx);
        if cx.update(|cx| done(harness.shell.read(cx))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("never: {what}");
}

fn frame(harness: &Harness, cx: &mut TestAppContext, url: &str) -> Option<usize> {
    cx.update(|cx| harness.shell.read(cx).media.painted_frame(url))
}

fn waking(harness: &Harness, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| harness.shell.read(cx).media.pending_wake().is_some())
}

/// Lets `ms` pass on the window's clock and the window redraw.
fn pass(cx: &mut TestAppContext, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_sticker_on_screen_asks_for_its_frames_and_stops_when_it_should(cx: &mut TestAppContext) {
    let harness = open_moving(cx);
    let url = "https://media.example/dance.webp";
    // Three frames of 100 ms.
    harness
        .mock
        .set_media(url, animated_webp(64, 64, &[100, 100, 100]), "image/webp");
    push_media(&harness, cx, "sticker-1", sticker(url, "image/webp"));
    until(&harness, cx, "the sticker plays", |shell| {
        shell.media.painted_frame(url).is_some()
    });
    assert!(shows(harness.window, "media-animation", cx));
    assert_eq!(harness.mock.media_calls(), 1, "one download for both");
    // The box is the sticker's, as for a still one.
    let size = bounds(harness.window, "media-box", cx).size;
    assert_eq!(size.width, size.height);

    // On screen: it shows a frame and has asked for the next.
    let first = frame(&harness, cx, url).unwrap();
    assert!(waking(&harness, cx), "the next frame is asked for");
    pass(cx, 100);
    let second = frame(&harness, cx, url).unwrap();
    assert_eq!(second, (first + 1) % 3, "one frame on");
    assert!(waking(&harness, cx));
    // And it loops.
    pass(cx, 100);
    pass(cx, 100);
    assert_eq!(frame(&harness, cx, url), Some(first));

    // Something over the conversation: it holds its frame and asks for
    // nothing while that is so.
    let held = frame(&harness, cx, url).unwrap();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_overlay(Overlay::Settings, window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    pass(cx, 100);
    assert!(!waking(&harness, cx), "held: nothing is asked for");
    pass(cx, 1_000);
    assert_eq!(frame(&harness, cx, url), Some(held));
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.close_overlay(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    // Back: it goes on from the frame it held, not from a second later.
    assert_eq!(frame(&harness, cx, url), Some(held));
    assert!(waking(&harness, cx));
    pass(cx, 100);
    assert_eq!(frame(&harness, cx, url), Some((held + 1) % 3));

    // Reduced motion: the first frame, as a picture, and no repaints.
    cx.update(|cx| settings::update(cx, |settings| settings.motion = MotionChoice::Off));
    cx.run_until_parked();
    pass(cx, 100);
    assert!(!shows(harness.window, "media-animation", cx));
    assert!(shows(harness.window, "media-image", cx));
    assert!(!waking(&harness, cx));
    pass(cx, 1_000);
    assert!(!waking(&harness, cx));
    cx.update(|cx| settings::update(cx, |settings| settings.motion = MotionChoice::On));
    cx.run_until_parked();
    assert!(shows(harness.window, "media-animation", cx));
    assert!(waking(&harness, cx));

    // Scrolled away (newer messages push it out of the window): it is not
    // painted, so nothing asks for a frame, however long one waits.
    for index in 0..40 {
        text(&harness, cx, &format!("filler-{index}"));
    }
    pass(cx, 100);
    let away = frame(&harness, cx, url);
    assert!(!shows(harness.window, "media-animation", cx));
    pass(cx, 100);
    assert!(!waking(&harness, cx), "nothing animated is on screen");
    pass(cx, 5_000);
    assert!(!waking(&harness, cx));
    assert_eq!(frame(&harness, cx, url), away);

    // Leaving the chat lets the frames go.
    cx.update(|cx| harness.shell.update(cx, |shell, cx| shell.close_chat(cx)));
    cx.run_until_parked();
    cx.update(|cx| assert_eq!(harness.shell.read(cx).media.animations(), (0, 0)));
}

#[gpui_kit::test]
fn a_gif_file_moves_and_a_still_sticker_does_not(cx: &mut TestAppContext) {
    let harness = open_moving(cx);
    // A sticker that does not move: one download, a picture, no repaints.
    let still = "https://media.example/still.webp";
    harness
        .mock
        .set_media(still, still_webp(64, 64), "image/webp");
    push_media(&harness, cx, "still-1", sticker(still, "image/webp"));
    until_shown(&harness, cx, "media-image");
    harness.settle(cx);
    pass(cx, 200);
    assert!(!shows(harness.window, "media-animation", cx));
    assert!(!waking(&harness, cx));
    assert_eq!(harness.mock.media_calls(), 1);

    // A real GIF file, sent as a picture.
    let gif = "https://media.example/party.gif";
    harness
        .mock
        .set_media(gif, animated_gif(48, 32, 4, 50), "image/gif");
    let mut media = Media::new(MediaKind::Image);
    media.mime_type = Some("image/gif".into());
    media.source = Some(MediaRef::new(gif));
    push_media(&harness, cx, "gif-1", media);
    until(&harness, cx, "the GIF plays", |shell| {
        shell.media.painted_frame(gif).is_some()
    });
    let first = frame(&harness, cx, gif).unwrap();
    pass(cx, 50);
    assert_eq!(frame(&harness, cx, gif), Some((first + 1) % 4));
    assert!(waking(&harness, cx));
}

#[gpui_kit::test]
fn a_sticker_cached_before_animations_were_kept_is_asked_about_once(cx: &mut TestAppContext) {
    let harness = open_moving(cx);
    let url = "https://media.example/old.webp";
    let bytes = animated_webp(64, 64, &[80, 80]);
    harness.mock.set_media(url, bytes.clone(), "image/webp");
    // What the earlier version left: the first frame only.
    let small = client_core::thumbnail(&bytes, 720).unwrap();
    harness
        .engine
        .store()
        .put_media(
            &client_core::thumbnail_key(url),
            &client_core::CachedMedia {
                bytes: small.bytes,
                mime: Some(small.mime.to_owned()),
                size: Some((small.width, small.height)),
            },
            Timestamp::now(),
            u64::MAX,
        )
        .unwrap();
    push_media(&harness, cx, "old-1", sticker(url, "image/webp"));
    assert!(
        shows(harness.window, "media-image", cx),
        "the frame at once"
    );
    until(&harness, cx, "the old sticker plays", |shell| {
        shell.media.painted_frame(url).is_some()
    });
    assert_eq!(harness.mock.media_calls(), 1);
    pass(cx, 500);
    harness.settle(cx);
    assert_eq!(harness.mock.media_calls(), 1, "and never again");
}

#[gpui_kit::test]
fn decoded_frames_stay_within_their_memory(cx: &mut TestAppContext) {
    let harness = open_moving(cx);
    // Room for one animation of 64 px and four frames (65 kB), not two.
    cx.update(|cx| harness.shell.read(cx).media.set_animation_budget(100_000));
    let (one, two) = (
        "https://media.example/one.webp",
        "https://media.example/two.webp",
    );
    for url in [one, two] {
        harness
            .mock
            .set_media(url, animated_webp(64, 64, &[100; 4]), "image/webp");
    }
    push_media(&harness, cx, "one", sticker(one, "image/webp"));
    until(&harness, cx, "the first plays", |shell| {
        shell.media.painted_frame(one).is_some()
    });
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).media.animations(),
            (4 * 64 * 64 * 4, 1)
        )
    });

    // A second one while the first is being looked at: there is no room,
    // so it stays a still picture, and the first keeps playing.
    push_media(&harness, cx, "two", sticker(two, "image/webp"));
    for _ in 0..100 {
        harness.settle(cx);
        std::thread::sleep(Duration::from_millis(5));
    }
    cx.update(|cx| {
        let shelf = &harness.shell.read(cx).media;
        let (bytes, count) = shelf.animations();
        assert!(bytes <= 100_000, "{bytes} bytes held");
        assert_eq!(count, 1);
        assert!(shelf.painted_frame(one).is_some());
        assert!(shelf.painted_frame(two).is_none());
    });
    assert!(shows(harness.window, "media-image", cx));
}

#[gpui_kit::test]
fn a_gif_that_is_a_video_says_so_and_a_vector_sticker_is_not_fetched(cx: &mut TestAppContext) {
    let harness = open_moving(cx);
    // A GIF on WhatsApp: a video flagged to play as one.
    let mut gif = Media::new(MediaKind::Video);
    gif.mime_type = Some("video/mp4".into());
    gif.source = Some(MediaRef::new("https://media.example/loop.mp4"));
    gif.size_bytes = Some(412_000);
    gif.gif = true;
    push_media(&harness, cx, "gif-video", gif);
    assert!(shows(harness.window, "gif-badge", cx));
    let tile = bounds(harness.window, "tile-row", cx);
    assert!(within(bounds(harness.window, "gif-badge", cx), tile));
    assert!(!waking(&harness, cx), "a tile: nothing plays");
    // The chat list calls it what it is.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap().chat.id.clone();
        let line = shell.list_rows.iter().find_map(|row| match row {
            ListRow::Chat(chat) if chat.id == open => {
                chat.last_message.as_ref().map(|last| last.text.clone())
            }
            _ => None,
        });
        assert_eq!(line.as_deref(), Some("GIF"));
    });

    // An ordinary video has no badge.
    let mut video = Media::new(MediaKind::Video);
    video.mime_type = Some("video/mp4".into());
    video.source = Some(MediaRef::new("https://media.example/clip.mp4"));
    push_media(&harness, cx, "video", video);
    harness.settle(cx);

    // A vector sticker (Lottie): said, and never downloaded.
    let calls = harness.mock.media_calls();
    push_media(
        &harness,
        cx,
        "lottie",
        sticker("https://media.example/s.was", "application/was"),
    );
    harness.settle(cx);
    assert!(shows(harness.window, "media-unavailable", cx));
    assert_eq!(harness.mock.media_calls(), calls);
}
