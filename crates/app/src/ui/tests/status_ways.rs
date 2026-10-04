//! The ways into Status, and what it says when it has nothing to list:
//! the rail's two buttons, the strip of updates above the chats, the
//! rings, the states of the list (on its way, offline, not available,
//! nobody yet) with their next action, at every interface size and in
//! both themes. And the rule under all of it: only the viewer sees a
//! story.

use super::*;
use crate::keys::Command;
use crate::settings::ThemeChoice;
use crate::ui::status::Updates;
use client_provider::{
    AccountId, Capabilities, ConnectionState, Contact, ContactId, Feature, Media, MediaKind,
    MediaRef, ProviderError, ProviderEvent, StoryBody, StoryStyle,
};
use provider_mock::MockConfig;

fn personal() -> AccountId {
    AccountId::new("acc_personal")
}

fn at(
    harness: &Harness,
    cx: &mut TestAppContext,
    selector: &'static str,
) -> Bounds<gpui_kit::Pixels> {
    VisualTestContext::from_window(harness.window.into(), cx)
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is not on screen"))
}

fn inside(inner: Bounds<gpui_kit::Pixels>, outer: Bounds<gpui_kit::Pixels>) -> bool {
    let slack = px(1.);
    inner.left() >= outer.left() - slack
        && inner.right() <= outer.right() + slack
        && inner.top() >= outer.top() - slack
        && inner.bottom() <= outer.bottom() + slack
}

/// The window with the account's stories listed.
fn open_listed(cx: &mut TestAppContext) -> Harness {
    let harness = open(cx, ShellOptions::default());
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    harness
}

/// A provider nobody of whose contacts has a story up.
fn open_with_nobody(cx: &mut TestAppContext) -> Harness {
    let harness = open(cx, ShellOptions::default());
    nobody_posts(&harness);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    harness
}

fn nobody_posts(harness: &Harness) {
    for story in harness.mock.stories_of(&personal()) {
        if !story.mine {
            harness.mock.contact_removes_story(&personal(), &story.id);
        }
    }
}

fn updates(harness: &Harness, cx: &mut TestAppContext) -> Updates {
    cx.update(|cx| harness.shell.read(cx).status_updates())
}

fn recent(harness: &Harness, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| harness.shell.read(cx).status.feed.recent.len())
}

fn status_active(harness: &Harness, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| harness.shell.read(cx).status_active())
}

/// Nothing was seen and nobody was told: no story of the store was
/// shown, and the provider got no view receipt.
fn nothing_was_seen(harness: &Harness) {
    let feed = harness.engine.story_feed(&personal()).unwrap();
    for author in feed.authors() {
        for item in &author.stories {
            assert!(
                item.viewed_at.is_none(),
                "a story was marked as seen without being shown"
            );
        }
    }
    assert!(harness.mock.story_views().is_empty(), "a receipt was sent");
}

fn selector(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

// ----- the rail --------------------------------------------------------------------

#[gpui_kit::test]
fn status_is_a_destination_in_the_rail_under_the_logo_with_its_keys_said(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    let rail_nav = at(&harness, cx, "rail-nav");
    let logo = at(&harness, cx, "rail-logo");
    let numbers = at(&harness, cx, "rail-numbers");
    assert!(rail_nav.top() >= logo.bottom() - px(1.), "under the logo");
    assert!(
        rail_nav.bottom() <= numbers.top() + px(1.),
        "above the numbers"
    );
    let (chats, status) = (
        at(&harness, cx, "nav-chats"),
        at(&harness, cx, "nav-status"),
    );
    assert!(inside(chats, rail_nav) && inside(status, rail_nav));
    assert!(status.top() >= chats.bottom(), "one under the other");
    assert_eq!(chats.size, status.size);
    // As large as the rail's other buttons: nobody has to look for it.
    assert_eq!(status.size, at(&harness, cx, "settings").size);

    // The chats are showing: marked, and the title says so.
    assert!(shows(harness.window, "nav-on-chats", cx));
    assert!(!shows(harness.window, "nav-on-status", cx));
    assert!(shows(harness.window, "status-dot", cx), "something new");
    assert!(inside(at(&harness, cx, "status-dot"), status));

    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "status-pane", cx));
    assert!(shows(harness.window, "nav-on-status", cx));
    assert!(!shows(harness.window, "nav-on-chats", cx));
    // The rail is where it was: the buttons did not move.
    assert_eq!(at(&harness, cx, "nav-status"), status);

    click(harness.window, "nav-chats", cx);
    assert!(!shows(harness.window, "status-pane", cx));
    assert!(shows(harness.window, "nav-on-chats", cx));

    // Each says its name and its keys when pointed at.
    for (id, name, command) in [
        ("nav-chats", "Chats", Command::ShowChats),
        ("nav-status", "Status", Command::ShowStatus),
    ] {
        let said = crate::ui::hints::hint(id).unwrap().to_string();
        let keys = crate::keys::keys_label(command).unwrap();
        assert!(said.starts_with(name) && said.ends_with(&keys), "{said}");
    }
    nothing_was_seen(&harness);
}

#[gpui_kit::test]
fn one_way_in_per_place_the_header_is_a_title_and_not_a_second_switch(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    assert!(shows(harness.window, "list-title", cx));
    for gone in ["mode-switch", "mode-status", "mode-chats"] {
        assert!(!shows(harness.window, gone, cx), "{gone}");
    }
    click(harness.window, "nav-status", cx);
    assert!(shows(harness.window, "list-title", cx));
    assert!(shows(harness.window, "new-status", cx));
}

#[gpui_kit::test]
fn a_provider_without_stories_has_no_button_no_strip_and_no_dot(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let caps = Capabilities {
        story_list: false,
        story_contacts: false,
        story_post: false,
        story_delete: false,
        story_view: false,
        story_viewers: false,
        story_reply: false,
        story_react: false,
        story_mute: false,
        story_privacy: false,
        story_privacy_edit: false,
        ..Capabilities::all()
    };
    let mock = MockProvider::new(MockConfig {
        capabilities: Some(caps),
        ..MockConfig::quiet()
    });
    let harness = open_over(cx, ShellOptions::default(), mock);
    harness.settle(cx);
    for absent in [
        "rail-nav",
        "nav-status",
        "story-strip",
        "status-dot",
        "status-line",
    ] {
        assert!(!shows(harness.window, absent, cx), "{absent}");
    }
    // The key does nothing either.
    press(harness.window, "alt-s", cx);
    assert!(!shows(harness.window, "status-pane", cx));
}

// ----- the strip above the chats ---------------------------------------------------

#[gpui_kit::test]
fn the_strip_above_the_chats_is_my_status_then_everybody_with_something_new(
    cx: &mut TestAppContext,
) {
    let harness = open_listed(cx);
    let strip = at(&harness, cx, "story-strip");
    let title = at(&harness, cx, "list-title");
    let list = at(&harness, cx, "list-resize");
    // Under the header, over the first chat, as wide as the list.
    assert!(strip.top() >= title.bottom());
    assert!(strip.bottom() <= at(&harness, cx, "row-avatar-0").top());
    assert!(strip.left() >= px(0.) && strip.right() <= list.right() + px(1.));
    assert_eq!(strip.size.height, crate::theme::story::STRIP());

    let mine = at(&harness, cx, "story-strip-mine");
    assert!(inside(mine, strip));
    assert!(inside(at(&harness, cx, "story-strip-add"), mine));
    let people = recent(&harness, cx);
    assert_eq!(people, 3, "the mock's three people with something new");
    let mut left = mine.right();
    for index in 0..people {
        let cell = at(
            &harness,
            cx,
            selector(format!("story-strip-author-{index}")),
        );
        assert!(inside(cell, strip), "{index}: {cell:?} in {strip:?}");
        assert_eq!(cell.left(), left, "{index}: side by side");
        assert_eq!(cell.size.width, crate::theme::story::STRIP_CELL());
        left = cell.right();
    }
    // The people whose stories were all seen are not in it: Status lists
    // them.
    assert!(!shows(
        harness.window,
        selector(format!("story-strip-author-{people}")),
        cx
    ));
    assert!(!shows(harness.window, "story-strip-note", cx));
    // Drawing it saw nothing.
    nothing_was_seen(&harness);
    assert_eq!(harness.mock.media_calls(), 0, "and fetched no story");
}

#[gpui_kit::test]
fn a_click_on_somebody_in_the_strip_plays_their_status(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    let (key, first) = cx.update(|cx| {
        let author = &harness.shell.read(cx).status.feed.recent[1];
        (author.key.clone(), author.stories[0].story.id.clone())
    });
    nothing_was_seen(&harness);
    click(harness.window, "story-strip-author-1", cx);
    harness.settle(cx);
    assert!(status_active(&harness, cx));
    assert!(shows(harness.window, "story-viewer", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let viewing = shell.status.viewing.as_ref().expect("the viewer is open");
        assert_eq!(viewing.author().map(|author| author.key.clone()), Some(key));
    });
    // Now, and only now, a story counts as seen: the one on screen, once
    // its content is there.
    for _ in 0..200 {
        harness.shell.update(cx, |shell, cx| {
            shell.story_tick(Duration::from_millis(40), cx);
        });
        harness.settle(cx);
        if harness
            .engine
            .store()
            .story(&personal(), &first)
            .unwrap()
            .is_some_and(|item| item.viewed_at.is_some())
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    let feed = harness.engine.story_feed(&personal()).unwrap();
    let seen: Vec<_> = feed
        .authors()
        .flat_map(|author| author.stories.iter())
        .filter(|item| item.viewed_at.is_some())
        .map(|item| item.story.id.clone())
        .collect();
    assert_eq!(seen, [first], "the story that was shown, and no other");
}

#[gpui_kit::test]
fn my_status_in_the_strip_opens_what_is_up_and_its_plus_posts(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    // The account has stories up: the cell opens them, with who saw them.
    assert!(!harness
        .engine
        .story_feed(&personal())
        .unwrap()
        .mine
        .is_empty());
    click(harness.window, "story-strip-mine", cx);
    harness.settle(cx);
    assert!(status_active(&harness, cx));
    assert!(shows(harness.window, "status-mine-pane", cx));
    // Back at the chats, the "+" is the sheet for a new one.
    click(harness.window, "nav-chats", cx);
    click(harness.window, "story-strip-add", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "post-send", cx), "the post sheet");
}

#[gpui_kit::test]
fn with_nothing_of_its_own_up_my_status_in_the_strip_is_the_way_to_post(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    for item in harness.engine.story_feed(&personal()).unwrap().mine {
        harness.engine.delete_story(&personal(), &item.story.id);
    }
    harness.settle(cx);
    assert!(harness
        .engine
        .story_feed(&personal())
        .unwrap()
        .mine
        .is_empty());
    click(harness.window, "story-strip-mine", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "post-send", cx));
}

#[gpui_kit::test]
fn the_strip_builds_only_the_people_in_view_however_many_there_are(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
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
    assert!(recent(&harness, cx) > 300);
    assert!(shows(harness.window, "story-strip-author-0", cx));
    assert!(
        !shows(harness.window, "story-strip-author-200", cx),
        "somebody far to the right was built"
    );
    // Scrolled to them, they are there, and the first ones are not.
    let cell = crate::theme::story::STRIP_CELL();
    harness.shell.update(cx, |shell, cx| {
        shell
            .status
            .strip_scroll
            .set_offset(gpui_kit::point(-(cell * 201.), px(0.)));
        cx.notify();
    });
    harness.settle(cx);
    let strip = at(&harness, cx, "story-strip");
    let there = at(&harness, cx, "story-strip-author-200");
    assert!(there.left() >= strip.left() - px(1.) && there.right() <= strip.right() + px(1.));
    assert!(!shows(harness.window, "story-strip-author-0", cx));
    assert!(!shows(harness.window, "story-strip-author-290", cx));
    // And a click on one of them plays theirs.
    let key = cx.update(|cx| harness.shell.read(cx).status.feed.recent[200].key.clone());
    click(harness.window, "story-strip-author-200", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let viewing = shell.status.viewing.as_ref().expect("the viewer is open");
        assert_eq!(viewing.author().map(|author| author.key.clone()), Some(key));
    });
}

#[gpui_kit::test]
fn the_strip_folds_to_a_line_that_counts_and_stays_as_it_was_left(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    let open = at(&harness, cx, "story-strip");
    click(harness.window, "story-strip-fold", cx);
    harness.settle(cx);
    let folded = at(&harness, cx, "story-strip");
    assert_eq!(folded.size.height, crate::theme::story::STRIP_FOLDED());
    assert!(folded.size.height < open.size.height);
    assert!(!shows(harness.window, "story-strip-mine", cx));
    assert!(
        shows(harness.window, "story-strip-count", cx),
        "how many are new"
    );
    assert!(!cx.update(|cx| settings::get(cx).story_strip), "remembered");
    // Folding it did not open Status.
    assert!(!status_active(&harness, cx));
    // The line opens it again; so does its command, from the palette.
    click(harness.window, "story-strip", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "story-strip-mine", cx));
    assert!(cx.update(|cx| settings::get(cx).story_strip));
    cx.update(|cx| {
        assert_eq!(
            harness
                .shell
                .read(cx)
                .palette_avail(Command::ToggleStatusStrip, cx),
            crate::ui::palette_steps::Avail::Enabled
        );
    });
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.run_status_command(Command::ToggleStatusStrip, window, cx)
        })
    })
    .unwrap();
    harness.settle(cx);
    assert!(!shows(harness.window, "story-strip-mine", cx));
    nothing_was_seen(&harness);
}

#[gpui_kit::test]
fn the_strip_says_in_a_few_words_why_nobody_is_in_it(cx: &mut TestAppContext) {
    let harness = open_with_nobody(cx);
    assert!(shows(harness.window, "story-strip-mine", cx));
    assert!(!shows(harness.window, "story-strip-author-0", cx));
    assert!(shows(harness.window, "story-strip-empty", cx));
    assert!(inside(
        at(&harness, cx, "story-strip-note"),
        at(&harness, cx, "story-strip")
    ));
    // Not available for this number yet.
    harness.mock.set_unavailable(None, Feature::Stories, true);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "story-strip-unavailable", cx));
    // The words lead to Status, where it is said in full.
    click(harness.window, "story-strip-note", cx);
    harness.settle(cx);
    assert!(status_active(&harness, cx));
    assert!(shows(harness.window, "status-state-unavailable", cx));
}

// ----- the states of the list ------------------------------------------------------

#[gpui_kit::test]
fn nobody_with_a_status_yet_is_explained_with_the_way_to_post_one(cx: &mut TestAppContext) {
    let harness = open_with_nobody(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Empty);
    assert!(shows(harness.window, "status-state-empty", cx));
    let (title, detail) = Updates::Empty.words();
    assert_eq!(title, "No updates yet");
    assert!(detail.contains("in the last 24 hours appear here"));
    let pane = at(&harness, cx, "status-pane");
    for control in ["status-none", "status-post-first", "status-check"] {
        assert!(inside(at(&harness, cx, control), pane), "{control}");
    }
    // The own status is still the first row, over the explanation.
    assert!(at(&harness, cx, "status-mine").bottom() <= at(&harness, cx, "status-none").top());
    click(harness.window, "status-post-first", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "post-send", cx), "the post sheet");
}

#[gpui_kit::test]
fn a_part_that_is_missing_for_the_number_says_so_and_check_again_asks_at_once(
    cx: &mut TestAppContext,
) {
    let harness = open_listed(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Listed);
    assert!(!shows(harness.window, "status-none", cx));

    harness.mock.set_unavailable(None, Feature::Stories, true);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Unavailable);
    assert!(shows(harness.window, "status-state-unavailable", cx));
    assert!(!shows(harness.window, "status-post-first", cx));
    assert!(!shows(harness.window, "problem", cx), "not a failure");
    let lists = |harness: &Harness| {
        harness
            .mock
            .story_calls()
            .iter()
            .filter(|call| **call == "list")
            .count()
    };

    // Still missing: asked all the same, at once, and it says so again.
    let before = lists(&harness);
    click(harness.window, "status-check", cx);
    harness.settle(cx);
    assert_eq!(lists(&harness), before + 1, "asked now, not in ten minutes");
    assert!(harness.mock.story_calls().contains(&"recheck"));
    assert_eq!(updates(&harness, cx), Updates::Unavailable);

    // The backend has it now, and the provider still remembers it as
    // missing: "Check again" makes it ask, and the updates are there.
    harness.mock.restore_at_recheck(Feature::Stories);
    click(harness.window, "status-check", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Listed);
    assert!(shows(harness.window, "status-author-0", cx));
    assert!(!shows(harness.window, "status-none", cx));
}

#[gpui_kit::test]
fn going_to_status_asks_again_and_a_missing_part_is_asked_about_again(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    harness.mock.set_unavailable(None, Feature::Stories, true);
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert_eq!(recent(&harness, cx), 0);
    // Meanwhile the backend got it, and the provider still remembers it
    // as missing. Nobody waits ten minutes: going to Status asks.
    harness.mock.restore_at_recheck(Feature::Stories);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert!(recent(&harness, cx) > 0);
    assert!(shows(harness.window, "status-author-0", cx));
}

#[gpui_kit::test]
fn the_key_r_checks_again_from_the_list(cx: &mut TestAppContext) {
    let harness = open_with_nobody(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Empty);
    // Somebody posts; the provider pushes nothing in this test.
    let lists = harness.mock.story_calls().len();
    press(harness.window, "r", cx);
    harness.settle(cx);
    assert!(harness.mock.story_calls().len() > lists, "asked");
    assert_eq!(
        crate::keys::binding(Command::StatusRefresh).map(|binding| binding.when),
        Some(crate::keys::When::Status)
    );
}

#[gpui_kit::test]
fn a_list_on_its_way_says_so_and_one_that_did_not_come_offers_to_ask_again(
    cx: &mut TestAppContext,
) {
    let harness = open_with_nobody(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    // Slow: the question is out, the answer is not here.
    harness.mock.set_story_latency(Duration::from_millis(150));
    click(harness.window, "status-check", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Loading);
    assert!(shows(harness.window, "status-state-loading", cx));
    assert!(!shows(harness.window, "status-check", cx));
    harness
        .runtime
        .block_on(async { tokio::time::sleep(Duration::from_millis(300)).await });
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Empty);
    harness.mock.set_story_latency(Duration::ZERO);

    // A dropped connection: said, never as a failure of the window, with
    // the way to ask again.
    harness
        .mock
        .fail_next_story_calls([ProviderError::Transient("eof".into())]);
    click(harness.window, "status-check", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Failed);
    assert!(shows(harness.window, "status-state-failed", cx));
    assert!(!shows(harness.window, "problem", cx));
    click(harness.window, "status-check", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Empty);
}

#[gpui_kit::test]
fn a_number_that_is_offline_says_that_instead_of_nothing(cx: &mut TestAppContext) {
    let harness = open_with_nobody(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    harness
        .engine
        .apply_event(ProviderEvent::ConnectionChanged {
            account_id: personal(),
            state: ConnectionState::Reconnecting,
        })
        .unwrap();
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Offline);
    assert!(shows(harness.window, "status-state-offline", cx));
    // What is already listed stays listed while it is offline.
    harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550001111"),
        "Somebody",
        StoryBody::Text {
            text: "hello".into(),
            style: StoryStyle::default(),
        },
    );
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::Listed);
}

#[gpui_kit::test]
fn a_provider_that_never_has_contacts_updates_says_so_without_a_button(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let caps = Capabilities {
        story_contacts: false,
        story_view: false,
        story_viewers: false,
        story_reply: false,
        story_react: false,
        ..Capabilities::all()
    };
    let mock = MockProvider::new(MockConfig {
        capabilities: Some(caps),
        ..MockConfig::quiet()
    });
    let harness = open_over(cx, ShellOptions::default(), mock);
    harness.settle(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    assert_eq!(updates(&harness, cx), Updates::NotOffered);
    assert!(shows(harness.window, "status-state-not-offered", cx));
    assert!(!shows(harness.window, "status-check", cx));
}

// ----- prompt ------------------------------------------------------------------------

#[gpui_kit::test]
fn the_stories_are_asked_for_at_startup_without_anybody_going_to_status(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // Nobody asked for anything: the window did, when it opened.
    harness.settle(cx);
    assert!(recent(&harness, cx) > 0, "the strip has people in it");
    assert!(shows(harness.window, "story-strip-author-0", cx));
    assert!(shows(harness.window, "status-dot", cx));
    nothing_was_seen(&harness);
}

#[gpui_kit::test]
fn a_story_that_arrives_as_an_event_is_in_the_strip_and_on_the_rail_at_once(
    cx: &mut TestAppContext,
) {
    let harness = open_with_nobody(cx);
    assert!(!shows(harness.window, "status-dot", cx));
    let story = harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550001111"),
        "Somebody",
        StoryBody::Text {
            text: "hello".into(),
            style: StoryStyle::default(),
        },
    );
    harness
        .engine
        .apply_event(ProviderEvent::StoryUpserted(story))
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "story-strip-author-0", cx));
    assert!(shows(harness.window, "status-dot", cx));
    assert!(shows(harness.window, "story-ring-1-1", cx));
}

// ----- an author known by a hidden-number id ------------------------------------------

/// WhatsApp names the author of a status by their hidden-number id more
/// and more often, while the chat with them goes by their number. The
/// address book says the two are one person: their status is listed
/// under the saved name, and their chat's picture wears the ring.
#[gpui_kit::test]
fn a_status_by_hidden_number_id_is_its_persons_name_ring_and_chat(cx: &mut TestAppContext) {
    let harness = open_with_nobody(cx);
    // Somebody the account has a direct chat with, by number (and whose
    // status is not muted).
    let muted = harness.mock.muted_authors(&personal());
    let (index, chat) = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .enumerate()
            .find_map(|(index, row)| match row {
                ListRow::Chat(chat)
                    if chat.kind == client_provider::ChatKind::Direct
                        && !muted
                            .iter()
                            .any(|author| author.as_str() == chat.id.as_str()) =>
                {
                    Some((index, chat.id.clone()))
                }
                _ => None,
            })
            .expect("a direct chat")
    });
    let lid = ContactId::new("lid:77123456789012");
    let mut contact = Contact::new(personal(), ContactId::new(chat.as_str()));
    contact.saved_name = Some("Saved Name".into());
    contact.alt_ids = vec![lid.clone()];
    harness.engine.store().upsert_contact(&contact).unwrap();
    harness.mock.contact_posts_story(
        &personal(),
        &lid,
        "their profile name",
        StoryBody::Text {
            text: "from a hidden number".into(),
            style: StoryStyle::default(),
        },
    );
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    cx.update(|cx| {
        let feed = &harness.shell.read(cx).status.feed;
        assert_eq!(feed.recent.len(), 1);
        assert_eq!(feed.recent[0].display_name(), "Saved Name");
        assert_eq!(feed.recent[0].author, lid);
    });
    assert!(shows(harness.window, "story-strip-author-0", cx));
    // The chat's row, known by the number, wears the ring; a click on it
    // plays the status.
    let ring = selector(format!("row-ring-{index}"));
    assert!(
        shows(harness.window, ring, cx),
        "the row's picture has a ring"
    );
    click(harness.window, ring, cx);
    harness.settle(cx);
    assert!(status_active(&harness, cx));
    assert!(shows(harness.window, "story-viewer", cx));
}

#[gpui_kit::test]
fn the_ring_on_a_chat_rows_picture_plays_the_status_and_does_not_open_the_chat(
    cx: &mut TestAppContext,
) {
    let harness = open_listed(cx);
    let (index, author) = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let author = shell.status.feed.recent[0].author.clone();
        let index = shell
            .list_rows
            .iter()
            .position(
                |row| matches!(row, ListRow::Chat(chat) if chat.id.as_str() == author.as_str()),
            )
            .expect("their chat is in the list");
        (index, author)
    });
    let ring = selector(format!("row-ring-{index}"));
    assert!(shows(harness.window, ring, cx));
    nothing_was_seen(&harness);
    click(harness.window, ring, cx);
    harness.settle(cx);
    assert!(shows(harness.window, "story-viewer", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.open.is_none(), "the chat was not opened");
        let viewing = shell.status.viewing.as_ref().unwrap();
        assert_eq!(viewing.author().map(|a| a.author.clone()), Some(author));
    });
}

// ----- the rule: only the viewer sees ------------------------------------------------

#[gpui_kit::test]
fn the_strip_the_rail_and_the_list_see_nothing_and_fetch_no_video(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        settings::update(cx, |settings| {
            settings.media = settings::MediaChoice::Everything
        })
    });
    // Somebody new with a video, first in the strip.
    let mut media = Media::new(MediaKind::Video);
    media.source = Some(MediaRef::new("https://files.example/clip.mp4"));
    harness.mock.contact_posts_story(
        &personal(),
        &ContactId::new("+15550002222"),
        "Video Person",
        StoryBody::Media(media),
    );
    harness
        .runtime
        .block_on(harness.engine.sync_stories(&personal()))
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "story-strip-author-0", cx));
    // With the strip on screen, and through folding and scrolling it:
    // nothing is fetched, nothing is seen.
    click(harness.window, "story-strip-fold", cx);
    click(harness.window, "story-strip", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.media_calls(), 0, "no story file was fetched");
    nothing_was_seen(&harness);
    // In the list of Status the first new picture of an author may be
    // fetched ahead; a video never is, and still nothing is seen.
    click(harness.window, "nav-status", cx);
    for _ in 0..50 {
        harness.settle(cx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        harness
            .engine
            .media_state(&client_core::file_key("https://files.example/clip.mp4")),
        client_core::MediaState::Idle
    );
    assert!(harness
        .engine
        .store()
        .media_size(&client_core::thumbnail_key(
            "https://files.example/clip.mp4"
        ))
        .unwrap()
        .is_none());
    nothing_was_seen(&harness);
    cx.update(|cx| {
        settings::update(cx, |settings| {
            settings.media = settings::MediaChoice::Images
        })
    });
}

// ----- where people look -------------------------------------------------------------

#[gpui_kit::test]
fn the_start_screen_and_the_shortcuts_sheet_say_where_status_is(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    assert!(shows(harness.window, "status-line", cx));
    let line = crate::ui::hints::status_line();
    for command in [Command::ShowStatus, Command::NewStatus] {
        assert!(
            line.contains(&crate::keys::keys_label(command).unwrap()),
            "{line}"
        );
    }
    // The line fits under the one about the palette, inside the pane.
    let (keys, status) = (
        at(&harness, cx, "keys-line"),
        at(&harness, cx, "status-line"),
    );
    assert!(status.top() >= keys.bottom() - px(1.));
    // The sheet has a section for Status, with the ways in.
    let rows = crate::ui::palette::shortcut_rows_for("status");
    let listed: Vec<Command> = rows
        .iter()
        .flat_map(|(_, rows)| rows.iter().map(|row| row.0.command))
        .collect();
    for command in [
        Command::ShowStatus,
        Command::NewStatus,
        Command::StatusRefresh,
    ] {
        assert!(listed.contains(&command), "{command:?} in {listed:?}");
    }
}

// ----- every size, both themes ----------------------------------------------------------

#[gpui_kit::test]
fn the_ways_in_keep_their_place_at_every_interface_size_in_both_themes(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    for theme in [ThemeChoice::Light, ThemeChoice::Dark] {
        for step in crate::theme::SCALE_STEPS {
            cx.update(|cx| {
                settings::update(cx, |settings| {
                    settings.theme = theme;
                    settings.interface_scale = step;
                })
            });
            harness.settle(cx);
            let what = format!("{theme:?} {step}%");
            // The rail: logo, the two buttons, the numbers, the tools.
            let rail_nav = at(&harness, cx, "rail-nav");
            let rail_width = crate::theme::metrics::RAIL_WIDTH();
            assert!(rail_nav.right() <= rail_width + px(1.), "{what}");
            assert!(
                rail_nav.top() >= at(&harness, cx, "rail-logo").bottom() - px(1.),
                "{what}"
            );
            assert!(
                rail_nav.bottom() <= at(&harness, cx, "rail-numbers").top() + px(1.),
                "{what}"
            );
            for button in ["nav-chats", "nav-status"] {
                let button = at(&harness, cx, button);
                assert!(inside(button, rail_nav), "{what}: {button:?}");
                assert_eq!(
                    button.size.width,
                    crate::theme::metrics::CONTROL(),
                    "{what}"
                );
            }
            assert!(
                inside(
                    at(&harness, cx, "status-dot"),
                    at(&harness, cx, "nav-status")
                ),
                "{what}: the dot"
            );
            // A number of the rail is still on screen under them.
            assert!(
                at(&harness, cx, "rail-account-0").top() >= rail_nav.bottom() - px(1.),
                "{what}"
            );

            // The strip: as wide as the list, over the search, its cells
            // side by side inside it, each name inside its cell.
            let strip = at(&harness, cx, "story-strip");
            let list = at(&harness, cx, "list-resize");
            assert!(strip.left() >= rail_width - px(1.), "{what}");
            assert!(strip.right() <= list.right() + px(1.), "{what}");
            assert_eq!(strip.size.height, crate::theme::story::STRIP(), "{what}");
            assert!(
                strip.top() >= at(&harness, cx, "list-title").bottom(),
                "{what}"
            );
            assert!(
                strip.bottom() <= at(&harness, cx, "filter-all").top(),
                "{what}"
            );
            let mine = at(&harness, cx, "story-strip-mine");
            assert!(inside(mine, strip), "{what}: {mine:?} in {strip:?}");
            assert!(inside(at(&harness, cx, "story-strip-add"), mine), "{what}");
            let mut left = mine.right();
            for index in 0..recent(&harness, cx) {
                let cell = at(
                    &harness,
                    cx,
                    selector(format!("story-strip-author-{index}")),
                );
                assert!(cell.top() >= strip.top() && cell.bottom() <= strip.bottom() + px(1.));
                assert_eq!(cell.left(), left, "{what} {index}");
                left = cell.right();
            }
            assert!(
                inside(at(&harness, cx, "story-strip-fold"), strip),
                "{what}"
            );
            // The ring of the first person, around their picture, inside
            // their cell.
            let ring = at(&harness, cx, "story-ring-3-3");
            assert!(
                ring.size.width > crate::theme::story::STRIP_AVATAR(),
                "{what}"
            );

            // Status: the list in the place of the chats, the page beside it.
            click(harness.window, "nav-status", cx);
            harness.settle(cx);
            let pane = at(&harness, cx, "status-pane");
            assert!(pane.left() >= rail_width - px(1.), "{what}");
            for row in ["status-mine", "status-author-0", "new-status"] {
                assert!(inside(at(&harness, cx, row), pane), "{what}: {row}");
            }
            assert!(
                at(&harness, cx, "status-main").left() >= pane.right() - px(1.),
                "{what}"
            );
            assert!(shows(harness.window, "nav-on-status", cx), "{what}");
            click(harness.window, "nav-chats", cx);
            harness.settle(cx);
        }
    }
    cx.update(|cx| {
        settings::update(cx, |settings| {
            settings.theme = ThemeChoice::Light;
            settings.interface_scale = 100;
        })
    });
    nothing_was_seen(&harness);
}

#[gpui_kit::test]
fn the_states_of_the_list_fit_the_pane_at_every_interface_size_in_both_themes(
    cx: &mut TestAppContext,
) {
    let harness = open_with_nobody(cx);
    click(harness.window, "nav-status", cx);
    harness.settle(cx);
    for theme in [ThemeChoice::Light, ThemeChoice::Dark] {
        for step in crate::theme::SCALE_STEPS {
            cx.update(|cx| {
                settings::update(cx, |settings| {
                    settings.theme = theme;
                    settings.interface_scale = step;
                })
            });
            let what = format!("{theme:?} {step}%");
            // Nobody yet, then not available for the number.
            for (missing, state, controls) in [
                (
                    false,
                    "status-state-empty",
                    &["status-post-first", "status-check"][..],
                ),
                (true, "status-state-unavailable", &["status-check"][..]),
            ] {
                harness
                    .mock
                    .set_unavailable(None, Feature::Stories, missing);
                harness
                    .runtime
                    .block_on(harness.engine.sync_stories(&personal()))
                    .unwrap();
                harness.settle(cx);
                let pane = at(&harness, cx, "status-pane");
                let note = at(&harness, cx, "status-none");
                assert!(inside(note, pane), "{what} {state}: {note:?} in {pane:?}");
                assert!(inside(at(&harness, cx, state), note), "{what} {state}");
                assert!(
                    note.top() >= at(&harness, cx, "status-mine").bottom() - px(1.),
                    "{what} {state}: under the own status"
                );
                for control in controls {
                    let control = at(&harness, cx, control);
                    assert!(inside(control, note), "{what} {state}: {control:?}");
                }
            }
        }
    }
    harness.mock.set_unavailable(None, Feature::Stories, false);
    cx.update(|cx| {
        settings::update(cx, |settings| {
            settings.theme = ThemeChoice::Light;
            settings.interface_scale = 100;
        })
    });
}

// ----- posting a video ------------------------------------------------------------------

/// A text and a picture are posted in `status.rs`; a video goes the same
/// way, from the strip's "+": it is not decoded here, so it is a tile in
/// the sheet, and it is queued, uploaded and posted once.
#[gpui_kit::test]
fn a_video_is_posted_from_a_file_from_the_strips_plus(cx: &mut TestAppContext) {
    let harness = open_listed(cx);
    click(harness.window, "story-strip-add", cx);
    harness.settle(cx);
    click(harness.window, "post-tab-media", cx);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    // An MP4's first box: enough for a file that says what it is.
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftypmp42\0\0\0\0mp42isom");
    bytes.extend_from_slice(&[0; 256]);
    std::fs::write(&path, bytes).unwrap();
    harness
        .shell
        .update(cx, |shell, cx| shell.story_paths(vec![path], cx));
    for _ in 0..200 {
        harness.settle(cx);
        if shows(harness.window, "post-video", cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(shows(harness.window, "post-video", cx), "the video's tile");
    type_text(harness.window, "A clip", cx);
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
    assert!(pending.post.is_some(), "under My status at once");
    match &pending.story.body {
        StoryBody::Media(media) => {
            assert_eq!(media.kind, MediaKind::Video);
            assert_eq!(media.caption.as_deref(), Some("A clip"));
        }
        other => panic!("{other:?}"),
    }
    harness
        .runtime
        .block_on(
            harness
                .engine
                .flush_story_posts_at(client_provider::Timestamp::now()),
        )
        .unwrap();
    assert_eq!(harness.mock.posted_stories().len(), 1);
}
