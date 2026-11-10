//! A video of a conversation plays in the window, and one that cannot be
//! played here offers the system's player.

use super::*;
use std::sync::Arc;

fn push_video(harness: &Harness, cx: &mut TestAppContext, id: &str, url: &str) {
    harness
        .mock
        .set_media(url, b"not really an mp4".to_vec(), "video/mp4");
    push_described_media(
        harness,
        cx,
        id,
        client_provider::MediaKind::Video,
        url,
        |media| {
            media.mime_type = Some("video/mp4".into());
            media.size_bytes = Some(17);
            media.duration_secs = Some(12);
        },
    );
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

#[gpui_kit::test]
fn a_video_plays_in_the_window_and_holds_on_space(cx: &mut TestAppContext) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    push_video(&harness, cx, "m-video", "https://media.example/holiday.mp4");
    click(harness.window, "media-open", cx);
    // Not here yet: the player opens when the file is.
    until(&harness, cx, |shell| shell.overlay == Overlay::Video);
    until_shown(&harness, cx, "video-frame");
    assert!(shows(harness.window, "video-controls", cx));
    assert!(
        cx.opened_url().is_none(),
        "nothing is handed to another player"
    );
    let playing = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).video.playing());
    assert!(playing(cx));

    press(harness.window, "space", cx);
    assert!(cx.update(|cx| harness.shell.read(cx).video.paused()));
    press(harness.window, "space", cx);
    assert!(!cx.update(|cx| harness.shell.read(cx).video.paused()));

    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(!playing(cx), "closing stops the decoder");
}

#[gpui_kit::test]
fn a_video_that_cannot_be_played_here_offers_the_system_player(cx: &mut TestAppContext) {
    let _guard =
        crate::video::install_opener(Arc::new(|_| Err("no video runtime was embedded".into())));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    push_video(&harness, cx, "m-video", "https://media.example/broken.mp4");
    click(harness.window, "media-open", cx);
    until(&harness, cx, |shell| shell.overlay == Overlay::Video);
    until_shown(&harness, cx, "video-failed");
    // The way out is there (the test platform opens nothing with it).
    assert!(shows(harness.window, "video-system", cx));
    assert!(!cx.update(|cx| harness.shell.read(cx).video.playing()));
}

#[gpui_kit::test]
fn a_video_is_muted_with_its_button_or_m_and_stays_so_for_the_next(cx: &mut TestAppContext) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    push_video(&harness, cx, "m-video", "https://media.example/holiday.mp4");
    click(harness.window, "media-open", cx);
    until_shown(&harness, cx, "video-frame");
    let silenced =
        |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).video.silenced());
    assert_eq!(silenced(cx), Some(false), "a video plays with its sound");

    click(harness.window, "video-mute", cx);
    assert_eq!(silenced(cx), Some(true));
    press(harness.window, "m", cx);
    assert_eq!(silenced(cx), Some(false));
    press(harness.window, "m", cx);

    press(harness.window, "escape", cx);
    click(harness.window, "media-open", cx);
    until_shown(&harness, cx, "video-frame");
    assert_eq!(silenced(cx), Some(true), "the next one starts silenced too");
}

/// The file's bytes that have arrived, as the engine reports them.
fn arrived(harness: &Harness, url: &str) -> Option<u64> {
    harness.engine.media_progress(&client_core::file_key(url))
}

/// Lets a slow download get to where half of it is here.
fn until_half_here(harness: &Harness, cx: &mut TestAppContext, url: &str, size: u64) {
    let deadline = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < deadline {
        harness.settle(cx);
        if arrived(harness, url) == Some(size / 2) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!(
        "half of the file never arrived: {:?}",
        arrived(harness, url)
    );
}

#[gpui_kit::test]
fn a_video_that_is_not_here_shows_its_download_on_the_bubble_and_opens_when_it_is(
    cx: &mut TestAppContext,
) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    let url = "https://media.example/slow.mp4";
    harness.mock.set_media_latency(Duration::from_secs(2));
    push_video(&harness, cx, "m-video", url);
    assert!(shows(harness.window, "video-play-badge", cx));

    // A click starts the download: the bubble shows how far it is, and
    // the player is not opened over an empty picture.
    click(harness.window, "media-open", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    until_shown(&harness, cx, "media-progress");
    assert!(!shows(harness.window, "video-play-badge", cx));
    until_half_here(&harness, cx, url, 17);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    // Another click on the bubble while it comes does not start it again,
    // nor open anything.
    let calls = harness.mock.media_calls();
    click(harness.window, "video-length", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.media_calls(), calls, "not asked for again");
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // It is here: the one who clicked is taken to it.
    until(&harness, cx, |shell| shell.overlay == Overlay::Video);
    until_shown(&harness, cx, "video-frame");
    assert!(cx.update(|cx| harness.shell.read(cx).video.playing()));
}

#[gpui_kit::test]
fn a_video_that_arrives_when_the_window_is_busy_is_left_ready_not_opened(cx: &mut TestAppContext) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    let url = "https://media.example/later.mp4";
    harness.mock.set_media_latency(Duration::from_secs(1));
    push_video(&harness, cx, "m-video", url);
    click(harness.window, "media-open", cx);
    until_shown(&harness, cx, "media-progress");

    // Something else is on screen when it arrives.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_overlay(Overlay::Shortcuts, window, cx)
        })
    })
    .unwrap();
    until(&harness, cx, |shell| {
        shell.media.file_state(url) == crate::ui::media::FileState::Ready
    });
    harness.settle(cx);
    assert_eq!(
        overlay(&harness, cx),
        Overlay::Shortcuts,
        "nothing is taken over"
    );
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(!cx.update(|cx| harness.shell.read(cx).video.playing()));

    // Ready: the bubble offers to play it, and the next click does.
    assert!(shows(harness.window, "video-play-badge", cx));
    assert!(!shows(harness.window, "media-progress", cx));
    click(harness.window, "media-open", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Video);
    until_shown(&harness, cx, "video-frame");
}

#[gpui_kit::test]
fn a_video_stopped_while_it_downloads_does_not_open_when_it_comes_anyway(cx: &mut TestAppContext) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    let url = "https://media.example/stopped.mp4";
    harness.mock.set_media_latency(Duration::from_secs(1));
    push_video(&harness, cx, "m-video", url);
    click(harness.window, "media-open", cx);
    until_shown(&harness, cx, "media-progress");

    // Stopped with the bubble's own button: the click that asked for it
    // is withdrawn.
    click(harness.window, "media-progress", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "media-progress", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).video.waiting.is_none()));

    // Fetched some other way, it is ready, and nothing opens.
    harness.mock.set_media_latency(Duration::ZERO);
    harness.engine.want_file(
        &cx.update(|cx| harness.shell.read(cx).account.clone().unwrap()),
        url,
    );
    until(&harness, cx, |shell| {
        shell.media.file_state(url) == crate::ui::media::FileState::Ready
    });
    harness.settle(cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn a_video_that_fails_to_download_shows_the_retry_on_the_bubble_not_the_player(
    cx: &mut TestAppContext,
) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    let url = "https://media.example/refused.mp4";
    push_video(&harness, cx, "m-video", url);
    harness
        .mock
        .fail_next_media([rejected("forbidden", "The file could not be read.")]);
    click(harness.window, "media-open", cx);
    until_shown(&harness, cx, "media-retry");
    assert_eq!(overlay(&harness, cx), Overlay::None, "no empty player");
    assert!(!shows(harness.window, "video-play-badge", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).video.waiting.is_none()));

    // The button tries again, and it comes: ready to play.
    click(harness.window, "media-retry", cx);
    until_shown(&harness, cx, "video-play-badge");
    assert_eq!(overlay(&harness, cx), Overlay::None);
    click(harness.window, "media-open", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Video);
    until_shown(&harness, cx, "video-frame");
}
