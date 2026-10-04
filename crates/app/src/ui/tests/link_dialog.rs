//! The screen that links a number: the placeholder that waits where the QR
//! code will be, and choosing between the QR code and a code to type, on
//! the form and after it. Everything runs in the headless window over the
//! mock provider; where things are is measured with the elements'
//! `debug_selector`s.

use super::super::numbers::LinkStage;
use super::*;
use client_provider::{Capabilities, LinkStep, ProviderError};
use provider_mock::MockConfig;

/// The linking screen, waiting for the phone: the number was created and
/// the provider has not shown a code yet.
fn start_link(harness: &Harness, cx: &mut TestAppContext) -> client_provider::AccountId {
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    stored_accounts(harness).last().unwrap().id.clone()
}

/// A window over a provider that can only do `caps`.
fn open_limited(cx: &mut TestAppContext, caps: Capabilities) -> Harness {
    cx.update(|cx| prepare(cx, None));
    open_over(
        cx,
        ShellOptions::default(),
        MockProvider::new(MockConfig {
            capabilities: Some(caps),
            ..MockConfig::quiet()
        }),
    )
}

fn step(harness: &Harness, cx: &mut TestAppContext) -> LinkStep {
    cx.update(
        |cx| match &harness.shell.read(cx).linking.as_ref().unwrap().stage {
            LinkStage::Waiting(status) => status.step.clone(),
            other => panic!("not waiting: {other:?}"),
        },
    )
}

fn flow_error(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .linking
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default()
    })
}

fn typed_phone(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        flow.phone.read(cx).value().to_string()
    })
}

fn centre_x(bounds: Bounds<gpui_kit::Pixels>) -> gpui_kit::Pixels {
    bounds.left() + bounds.size.width / 2.
}

fn near(a: gpui_kit::Pixels, b: gpui_kit::Pixels) -> bool {
    (a - b).abs() <= px(1.)
}

fn clipboard(cx: &mut TestAppContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

// ----- the placeholder ------------------------------------------------------

#[gpui_kit::test]
fn a_placeholder_waits_where_the_qr_code_will_be_and_nothing_moves(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    start_link(&harness, cx);

    // No code yet: the placeholder, with the mark on its plate.
    assert_eq!(step(&harness, cx), LinkStep::Starting);
    assert!(!shows(harness.window, "link-qr", cx));
    let frame = bounds(harness.window, "link-qr-frame", cx);
    let waiting = bounds(harness.window, "link-qr-skeleton", cx);
    let plate = bounds(harness.window, "link-qr-plate", cx);
    assert_eq!(waiting, frame);
    assert_eq!(frame.size.width, frame.size.height);
    assert!(within(plate, frame));
    assert!(near(centre_x(plate), centre_x(frame)));
    assert!(near(centre_y(plate), centre_y(frame)));
    let below = bounds(harness.window, "link-use-code", cx);
    let cancel = bounds(harness.window, "link-cancel", cx);

    // The code arrives: in the same place, at the same size, and what is
    // under it has not moved either.
    link_round(&harness, cx);
    assert!(!shows(harness.window, "link-qr-skeleton", cx));
    assert_eq!(bounds(harness.window, "link-qr", cx), waiting);
    assert_eq!(bounds(harness.window, "link-qr-frame", cx), frame);
    assert_eq!(bounds(harness.window, "link-use-code", cx), below);
    assert_eq!(bounds(harness.window, "link-cancel", cx), cancel);
}

#[gpui_kit::test]
fn the_screen_is_already_the_wait_while_the_number_is_created(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    // The request is on its way and has not been answered.
    click(harness.window, "link-submit", cx);
    assert!(shows(harness.window, "link-creating", cx));
    let creating = bounds(harness.window, "link-qr-skeleton", cx);
    let offer = bounds(harness.window, "link-use-code", cx);
    // Nothing to ask a code for yet: the offer is there and does nothing.
    click(harness.window, "link-use-code", cx);
    assert!(!shows(harness.window, "link-phone-step", cx));

    harness.settle(cx);
    assert!(!shows(harness.window, "link-creating", cx));
    assert_eq!(bounds(harness.window, "link-qr-skeleton", cx), creating);
    assert_eq!(bounds(harness.window, "link-use-code", cx), offer);
    link_round(&harness, cx);
    assert_eq!(bounds(harness.window, "link-qr", cx), creating);
}

#[gpui_kit::test]
fn the_placeholder_and_the_code_match_at_every_size_and_in_both_themes(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let new = start_link(&harness, cx);
    let window_size = size(px(1240.), px(800.));

    for dark in [false, true] {
        cx.update(|cx| {
            settings::update(cx, |settings| {
                settings.theme = if dark {
                    ThemeChoice::Dark
                } else {
                    ThemeChoice::Light
                }
            })
        });
        assert_eq!(cx.update(|cx| theme::palette(cx).is_dark()), dark);
        for scale in theme::SCALE_STEPS {
            cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = scale));
            let at = format!("{scale}% dark={dark}");

            // A code that cannot be drawn: the placeholder stands in.
            harness.mock.garble_code(&new, true);
            link_round(&harness, cx);
            assert!(shows(harness.window, "link-qr-unreadable", cx), "{at}");
            assert!(!shows(harness.window, "link-qr", cx), "{at}");
            let card = bounds(harness.window, "add-number", cx);
            let frame = bounds(harness.window, "link-qr-frame", cx);
            let waiting = bounds(harness.window, "link-qr-skeleton", cx);
            let plate = bounds(harness.window, "link-qr-plate", cx);
            assert_eq!(waiting, frame, "{at}");
            assert_eq!(frame.size.width, theme::px(232.), "{at}");
            assert_eq!(frame.size.height, theme::px(232.), "{at}");
            assert!(
                within(plate, frame) && near(centre_x(plate), centre_x(frame)),
                "{at}"
            );
            assert!(near(centre_x(frame), centre_x(card)), "{at}: centred");

            // The next one can: it is where the placeholder was.
            harness.mock.garble_code(&new, false);
            link_round(&harness, cx);
            assert!(!shows(harness.window, "link-qr-unreadable", cx), "{at}");
            assert!(!shows(harness.window, "link-qr-skeleton", cx), "{at}");
            assert_eq!(bounds(harness.window, "link-qr", cx), waiting, "{at}");

            // Nothing leaves the card, and the card is inside the window.
            for part in ["link-qr-frame", "link-hint", "link-use-code", "link-cancel"] {
                let part_bounds = bounds(harness.window, part, cx);
                assert!(
                    within(part_bounds, card),
                    "{at}: {part} {part_bounds:?} outside {card:?}"
                );
            }
            assert!(
                card.right() <= window_size.width && card.bottom() <= window_size.height,
                "{at}: {card:?}"
            );
            assert!(card.left() >= px(0.) && card.top() >= px(0.), "{at}");
        }
    }
}

#[gpui_kit::test]
fn the_code_fades_in_over_the_placeholder_and_then_nothing_repaints(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        settings::init(None, Some(Appearance::Light), cx);
        settings::update(cx, |settings| {
            settings.motion = MotionChoice::On;
            settings.palette_tip_done = true;
        });
    });
    let harness = open_prepared(cx, ShellOptions::default());
    let new = start_link(&harness, cx);
    let look = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            let look = shell.qr_look(shell.linking.as_ref().unwrap(), cx);
            (look.shown, look.light.is_some(), look.moving)
        })
    };

    // Waiting: the light passes over the placeholder.
    assert_eq!(look(cx), (0., true, true));
    let waiting = bounds(harness.window, "link-qr-skeleton", cx);

    // The code arrives and comes in over it, in the same place.
    link_round(&harness, cx);
    let (shown, _, moving) = look(cx);
    assert_eq!(shown, 0., "it starts from nothing");
    assert!(moving);
    assert_eq!(bounds(harness.window, "link-qr", cx), waiting);
    assert_eq!(bounds(harness.window, "link-qr-skeleton", cx), waiting);
    cx.executor()
        .advance_clock(crate::motion::link::CROSSFADE / 2);
    let (half, _, moving) = look(cx);
    assert!(half > 0. && half < 1., "{half}");
    assert!(moving);

    // And then it is there: the placeholder is gone and nothing moves.
    link_round(&harness, cx);
    assert_eq!(look(cx), (1., false, false));
    assert!(!shows(harness.window, "link-qr-skeleton", cx));
    assert_eq!(bounds(harness.window, "link-qr", cx), waiting);

    // A code that replaces another is just the next one: no fade.
    harness.mock.garble_code(&new, true);
    link_round(&harness, cx);
    assert_eq!(look(cx).0, 0.);
    assert!(shows(harness.window, "link-qr-skeleton", cx));
    // A wait that goes on does not keep the window repainting.
    cx.executor()
        .advance_clock(crate::motion::link::SWEEP_FOR + Duration::from_secs(1));
    assert_eq!(look(cx), (0., false, false));
}

#[gpui_kit::test]
fn the_placeholder_is_still_when_motion_is_off(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    start_link(&harness, cx);
    let look = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            let look = shell.qr_look(shell.linking.as_ref().unwrap(), cx);
            (look.shown, look.light.is_some(), look.moving)
        })
    };
    assert_eq!(look(cx), (0., false, false));
    // And the code is there at once.
    link_round(&harness, cx);
    assert_eq!(look(cx), (1., false, false));
}

#[gpui_kit::test]
fn the_placeholder_comes_back_when_the_session_starts_over(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let new = start_link(&harness, cx);
    link_round(&harness, cx);
    let code = bounds(harness.window, "link-qr", cx);

    // The code ran out and the provider is making another: no stale code
    // is left to scan.
    harness
        .mock
        .stop_link(&new, "The number was not linked in time.");
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-stopped", cx));
    click(harness.window, "link-retry", cx);
    // Asked, and not answered yet: the placeholder, where the code was.
    assert_eq!(bounds(harness.window, "link-qr-skeleton", cx), code);
    assert!(!shows(harness.window, "link-qr", cx));
}

// ----- QR code or code to type ----------------------------------------------

#[gpui_kit::test]
fn the_form_says_what_each_way_of_linking_is(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    let card = bounds(harness.window, "add-number", cx);
    let by_scan = bounds(harness.window, "link-method-qr", cx);
    let by_code = bounds(harness.window, "link-method-code", cx);
    // Two choices of the same size, side by side across the form.
    assert_eq!(by_scan.size, by_code.size);
    assert!(by_scan.right() <= by_code.left());
    assert!(within(by_scan, card) && within(by_code, card));
    assert!(by_scan.size.width + by_code.size.width > card.size.width * 0.8);
    assert!(shows(harness.window, "link-method-qr-chosen", cx));
    assert!(!shows(harness.window, "link-phone", cx));

    // The whole form is in sight at the usual size, with either way.
    let fits = |cx: &mut TestAppContext| {
        let card = bounds(harness.window, "add-number", cx);
        let last = bounds(harness.window, "link-history", cx);
        let button = bounds(harness.window, "link-submit", cx);
        assert!(last.bottom() <= button.top(), "{last:?} under {button:?}");
        assert!(within(button, card) && within(last, card));
    };
    fits(cx);
    click(harness.window, "link-method-code", cx);
    fits(cx);
    assert!(shows(harness.window, "link-method-code-chosen", cx));
    assert!(!shows(harness.window, "link-method-qr-chosen", cx));
    assert!(shows(harness.window, "link-phone", cx));
    // The longest form still fits its card, at every size.
    for scale in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = scale));
        let card = bounds(harness.window, "add-number", cx);
        for part in ["link-method", "link-phone", "link-submit"] {
            let part_bounds = bounds(harness.window, part, cx);
            assert!(
                within(part_bounds, card),
                "{scale}%: {part} {part_bounds:?} outside {card:?}"
            );
        }
        assert!(
            card.bottom() <= px(800.) && card.top() >= px(0.),
            "{scale}%"
        );
    }
}

#[gpui_kit::test]
fn the_qr_code_is_left_for_a_code_and_taken_up_again_on_the_same_number(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let new = start_link(&harness, cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    let card = bounds(harness.window, "add-number", cx);

    // The way to a code is on the scan step, across the card.
    let offer = bounds(harness.window, "link-use-code", cx);
    assert!(offer.size.width > card.size.width * 0.8);
    click(harness.window, "link-use-code", cx);
    assert!(shows(harness.window, "link-phone-step", cx));
    assert!(!shows(harness.window, "link-qr-frame", cx));
    // The number starts with the country of the place it connects from,
    // and the field has the keyboard.
    assert_eq!(typed_phone(&harness, cx), "+58 ");
    assert_eq!(harness.mock.pairing_calls(), 0, "nothing asked yet");

    // Not a number yet: said, and nothing asked of the provider.
    type_text(harness.window, "412", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert!(flow_error(&harness, cx).contains("too short"));
    assert!(shows(harness.window, "link-error", cx));
    assert!(shows(harness.window, "link-phone-step", cx));
    assert_eq!(harness.mock.pairing_calls(), 0);

    // The whole number, and Enter: the code, for the same number.
    type_text(harness.window, " 123 4567", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.pairing_calls(), 1);
    assert_eq!(harness.mock.create_calls(), 1, "no second number");
    assert_eq!(stored_accounts(&harness).len(), 3);
    assert!(harness.mock.deleted_accounts().is_empty());
    assert!(!shows(harness.window, "link-phone-step", cx));
    assert!(!shows(harness.window, "link-error", cx));
    assert!(matches!(
        step(&harness, cx),
        LinkStep::TypeCode { code, .. } if code == "WUAP-1234"
    ));
    let code = bounds(harness.window, "link-code", cx);
    let card = bounds(harness.window, "add-number", cx);
    for part in [
        "link-code",
        "link-code-for",
        "link-code-life",
        "link-code-copy",
        "link-code-steps",
        "link-use-qr",
        "link-cancel",
    ] {
        let part_bounds = bounds(harness.window, part, cx);
        assert!(
            within(part_bounds, card),
            "{part} {part_bounds:?} outside {card:?}"
        );
    }
    assert!(near(centre_x(code), centre_x(card)));
    // Large: each of its eight characters has a box of its own.
    assert!(code.size.width > theme::px(8. * 34.));

    // Copied as it is typed.
    click(harness.window, "link-code-copy", cx);
    assert_eq!(clipboard(cx), "WUAP-1234");
    cx.update(|cx| assert!(harness.shell.read(cx).linking.as_ref().unwrap().copied));
    cx.executor().advance_clock(crate::motion::link::COPIED);
    cx.run_until_parked();
    cx.update(|cx| assert!(!harness.shell.read(cx).linking.as_ref().unwrap().copied));

    // It runs out: said, with the way to another, which is another code
    // for the same number.
    harness.mock.expire_code(&new);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-code-renew", cx));
    assert!(!shows(harness.window, "link-code-copy", cx));
    assert_eq!(bounds(harness.window, "link-code", cx), code);
    click(harness.window, "link-code-renew", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.pairing_calls(), 2);
    assert!(matches!(
        step(&harness, cx),
        LinkStep::TypeCode { code, .. } if code == "WUAP-1235"
    ));
    assert!(shows(harness.window, "link-code-copy", cx));

    // Back to the QR code: the same number, nothing created or deleted.
    click(harness.window, "link-use-qr", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-qr", cx));
    assert!(!shows(harness.window, "link-code", cx));
    assert_eq!(harness.mock.create_calls(), 1);
    assert_eq!(stored_accounts(&harness).len(), 3);
    assert!(harness.mock.deleted_accounts().is_empty());
    // And it links.
    harness.mock.complete_link(&new);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-done", cx));
}

#[gpui_kit::test]
fn the_code_waits_in_boxes_of_the_size_it_will_have(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    click(harness.window, "link-method-code", cx);
    click(harness.window, "link-phone", cx);
    type_text(harness.window, "+56 9 5550 1234", cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);

    // Linking with a code from the form: empty boxes, not a QR code.
    assert_eq!(step(&harness, cx), LinkStep::Starting);
    assert!(!shows(harness.window, "link-qr-frame", cx));
    let waiting = bounds(harness.window, "link-code-skeleton", cx);
    let life = bounds(harness.window, "link-code-life", cx);
    link_round(&harness, cx);
    assert_eq!(bounds(harness.window, "link-code", cx), waiting);
    assert_eq!(
        bounds(harness.window, "link-code-life", cx).origin,
        life.origin
    );
    // The number it is for is said.
    assert!(shows(harness.window, "link-code-for", cx));
}

#[gpui_kit::test]
fn a_code_that_is_refused_is_said_and_can_be_asked_for_again(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    start_link(&harness, cx);
    link_round(&harness, cx);
    click(harness.window, "link-use-code", cx);
    type_text(harness.window, "412 123 4567", cx);

    harness.mock.fail_next_pairing_codes([
        ProviderError::RateLimited {
            retry_after: Some(Duration::from_secs(30)),
        },
        rejected("already_linked", "The account is already linked."),
        ProviderError::Transient("eof".into()),
        rejected("invalid_phone", "That does not look like a phone number."),
    ]);
    for said in [
        "Too many requests. Try again in 30 seconds.",
        "The account is already linked.",
        "Could not reach the provider. Try again.",
        "That does not look like a phone number.",
    ] {
        click(harness.window, "link-get-code", cx);
        harness.settle(cx);
        assert_eq!(flow_error(&harness, cx), said);
        // Never a dead end: the number, the button and the way back.
        assert!(shows(harness.window, "link-error", cx));
        assert!(shows(harness.window, "link-code-phone", cx));
        assert!(shows(harness.window, "link-get-code", cx));
        assert!(shows(harness.window, "link-use-qr", cx));
        let card = bounds(harness.window, "add-number", cx);
        assert!(within(bounds(harness.window, "link-error", cx), card));
    }
    assert_eq!(harness.mock.pairing_calls(), 4);
    assert_eq!(typed_phone(&harness, cx), "+58 412 123 4567", "kept");

    // The same button, once more.
    click(harness.window, "link-get-code", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-code", cx));
    assert!(!shows(harness.window, "link-error", cx));
    assert_eq!(harness.mock.create_calls(), 1);

    // Another code that is refused: the code that is there stays, with
    // the refusal and the button under it.
    let new = stored_accounts(&harness).last().unwrap().id.clone();
    harness.mock.expire_code(&new);
    link_round(&harness, cx);
    harness
        .mock
        .fail_next_pairing_codes([ProviderError::RateLimited { retry_after: None }]);
    click(harness.window, "link-code-renew", cx);
    harness.settle(cx);
    assert_eq!(
        flow_error(&harness, cx),
        "Too many requests. Wait a moment and try again."
    );
    assert!(shows(harness.window, "link-code-renew", cx));
    click(harness.window, "link-code-renew", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-code-copy", cx));
    assert!(!shows(harness.window, "link-error", cx));
}

#[gpui_kit::test]
fn typing_the_number_is_left_without_asking_the_provider_anything(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    start_link(&harness, cx);
    link_round(&harness, cx);
    let code = bounds(harness.window, "link-qr", cx);

    click(harness.window, "link-use-code", cx);
    click(harness.window, "link-use-qr", cx);
    assert_eq!(bounds(harness.window, "link-qr", cx), code);
    assert_eq!(harness.mock.pairing_calls(), 0);

    // Escape leaves the number being typed, not the linking.
    click(harness.window, "link-use-code", cx);
    assert!(shows(harness.window, "link-phone-step", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "link-phone-step", cx));
    assert!(!shows(harness.window, "link-confirm-cancel", cx));
    assert!(shows(harness.window, "link-qr", cx));
    // And Escape from there asks, as it always did.
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "link-confirm-cancel", cx));
    press(harness.window, "escape", cx);
    assert_eq!(harness.mock.pairing_calls(), 0);

    // The phone scans the code while the number is being typed: linked.
    click(harness.window, "link-use-code", cx);
    let new = stored_accounts(&harness).last().unwrap().id.clone();
    harness.mock.complete_link(&new);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-done", cx));
    assert!(!shows(harness.window, "link-phone-step", cx));
}

#[gpui_kit::test]
fn the_keyboard_reaches_the_code_and_the_way_back(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    start_link(&harness, cx);
    link_round(&harness, cx);

    // Tab to "Link with phone number instead", Enter.
    for _ in 0..6 {
        press(harness.window, "tab", cx);
        press(harness.window, "enter", cx);
        if shows(harness.window, "link-phone-step", cx) {
            break;
        }
        // Enter landed on a way out: back to the wait.
        if shows(harness.window, "link-confirm-cancel", cx) {
            press(harness.window, "escape", cx);
        }
    }
    assert!(shows(harness.window, "link-phone-step", cx));
    // The field has the keyboard at once.
    type_text(harness.window, "412 123 4567", cx);
    assert_eq!(typed_phone(&harness, cx), "+58 412 123 4567");
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-code", cx));

    // Tab to "Use QR code instead", Enter. Copying on the way is no harm.
    for _ in 0..6 {
        press(harness.window, "tab", cx);
        press(harness.window, "enter", cx);
        harness.settle(cx);
        if shows(harness.window, "link-qr", cx) {
            break;
        }
        if shows(harness.window, "link-confirm-cancel", cx) {
            press(harness.window, "escape", cx);
        }
    }
    assert!(shows(harness.window, "link-qr", cx));
    assert_eq!(harness.mock.create_calls(), 1);
    assert!(harness.mock.deleted_accounts().is_empty());
}

// ----- what the provider cannot do --------------------------------------------

#[gpui_kit::test]
fn a_provider_without_codes_offers_only_the_qr_code(cx: &mut TestAppContext) {
    let harness = open_limited(
        cx,
        Capabilities {
            link_by_code: false,
            link_back_to_scan: false,
            ..Capabilities::all()
        },
    );
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "link-method", cx));
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-qr-skeleton", cx));
    assert!(!shows(harness.window, "link-use-code", cx));
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    assert!(!shows(harness.window, "link-use-code", cx));
    // Not from the code either.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            assert!(!shell.linking.as_ref().unwrap().ask_phone);
            cx.notify();
        })
    });
    assert_eq!(harness.mock.pairing_calls(), 0);
}

#[gpui_kit::test]
fn a_provider_with_no_way_back_says_so_before_the_code_is_asked_for(cx: &mut TestAppContext) {
    let harness = open_limited(
        cx,
        Capabilities {
            link_back_to_scan: false,
            ..Capabilities::all()
        },
    );
    start_link(&harness, cx);
    link_round(&harness, cx);
    // While the number is typed the QR code is still there to go back to.
    click(harness.window, "link-use-code", cx);
    assert!(shows(harness.window, "link-use-qr", cx));
    click(harness.window, "link-use-qr", cx);
    assert!(shows(harness.window, "link-qr", cx));

    click(harness.window, "link-use-code", cx);
    type_text(harness.window, "412 123 4567", cx);
    click(harness.window, "link-get-code", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-code", cx));
    // Once it is a code, the provider has no way back: none is offered,
    // and the number still links, or is cancelled as before.
    assert!(!shows(harness.window, "link-use-qr", cx));
    assert!(shows(harness.window, "link-cancel", cx));
    let new = stored_accounts(&harness).last().unwrap().id.clone();
    harness.mock.complete_link(&new);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-done", cx));
}

#[gpui_kit::test]
fn a_number_linked_before_is_asked_for_with_its_own_number(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let first = stored_accounts(&harness)[0].clone();
    harness
        .runtime
        .block_on(harness.engine.unlink_account(&first.id))
        .unwrap();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.reload_accounts(cx);
        })
    });
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.reconnect_number(first.id.clone(), window, cx);
        })
    })
    .unwrap();
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    click(harness.window, "link-use-code", cx);
    // Its own number, whole: Enter is enough.
    let own = first.phone.unwrap();
    assert_eq!(
        typed_phone(&harness, cx).replace(' ', ""),
        own.replace(' ', "")
    );
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-code", cx));
    assert_eq!(stored_accounts(&harness).len(), 2);
}
