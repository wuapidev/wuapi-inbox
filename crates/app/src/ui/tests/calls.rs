//! Calls in the window: the card of a call that rings or is on, and the
//! button that places one.

use super::keyboard::{open_chat_one, open_ids};
use super::*;
use client_provider::{CallEvent, CallState, Capabilities, ContactId, ProviderEvent, Timestamp};

/// The provider says where a call with the open chat's contact stands.
fn signal(harness: &Harness, cx: &mut TestAppContext, state: CallState, video: bool) {
    let (account, chat) = open_ids(harness, cx);
    harness
        .engine
        .apply_event(ProviderEvent::Call(CallEvent {
            account_id: account,
            call_id: "call-1".into(),
            peer: ContactId::new(chat.as_str()),
            chat_id: chat,
            video,
            state,
            at: Timestamp::now(),
        }))
        .unwrap();
    cx.run_until_parked();
}

fn ringing() -> CallState {
    CallState::Ringing {
        incoming: true,
        group: false,
    }
}

/// What the card says about the call.
fn said(harness: &Harness, cx: &mut TestAppContext) -> Option<&'static str> {
    cx.update(|_| {
        harness
            .engine
            .calls()
            .first()
            .map(super::super::calls::call_words)
    })
}

#[gpui_kit::test]
fn a_call_that_comes_in_is_answered_and_hung_up_from_its_card(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    assert!(!shows(harness.window, "call-card", cx));

    signal(&harness, cx, ringing(), false);
    assert!(shows(harness.window, "call-card", cx));
    assert!(shows(harness.window, "call-accept", cx));
    assert_eq!(said(&harness, cx), Some("Incoming voice call"));
    // The name on the card is the chat's.
    let title = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .title
            .clone()
    });
    let named = cx.update(|_| {
        let call = &harness.engine.calls()[0];
        harness
            .engine
            .store()
            .chat(&call.event.account_id, &call.event.chat_id)
            .unwrap()
            .map(|chat| chat.title)
    });
    assert_eq!(named, Some(title));

    // Answering stops the ringing at once, whatever the network does.
    click(harness.window, "call-accept", cx);
    assert_eq!(said(&harness, cx), Some("Connecting…"));
    assert!(!shows(harness.window, "call-accept", cx));
    assert!(shows(harness.window, "call-mute", cx));

    signal(&harness, cx, CallState::Connected, false);
    assert_eq!(said(&harness, cx), Some("On a call"));
    click(harness.window, "call-mute", cx);
    assert_eq!(said(&harness, cx), Some("On a call, microphone off"));

    // Hanging up takes the card away at once.
    click(harness.window, "call-end", cx);
    assert!(!shows(harness.window, "call-card", cx));
    assert!(harness.engine.calls().is_empty());
}

#[gpui_kit::test]
fn a_call_the_other_side_ends_leaves_the_screen(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    signal(&harness, cx, ringing(), false);
    assert!(shows(harness.window, "call-card", cx));
    signal(
        &harness,
        cx,
        CallState::Ended {
            missed: true,
            reason: None,
        },
        false,
    );
    assert!(!shows(harness.window, "call-card", cx));
}

#[gpui_kit::test]
fn a_video_call_is_declined_but_not_answered_where_there_is_no_video(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(Capabilities {
            video_calls: false,
            ..Capabilities::all()
        }),
        ..provider_mock::MockConfig::quiet()
    });
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    signal(&harness, cx, ringing(), true);
    assert_eq!(said(&harness, cx), Some("Incoming video call"));
    assert!(
        !shows(harness.window, "call-accept", cx),
        "nothing to answer it with"
    );
    assert!(shows(harness.window, "call-end", cx));
    click(harness.window, "call-end", cx);
    assert!(!shows(harness.window, "call-card", cx));
}

#[gpui_kit::test]
fn the_call_button_is_there_only_where_a_call_can_be_placed(cx: &mut TestAppContext) {
    // The demo provider carries calls: a person's chat has the button.
    let harness = open_chat_one(cx);
    let direct = cx.update(|cx| {
        harness.shell.read(cx).open.as_ref().unwrap().chat.kind == client_provider::ChatKind::Direct
    });
    assert_eq!(shows(harness.window, "chat-call", cx), direct);
    // While a call is on, a second one is not offered.
    signal(&harness, cx, ringing(), false);
    assert!(!shows(harness.window, "chat-call", cx));

    // A provider without calls: no button anywhere.
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(Capabilities {
            calls: false,
            ..Capabilities::all()
        }),
        ..provider_mock::MockConfig::quiet()
    });
    let without = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    assert!(!shows(without.window, "chat-call", cx));
}

#[gpui_kit::test]
fn a_call_with_video_shows_both_sides_and_the_camera_turns_off(cx: &mut TestAppContext) {
    // The demo provider carries video: the chat of a person has both
    // buttons.
    let harness = open_chat_one(cx);
    assert!(shows(harness.window, "chat-video-call", cx));
    click(harness.window, "chat-video-call", cx);
    harness.settle(cx);
    // The provider says it rings there, and then that it was answered:
    // the call it placed is the first of the demo world.
    let (account, chat) = open_ids(&harness, cx);
    let says = |state: CallState| {
        ProviderEvent::Call(CallEvent {
            account_id: account.clone(),
            call_id: "mock-call-1".into(),
            peer: ContactId::new(chat.as_str()),
            chat_id: chat.clone(),
            video: true,
            state,
            at: Timestamp::now(),
        })
    };
    let rings = CallState::Ringing {
        incoming: false,
        group: false,
    };
    harness.engine.apply_event(says(rings)).unwrap();
    cx.run_until_parked();
    // While it rings there is nothing to draw, and no second call.
    assert_eq!(said(&harness, cx), Some("Calling…"));
    assert!(!shows(harness.window, "call-video", cx));
    assert!(!shows(harness.window, "chat-video-call", cx));
    harness
        .engine
        .apply_event(says(CallState::Connected))
        .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "call-video", cx));
    assert!(shows(harness.window, "call-video-remote", cx));
    assert!(shows(harness.window, "call-video-local", cx));
    // The pictures keep to their stage, whatever their proportions (the
    // demo's are upright, like a phone's): the switches are under it,
    // inside the card.
    let card = bounds(harness.window, "call-card", cx);
    let stage = bounds(harness.window, "call-video", cx);
    let uneven = (stage.size.height - stage.size.width).abs();
    assert!(
        uneven <= gpui_kit::px(3.),
        "a square stage, but for the card's border"
    );
    assert!(within(
        bounds(harness.window, "call-video-remote", cx),
        stage
    ));
    assert!(within(
        bounds(harness.window, "call-video-local", cx),
        stage
    ));
    let switch = bounds(harness.window, "call-camera", cx);
    assert!(switch.top() >= stage.bottom() && within(switch, card));

    // The camera off: the own picture goes, theirs stays, and the call
    // goes on.
    click(harness.window, "call-camera", cx);
    cx.run_until_parked();
    assert!(harness.engine.calls()[0].camera_off);
    assert!(!shows(harness.window, "call-video-local", cx));
    assert!(shows(harness.window, "call-video-remote", cx));
    click(harness.window, "call-camera", cx);
    cx.run_until_parked();
    assert!(shows(harness.window, "call-video-local", cx));

    click(harness.window, "call-end", cx);
    assert!(!shows(harness.window, "call-card", cx));
}

#[gpui_kit::test]
fn a_call_that_is_on_says_for_how_long(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    signal(&harness, cx, CallState::Connected, false);
    let call = harness.engine.calls().remove(0);
    let later = Timestamp::from_millis(call.connected_at.unwrap().as_millis() + 65_000);
    assert_eq!(
        super::super::calls::call_time(&call, later).as_deref(),
        Some("1:05")
    );
    // A voice call has no stage and no camera switch.
    assert!(!shows(harness.window, "call-video", cx));
    assert!(!shows(harness.window, "call-camera", cx));
    assert!(shows(harness.window, "call-mute", cx));
}

#[gpui_kit::test]
fn a_call_that_rings_is_heard_until_it_is_answered_or_over(cx: &mut TestAppContext) {
    use crate::ring::Ringing;
    let harness = open_chat_one(cx);
    let rings =
        |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).call_deck.ringer.ringing());
    assert_eq!(rings(cx), None);
    // Somebody calls: it rings here.
    signal(&harness, cx, ringing(), false);
    assert_eq!(rings(cx), Some(Ringing::Incoming));
    // Answered: the ring stops at once, for the voices.
    click(harness.window, "call-accept", cx);
    cx.run_until_parked();
    assert_eq!(rings(cx), None);
    signal(
        &harness,
        cx,
        CallState::Ended {
            missed: false,
            reason: None,
        },
        false,
    );
    assert_eq!(rings(cx), None);

    // A call the account places rings there, quietly here; a call nobody
    // took stops ringing when it ends.
    let calling = CallState::Ringing {
        incoming: false,
        group: false,
    };
    signal(&harness, cx, calling, false);
    assert_eq!(rings(cx), Some(Ringing::Outgoing));
    signal(
        &harness,
        cx,
        CallState::Ended {
            missed: true,
            reason: None,
        },
        false,
    );
    assert_eq!(rings(cx), None);
}

#[gpui_kit::test]
fn a_calls_card_is_dragged_to_where_it_is_wanted_and_stays_in_the_window(cx: &mut TestAppContext) {
    use gpui_kit::{point, px, Modifiers, MouseButton, VisualTestContext};
    let harness = open_chat_one(cx);
    signal(&harness, cx, CallState::Connected, false);
    let before = bounds(harness.window, "call-card", cx);
    // Pressed on its name and moved: the card goes with the pointer.
    let drag = |by: gpui_kit::Point<gpui_kit::Pixels>, cx: &mut TestAppContext| {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        let from = visual.debug_bounds("call-name").unwrap().center();
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(from + by, Some(MouseButton::Left), Modifiers::none());
        visual.simulate_mouse_up(from + by, MouseButton::Left, Modifiers::none());
        visual.run_until_parked();
    };
    drag(point(px(-300.), px(120.)), cx);
    let after = bounds(harness.window, "call-card", cx);
    assert_eq!(after.left(), before.left() - px(300.));
    assert_eq!(after.top(), before.top() + px(120.));
    // Let go, it stays; the pointer moving later moves nothing.
    {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_mouse_move(point(px(50.), px(50.)), None, Modifiers::none());
        visual.run_until_parked();
    }
    assert_eq!(bounds(harness.window, "call-card", cx), after);
    // Its switches still work where it is.
    click(harness.window, "call-mute", cx);
    assert_eq!(said(&harness, cx), Some("On a call, microphone off"));
    // Thrown past the window's corner, it stops at the edges.
    drag(point(px(-5000.), px(-5000.)), cx);
    let cornered = bounds(harness.window, "call-card", cx);
    assert!(cornered.left() >= px(0.) && cornered.top() >= px(0.));
    drag(point(px(9000.), px(9000.)), cx);
    let far = bounds(harness.window, "call-card", cx);
    let window = cx.update(|cx| {
        harness
            .window
            .update(cx, |_, window, _| window.viewport_size())
            .unwrap()
    });
    assert!(far.right() <= window.width && far.top() < window.height);
}

#[gpui_kit::test]
fn a_calls_card_is_made_larger_and_smaller_by_its_corner(cx: &mut TestAppContext) {
    use gpui_kit::{point, px, Modifiers, MouseButton, VisualTestContext};
    let harness = open_chat_one(cx);
    signal(&harness, cx, CallState::Connected, false);
    // Moved away from the window's edge first, so that there is room to
    // pull its corner out.
    {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        let from = visual.debug_bounds("call-name").unwrap().center();
        let to = from + point(px(-400.), px(0.));
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        visual.run_until_parked();
    }
    let before = bounds(harness.window, "call-card", cx);
    let pull = |by: f32, cx: &mut TestAppContext| {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        let from = visual.debug_bounds("call-size").unwrap().center();
        visual.simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
        let to = from + point(px(by), px(0.));
        visual.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
        visual.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        visual.run_until_parked();
    };
    // Pulled out by its corner it is wider, and the corner across from it
    // has not moved.
    pull(120., cx);
    let larger = bounds(harness.window, "call-card", cx);
    assert_eq!(larger.size.width, before.size.width + px(120.));
    assert_eq!(larger.left(), before.left());
    assert_eq!(larger.top(), before.top());
    // Pushed in past the least it can be, it stops there, still a card.
    pull(-380., cx);
    let least = bounds(harness.window, "call-card", cx);
    assert!(least.size.width < before.size.width && least.size.width > px(200.));
    assert_eq!(least.left(), before.left());
    assert!(within(bounds(harness.window, "call-mute", cx), least));
    // It is the card that was sized, not moved: the pointer let go, it
    // stays.
    {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_mouse_move(point(px(40.), px(40.)), None, Modifiers::none());
        visual.run_until_parked();
    }
    assert_eq!(bounds(harness.window, "call-card", cx), least);
}

#[gpui_kit::test]
fn somebody_is_added_to_a_call_and_shows_beside_the_others(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    click(harness.window, "chat-video-call", cx);
    harness.settle(cx);
    let (account, chat) = open_ids(&harness, cx);
    let says = |state: CallState| {
        ProviderEvent::Call(CallEvent {
            account_id: account.clone(),
            call_id: "mock-call-1".into(),
            peer: ContactId::new(chat.as_str()),
            chat_id: chat.clone(),
            video: true,
            state,
            at: Timestamp::now(),
        })
    };
    // While it rings nobody can be added: there is no call yet.
    let rings = CallState::Ringing {
        incoming: false,
        group: false,
    };
    harness.engine.apply_event(says(rings)).unwrap();
    cx.run_until_parked();
    assert!(!shows(harness.window, "call-add", cx));
    harness
        .engine
        .apply_event(says(CallState::Connected))
        .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "call-video-remote", cx));
    assert!(!shows(harness.window, "call-video-guest-0", cx));

    // The switch opens a search of the contacts, less who is there.
    click(harness.window, "call-add", cx);
    cx.run_until_parked();
    assert!(shows(harness.window, "call-add-search", cx));
    assert!(shows(harness.window, "call-add-0", cx));
    click(harness.window, "call-add-0", cx);
    harness.settle(cx);
    let asked = harness.mock.call_invites();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].0, "mock-call-1");
    assert_ne!(
        asked[0].1.as_str(),
        chat.as_str(),
        "not who it is with already"
    );
    // Asked, the search is done with.
    assert!(!shows(harness.window, "call-add-search", cx));

    // A call with several: each has a place, side by side, in the stage.
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(100));
    cx.run_until_parked();
    let stage = bounds(harness.window, "call-video", cx);
    let (first, second) = (
        bounds(harness.window, "call-video-guest-0", cx),
        bounds(harness.window, "call-video-guest-1", cx),
    );
    assert!(within(first, stage) && within(second, stage));
    assert!(first.right() <= second.left(), "side by side");
    assert!(!shows(harness.window, "call-video-remote", cx));
    // The own camera keeps its corner.
    assert!(shows(harness.window, "call-video-local", cx));
}

#[test]
fn the_people_of_a_call_share_its_pictures() {
    use super::super::calls::grid_of;
    assert_eq!(grid_of(1), (1, 1, 1.0));
    assert_eq!(grid_of(2), (2, 1, 1.5));
    assert_eq!(grid_of(4), (2, 2, 1.0));
    assert_eq!(grid_of(5), (3, 2, 1.0));
    assert_eq!(grid_of(7), (3, 3, 1.0));
}
