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
    assert_eq!(overlay(&harness, cx), Overlay::Video);
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
    until_shown(&harness, cx, "video-failed");
    // The way out is there (the test platform opens nothing with it).
    assert!(shows(harness.window, "video-system", cx));
    assert!(!cx.update(|cx| harness.shell.read(cx).video.playing()));
}
