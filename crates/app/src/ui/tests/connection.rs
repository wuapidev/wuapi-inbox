//! Whether a number is connected is said where it is looked at: on top of
//! its chats, over the composer of an open chat, on its face in the rail
//! and in the settings. Where the number needs somebody, the way to link
//! it again is next to the words.

use super::super::connection::connection_words;
use super::super::numbers::LinkStage;
use super::*;
use client_provider::{
    AccountId, Capabilities, ConnectionState, HistoryImport, NewAccount, Provider as _,
    ProviderEvent,
};
use provider_mock::MockConfig;

fn first(harness: &Harness) -> AccountId {
    stored_accounts(harness)[0].id.clone()
}

/// The provider says the first number's connection changed.
fn becomes(harness: &Harness, state: ConnectionState, cx: &mut TestAppContext) {
    harness
        .engine
        .apply_event(ProviderEvent::ConnectionChanged {
            account_id: first(harness),
            state,
        })
        .unwrap();
    harness.settle(cx);
}

fn stopped(reason: &str) -> ConnectionState {
    ConnectionState::Disconnected {
        reason: Some(reason.into()),
    }
}

/// The sentence on top of the chats of the number on screen.
fn sentence(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let account = shell
            .accounts
            .iter()
            .find(|account| Some(&account.id) == shell.account.as_ref())
            .unwrap();
        connection_words(account).sentence
    })
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

/// The number the linking screen is showing a code for.
fn linking(harness: &Harness, cx: &mut TestAppContext) -> AccountId {
    cx.update(
        |cx| match &harness.shell.read(cx).linking.as_ref().unwrap().stage {
            LinkStage::Waiting(status) => status.account.id.clone(),
            other => panic!("not waiting: {other:?}"),
        },
    )
}

/// A window over a provider that cannot manage its numbers.
fn open_without_managing(cx: &mut TestAppContext, options: ShellOptions) -> Harness {
    cx.update(|cx| prepare(cx, None));
    open_over(
        cx,
        options,
        MockProvider::new(MockConfig {
            capabilities: Some(Capabilities {
                manage_accounts: false,
                ..Capabilities::all()
            }),
            ..MockConfig::quiet()
        }),
    )
}

fn with_a_chat_open() -> ShellOptions {
    ShellOptions {
        open_chat: Some(1),
        ..Default::default()
    }
}

#[gpui_kit::test]
fn a_connected_number_says_so_quietly_on_top_of_its_chats(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    assert!(shows(harness.window, "connection-banner", cx));
    assert!(shows(harness.window, "connection-state-connected", cx));
    assert!(!shows(harness.window, "connection-banner-action", cx));
    assert_eq!(sentence(&harness, cx), "Connected");
    // One line, and no taller than the strips that say something is wrong.
    let quiet = bounds(harness.window, "connection-banner", cx);
    becomes(&harness, stopped("proxy_paused"), cx);
    let loud = bounds(harness.window, "connection-banner", cx);
    assert_eq!(quiet.top(), loud.top());
    assert!(loud.size.height > quiet.size.height, "{quiet:?} {loud:?}");
}

#[gpui_kit::test]
fn a_number_on_its_way_back_says_so_without_asking_for_anything(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    becomes(&harness, ConnectionState::Reconnecting, cx);
    assert!(shows(harness.window, "connection-state-reconnecting", cx));
    assert!(!shows(harness.window, "connection-banner-action", cx));
    assert_eq!(
        sentence(&harness, cx),
        "Reconnecting… Messages you send will go out when it is back."
    );
    becomes(&harness, ConnectionState::Connecting, cx);
    assert!(shows(harness.window, "connection-state-connecting", cx));
    assert!(!shows(harness.window, "connection-banner-action", cx));
    // Back: the quiet line again.
    becomes(&harness, ConnectionState::Connected, cx);
    assert!(shows(harness.window, "connection-state-connected", cx));
}

#[gpui_kit::test]
fn a_number_that_dropped_says_why_and_is_reconnected_from_there(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    becomes(&harness, stopped("proxy_paused"), cx);
    assert!(shows(harness.window, "connection-state-disconnected", cx));
    assert!(shows(harness.window, "connection-banner-action", cx));
    assert_eq!(sentence(&harness, cx), "Not connected: its proxy is paused");
    assert_eq!(overlay(&harness, cx), Overlay::None);

    click(harness.window, "connection-banner-action", cx);
    assert_eq!(overlay(&harness, cx), Overlay::AddNumber);
    assert!(cx.update(|cx| harness.shell.read(cx).linking.is_some()));
    // The session was only restarted: no number was made or removed.
    harness.settle(cx);
    assert_eq!(stored_accounts(&harness).len(), 2);
}

#[gpui_kit::test]
fn a_logged_out_number_is_linked_again_from_the_strip(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = first(&harness);
    harness
        .runtime
        .block_on(harness.mock.unlink_account(&account))
        .unwrap();
    becomes(&harness, ConnectionState::LoggedOut, cx);
    assert!(shows(harness.window, "connection-state-logged-out", cx));
    assert_eq!(
        sentence(&harness, cx),
        "This number was logged out. Link it again."
    );

    click(harness.window, "connection-banner-action", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    assert_eq!(linking(&harness, cx), account);
    assert_eq!(stored_accounts(&harness).len(), 2);
}

#[gpui_kit::test]
fn a_number_that_never_linked_is_linked_from_the_strip(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let new = NewAccount {
        name: Some("Never linked".into()),
        place: None,
        pairing_phone: None,
        history: HistoryImport::Off,
        request_id: "elsewhere".into(),
    };
    let id = harness
        .runtime
        .block_on(harness.mock.create_account(&new))
        .unwrap()
        .account
        .id;
    harness
        .mock
        .stop_link(&id, "The number was not linked in time.");
    harness
        .runtime
        .block_on(harness.mock.link_status(&id))
        .unwrap();
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();

    click(harness.window, "rail-account-2", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "connection-state-unlinked", cx));
    assert_eq!(
        sentence(&harness, cx),
        "This number was created and never linked to a phone."
    );
    click(harness.window, "connection-banner-action", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    assert_eq!(linking(&harness, cx), id);
}

#[gpui_kit::test]
fn an_open_chat_says_its_number_is_not_connected_and_still_sends(cx: &mut TestAppContext) {
    let harness = open(cx, with_a_chat_open());
    assert!(!shows(harness.window, "conversation-connection", cx));

    becomes(&harness, stopped("session_not_found"), cx);
    assert!(shows(harness.window, "conversation-connection", cx));
    assert!(shows(harness.window, "conversation-connection-action", cx));
    // Over the composer, which is still there: what is written waits in
    // the outbox.
    let notice = bounds(harness.window, "conversation-connection", cx);
    let composer = bounds(harness.window, "composer", cx);
    assert!(notice.bottom() <= composer.top(), "{notice:?} {composer:?}");
    let field = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", field.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("for later", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let newest = cx.update(|cx| newest_message(harness.shell.read(cx)));
    assert_eq!(newest.map(|(text, ..)| text).as_deref(), Some("for later"));

    click(harness.window, "conversation-connection-action", cx);
    assert_eq!(overlay(&harness, cx), Overlay::AddNumber);
}

#[gpui_kit::test]
fn an_open_chat_says_nothing_once_its_number_is_back(cx: &mut TestAppContext) {
    let harness = open(cx, with_a_chat_open());
    becomes(&harness, ConnectionState::Reconnecting, cx);
    // On its way back: said, with nothing to press.
    assert!(shows(harness.window, "conversation-connection", cx));
    assert!(!shows(harness.window, "conversation-connection-action", cx));
    becomes(&harness, ConnectionState::Connected, cx);
    assert!(!shows(harness.window, "conversation-connection", cx));
}

#[gpui_kit::test]
fn a_provider_that_cannot_reconnect_says_the_state_without_a_button(cx: &mut TestAppContext) {
    let harness = open_without_managing(cx, with_a_chat_open());
    for state in [stopped("link_timeout"), ConnectionState::LoggedOut] {
        becomes(&harness, state, cx);
        assert!(shows(harness.window, "connection-banner", cx));
        assert!(!shows(harness.window, "connection-banner-action", cx));
        assert!(shows(harness.window, "conversation-connection", cx));
        assert!(!shows(harness.window, "conversation-connection-action", cx));
    }
}

#[gpui_kit::test]
fn the_rail_marks_a_number_that_needs_somebody_and_names_its_state(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let hint = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            shell.rail_hint(&shell.accounts[0]).to_string()
        })
    };
    let dot = bounds(harness.window, "rail-led-a0", cx);
    assert!(hint(cx).ends_with(" · Connected"), "{}", hint(cx));

    // On its way back: the same light, in another colour.
    becomes(&harness, ConnectionState::Reconnecting, cx);
    assert_eq!(bounds(harness.window, "rail-led-a0", cx), dot);
    assert!(hint(cx).ends_with(" · Reconnecting"), "{}", hint(cx));

    for (state, said) in [
        (stopped("proxy_paused"), " · Not connected"),
        (ConnectionState::LoggedOut, " · Logged out"),
    ] {
        becomes(&harness, state, cx);
        let mark = bounds(harness.window, "rail-led-a0", cx);
        let cell = bounds(harness.window, "rail-account-0", cx);
        // A mark, larger than the light, on the same corner and inside
        // the rail.
        assert_eq!(mark.size.width, mark.size.height);
        assert!(mark.size.width >= dot.size.width + px(3.), "{mark:?}");
        assert_eq!(mark.right(), dot.right());
        assert_eq!(mark.bottom(), dot.bottom());
        assert!(mark.right() <= theme::metrics::RAIL_WIDTH());
        assert!(!mark.contains(&cell.center()));
        assert!(hint(cx).ends_with(said), "{}", hint(cx));
    }
}

#[gpui_kit::test]
fn the_settings_say_why_a_number_is_not_connected(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "settings", cx);
    assert!(!shows(harness.window, "number-reason-0", cx));
    becomes(&harness, stopped("free_limit_reached"), cx);
    assert!(shows(harness.window, "number-reason-0", cx));
    assert!(!shows(harness.window, "number-reason-1", cx));
    // No reason given: nothing is made up.
    becomes(&harness, ConnectionState::Disconnected { reason: None }, cx);
    assert!(!shows(harness.window, "number-reason-0", cx));
}
