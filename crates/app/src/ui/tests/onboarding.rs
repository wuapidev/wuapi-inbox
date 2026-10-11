//! The screen of a window without a number: it says what to do first,
//! and leaves once a number is there.

use super::super::shell::{CloudOffer, ShellEvent};
use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The demo provider with its numbers taken away, as a provider is before
/// anything was linked to it.
fn without_numbers() -> MockProvider {
    use client_provider::Provider as _;
    let mock = MockProvider::quiet();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for account in mock.list_accounts().await.unwrap() {
            mock.delete_account(&account.id).await.unwrap();
        }
        assert!(mock.list_accounts().await.unwrap().is_empty());
    });
    mock
}

/// A window over a provider that has no number, with the cloud as given.
fn open_empty(cx: &mut TestAppContext, cloud: CloudOffer) -> Harness {
    cx.update(|cx| prepare(cx, None));
    open_over(
        cx,
        ShellOptions {
            cloud,
            session: SessionKind::Keychain,
            ..Default::default()
        },
        without_numbers(),
    )
}

/// What the shell tells its owner from here on.
fn events(harness: &Harness, cx: &mut TestAppContext) -> Rc<RefCell<Vec<ShellEvent>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let told = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&harness.shell, move |_, event: &ShellEvent, _| {
            told.borrow_mut().push(*event)
        })
        .detach();
    });
    seen
}

#[gpui_kit::test]
fn a_window_without_a_number_says_how_to_link_one(cx: &mut TestAppContext) {
    let harness = open_empty(cx, CloudOffer::Unavailable);
    cx.update(|cx| assert!(harness.shell.read(cx).accounts.is_empty()));
    assert!(shows(harness.window, "onboarding", cx));
    assert!(shows(harness.window, "onboarding-link", cx));
    assert!(
        !shows(harness.window, "start", cx),
        "it takes the start screen's place"
    );
    // All of it is on screen, inside the conversation pane.
    let panel = bounds(harness.window, "onboarding", cx);
    let button = bounds(harness.window, "onboarding-link", cx);
    let list = cx.update(|cx| harness.shell.read(cx).list_px);
    assert!(
        panel.left() >= theme::metrics::RAIL_WIDTH() + list && panel.right() <= px(1240.),
        "{panel:?}"
    );
    assert!(
        panel.top() >= theme::metrics::HEADER_HEIGHT() && panel.bottom() <= px(800.),
        "{panel:?}"
    );
    assert!(within(button, panel), "{button:?} in {panel:?}");
}

#[gpui_kit::test]
fn with_numbers_the_start_screen_is_as_it_was(cx: &mut TestAppContext) {
    // The demo provider has two numbers, and the cloud on offer changes
    // nothing for it.
    let harness = open(
        cx,
        ShellOptions {
            cloud: CloudOffer::Offered,
            ..Default::default()
        },
    );
    assert!(shows(harness.window, "start", cx));
    assert!(!shows(harness.window, "onboarding", cx));
    assert!(!shows(harness.window, "onboarding-link", cx));
    assert!(!shows(harness.window, "onboarding-cloud", cx));
}

#[gpui_kit::test]
fn the_button_opens_the_linking_screen_and_a_number_ends_the_onboarding(cx: &mut TestAppContext) {
    let harness = open_empty(cx, CloudOffer::Unavailable);
    click(harness.window, "onboarding-link", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::AddNumber);
        assert!(shell.linking.is_some());
    });
    assert!(shows(harness.window, "link-submit", cx));

    // The same screen as from the rail. With no number to take a default
    // from, where it connects from is the user's to choose.
    harness.shell.update(cx, |shell, _| {
        let flow = shell.linking.as_mut().unwrap();
        assert!(flow.place.is_none());
        flow.place = flow.places.as_ref().unwrap().first().cloned();
        assert!(flow.place.is_some());
    });
    type_text(harness.window, "Support", cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    assert_eq!(stored_accounts(&harness).len(), 1);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).accounts.len(), 1));
    assert!(!shows(harness.window, "onboarding", cx));
    assert!(!shows(harness.window, "onboarding-link", cx));
}

#[gpui_kit::test]
fn enter_links_the_first_number(cx: &mut TestAppContext) {
    let harness = open_empty(cx, CloudOffer::Unavailable);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
    press(harness.window, "enter", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::AddNumber);
        assert!(shell.linking.is_some());
    });

    // With numbers, Enter on the start screen opens no linking screen.
    cx.update(|cx| prepare(cx, None));
    let demo = open_prepared(cx, ShellOptions::default());
    press(demo.window, "enter", cx);
    cx.update(|cx| assert_ne!(demo.shell.read(cx).overlay, Overlay::AddNumber));
}

#[gpui_kit::test]
fn a_provider_that_links_no_numbers_is_not_offered_the_button(cx: &mut TestAppContext) {
    use client_provider::Capabilities;
    use client_provider::Provider as _;
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(Capabilities {
            link_accounts: false,
            ..Capabilities::all()
        }),
        ..provider_mock::MockConfig::quiet()
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for account in mock.list_accounts().await.unwrap() {
            mock.delete_account(&account.id).await.unwrap();
        }
    });
    let harness = open_over(
        cx,
        ShellOptions {
            session: SessionKind::Keychain,
            ..Default::default()
        },
        mock,
    );
    assert!(shows(harness.window, "onboarding", cx));
    assert!(!shows(harness.window, "onboarding-link", cx));
    press(harness.window, "enter", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}

#[gpui_kit::test]
fn the_cloud_is_offered_only_where_it_can_be_connected(cx: &mut TestAppContext) {
    for cloud in [CloudOffer::Unavailable, CloudOffer::Connected] {
        let harness = open_empty(cx, cloud);
        assert!(shows(harness.window, "onboarding", cx));
        assert!(
            !shows(harness.window, "onboarding-cloud", cx),
            "no card for {cloud:?}"
        );
    }

    let harness = open_empty(cx, CloudOffer::Offered);
    let seen = events(&harness, cx);
    assert!(shows(harness.window, "onboarding-cloud", cx));
    // The card and the button above it both fit the window.
    let panel = bounds(harness.window, "onboarding", cx);
    assert!(
        panel.top() >= theme::metrics::HEADER_HEIGHT() && panel.bottom() <= px(800.),
        "{panel:?}"
    );
    assert!(within(
        bounds(harness.window, "onboarding-cloud", cx),
        panel
    ));
    assert!(within(bounds(harness.window, "onboarding-link", cx), panel));

    click(harness.window, "onboarding-cloud", cx);
    assert_eq!(*seen.borrow(), vec![ShellEvent::ConnectCloud]);
    // Asking for the cloud opens nothing here: the owner answers it.
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}

#[gpui_kit::test]
fn the_demo_data_never_shows_it_while_its_numbers_are_not_read_yet(cx: &mut TestAppContext) {
    // The demo session with an empty list: the start screen, as before.
    cx.update(|cx| prepare(cx, None));
    let harness = open_over(cx, ShellOptions::default(), without_numbers());
    cx.update(|cx| assert!(harness.shell.read(cx).accounts.is_empty()));
    assert!(shows(harness.window, "start", cx));
    assert!(!shows(harness.window, "onboarding", cx));
    press(harness.window, "enter", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}
