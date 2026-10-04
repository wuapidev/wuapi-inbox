//! The count and the status light of a rail item are cut out of its ring:
//! drawn above the ring and the face, on a rim of the rail's background,
//! so the ring stops before them and nothing runs behind the digits.

use super::*;
use crate::theme::{metrics, Palette};
use crate::ui::rail::KNOCKOUT;

#[test]
fn the_ring_is_under_the_face_and_the_count_and_light_are_on_top() {
    use gpui_kit::div;
    let names = |badge: bool, light: bool| -> Vec<&'static str> {
        Shell::rail_layers(div(), div(), badge.then(div), light.then(div))
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    };
    assert_eq!(names(true, true), ["ring", "face", "badge", "light"]);
    assert_eq!(names(false, true), ["ring", "face", "light"]);
    assert_eq!(names(true, false), ["ring", "face", "badge"]);
    // Opaque, on a rim of the rail's own background, in both themes.
    for palette in [Palette::light(), Palette::dark()] {
        for muted in [false, true] {
            let (fill, ink, rim) = Shell::rail_badge_colours(muted, &palette);
            assert_eq!(
                fill.a, 1.,
                "{:?}: a count's fill is opaque",
                palette.appearance
            );
            assert_eq!(rim, palette.background);
            assert_eq!(rim.a, 1.);
            assert_ne!(fill, ink);
        }
    }
}

#[gpui_kit::test]
fn a_selected_numbers_count_and_light_stand_clear_of_its_ring(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 1);
    let accounts = stored_accounts(&harness);
    let selected = accounts[0].id.clone();
    cx.update(|cx| {
        assert_eq!(harness.shell.read(cx).account.as_ref(), Some(&selected));
    });
    let chat = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    let chat = client_provider::ChatId::new(chat);

    let count = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .unread
                .get(&selected)
                .copied()
                .unwrap_or(0)
        })
    };
    let mut widths = Vec::new();
    // A short count and one that is written "99+".
    for wanted in [3u32, 250] {
        click(harness.window, "list-menu", cx);
        click(harness.window, "menu-read-all", cx);
        harness.settle(cx);
        harness.mock.set_unread(&selected, &chat, wanted);
        harness.runtime.block_on(harness.engine.refresh()).unwrap();
        cx.run_until_parked();
        let unread = count(cx);
        assert_eq!(unread, wanted);

        for step in theme::SCALE_STEPS {
            cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
            cx.run_until_parked();
            let what = format!("{wanted} at {step}%");
            let cell = bounds(harness.window, "rail-account-0", cx);
            let ring = bounds(harness.window, "rail-ring-0", cx);
            let badge = bounds_of(harness.window, &format!("rail-badge-{unread}"), cx);
            let fill = bounds_of(harness.window, &format!("rail-badge-fill-{unread}"), cx);
            let light = bounds(harness.window, "rail-led-a0", cx);
            let rim = KNOCKOUT();

            // The ring is the cell's outline, and the count and the light
            // sit on it: they are what covers it there.
            assert_eq!(ring, cell, "{what}");
            assert!(intersects(badge, ring) && intersects(light, ring), "{what}");
            // The count's fill is inside a rim of the same width all round.
            assert!(within(fill, badge), "{what}");
            for gap in [
                fill.left() - badge.left(),
                badge.right() - fill.right(),
                fill.top() - badge.top(),
                badge.bottom() - fill.bottom(),
            ] {
                assert!((gap - rim).abs() <= px(0.5), "{what}: a rim of {gap:?}");
            }
            assert!(rim >= px(1.5), "{what}: the rim is {rim:?}");
            // Anchored to the top right corner, inside the rail, and
            // never over the middle of the face.
            assert!(badge.right() <= metrics::RAIL_WIDTH(), "{what}: {badge:?}");
            assert!(
                badge.right() > cell.right() && badge.top() < cell.top(),
                "{what}"
            );
            assert!(
                !badge.contains(&cell.center()),
                "{what}: over the face's middle"
            );
            assert!(badge.bottom() < cell.center().y, "{what}");
            // The light: the bottom right corner, the same rim, clear of
            // the count.
            assert_eq!(light.size.width, light.size.height, "{what}");
            assert!(light.bottom() >= cell.bottom() - px(4.), "{what}");
            assert!(light.right() <= metrics::RAIL_WIDTH(), "{what}");
            assert!(!intersects(light, badge), "{what}");
            if step == 100 {
                widths.push((badge.size.width, badge.right() - cell.right()));
            }
        }
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
        cx.run_until_parked();
    }
    // A longer count is a pill that grew to the left: the same right edge.
    assert!(widths[1].0 > widths[0].0 + px(6.), "{widths:?}");
    assert_eq!(widths[1].1, widths[0].1, "{widths:?}");
}
