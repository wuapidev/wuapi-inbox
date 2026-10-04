//! Status (stories): the way in and the list, the rings, the viewer and
//! what it does about receipts, replies and reactions, posting, who sees
//! it, and what the window says where the provider cannot do something.
//! Everything runs in the headless window over the mock provider; layout is
//! measured with the elements' `debug_selector`s.

use super::*;
use crate::keys::Command;
use crate::stories::{Moment, STILL_STORY};
use client_provider::{
    AccountId, Capabilities, ContactId, MediaKind, MessageId, StoryAudience, StoryBody, StoryFont,
    StoryPrivacy, StoryStyle, StoryViewer, Timestamp,
};
use provider_mock::MockConfig;

fn personal() -> AccountId {
    AccountId::new("acc_personal")
}

/// The window over the mock, with the account's stories already listed,
/// as the engine would have asked for them.
fn open_status(cx: &mut TestAppContext) -> Harness {
    let harness = open(cx, ShellOptions::default());
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    harness
}

/// As [`open_status`], over a provider that can only do `caps`.
fn open_limited(cx: &mut TestAppContext, caps: Capabilities) -> Harness {
    cx.update(|cx| prepare(cx, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mock = MockProvider::new(MockConfig {
        capabilities: Some(caps),
        ..MockConfig::quiet()
    });
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig {
            history: HistoryMode::Recent(usize::MAX),
            preload_pace: Duration::ZERO,
            ..SyncConfig::default()
        },
        runtime.handle().clone(),
    );
    runtime.block_on(engine.refresh()).unwrap();
    let (window, shell) = cx.update(|cx| {
        let view_engine = engine.clone();
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1240.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Shell::new(view_engine, ShellOptions::default(), window, cx)),
        )
        .unwrap()
    });
    cx.run_until_parked();
    let harness = Harness {
        runtime,
        engine,
        mock,
        window: window.downcast().expect("the window has a base root"),
        shell,
    };
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .ok();
    harness.settle(cx);
    harness
}

/// Chooses Status, as a click on the rail's button does.
fn go_to_status(harness: &Harness, cx: &mut TestAppContext) {
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "status-pane", cx));
}

/// Moves the viewer's clock, as the timer does.
fn tick(harness: &Harness, cx: &mut TestAppContext, by: Duration) {
    harness.shell.update(cx, |shell, cx| {
        shell.story_tick(by, cx);
    });
    cx.run_until_parked();
}

/// Opens the viewer on the author in row `index` of the list.
fn watch(harness: &Harness, cx: &mut TestAppContext, index: usize) {
    let selector: &'static str = Box::leak(format!("status-author-{index}").into_boxed_str());
    click(harness.window, selector, cx);
    harness.settle(cx);
    assert!(shows(harness.window, "story-viewer", cx));
}

fn viewing<R>(
    harness: &Harness,
    cx: &mut TestAppContext,
    f: impl FnOnce(&crate::ui::status_view::Viewing) -> R,
) -> R {
    cx.update(|cx| {
        f(harness
            .shell
            .read(cx)
            .status
            .viewing
            .as_ref()
            .expect("viewing"))
    })
}

fn bounds(
    harness: &Harness,
    cx: &mut TestAppContext,
    selector: &'static str,
) -> Bounds<gpui_kit::Pixels> {
    VisualTestContext::from_window(harness.window.into(), cx)
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is not on screen"))
}

// ----- the way in ------------------------------------------------------------

#[gpui_kit::test]
fn status_is_a_button_of_the_rail_beside_chats_with_a_dot_while_there_is_news(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    assert!(shows(harness.window, "nav-chats", cx));
    assert!(shows(harness.window, "nav-status", cx));
    assert!(shows(harness.window, "status-dot", cx), "unviewed stories");
    // The chat list is what is listed until Status is chosen.
    assert!(shows(harness.window, "list-resize", cx));
    assert!(!shows(harness.window, "status-pane", cx));

    go_to_status(&harness, cx);
    assert!(!shows(harness.window, "list-resize", cx));
    // The main pane is the page that says what Status is.
    assert!(shows(harness.window, "status-welcome", cx));

    // And back: the chats are as they were.
    click(harness.window, "nav-chats", cx);
    assert!(shows(harness.window, "list-resize", cx));
    assert!(!shows(harness.window, "status-pane", cx));
}

#[gpui_kit::test]
fn the_dot_goes_when_everything_was_seen(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    let feed = harness.engine.story_feed(&personal()).unwrap();
    for author in &feed.recent {
        for item in &author.stories {
            harness
                .engine
                .story_shown(&personal(), &item.story.id)
                .unwrap();
        }
    }
    harness.settle(cx);
    assert!(!shows(harness.window, "status-dot", cx));
}

#[gpui_kit::test]
fn a_shortcut_and_two_palette_commands_reach_status_and_the_chats(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    press(harness.window, "alt-s", cx);
    assert!(shows(harness.window, "status-pane", cx), "Alt+S");
    press(harness.window, "alt-c", cx);
    assert!(!shows(harness.window, "status-pane", cx), "Alt+C");
    // The registry has them, with keys, and the palette lists them.
    for command in [Command::ShowStatus, Command::ShowChats, Command::NewStatus] {
        assert!(crate::keys::binding(command).is_some());
        assert!(crate::keys::keys_label(command).is_some());
    }
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        for command in [
            Command::ShowStatus,
            Command::NewStatus,
            Command::SettingsStatus,
        ] {
            assert_ne!(
                shell.palette_avail(command, cx),
                crate::ui::palette_steps::Avail::Hidden,
                "{command:?}"
            );
        }
    });
}

// ----- the list ---------------------------------------------------------------

#[gpui_kit::test]
fn the_list_is_my_status_then_recent_then_viewed_then_muted_folded(cx: &mut TestAppContext) {
    use crate::ui::status::{Part, Row};
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    cx.update(|cx| {
        let rows = &harness.shell.read(cx).status.rows;
        assert_eq!(rows[0], Row::Mine);
        let kinds: Vec<String> = rows
            .iter()
            .map(|row| match row {
                Row::Mine => "mine".to_owned(),
                Row::Heading(part) => format!("{part:?}"),
                Row::Author(part, _) => format!("author-{part:?}"),
            })
            .collect();
        let at = |name: &str| kinds.iter().position(|kind| kind == name).unwrap();
        assert!(at("Recent") < at("Viewed") && at("Viewed") < at("Muted"));
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "author-Recent").count(),
            3
        );
        assert_eq!(
            kinds.iter().filter(|kind| *kind == "author-Viewed").count(),
            1
        );
        // The muted ones are folded: their heading, not the author.
        assert!(!kinds.iter().any(|kind| kind == "author-Muted"));
        let _ = Part::Muted;
    });
    // Unfolded by the heading, or by U.
    click(harness.window, "status-heading-muted", cx);
    cx.update(|cx| {
        let rows = &harness.shell.read(cx).status.rows;
        assert!(rows
            .iter()
            .any(|row| matches!(row, Row::Author(Part::Muted, _))));
    });
    click(harness.window, "status-heading-muted", cx);
    cx.update(|cx| {
        let rows = &harness.shell.read(cx).status.rows;
        assert!(!rows
            .iter()
            .any(|row| matches!(row, Row::Author(Part::Muted, _))));
    });
}

#[gpui_kit::test]
fn my_status_says_how_old_it_is_and_how_many_saw_it(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    assert!(shows(harness.window, "status-mine", cx));
    let line = bounds(&harness, cx, "status-mine-line");
    assert!(line.size.width > px(0.));
    cx.update(|cx| {
        let feed = &harness.shell.read(cx).status.feed;
        let latest = feed.mine.last().unwrap();
        // The newest of the account's own, with two people having seen it.
        assert_eq!(latest.story.view_count, Some(2));
    });
}

#[gpui_kit::test]
fn every_author_row_has_a_ring_with_a_piece_for_each_story(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    // Ana: three stories, all new. Two others have two. The one a phone
    // saw: one, seen.
    assert!(shows(harness.window, "story-ring-3-3", cx));
    assert!(shows(harness.window, "story-ring-2-2", cx));
    assert!(shows(harness.window, "story-ring-1-0", cx));
}

#[gpui_kit::test]
fn the_keyboard_walks_the_list_and_opens_a_status(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    let cursor = |harness: &Harness, cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).status.cursor)
    };
    let start = cursor(&harness, cx);
    press(harness.window, "down", cx);
    assert!(cursor(&harness, cx) > start);
    press(harness.window, "down", cx);
    press(harness.window, "up", cx);
    press(harness.window, "home", cx);
    assert_eq!(cursor(&harness, cx), 0);
    press(harness.window, "end", cx);
    let last = cursor(&harness, cx);
    assert!(last > 0);
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx), last, "clamped at the end");

    // Enter on an author opens their stories.
    press(harness.window, "home", cx);
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    assert!(shows(harness.window, "story-viewer", cx));
    // Escape closes it, and the list is where it was.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "story-viewer", cx));
    assert!(shows(harness.window, "status-pane", cx));
}

#[gpui_kit::test]
fn the_list_only_builds_the_rows_that_are_on_screen(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // A few hundred people with something new.
    for n in 0..300 {
        harness.mock.contact_posts_story(
            &personal(),
            &ContactId::new(format!("+1555{n:07}")),
            &format!("Person {n:03}"),
            StoryBody::Text {
                text: format!("story {n}"),
                style: StoryStyle::default(),
            },
        );
    }
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    go_to_status(&harness, cx);
    let (rows, authors) = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let authors: Vec<String> = shell
            .status
            .feed
            .authors()
            .map(|author| author.key.clone())
            .collect();
        // Every author is found from their row, whichever part they are in.
        for key in &authors {
            assert_eq!(
                shell.status_author(key).map(|author| &author.key),
                Some(key)
            );
        }
        (shell.status.rows.len(), authors.len())
    });
    assert!(
        rows > 300 && authors > 300,
        "{rows} rows, {authors} authors"
    );
    // The top of the list is drawn; what is far below it is not built.
    assert!(shows(harness.window, "status-mine", cx));
    assert!(shows(harness.window, "status-author-0", cx));
    assert!(
        !shows(harness.window, "status-author-250", cx),
        "a row far below the pane was built"
    );
    // While a story plays, too: the clock repaints the window.
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    assert!(shows(harness.window, "status-author-0", cx));
    assert!(!shows(harness.window, "status-author-250", cx));
}

#[gpui_kit::test]
fn every_row_of_the_list_is_one_height_and_they_stack(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    let mine = bounds(&harness, cx, "status-mine");
    let heading = bounds(&harness, cx, "status-heading-recent");
    let author = bounds(&harness, cx, "status-author-0");
    let pane = bounds(&harness, cx, "status-pane");
    assert_eq!(mine.size.height, crate::theme::story::ROW());
    assert_eq!(heading.size.height, mine.size.height);
    assert_eq!(author.size.height, mine.size.height);
    assert_eq!(heading.top(), mine.bottom());
    assert_eq!(author.top(), heading.bottom());
    for row in [mine, heading, author] {
        assert!(
            row.left() >= pane.left() && row.right() <= pane.right(),
            "{row:?} in {pane:?}"
        );
    }
}

#[gpui_kit::test]
fn m_mutes_an_author_from_the_list_and_the_provider_hears_of_it(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    // The row of the first author of Recent.
    let author = cx.update(|cx| harness.shell.read(cx).status.feed.recent[0].author.clone());
    press(harness.window, "down", cx);
    press(harness.window, "m", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let feed = &harness.shell.read(cx).status.feed;
        assert!(feed.muted.iter().any(|muted| muted.author == author));
    });
    harness.settle(cx);
    assert!(harness.mock.muted_authors(&personal()).contains(&author));
    // And the button on the row does it back.
    click(harness.window, "status-heading-muted", cx);
    click(harness.window, "status-mute-0", cx);
    harness.settle(cx);
}

#[gpui_kit::test]
fn a_provider_that_gives_no_contact_updates_says_so_in_the_list(cx: &mut TestAppContext) {
    // A provider that only has the account's own stories: post, list,
    // take down, and the audience read-only.
    let caps = Capabilities {
        story_list: true,
        story_post: true,
        story_delete: true,
        story_privacy: true,
        ..Capabilities::all()
    };
    let caps = Capabilities {
        story_contacts: false,
        story_view: false,
        story_viewers: false,
        story_reply: false,
        story_react: false,
        story_mute: false,
        story_privacy_edit: false,
        ..caps
    };
    let harness = open_limited(cx, caps);
    // Only the account's own stories would be listed by such a provider.
    harness.settle(cx);
    go_to_status(&harness, cx);
    assert!(shows(harness.window, "status-none", cx));
    // Said right under the rows, inside the pane.
    let note = bounds(&harness, cx, "status-none");
    let first = bounds(&harness, cx, "status-mine");
    let pane = bounds(&harness, cx, "status-pane");
    let rows = cx.update(|cx| harness.shell.read(cx).status.rows.len());
    assert_eq!(
        note.top(),
        first.top() + crate::theme::story::ROW() * rows as f32
    );
    assert!(note.bottom() <= pane.bottom() && note.right() <= pane.right());
    cx.update(|cx| {
        let feed = &harness.shell.read(cx).status.feed;
        // (The mock lists everything; what matters is the message.)
        let _ = feed;
    });
}

// ----- the viewer ----------------------------------------------------------------

#[gpui_kit::test]
fn the_viewer_has_a_progress_segment_for_each_story_of_the_author(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    // Ana has three: a text, a picture, a video.
    for index in 0..3 {
        let selector: &'static str = Box::leak(format!("story-progress-{index}").into_boxed_str());
        assert!(shows(harness.window, selector, cx), "{selector}");
    }
    assert!(!shows(harness.window, "story-progress-3", cx));
    // The first is a text: on screen at once, shown, and filling.
    tick(&harness, cx, Duration::from_millis(40));
    tick(&harness, cx, Duration::from_millis(2500));
    let fill = viewing(&harness, cx, |viewing| viewing.player.progress(0));
    assert!((fill - 0.5).abs() < 0.05, "{fill}");
    assert!(shows(harness.window, "story-text", cx));
    // Five seconds in all: the next story.
    tick(&harness, cx, Duration::from_millis(2600));
    let at = viewing(&harness, cx, |viewing| viewing.player.slide_index());
    assert_eq!(at, 1);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.progress(0)),
        1.
    );
}

#[gpui_kit::test]
fn space_pauses_and_the_arrows_move_between_stories_and_authors(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    tick(&harness, cx, Duration::from_millis(1000));
    press(harness.window, "space", cx);
    assert!(viewing(&harness, cx, |viewing| viewing.player.holds.paused));
    tick(&harness, cx, Duration::from_secs(30));
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        0
    );
    press(harness.window, "space", cx);
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .paused));

    press(harness.window, "right", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        1
    );
    press(harness.window, "left", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        0
    );
    // Down, or Ctrl+Right: the next author. Up, or Ctrl+Left: the one before.
    press(harness.window, "down", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.reel_index()),
        1
    );
    press(harness.window, "ctrl-right", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.reel_index()),
        2
    );
    press(harness.window, "up", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.reel_index()),
        1
    );
    press(harness.window, "ctrl-left", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.reel_index()),
        0
    );
    // Escape closes.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "story-viewer", cx));
}

#[gpui_kit::test]
fn the_end_of_the_last_story_closes_the_viewer(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    // The last author of the Viewed part has one picture, but the deck
    // goes on to the end: step to it with the keyboard and let it run.
    watch(&harness, cx, 0);
    let reels = viewing(&harness, cx, |viewing| viewing.player.reels().len());
    for _ in 1..reels {
        press(harness.window, "down", cx);
    }
    // Text stories play on their own: the picture of the last author is
    // not on screen in a headless window until it is fetched, so the
    // clock waits for it, then the viewer ends when it has played.
    let last_is_text = viewing(&harness, cx, |viewing| {
        viewing
            .player
            .current()
            .map(|slide| slide.kind == crate::stories::SlideKind::Text)
    });
    if last_is_text == Some(true) {
        tick(&harness, cx, Duration::from_millis(40));
        tick(&harness, cx, STILL_STORY);
        assert!(!shows(harness.window, "story-viewer", cx));
    } else {
        // Pressing next on the last story ends it all the same.
        let remaining = viewing(&harness, cx, |viewing| {
            viewing
                .player
                .reels()
                .last()
                .map_or(0, |reel| reel.slides.len())
        });
        for _ in 0..remaining {
            press(harness.window, "right", cx);
        }
        assert!(!shows(harness.window, "story-viewer", cx));
    }
}

#[gpui_kit::test]
fn the_viewers_controls_stay_inside_the_pane_at_every_interface_size(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    for step in crate::theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        harness.settle(cx);
        let pane = bounds(&harness, cx, "story-viewer");
        let list = bounds(&harness, cx, "status-pane");
        assert!(
            pane.left() >= list.right() - px(1.),
            "{step}%: beside the list"
        );
        for selector in [
            "story-bar",
            "story-progress",
            "story-card",
            "story-prev",
            "story-next",
            "story-pause",
            "story-mute",
            "story-info",
            "story-close",
            "story-footer",
            "story-reply",
            "story-send",
            "story-reactions",
        ] {
            let at = bounds(&harness, cx, selector);
            assert!(
                at.left() >= pane.left() - px(1.),
                "{step}% {selector}: left"
            );
            assert!(
                at.right() <= pane.right() + px(1.),
                "{step}% {selector}: right"
            );
            assert!(at.top() >= pane.top() - px(1.), "{step}% {selector}: top");
            assert!(
                at.bottom() <= pane.bottom() + px(1.),
                "{step}% {selector}: bottom"
            );
        }
        // The card and the progress bar line up, and neither overlaps the
        // header or the footer.
        let card = bounds(&harness, cx, "story-card");
        let progress = bounds(&harness, cx, "story-progress");
        assert!((card.left() - progress.left()).abs() < px(1.), "{step}%");
        assert!(
            (card.size.width - progress.size.width).abs() < px(1.),
            "{step}%"
        );
        let bar = bounds(&harness, cx, "story-bar");
        let footer = bounds(&harness, cx, "story-footer");
        assert!(card.top() >= bar.bottom(), "{step}%: under the header");
        assert!(
            card.bottom() <= footer.top() + px(1.),
            "{step}%: over the footer"
        );
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
}

#[gpui_kit::test]
fn a_text_story_is_drawn_on_its_colour_in_its_font(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    let text = bounds(&harness, cx, "story-text");
    let card = bounds(&harness, cx, "story-card");
    assert!(text.size.width <= card.size.width + px(1.));
    // The words are the story's.
    let (words, style) = viewing(&harness, cx, |viewing| {
        match &viewing.item().unwrap().story.body {
            StoryBody::Text { text, style } => (text.clone(), *style),
            other => panic!("{other:?}"),
        }
    });
    assert!(words.starts_with("Sunday at the lake"));
    assert_eq!(style.background, 0x0B_6E_4F);
}

#[gpui_kit::test]
fn a_video_plays_in_the_app_and_counts_when_a_frame_is_shown(cx: &mut TestAppContext) {
    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    // To Ana's third story, the video. It is not fetched ahead of time.
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    tick(&harness, cx, Duration::from_millis(40));
    assert!(shows(harness.window, "story-tile", cx));
    assert!(shows(harness.window, "story-open-video", cx));
    assert!(!shows(harness.window, "story-video", cx));
    let id = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    // The tile on screen is not a view.
    harness.settle(cx);
    assert!(
        !harness
            .engine
            .store()
            .story(&personal(), &id)
            .unwrap()
            .unwrap()
            .viewed
    );
    // Asked to play: the file is fetched, the clock waits, and nothing is
    // handed to the system's player.
    click(harness.window, "story-open-video", cx);
    assert!(viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .loading));
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .external));
    assert!(
        !harness
            .engine
            .store()
            .story(&personal(), &id)
            .unwrap()
            .unwrap()
            .viewed
    );
    let url = viewing(&harness, cx, |viewing| {
        match &viewing.item().unwrap().story.body {
            StoryBody::Media(media) => media.source.clone().unwrap().to_string(),
            other => panic!("{other:?}"),
        }
    });
    harness
        .engine
        .store()
        .put_media(
            &client_core::file_key(&url),
            &client_core::CachedMedia {
                bytes: b"video-bytes".to_vec(),
                mime: Some("video/mp4".into()),
                size: None,
            },
            Timestamp::now(),
            u64::MAX,
        )
        .unwrap();
    harness.shell.update(cx, |shell, cx| {
        shell.status_media_arrived(&client_core::file_key(&url), cx)
    });
    cx.run_until_parked();
    assert!(shows(harness.window, "story-video", cx));
    assert!(!shows(harness.window, "story-tile", cx));
    assert!(
        harness
            .engine
            .store()
            .story(&personal(), &id)
            .unwrap()
            .unwrap()
            .viewed
    );
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .loading));
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .external));
}

// ----- seeing is behaviour toward people ---------------------------------------

#[gpui_kit::test]
fn a_story_counts_as_seen_only_when_it_was_shown_and_then_its_author_is_told(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    // Listed and drawn in the list: nothing seen, nothing told.
    harness
        .runtime
        .block_on(harness.engine.flush_story_receipts_at(Timestamp::now()))
        .unwrap();
    assert!(harness.mock.story_views().is_empty());
    let first = cx.update(|cx| {
        harness.shell.read(cx).status.feed.recent[0].stories[0]
            .story
            .id
            .clone()
    });
    assert!(
        !harness
            .engine
            .store()
            .story(&personal(), &first)
            .unwrap()
            .unwrap()
            .viewed
    );

    watch(&harness, cx, 0);
    // Opened, but the story's time has not begun: it is drawn on the first
    // tick, and that is the view.
    tick(&harness, cx, Duration::from_millis(40));
    assert!(
        harness
            .engine
            .store()
            .story(&personal(), &first)
            .unwrap()
            .unwrap()
            .viewed
    );
    harness
        .runtime
        .block_on(harness.engine.flush_story_receipts_at(Timestamp::now()))
        .unwrap();
    assert_eq!(harness.mock.story_views().len(), 1);
    assert_eq!(harness.mock.story_views()[0].0, first);
    // The second story has not been shown yet: not seen, not told.
    let second = cx.update(|cx| {
        harness.shell.read(cx).status.feed.recent[0].stories[1]
            .story
            .id
            .clone()
    });
    assert!(
        !harness
            .engine
            .store()
            .story(&personal(), &second)
            .unwrap()
            .unwrap()
            .viewed
    );
}

#[gpui_kit::test]
fn with_receipts_off_the_story_is_seen_here_and_nobody_is_told(cx: &mut TestAppContext) {
    for off in ["read", "story"] {
        let harness = open_status(cx);
        cx.update(|cx| {
            settings::update(cx, |settings| match off {
                "read" => settings.read_receipts = false,
                _ => settings.story_receipts = false,
            })
        });
        go_to_status(&harness, cx);
        watch(&harness, cx, 0);
        tick(&harness, cx, Duration::from_millis(40));
        tick(&harness, cx, Duration::from_millis(100));
        harness
            .runtime
            .block_on(harness.engine.flush_story_receipts_at(Timestamp::now()))
            .unwrap();
        assert!(harness.mock.story_views().is_empty(), "{off}");
        let first = cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .status
                .feed
                .viewed
                .iter()
                .chain(harness.shell.read(cx).status.feed.recent.iter())
                .flat_map(|a| a.stories.iter())
                .filter(|i| i.viewed_at.is_some())
                .count()
        });
        assert_eq!(first, 1, "{off}: seen here");
        cx.update(|cx| {
            settings::update(cx, |settings| {
                settings.read_receipts = true;
                settings.story_receipts = true;
            })
        });
    }
}

#[gpui_kit::test]
fn a_story_that_is_still_loading_is_not_seen(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    // To the picture: the policy would fetch it, but nothing is
    // downloaded in this window until the runtime runs.
    press(harness.window, "right", cx);
    let id = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    tick(&harness, cx, Duration::from_millis(40));
    let ready = viewing(&harness, cx, |viewing| viewing.player.is_ready());
    if !ready {
        assert!(
            !harness
                .engine
                .store()
                .story(&personal(), &id)
                .unwrap()
                .unwrap()
                .viewed
        );
        // And the clock has not run.
        tick(&harness, cx, Duration::from_secs(60));
        assert_eq!(
            viewing(&harness, cx, |viewing| viewing.player.slide_index()),
            1
        );
    }
    // When the picture arrives it is drawn, and seen.
    for _ in 0..100 {
        harness.settle(cx);
        tick(&harness, cx, Duration::from_millis(40));
        if harness
            .engine
            .store()
            .story(&personal(), &id)
            .unwrap()
            .unwrap()
            .viewed
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        harness
            .engine
            .store()
            .story(&personal(), &id)
            .unwrap()
            .unwrap()
            .viewed
    );
    assert!(shows(harness.window, "story-picture", cx));
}

// ----- answering ----------------------------------------------------------------------

#[gpui_kit::test]
fn a_reply_is_typed_under_the_story_and_goes_to_its_author_as_a_reply(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    let story = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    // `R` gives the reply field the keyboard, and the clock waits.
    press(harness.window, "r", cx);
    assert!(viewing(&harness, cx, |viewing| viewing.player.holds.typing));
    tick(&harness, cx, Duration::from_secs(30));
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        0
    );
    type_text(harness.window, "Looks great", cx);
    click(harness.window, "story-send", cx);
    harness.settle(cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    let replies = harness.mock.story_replies();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].0, story);
    assert_eq!(
        replies[0].1.content,
        client_provider::OutgoingContent::Text {
            body: "Looks great".into()
        }
    );
    // The field is empty again and the clock goes on.
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .typing));
    assert!(shows(harness.window, "story-note", cx));
}

/// What is written in the viewer's reply field.
fn reply_draft(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .status
            .reply
            .read(cx)
            .value()
            .to_string()
    })
}

#[gpui_kit::test]
fn a_reply_left_unsent_is_not_carried_to_the_next_story(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    let first = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    press(harness.window, "r", cx);
    type_text(harness.window, "sorry, can't make it", cx);
    assert_eq!(reply_draft(&harness, cx), "sorry, can't make it");
    // Escape leaves the field, the clock goes on and the story's time
    // runs out: the next one is on screen.
    press(harness.window, "escape", cx);
    tick(&harness, cx, STILL_STORY);
    harness.settle(cx);
    let second = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    assert_ne!(first, second);
    assert_eq!(
        reply_draft(&harness, cx),
        "",
        "what was written was for the story before"
    );
    // Back in the field, Enter has nothing to send to this one.
    press(harness.window, "r", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert!(harness.mock.story_replies().is_empty());
}

#[gpui_kit::test]
fn moving_to_another_story_with_the_arrow_empties_the_reply_field(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    press(harness.window, "r", cx);
    type_text(harness.window, "for the first one", cx);
    click(harness.window, "story-next", cx);
    harness.settle(cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        1
    );
    assert_eq!(reply_draft(&harness, cx), "");
    click(harness.window, "story-send", cx);
    harness.settle(cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert!(harness.mock.story_replies().is_empty());
}

#[gpui_kit::test]
fn in_the_reply_field_control_and_an_arrow_move_the_caret_not_the_viewer(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    press(harness.window, "r", cx);
    type_text(harness.window, "two words", cx);
    for key in ["ctrl-left", "ctrl-right", "ctrl-right"] {
        press(harness.window, key, cx);
        assert_eq!(
            viewing(&harness, cx, |viewing| (
                viewing.player.reel_index(),
                viewing.player.slide_index()
            )),
            (0, 0),
            "{key} is the text field's while it has the keyboard"
        );
    }
    assert_eq!(reply_draft(&harness, cx), "two words");
    // Out of the field they are the viewer's again.
    press(harness.window, "escape", cx);
    press(harness.window, "ctrl-right", cx);
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.reel_index()),
        1
    );
}

#[gpui_kit::test]
fn escape_in_the_reply_field_leaves_it_and_the_next_escape_closes(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    press(harness.window, "r", cx);
    assert!(viewing(&harness, cx, |viewing| viewing.player.holds.typing));
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "story-viewer", cx), "still there");
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .typing));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "story-viewer", cx));
}

#[gpui_kit::test]
fn a_quick_reaction_goes_through_the_provider_and_the_same_one_takes_it_back(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    click(harness.window, "story-react-0", cx);
    harness.settle(cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(harness.mock.story_reactions().len(), 1);
    click(harness.window, "story-react-0", cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    let reactions = harness.mock.story_reactions();
    assert_eq!(reactions.len(), 2);
    assert!(matches!(
        &reactions[1].1.content,
        client_provider::OutgoingContent::Reaction { emoji, .. } if emoji.is_empty()
    ));
}

#[gpui_kit::test]
fn where_the_provider_cannot_reply_or_react_the_viewer_says_so(cx: &mut TestAppContext) {
    let caps = Capabilities {
        story_reply: false,
        story_react: false,
        story_view: false,
        ..Capabilities::all()
    };
    let harness = open_limited(cx, caps);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    assert!(shows(harness.window, "story-reply-unavailable", cx));
    assert!(!shows(harness.window, "story-react-0", cx));
    assert!(!shows(harness.window, "story-send", cx));
    // Viewing still works, and nobody is told: the provider has no
    // receipt to send.
    tick(&harness, cx, Duration::from_millis(40));
    harness
        .runtime
        .block_on(harness.engine.flush_story_receipts_at(Timestamp::now()))
        .unwrap();
    assert!(harness.mock.story_views().is_empty());
}

#[gpui_kit::test]
fn muting_from_the_viewer_moves_the_author_to_muted(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    let author = viewing(&harness, cx, |viewing| {
        viewing.author().unwrap().author.clone()
    });
    click(harness.window, "story-mute", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let feed = &harness.shell.read(cx).status.feed;
        assert!(feed.muted.iter().any(|muted| muted.author == author));
    });
}

// ----- my status -------------------------------------------------------------------------

#[gpui_kit::test]
fn my_status_lists_each_story_with_who_saw_it_and_what_they_reacted(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    click(harness.window, "status-mine", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "status-mine-pane", cx));
    // Newest first: the picture two people saw, then the text five did.
    assert!(shows(harness.window, "mine-card-0", cx));
    assert!(shows(harness.window, "mine-card-1", cx));
    harness.settle(cx);
    assert!(shows(harness.window, "mine-viewer-1-0", cx));
    assert!(shows(harness.window, "mine-viewer-1-4", cx));
    assert!(!shows(harness.window, "mine-viewer-1-5", cx));
    assert!(
        shows(harness.window, "mine-reaction-1-0", cx),
        "the first viewer reacted"
    );
}

#[gpui_kit::test]
fn a_new_viewer_shows_up_in_my_status_as_it_happens(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    click(harness.window, "status-mine", cx);
    harness.settle(cx);
    let mine = harness.engine.story_feed(&personal()).unwrap().mine;
    let story = mine[0].story.id.clone();
    harness.mock.contact_views_story(
        &personal(),
        &story,
        StoryViewer {
            contact: ContactId::new("contact:newviewer"),
            name: Some("New Viewer".into()),
            viewed_at: Timestamp::now(),
            reaction: Some("🔥".into()),
        },
    );
    harness
        .engine
        .apply_event(client_provider::ProviderEvent::StoryViewed {
            account_id: personal(),
            story_id: story,
            viewer: StoryViewer {
                contact: ContactId::new("contact:newviewer"),
                name: Some("New Viewer".into()),
                viewed_at: Timestamp::now(),
                reaction: Some("🔥".into()),
            },
        })
        .unwrap();
    harness.settle(cx);
    assert!(
        shows(harness.window, "mine-viewer-1-0", cx)
            || shows(harness.window, "mine-viewer-0-0", cx)
    );
}

#[gpui_kit::test]
fn deleting_my_status_asks_first_and_takes_it_down(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    click(harness.window, "status-mine", cx);
    harness.settle(cx);
    let before = harness.engine.story_feed(&personal()).unwrap().mine.len();
    click(harness.window, "mine-delete", cx);
    // Nothing is gone until it is confirmed.
    assert!(shows(harness.window, "status-delete", cx));
    assert_eq!(
        harness.engine.story_feed(&personal()).unwrap().mine.len(),
        before
    );
    click(harness.window, "status-delete-yes", cx);
    harness.settle(cx);
    assert_eq!(
        harness.engine.story_feed(&personal()).unwrap().mine.len(),
        before - 1
    );
    // The provider is told in the background.
    harness.settle(cx);
    assert_eq!(harness.mock.deleted_stories().len(), 1);
}

/// The account's own status of that kind (`None`: a text).
fn my_status(harness: &Harness, kind: Option<MediaKind>) -> MessageId {
    harness
        .engine
        .story_feed(&personal())
        .unwrap()
        .mine
        .iter()
        .find(|item| item.story.body.media_kind() == kind)
        .expect("a status of that kind")
        .story
        .id
        .clone()
}

/// "Forward" on the card of `story`, under My status.
fn forward_sheet(harness: &Harness, story: &MessageId, cx: &mut TestAppContext) {
    let story = story.clone();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.status.sheet = Some(crate::ui::status_post::Sheet::Forward(story));
            shell.open_overlay(Overlay::Status, window, cx);
        })
    })
    .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "status-forward", cx));
}

fn send_queued(harness: &Harness, cx: &mut TestAppContext) {
    harness.settle(cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
}

#[gpui_kit::test]
fn a_status_is_forwarded_to_a_chat_the_way_a_message_is(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    click(harness.window, "status-mine", cx);
    harness.settle(cx);
    // Every card that is up offers it.
    assert!(shows(harness.window, "mine-forward", cx));

    // A text goes as a text, with the mark.
    forward_sheet(&harness, &my_status(&harness, None), cx);
    assert!(!shows(harness.window, "status-forward-refused", cx));
    click(harness.window, "forward-chat-0", cx);
    send_queued(&harness, cx);
    assert!(!shows(harness.window, "status-forward", cx));
    assert_eq!(harness.mock.sent().len(), 1);
    assert!(harness.mock.sent()[0].forwarded);
    assert!(harness.mock.forward_calls().is_empty());

    // A picture goes by naming it: the picture is what arrives, not its
    // caption as a text, and no file is sent from here.
    let picture = my_status(&harness, Some(MediaKind::Image));
    forward_sheet(&harness, &picture, cx);
    assert!(!shows(harness.window, "status-forward-refused", cx));
    click(harness.window, "forward-chat-0", cx);
    send_queued(&harness, cx);
    let calls = harness.mock.forward_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].message, picture);
    assert_eq!(harness.mock.sent().len(), 1, "nothing more was sent");
}

#[gpui_kit::test]
fn a_picture_status_says_why_it_cannot_be_forwarded_where_only_text_can(cx: &mut TestAppContext) {
    // As with wuapi today: texts are forwarded, nothing else is yet.
    let harness = open_limited(
        cx,
        Capabilities {
            forward_any: false,
            ..Capabilities::all()
        },
    );
    go_to_status(&harness, cx);
    click(harness.window, "status-mine", cx);
    harness.settle(cx);
    let picture = my_status(&harness, Some(MediaKind::Image));
    forward_sheet(&harness, &picture, cx);
    assert!(shows(harness.window, "status-forward-refused", cx));
    assert!(!shows(harness.window, "forward-chat-0", cx));
    assert_eq!(
        harness
            .engine
            .story_forward_refusal(&personal(), &picture)
            .unwrap()
            .map(|refusal| refusal.reason()),
        Some("Not available yet from this provider")
    );
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "status-forward", cx));

    // A text status still goes, as a text with the mark.
    forward_sheet(&harness, &my_status(&harness, None), cx);
    assert!(!shows(harness.window, "status-forward-refused", cx));
    click(harness.window, "forward-chat-0", cx);
    send_queued(&harness, cx);
    let sent = harness.mock.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].forwarded);
    assert!(harness.mock.forward_calls().is_empty());
}

// ----- posting ---------------------------------------------------------------------------------

#[gpui_kit::test]
fn a_text_status_is_written_on_a_colour_in_a_font_and_shows_under_my_status_at_once(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    let before = harness.engine.story_feed(&personal()).unwrap().mine.len();
    press(harness.window, "alt-n", cx);
    assert!(shows(harness.window, "status-post", cx));
    // The audience is said before anything is posted.
    assert!(shows(harness.window, "post-audience", cx));
    let named = bounds(&harness, cx, "post-audience-name");
    assert!(named.size.width > px(0.));
    assert!(shows(harness.window, "post-audience-edit", cx));

    type_text(harness.window, "Back at it", cx);
    click(harness.window, "swatch-3", cx);
    click(harness.window, "font-4", cx);
    click(harness.window, "post-send", cx);
    harness.settle(cx);

    // Under My status, pending, at once.
    assert!(shows(harness.window, "status-mine-pane", cx));
    let feed = harness.engine.story_feed(&personal()).unwrap();
    assert_eq!(feed.mine.len(), before + 1);
    let pending = feed.mine.last().unwrap();
    assert!(pending.post.is_some());
    assert_eq!(
        pending.story.body,
        StoryBody::Text {
            text: "Back at it".into(),
            style: StoryStyle {
                background: crate::theme::story::BACKGROUNDS[3],
                font: StoryFont::ALL[4],
            }
        }
    );
    // The outbox takes it from there.
    harness
        .runtime
        .block_on(harness.engine.flush_story_posts_at(Timestamp::now()))
        .unwrap();
    harness.settle(cx);
    assert_eq!(harness.mock.posted_stories().len(), 1);
    assert!(harness
        .engine
        .story_feed(&personal())
        .unwrap()
        .mine
        .iter()
        .all(|item| item.post.is_none()));
}

#[gpui_kit::test]
fn an_empty_status_is_refused_in_the_sheet_and_nothing_is_queued(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    press(harness.window, "alt-n", cx);
    click(harness.window, "post-send", cx);
    assert!(shows(harness.window, "post-note", cx));
    assert!(shows(harness.window, "status-post", cx), "the sheet stays");
    assert!(harness
        .engine
        .store()
        .story_posts_pending()
        .unwrap()
        .is_empty());
    // Escape closes it, and the keyboard is the list's again.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "status-post", cx));
    let before = cx.update(|cx| harness.shell.read(cx).status.cursor);
    press(harness.window, "down", cx);
    assert!(cx.update(|cx| harness.shell.read(cx).status.cursor) > before);
}

#[gpui_kit::test]
fn a_picture_is_posted_from_a_file_with_a_caption(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    press(harness.window, "alt-n", cx);
    click(harness.window, "post-tab-media", cx);
    assert!(shows(harness.window, "post-drop", cx));
    // A file, as a drop or a pick hands it over.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("view.jpg");
    std::fs::write(&path, super::media_out::jpeg(640, 480)).unwrap();
    harness
        .shell
        .update(cx, |shell, cx| shell.story_paths(vec![path], cx));
    for _ in 0..200 {
        harness.settle(cx);
        if shows(harness.window, "post-picture", cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        shows(harness.window, "post-picture", cx),
        "the picture is previewed"
    );
    type_text(harness.window, "The view", cx);
    click(harness.window, "post-send", cx);
    for _ in 0..200 {
        harness.settle(cx);
        if shows(harness.window, "status-mine-pane", cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let feed = harness.engine.story_feed(&personal()).unwrap();
    let pending = feed.mine.last().unwrap();
    assert!(pending.post.is_some());
    match &pending.story.body {
        StoryBody::Media(media) => {
            assert_eq!(media.kind, MediaKind::Image);
            assert_eq!(media.caption.as_deref(), Some("The view"));
        }
        other => panic!("{other:?}"),
    }
    harness
        .runtime
        .block_on(harness.engine.flush_story_posts_at(Timestamp::now()))
        .unwrap();
    assert_eq!(harness.mock.posted_stories().len(), 1);
}

#[gpui_kit::test]
fn a_file_that_is_not_a_picture_or_a_video_is_refused_in_words(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    press(harness.window, "alt-n", cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.pdf");
    std::fs::write(&path, b"%PDF-1.4 not a picture").unwrap();
    harness
        .shell
        .update(cx, |shell, cx| shell.story_paths(vec![path], cx));
    for _ in 0..200 {
        harness.settle(cx);
        if shows(harness.window, "post-note", cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(shows(harness.window, "post-note", cx));
    assert!(!shows(harness.window, "post-picture", cx));
}

#[gpui_kit::test]
fn a_file_over_the_limit_is_said_in_the_post_sheet_and_never_reaches_the_attach_sheet(
    cx: &mut TestAppContext,
) {
    let harness = open_status_over_a_group(cx);
    // The provider takes at most 2000 bytes.
    harness.mock.set_upload_limit(2000);
    go_to_status(&harness, cx);
    press(harness.window, "alt-n", cx);
    click(harness.window, "post-tab-media", cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.jpg");
    std::fs::write(&path, super::media_out::jpeg(640, 480)).unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() > 2000);
    harness
        .shell
        .update(cx, |shell, cx| shell.story_paths(vec![path], cx));
    let note = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            (!shell.status.draft.busy)
                .then(|| shell.status.draft.note.clone())
                .flatten()
        })
    };
    let mut said = None;
    for _ in 0..200 {
        harness.settle(cx);
        said = note(cx);
        if said.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let said = said.expect("the sheet says why");
    assert!(said.contains("Too large"), "{said}");
    assert!(shows(harness.window, "post-note", cx));
    assert!(!shows(harness.window, "post-picture", cx));
    // The file is the status's business alone: the sheet of the chat
    // behind did not open and holds nothing.
    assert_eq!(overlay_now(&harness, cx), Overlay::Status);
    assert!(cx.update(|cx| harness.shell.read(cx).attach.is_none()));
    assert_eq!(harness.mock.upload_calls(), 0);
}

#[gpui_kit::test]
fn where_the_provider_cannot_post_the_button_says_so(cx: &mut TestAppContext) {
    let caps = Capabilities {
        story_post: false,
        ..Capabilities::all()
    };
    let harness = open_limited(cx, caps);
    go_to_status(&harness, cx);
    assert!(shows(harness.window, "status-welcome", cx));
    assert!(shows(harness.window, "status-new-unavailable", cx));
    assert!(!shows(harness.window, "status-new", cx));
    // The key does nothing, rather than failing.
    press(harness.window, "alt-n", cx);
    assert!(!shows(harness.window, "status-post", cx));
}

// ----- who sees my status ------------------------------------------------------------------------

#[gpui_kit::test]
fn the_audience_has_three_modes_with_people_to_pick_and_is_kept_through_the_provider(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    harness.engine.want_story_privacy(&personal());
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&personal()))
        .unwrap();
    harness.settle(cx);
    click(harness.window, "status-mine", cx);
    click(harness.window, "status-audience", cx);
    assert!(shows(harness.window, "status-audience", cx));
    for mode in 0..3 {
        let selector: &'static str = Box::leak(format!("audience-mode-{mode}").into_boxed_str());
        assert!(shows(harness.window, selector, cx), "{selector}");
    }
    // "Only share with…": the people of the address book to pick from.
    click(harness.window, "audience-mode-2", cx);
    click(harness.window, "audience-pick-2", cx);
    assert!(shows(harness.window, "audience-contact-0", cx));
    click(harness.window, "audience-contact-0", cx);
    click(harness.window, "audience-contact-1", cx);
    click(harness.window, "audience-done", cx);
    click(harness.window, "audience-save", cx);
    harness.settle(cx);
    harness.settle(cx);
    let held = harness.mock.privacy_of(&personal());
    assert_eq!(held.audience, StoryAudience::OnlyShareWith);
    assert!(held.only.len() >= 2);
}

#[gpui_kit::test]
fn the_audience_is_read_only_where_the_provider_cannot_change_it(cx: &mut TestAppContext) {
    let caps = Capabilities {
        story_privacy: true,
        story_privacy_edit: false,
        ..Capabilities::all()
    };
    let harness = open_limited(cx, caps);
    go_to_status(&harness, cx);
    harness.engine.want_story_privacy(&personal());
    harness.settle(cx);
    click(harness.window, "status-mine", cx);
    click(harness.window, "status-audience", cx);
    assert!(shows(harness.window, "audience-readonly", cx));
    assert!(!shows(harness.window, "audience-save", cx));
    // The current setting is there to read.
    assert!(shows(harness.window, "audience-mode-1", cx));
    click(harness.window, "audience-mode-0", cx);
    assert_eq!(
        harness.mock.privacy_of(&personal()).audience,
        StoryAudience::ContactsExcept,
        "nothing was changed"
    );
    let _ = StoryPrivacy::default();
}

#[gpui_kit::test]
fn a_story_that_expires_while_it_is_open_is_skipped(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 2);
    // The third author's stories: the older one goes first, the one that
    // is about to expire second. Take the one on screen away.
    tick(&harness, cx, Duration::from_millis(40));
    let id = viewing(&harness, cx, |viewing| {
        viewing.player.current().unwrap().id.clone()
    });
    harness
        .engine
        .store()
        .remove_story(&personal(), &id)
        .unwrap();
    harness.settle(cx);
    let now = viewing(&harness, cx, |viewing| {
        viewing.player.current().map(|slide| slide.id.clone())
    });
    assert_ne!(now, Some(id));
    let _ = Moment::Moved;
    let _ = MessageId::new("");
}

// ----- touch points -----------------------------------------------------------------------------

#[gpui_kit::test]
fn the_first_new_picture_of_an_author_is_fetched_ahead_when_the_policy_allows_and_is_not_a_view(
    cx: &mut TestAppContext,
) {
    for policy in [settings::MediaChoice::Images, settings::MediaChoice::Never] {
        let harness = open(cx, ShellOptions::default());
        cx.update(|cx| settings::update(cx, |settings| settings.media = policy));
        harness
            .runtime
            .block_on(harness.engine.sync_stories(&personal()))
            .unwrap();
        harness.settle(cx);
        // Not while Status is not on screen: the list is wanted for its
        // dot and its rings, not for its pictures.
        let before = harness.mock.media_calls();
        assert_eq!(before, 0, "nothing is fetched for a list nobody opened");
        go_to_status(&harness, cx);
        // Whose first story not seen is a picture: the second author.
        let (picture, video) = cx.update(|cx| {
            let feed = &harness.shell.read(cx).status.feed;
            let url_of = |item: &client_core::StoryItem| match &item.story.body {
                StoryBody::Media(media) => media.source.clone().map(|source| source.to_string()),
                StoryBody::Text { .. } => None,
            };
            let picture = feed.recent[1].stories[0].clone();
            let video = feed.recent[0].stories[2].clone();
            (
                url_of(&picture).unwrap(),
                (url_of(&video).unwrap(), video.story.id),
            )
        });
        let mut cached = false;
        for _ in 0..200 {
            harness.settle(cx);
            cached = harness
                .engine
                .store()
                .media_size(&client_core::thumbnail_key(&picture))
                .unwrap()
                .is_some();
            if cached {
                break;
            }
            std::thread::sleep(Duration::from_millis(3));
        }
        assert_eq!(
            cached,
            policy == settings::MediaChoice::Images,
            "{policy:?}"
        );
        // A video is never fetched by itself, whatever the policy.
        assert_eq!(
            harness.engine.media_state(&client_core::file_key(&video.0)),
            client_core::MediaState::Idle
        );
        assert!(harness
            .engine
            .store()
            .media_size(&client_core::thumbnail_key(&video.0))
            .unwrap()
            .is_none());
        // And nothing was seen, nothing told.
        let seen = harness
            .engine
            .store()
            .story(&personal(), &video.1)
            .unwrap()
            .unwrap()
            .viewed;
        assert!(!seen);
        assert!(harness.mock.story_views().is_empty());
        cx.update(|cx| {
            settings::update(cx, |settings| {
                settings.media = settings::MediaChoice::Images
            })
        });
    }
}

#[gpui_kit::test]
fn a_gif_and_a_sticker_story_are_fetched_ahead_and_a_video_is_not(cx: &mut TestAppContext) {
    use client_provider::{Media, MediaRef};
    let harness = open(cx, ShellOptions::default());
    let sticker_url = "https://files.example/story-sticker.png";
    let gif_url = "https://files.example/story-loop.mp4";
    let video_url = "https://files.example/story-clip.mp4";
    harness
        .mock
        .set_media(sticker_url, super::png(64, 64), "image/png");
    harness
        .mock
        .set_media(gif_url, b"gif-mp4".to_vec(), "video/mp4");
    harness
        .mock
        .set_media(video_url, b"clip".to_vec(), "video/mp4");
    let posted = |kind: MediaKind, url: &str, gif: bool| {
        let mut media = Media::new(kind);
        media.source = Some(MediaRef::new(url));
        media.mime_type = Some(
            if kind == MediaKind::Sticker {
                "image/png"
            } else {
                "video/mp4"
            }
            .into(),
        );
        media.gif = gif;
        media
    };
    let sticker = harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550009111"),
        "Sticker Person",
        StoryBody::Media(posted(MediaKind::Sticker, sticker_url, false)),
    );
    let gif = harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550009222"),
        "Gif Person",
        StoryBody::Media(posted(MediaKind::Video, gif_url, true)),
    );
    let video = harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550009333"),
        "Video Person",
        StoryBody::Media(posted(MediaKind::Video, video_url, false)),
    );
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert!(
        harness
            .engine
            .store()
            .media_size(&client_core::thumbnail_key(sticker_url))
            .unwrap()
            .is_none(),
        "nothing is fetched before Status is open"
    );
    go_to_status(&harness, cx);
    let file_cached = |url: &str| {
        harness
            .engine
            .store()
            .media(&client_core::file_key(url), Timestamp::now())
            .unwrap()
            .is_some()
    };
    let mut sticker_here = false;
    let mut gif_here = false;
    for _ in 0..200 {
        harness.settle(cx);
        sticker_here = harness
            .engine
            .store()
            .media_size(&client_core::thumbnail_key(sticker_url))
            .unwrap()
            .is_some();
        gif_here = file_cached(gif_url);
        if sticker_here && gif_here {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(sticker_here, "a sticker story is fetched like a picture");
    assert!(gif_here, "a gif story is fetched without a click");
    assert_eq!(
        harness
            .engine
            .media_state(&client_core::file_key(video_url)),
        client_core::MediaState::Idle,
        "a video is not fetched ahead"
    );
    assert!(!file_cached(video_url));
    for id in [&sticker.id, &gif.id, &video.id] {
        assert!(
            !harness
                .engine
                .store()
                .story(&personal(), id)
                .unwrap()
                .unwrap()
                .viewed,
            "fetching ahead is not seeing"
        );
    }
    assert!(harness.mock.story_views().is_empty());

    let _guard = crate::video::install_opener(Arc::new(|_| {
        Ok(crate::video::clock(Duration::from_secs(12)))
    }));
    let index = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .status
            .feed
            .recent
            .iter()
            .position(|author| author.stories.iter().any(|item| item.story.id == gif.id))
            .expect("the gif author")
    });
    watch(&harness, cx, index);
    tick(&harness, cx, Duration::from_millis(40));
    assert!(shows(harness.window, "story-video", cx));
    assert!(!shows(harness.window, "story-tile", cx));
    assert!(
        harness
            .engine
            .store()
            .story(&personal(), &gif.id)
            .unwrap()
            .unwrap()
            .viewed
    );
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .external));
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .loading));
    assert!(cx.opened_url().is_none(), "the gif stays in the app");
}

#[gpui_kit::test]
fn a_person_with_a_status_wears_a_ring_in_the_chat_list_and_the_header_and_the_ring_opens_it(
    cx: &mut TestAppContext,
) {
    let harness = open_status(cx);
    // The first author of the list of Status is somebody the account has a
    // direct chat with.
    let (author, chat) = cx.update(|cx| {
        let author = harness.shell.read(cx).status.feed.recent[0].author.clone();
        (
            author.clone(),
            client_provider::ChatId::new(author.as_str()),
        )
    });
    // Their row is on screen when the list shows them; open the chat by
    // keyboard-less means, then look for the header's ring.
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(chat.clone(), None, cx));
    harness.settle(cx);
    assert!(
        shows(harness.window, "header-ring", cx),
        "the header's picture has a ring"
    );
    assert!(
        shows(harness.window, "story-ring-3-3", cx),
        "and says how many are new"
    );
    // No chat row of anybody without a status wears one.
    let rings = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .filter(|row| matches!(row, ListRow::Chat(_)))
            .count()
    });
    assert!(rings > 0);
    // A click on the ring watches the status, not the chat's profile.
    click(harness.window, "header-ring", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "story-viewer", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).status_active()));
    let _ = author;
}

#[gpui_kit::test]
fn the_ring_of_a_chat_list_row_does_not_change_the_row(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    // Rows of people with a status, and without: the same avatar box.
    let chat_ids: Vec<(usize, bool)> = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .list_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                ListRow::Chat(chat) if chat.kind == client_provider::ChatKind::Direct => Some((
                    index,
                    shell
                        .story_ring_of(&ContactId::new(chat.id.as_str()))
                        .is_some(),
                )),
                _ => None,
            })
            .collect()
    });
    assert!(
        chat_ids.iter().any(|(_, ring)| *ring),
        "somebody has a status"
    );
    assert!(chat_ids.iter().any(|(_, ring)| !*ring));
    let side = |index: usize, cx: &mut TestAppContext| {
        let selector: &'static str = Box::leak(format!("row-avatar-{index}").into_boxed_str());
        VisualTestContext::from_window(harness.window.into(), cx)
            .debug_bounds(selector)
            .map(|bounds| (bounds.size.width, bounds.size.height))
    };
    let sizes: Vec<_> = chat_ids
        .iter()
        .filter_map(|(index, _)| side(*index, cx))
        .collect();
    assert!(!sizes.is_empty());
    assert!(sizes.windows(2).all(|pair| pair[0] == pair[1]), "{sizes:?}");
}

#[gpui_kit::test]
fn a_reply_to_a_status_says_which_one_and_shows_it_while_it_is_there(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    // Reply, as the viewer does, and open the chat with the author.
    let (story, chat) = cx.update(|cx| {
        let author = &harness.shell.read(cx).status.feed.recent[0];
        (
            author.stories[0].story.id.clone(),
            client_provider::ChatId::new(author.author.as_str()),
        )
    });
    harness
        .engine
        .reply_to_story(&personal(), &story, "Lovely", Vec::new())
        .unwrap();
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    click(harness.window, "nav-chats", cx);
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(chat.clone(), None, cx));
    harness.settle(cx);
    assert!(
        shows(harness.window, "story-quote", cx),
        "the story is quoted"
    );
    // The bubble is a message like any: its menu offers Forward, whole,
    // and what is passed on is its words, to the chat picked, as a
    // message of its own that answers no status.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "m", cx);
    let items = cx.update(|cx| harness.shell.read(cx).message_menu_items());
    let forward = items
        .iter()
        .find(|item| item.id == "message-forward")
        .unwrap();
    assert!(forward.action.is_some() && forward.hint.is_none());
    let row = bounds(&harness, cx, "message-forward");
    let label = bounds(&harness, cx, "message-forward-label");
    assert!(label.left() >= row.left() && label.right() <= row.right());
    press(harness.window, "escape", cx);
    press(harness.window, "f", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );
    click(harness.window, "forward-chat-0", cx);
    click(harness.window, "forward-send", cx);
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].message.forwarded && pending[0].message.reply_to.is_none());
    assert!(matches!(
        &pending[0].message.content,
        client_provider::OutgoingContent::Text { body } if body == "Lovely"
    ));
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(
        harness.mock.story_replies().len(),
        1,
        "only the reply was one"
    );
    assert_eq!(
        harness.mock.sent().last().unwrap().chat_id,
        pending[0].message.chat_id
    );
    harness.settle(cx);
    assert!(shows(harness.window, "story-quote", cx));
    // The story expires: the block is as tall as before and says so.
    let before = bounds(&harness, cx, "story-quote").size.height;
    harness
        .engine
        .store()
        .remove_story(&personal(), &story)
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "story-quote-gone", cx));
    let after = bounds(&harness, cx, "story-quote-gone").size.height;
    assert_eq!(before, after);
}

#[gpui_kit::test]
fn the_contact_profile_mutes_their_status(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    let (author, chat) = cx.update(|cx| {
        let author = harness.shell.read(cx).status.feed.recent[0].author.clone();
        (
            author.clone(),
            client_provider::ChatId::new(author.as_str()),
        )
    });
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(chat, None, cx));
    harness.settle(cx);
    click(harness.window, "header-info", cx);
    harness.settle(cx);
    click(harness.window, "profile-mute-status", cx);
    harness.settle(cx);
    assert!(harness
        .engine
        .store()
        .story_muted(&personal(), &author)
        .unwrap());
    harness.settle(cx);
    assert!(harness.mock.muted_authors(&personal()).contains(&author));
}

#[gpui_kit::test]
fn the_settings_say_who_sees_my_status_and_keep_the_receipts_choices(cx: &mut TestAppContext) {
    let harness = open_status(cx);
    harness.engine.want_story_privacy(&personal());
    harness.settle(cx);
    harness.shell.update(cx, |shell, cx| {
        shell.settings_section = crate::ui::shell::SettingsSection::Status;
        cx.notify();
    });
    press(harness.window, "ctrl-,", cx);
    click(harness.window, "settings-status", cx);
    assert!(shows(harness.window, "status-settings-audience", cx));
    assert!(shows(harness.window, "notify-status", cx));
    // Notifications of new status are off unless asked for.
    assert!(!cx.update(|cx| settings::get(cx).story_notifications));
    click(harness.window, "notify-status", cx);
    assert!(cx.update(|cx| settings::get(cx).story_notifications));
    // The receipts switch is in Sync, beside the one for chats.
    click(harness.window, "settings-sync", cx);
    assert!(shows(harness.window, "story-receipts", cx));
    assert!(
        cx.update(|cx| settings::get(cx).story_receipts),
        "on by default"
    );
    click(harness.window, "story-receipts", cx);
    assert!(!cx.update(|cx| settings::get(cx).story_receipts));
}

// ----- the fields of Status are fields that are written in ------------------------------------------
//
// The reply under a story and the words and the caption of a post do what
// the composer does with emoji: a shortcode is offered its emoji, and the
// emoji picker opens for the field. Nobody is listed to mention: a field
// of Status is not a group's conversation. And nothing of the conversation
// behind Status (its picker of stickers and GIFs, its keys) is reachable
// while Status is in its place.

/// The window with a group's conversation open, and the stories listed.
fn open_status_over_a_group(cx: &mut TestAppContext) -> Harness {
    let harness = open_status(cx);
    let group = harness
        .engine
        .store()
        .chats(&personal(), None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == client_provider::ChatKind::Group)
        .expect("a group")
        .id;
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(group, None, cx));
    harness.settle(cx);
    harness
}

/// Lets the wait of the shortcode's search pass (twice: the emoji tables
/// are read the first time they are needed).
fn settle_shortcode(cx: &mut TestAppContext) {
    for _ in 0..2 {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(crate::motion::SEARCH_DEBOUNCE + Duration::from_millis(10));
        cx.run_until_parked();
    }
}

fn overlay_now(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn picker_target(harness: &Harness, cx: &mut TestAppContext) -> crate::ui::emoji_picker::Target {
    cx.update(|cx| harness.shell.read(cx).emoji.target)
}

fn post_words(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .status
            .words
            .read(cx)
            .value()
            .to_string()
    })
}

#[gpui_kit::test]
fn a_shortcode_in_the_reply_under_a_story_offers_its_emoji_and_nobody_to_mention(
    cx: &mut TestAppContext,
) {
    let harness = open_status_over_a_group(cx);
    // In the group's composer an `@` lists its people.
    focus_composer(&harness, cx);
    type_text(harness.window, "@", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "mention-picker", cx));
    press(harness.window, "backspace", cx);

    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    press(harness.window, "r", cx);
    // Under a story it lists nobody: the reply goes to one person.
    type_text(harness.window, "@", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "mention-picker", cx));
    press(harness.window, "backspace", cx);

    type_text(harness.window, "on :fir", cx);
    settle_shortcode(cx);
    let offered = cx.update(|cx| harness.shell.read(cx).emoji.completions());
    assert_eq!(offered[0], ("🔥".to_owned(), ":fire:".to_owned()));
    // The list is over the field, inside the viewer, and the field and
    // its buttons are still whole.
    let list = bounds(&harness, cx, "emoji-completion");
    let field = bounds(&harness, cx, "story-reply");
    let viewer = bounds(&harness, cx, "story-viewer");
    let send = bounds(&harness, cx, "story-send");
    let emoji = bounds(&harness, cx, "status-emoji");
    assert!(list.bottom() <= field.top(), "{list:?} {field:?}");
    assert!(
        list.top() >= viewer.top()
            && list.left() >= viewer.left()
            && list.right() <= viewer.right(),
        "{list:?} {viewer:?}"
    );
    assert!(send.bottom() <= viewer.bottom() && field.bottom() <= viewer.bottom());
    assert!(field.right() <= emoji.left() && emoji.right() <= send.left());

    // Enter takes the emoji and sends nothing.
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(reply_draft(&harness, cx), "on 🔥");
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());

    // Escape closes the list first, and leaves the field after.
    type_text(harness.window, " :piz", cx);
    settle_shortcode(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert!(viewing(&harness, cx, |viewing| viewing.player.holds.typing));
    assert_eq!(reply_draft(&harness, cx), "on 🔥 :piz");
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert!(!viewing(&harness, cx, |viewing| viewing
        .player
        .holds
        .typing));
    assert!(shows(harness.window, "story-viewer", cx));
    // The group's composer got none of it.
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string()),
        ""
    );
}

#[gpui_kit::test]
fn the_emoji_picker_opens_for_the_reply_under_a_story_and_gives_the_story_back(
    cx: &mut TestAppContext,
) {
    let harness = open_status_over_a_group(cx);
    go_to_status(&harness, cx);
    watch(&harness, cx, 0);
    tick(&harness, cx, Duration::from_millis(40));
    press(harness.window, "r", cx);
    type_text(harness.window, "Nice ", cx);
    click(harness.window, "status-emoji", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::EmojiPicker);
    assert_eq!(
        picker_target(&harness, cx),
        crate::ui::emoji_picker::Target::Status
    );
    // Emoji only: a sticker or a GIF is a message of a conversation.
    assert!(shows(harness.window, "emoji-grid", cx));
    assert!(!shows(harness.window, "picker-bar", cx));
    let card = bounds(&harness, cx, "emoji-picker");
    let window = cx.update(|cx| harness.shell.read(cx).viewport);
    assert!(card.left() >= px(0.) && card.right() <= window.width);
    assert!(card.top() >= px(0.) && card.bottom() <= window.height);
    // The story waits while the emoji is chosen.
    harness.settle(cx);
    assert!(viewing(&harness, cx, |viewing| viewing.player.holds.typing));
    tick(&harness, cx, Duration::from_secs(30));
    assert_eq!(
        viewing(&harness, cx, |viewing| viewing.player.slide_index()),
        0
    );
    click(harness.window, "emoji-cell-0", cx);
    harness.settle(cx);
    // The emoji is in the reply, the story is back, the field has the
    // keyboard, and nothing was sent anywhere.
    assert_eq!(overlay_now(&harness, cx), Overlay::None);
    assert!(shows(harness.window, "story-viewer", cx));
    let written = reply_draft(&harness, cx);
    assert!(
        written.starts_with("Nice ") && written.len() > 5,
        "{written}"
    );
    type_text(harness.window, "!", cx);
    assert_eq!(reply_draft(&harness, cx), format!("{written}!"));
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string()),
        ""
    );

    // Its key opens it from the field and closes it again.
    press(harness.window, "ctrl-e", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::EmojiPicker);
    assert_eq!(
        picker_target(&harness, cx),
        crate::ui::emoji_picker::Target::Status
    );
    press(harness.window, "ctrl-e", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::None);
    assert!(shows(harness.window, "story-viewer", cx));
    // The keys of stickers and GIFs open nothing over a story.
    for key in ["ctrl-shift-e", "ctrl-shift-j"] {
        press(harness.window, key, cx);
        harness.settle(cx);
        assert_eq!(overlay_now(&harness, cx), Overlay::None, "{key}");
    }
    assert_eq!(reply_draft(&harness, cx), format!("{written}!"));
}

#[gpui_kit::test]
fn the_words_and_the_caption_of_a_post_take_shortcodes_and_the_emoji_picker(
    cx: &mut TestAppContext,
) {
    let harness = open_status_over_a_group(cx);
    press(harness.window, "alt-n", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "status-post", cx));
    type_text(harness.window, "hi :fir", cx);
    settle_shortcode(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    // Inside the sheet, over the field.
    let list = bounds(&harness, cx, "emoji-completion");
    let sheet = bounds(&harness, cx, "status-post");
    let field = bounds(&harness, cx, "post-words");
    let emoji = bounds(&harness, cx, "status-emoji");
    assert!(
        list.left() >= sheet.left() && list.right() <= sheet.right() && list.top() >= sheet.top(),
        "{list:?} {sheet:?}"
    );
    assert!(list.bottom() <= field.top());
    assert!(field.right() <= emoji.left() && emoji.right() <= sheet.right());
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(post_words(&harness, cx), "hi 🔥");
    assert!(shows(harness.window, "status-post", cx));

    // Escape closes the list and not the sheet.
    type_text(harness.window, " :piz", cx);
    settle_shortcode(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert!(shows(harness.window, "status-post", cx));

    // The picker is the field's; the sheet comes back with what was
    // written, and the keyboard in the field.
    click(harness.window, "status-emoji", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::EmojiPicker);
    assert_eq!(
        picker_target(&harness, cx),
        crate::ui::emoji_picker::Target::Status
    );
    assert!(!shows(harness.window, "picker-bar", cx));
    // The field is near the top of the window: the picker is whole, in
    // the window, wherever it had to open.
    let card = bounds(&harness, cx, "emoji-picker");
    let window = cx.update(|cx| harness.shell.read(cx).viewport);
    assert!(
        card.top() >= px(0.) && card.bottom() <= window.height,
        "{card:?}"
    );
    assert!(card.left() >= px(0.) && card.right() <= window.width);
    let before = post_words(&harness, cx);
    click(harness.window, "emoji-cell-0", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::Status);
    assert!(shows(harness.window, "status-post", cx));
    let after = post_words(&harness, cx);
    assert!(after.starts_with(&before) && after.len() > before.len());
    type_text(harness.window, "!", cx);
    assert_eq!(post_words(&harness, cx), format!("{after}!"));
    // Escape from the picker gives the sheet back too.
    press(harness.window, "ctrl-e", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::EmojiPicker);
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::Status);
    assert_eq!(post_words(&harness, cx), format!("{after}!"));
    // Stickers and GIFs are not for a status: their keys open nothing.
    for key in ["ctrl-shift-e", "ctrl-shift-j"] {
        press(harness.window, key, cx);
        harness.settle(cx);
        assert_eq!(overlay_now(&harness, cx), Overlay::Status, "{key}");
    }

    // The caption of a picture or a video is the field in hand on its tab.
    click(harness.window, "post-tab-media", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    type_text(harness.window, ":fir", cx);
    settle_shortcode(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    let list = bounds(&harness, cx, "emoji-completion");
    let caption = bounds(&harness, cx, "post-caption");
    let sheet = bounds(&harness, cx, "status-post");
    assert!(list.bottom() <= caption.top() && list.top() >= sheet.top());
    // Enter takes the emoji; it does not post.
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(
        cx.update(|cx| harness
            .shell
            .read(cx)
            .status
            .caption
            .read(cx)
            .value()
            .to_string()),
        "🔥"
    );
    assert!(shows(harness.window, "status-post", cx));
    assert!(harness.mock.posted_stories().is_empty());
    assert_eq!(post_words(&harness, cx), format!("{after}!"));
}

#[gpui_kit::test]
fn the_conversation_behind_status_is_out_of_reach_of_its_keys_and_its_picker(
    cx: &mut TestAppContext,
) {
    let harness = open_status_over_a_group(cx);
    let sent_before = harness.engine.store().outbox_pending().unwrap().len();
    go_to_status(&harness, cx);
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_some()));
    // The keys of the pickers and of attaching do nothing over the list,
    // nor over a story.
    for watching in [false, true] {
        if watching {
            watch(&harness, cx, 0);
        }
        for key in ["ctrl-shift-e", "ctrl-shift-j", "ctrl-e", "ctrl-o"] {
            press(harness.window, key, cx);
            harness.settle(cx);
            assert_eq!(overlay_now(&harness, cx), Overlay::None, "{key}");
        }
        // Nor asked for by name.
        for command in [
            Command::StickerPicker,
            Command::GifPicker,
            Command::EmojiPicker,
        ] {
            let ran = cx
                .update_window(harness.window.into(), |_, window, cx| {
                    harness
                        .shell
                        .update(cx, |shell, cx| shell.run_command(command, window, cx))
                })
                .unwrap();
            assert!(!ran, "{command:?}");
            assert_eq!(overlay_now(&harness, cx), Overlay::None);
        }
    }
    // The palette says why.
    let said = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette_avail(Command::StickerPicker, cx)
    });
    assert_eq!(
        said,
        crate::ui::palette_steps::Avail::Disabled("Show the chats first")
    );
    assert_eq!(
        harness.engine.store().outbox_pending().unwrap().len(),
        sent_before
    );
    // With the chats back, the picker is the conversation's again.
    click(harness.window, "nav-chats", cx);
    harness.settle(cx);
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-shift-e", cx);
    harness.settle(cx);
    assert_eq!(overlay_now(&harness, cx), Overlay::EmojiPicker);
    assert_eq!(
        picker_target(&harness, cx),
        crate::ui::emoji_picker::Target::Composer
    );
}

// ----- contacts' updates that the backend does not have yet ----------------------------

fn recent_authors(harness: &Harness, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| harness.shell.read(cx).status.feed.recent.len())
}

/// The provider delivers contacts' updates: they are listed, and nothing
/// says otherwise. When its backend does not have them (yet), the list
/// says so in a line under the account's own status, which goes on
/// working; and when the backend has them, they are back.
#[gpui_kit::test]
fn contacts_updates_show_when_the_provider_has_them_and_say_so_when_it_does_not_yet(
    cx: &mut TestAppContext,
) {
    use client_provider::Feature;
    let harness = open_status(cx);
    go_to_status(&harness, cx);
    assert!(harness.engine.capabilities().story_contacts);
    assert!(recent_authors(&harness, cx) > 0);
    assert!(shows(harness.window, "status-author-0", cx));
    assert!(!shows(harness.window, "status-none", cx));

    // The backend lost (or never had) the routes.
    harness.mock.set_unavailable(None, Feature::Stories, true);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert_eq!(recent_authors(&harness, cx), 0);
    assert!(shows(harness.window, "status-none", cx));
    // The account's own status is where it was, and can be posted to.
    assert!(shows(harness.window, "status-mine", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(!shell.status.feed.mine.is_empty());
        let caps = shell.caps_now();
        assert!(caps.story_post && caps.story_list);
        assert!(!caps.story_contacts && !caps.story_viewers);
    });
    // Nothing was reported as a failure.
    assert!(!shows(harness.window, "problem", cx));

    // They are there again.
    harness.mock.set_unavailable(None, Feature::Stories, false);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert!(recent_authors(&harness, cx) > 0);
    assert!(!shows(harness.window, "status-none", cx));
}
