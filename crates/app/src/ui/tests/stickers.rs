//! The sticker and GIF picker in the real window: its tabs, its keyboard,
//! what it builds, what it sends, what it keeps, and what it asks of the
//! network (nothing, unless the user turned the online search on).

use super::media_out::{jpeg, wait};
use super::*;
use crate::keys::Command;
use crate::settings::PickerTab;
use crate::ui::emoji_picker::Zone;
use crate::ui::picker::{Source, Tab};
use client_core::fixtures::{animated_gif, animated_webp, still_webp};
use client_core::{content_id, LibraryItem, LibraryKind, LibrarySource};
use client_provider::{MediaKind, MessageContent, MessageId, ProviderEvent};
use std::rc::Rc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn open_chat_one(cx: &mut TestAppContext) -> Harness {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    // Whether files can be sent is asked of the provider.
    harness.settle(cx);
    harness
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn tab(harness: &Harness, cx: &mut TestAppContext) -> Tab {
    cx.update(|cx| harness.shell.read(cx).picker.tab)
}

fn picker_tab_setting(cx: &mut TestAppContext) -> PickerTab {
    cx.update(|cx| settings::get(cx).picker_tab)
}

/// Lets the engine's background work run until `done` says so; unlike
/// `wait`, `done` may read the window.
fn wait_for(
    harness: &Harness,
    cx: &mut TestAppContext,
    what: &str,
    done: impl Fn(&mut TestAppContext) -> bool,
) {
    for _ in 0..400 {
        harness.settle(cx);
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("never: {what}");
}

fn mp4(tag: u8) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 24];
    bytes.extend_from_slice(b"ftypisom");
    bytes.extend_from_slice(&[tag; 64]);
    bytes
}

/// A still sticker of a distinct size: the same id never twice.
fn still(side: u32) -> Vec<u8> {
    still_webp(side, side)
}

fn save_sticker(harness: &Harness, bytes: Vec<u8>, favorite: bool) -> LibraryItem {
    let (item, _) = harness
        .engine
        .library_save_sticker(bytes, "image/webp", LibrarySource::Received, favorite)
        .unwrap();
    item
}

fn save_gif(harness: &Harness, bytes: Vec<u8>, name: &str) -> LibraryItem {
    harness
        .engine
        .library_import_gif(bytes, Some(name.to_owned()), LibrarySource::Imported)
        .unwrap()
}

fn titles(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| harness.shell.read(cx).picker.grid().titles())
}

fn tile_ids(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .tiles
            .iter()
            .map(|tile| tile.key())
            .collect()
    })
}

fn cursor(harness: &Harness, cx: &mut TestAppContext) -> (usize, Zone) {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        (shell.picker.grid().cursor, shell.emoji.zone)
    })
}

fn tile_name(index: usize) -> &'static str {
    Box::leak(format!("picker-tile-{index}").into_boxed_str())
}

fn newest_sent(harness: &Harness) -> client_provider::OutgoingMessage {
    harness.mock.sent().pop().expect("something was sent")
}

/// Runs the outbox until the mock has been asked to send.
fn flush(harness: &Harness) {
    harness.runtime.block_on(async {
        harness.engine.flush_outbox().await.unwrap();
    });
}

/// The composer, then Ctrl+Shift+E: the picker on its Stickers tab.
fn open_stickers(harness: &Harness, cx: &mut TestAppContext) {
    focus_composer(harness, cx);
    press(harness.window, "ctrl-shift-e", cx);
    cx.run_until_parked();
    assert_eq!(overlay(harness, cx), Overlay::EmojiPicker);
    assert_eq!(tab(harness, cx), Tab::Stickers);
}

fn open_gifs(harness: &Harness, cx: &mut TestAppContext) {
    focus_composer(harness, cx);
    press(harness.window, "ctrl-shift-j", cx);
    cx.run_until_parked();
    assert_eq!(overlay(harness, cx), Overlay::EmojiPicker);
    assert_eq!(tab(harness, cx), Tab::Gifs);
}

// ----- the tabs ------------------------------------------------------------------------

#[gpui_kit::test]
fn the_button_opens_on_the_tab_used_last_and_the_shortcuts_open_directly(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    focus_composer(&harness, cx);

    // The first time, the button opens the emoji, with the three tabs over
    // the search.
    click(harness.window, "emoji", cx);
    assert_eq!(tab(&harness, cx), Tab::Emoji);
    let picker = bounds(harness.window, "emoji-picker", cx);
    for selector in [
        "picker-tab-0",
        "picker-tab-1",
        "picker-tab-2",
        "picker-tabs",
    ] {
        assert!(
            within(bounds(harness.window, selector, cx), picker),
            "{selector}"
        );
    }
    assert!(
        shows(harness.window, "emoji-grid", cx),
        "the emoji picker is in it"
    );
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // Shift+E is the stickers, Shift+J the GIFs; each again closes it, and
    // from one tab the other's key changes tab.
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-shift-e", cx);
    assert_eq!(
        (overlay(&harness, cx), tab(&harness, cx)),
        (Overlay::EmojiPicker, Tab::Stickers)
    );
    assert!(shows(harness.window, "picker-grid", cx));
    assert!(!shows(harness.window, "emoji-grid", cx));
    press(harness.window, "ctrl-shift-j", cx);
    assert_eq!(
        (overlay(&harness, cx), tab(&harness, cx)),
        (Overlay::EmojiPicker, Tab::Gifs)
    );
    press(harness.window, "ctrl-e", cx);
    assert_eq!(
        (overlay(&harness, cx), tab(&harness, cx)),
        (Overlay::EmojiPicker, Tab::Emoji)
    );
    assert!(shows(harness.window, "emoji-grid", cx));
    press(harness.window, "ctrl-shift-j", cx);
    assert_eq!(tab(&harness, cx), Tab::Gifs);
    press(harness.window, "ctrl-shift-j", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // The tab last used is where the button opens it next, and that is
    // remembered for the next start.
    assert_eq!(picker_tab_setting(cx), PickerTab::Gifs);
    click(harness.window, "emoji", cx);
    assert_eq!(
        (overlay(&harness, cx), tab(&harness, cx)),
        (Overlay::EmojiPicker, Tab::Gifs)
    );
    // A click on a tab changes it.
    click(harness.window, "picker-tab-2", cx);
    assert_eq!(tab(&harness, cx), Tab::Stickers);
    assert_eq!(picker_tab_setting(cx), PickerTab::Stickers);
    click(harness.window, "picker-tab-0", cx);
    assert_eq!(tab(&harness, cx), Tab::Emoji);
    assert!(shows(harness.window, "emoji-grid", cx));

    // Ctrl+E is the emoji, whatever tab was last used.
    press(harness.window, "escape", cx);
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-e", cx);
    assert_eq!(tab(&harness, cx), Tab::Emoji);
}

#[gpui_kit::test]
fn the_shortcuts_are_in_the_registry_and_the_palette(cx: &mut TestAppContext) {
    for command in [
        Command::StickerPicker,
        Command::GifPicker,
        Command::FavoriteSticker,
        Command::SaveSticker,
        Command::SaveGif,
        Command::ImportSticker,
        Command::ImportGif,
        Command::SettingsStickers,
    ] {
        let binding = crate::keys::binding(command).expect("registered");
        assert!(!binding.label.is_empty(), "{command:?}");
    }
    assert!(crate::keys::keys_label(Command::StickerPicker).is_some());
    assert!(crate::keys::keys_label(Command::GifPicker).is_some());
    let harness = open_chat_one(cx);
    // The palette lists them by name and runs them.
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "Stickers", cx);
    let rows: Vec<String> = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .filter_map(|item| match item {
                crate::ui::palette::Item::Command(command) => {
                    Some(crate::keys::label(*command).to_owned())
                }
                _ => None,
            })
            .collect()
    });
    assert!(rows.iter().any(|title| title == "Stickers"), "{rows:?}");
}

// ----- the stickers ------------------------------------------------------------------------------

#[gpui_kit::test]
fn the_stickers_tab_lists_recent_favorites_and_the_packs(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let a = save_sticker(&harness, still(40), false);
    let b = save_sticker(&harness, still(48), true);
    let c = save_sticker(&harness, still(56), false);
    harness
        .engine
        .store()
        .library_touch(&c.id, client_provider::Timestamp::now())
        .unwrap();
    // Two of them are a pack of their own.
    let pack = harness
        .engine
        .store()
        .library_pack("p1", "Cats", client_provider::Timestamp::now())
        .unwrap();
    harness
        .engine
        .store()
        .library_set_pack(&[a.id.clone(), c.id.clone()], Some(&pack.id))
        .unwrap();

    open_stickers(&harness, cx);
    assert_eq!(
        titles(&harness, cx),
        [
            "Recently used",
            "Favorites",
            "Cats",
            "All stickers",
            "From your chats"
        ]
    );
    // Recent: c. Favorites: b. Cats: a, c (newest first). All: b, the one
    // that is in no pack.
    let ids = tile_ids(&harness, cx);
    assert_eq!(ids[0], c.id);
    assert_eq!(ids[1], b.id);
    assert!(ids.contains(&a.id));
    assert_eq!(ids.len(), 1 + 1 + 2 + 1);
    // They are drawn, with their sections' headings.
    assert!(shows(harness.window, "picker-tile-0", cx));
    assert!(shows(harness.window, "picker-heading-0", cx));
    // The picker sits where the emoji picker does, above its button.
    let picker = bounds(harness.window, "emoji-picker", cx);
    let button = bounds(harness.window, "emoji", cx);
    assert!(picker.bottom() <= button.top(), "{picker:?} {button:?}");
}

#[gpui_kit::test]
fn favorites_say_when_the_phones_are_not_available_with_this_provider(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let mock = provider_mock::MockProvider::quiet();
    mock.set_sticker_favorites_available(false);
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    save_sticker(&harness, still(40), true);
    open_stickers(&harness, cx);
    let notes: Vec<String> = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
            .collect()
    });
    assert!(
        notes
            .iter()
            .any(|note| note.contains("not available yet from this provider")
                && note.contains("you star here are kept as favorites")),
        "{notes:?}"
    );
    assert!(shows(harness.window, "picker-note-1", cx));
    // The local favorite works all the same; the section with nothing
    // in it comes after those with something.
    assert_eq!(
        titles(&harness, cx),
        ["Favorites", "All stickers", "From your chats"]
    );
    assert!(
        harness.mock.sticker_calls().is_empty(),
        "the provider is never asked"
    );
}

#[gpui_kit::test]
fn a_provider_with_favorites_syncs_them_into_the_library_in_the_background(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    let account = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .account_id
            .clone()
    });
    let webp = still(44);
    harness
        .mock
        .star_sticker_on_phone(&account, "phone-1", webp.clone(), "image/webp");
    open_stickers(&harness, cx);
    wait(&harness, cx, "the phone's favorite arrives", || {
        harness
            .engine
            .store()
            .library_item(&content_id(&webp))
            .unwrap()
            .is_some_and(|item| item.favorite)
    });
    cx.run_until_parked();
    // It is in the open picker, under Favorites.
    assert!(tile_ids(&harness, cx).contains(&content_id(&webp)));
    let item = harness
        .engine
        .store()
        .library_item(&content_id(&webp))
        .unwrap()
        .unwrap();
    assert_eq!(item.source, LibrarySource::Phone);
}

#[gpui_kit::test]
fn a_click_sends_the_sticker_at_once_with_no_caption(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let item = save_sticker(&harness, still(40), false);
    open_stickers(&harness, cx);
    // Only a section's first tile is in view first: the sticker is under
    // "All stickers".
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    click(harness.window, tile_name(index), cx);
    wait(&harness, cx, "the sticker is queued", || {
        !harness.engine.store().outbox_pending().unwrap().is_empty()
    });

    // It is in the conversation already, waiting to be sent, and the
    // picker closed.
    assert_eq!(overlay(&harness, cx), Overlay::None);
    let media = newest_media(&harness, cx);
    let (kind, status, caption) = media.last().unwrap().clone();
    assert_eq!(
        (kind, status, caption),
        (MediaKind::Sticker, DeliveryStatus::Pending, None)
    );
    flush(&harness);
    let sent = newest_sent(&harness);
    assert!(matches!(
        sent.content,
        OutgoingContent::Media {
            kind: MediaKind::Sticker,
            gif: false,
            caption: None,
            ..
        }
    ));
    assert_eq!(harness.mock.uploaded().last().unwrap().1, "image/webp");
    // It is the first of the recent ones now.
    let recent = harness
        .engine
        .store()
        .library_recent(LibraryKind::Sticker, 5)
        .unwrap();
    assert_eq!(recent[0].id, item.id);
}

#[gpui_kit::test]
fn a_sticker_sent_while_replying_is_that_reply(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let item = save_sticker(&harness, still(40), true);
    focused_reply_target(&harness, cx);
    open_stickers(&harness, cx);
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    click(harness.window, tile_name(index), cx);
    wait(&harness, cx, "queued", || {
        !harness.engine.store().outbox_pending().unwrap().is_empty()
    });
    flush(&harness);
    let sent = newest_sent(&harness);
    assert_eq!(sent.reply_to, Some(MessageId::new("quoted")));
    // The reply is over.
    let compose = cx.update(|cx| {
        matches!(
            harness.shell.read(cx).acting.compose,
            crate::ui::message_actions::Compose::New
        )
    });
    assert!(compose);
}

/// A message to answer, and the composer set to answer it.
fn focused_reply_target(harness: &Harness, cx: &mut TestAppContext) {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let message = client_provider::Message {
        id: MessageId::new("quoted"),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: client_provider::ContactId::new("them"),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: client_provider::Timestamp::from_millis(
            client_provider::Timestamp::now().as_millis() + 90_000,
        ),
        content: MessageContent::text("a question"),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: Default::default(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message.clone()))
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            shell.acting.compose = crate::ui::message_actions::Compose::Reply(Box::new(message));
        })
    });
}

#[gpui_kit::test]
fn when_files_cannot_be_sent_the_tab_says_so_instead_of_failing_after_the_click(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| prepare(cx, None));
    let mock = provider_mock::MockProvider::quiet();
    mock.set_uploads_available(false);
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    let item = save_sticker(&harness, still(40), false);
    open_stickers(&harness, cx);
    // Said in the strip, before anything is clicked.
    assert!(shows(harness.window, "picker-said", cx));
    let said = cx.update(|cx| harness.shell.read(cx).picker_cannot_send());
    assert!(said.unwrap().contains("not available"));
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    click(harness.window, tile_name(index), cx);
    harness.settle(cx);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    assert_eq!(
        overlay(&harness, cx),
        Overlay::EmojiPicker,
        "nothing was sent, nothing closed"
    );
}

// ----- the keyboard --------------------------------------------------------------------------------

#[gpui_kit::test]
fn tab_walks_search_tabs_and_grid_and_the_arrows_walk_the_grid(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let items: Vec<LibraryItem> = (0..9)
        .map(|n| save_sticker(&harness, still(20 + n), false))
        .collect();
    open_stickers(&harness, cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);

    // Search, tabs, grid, and around.
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Tabs);
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Grid);
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);
    press(harness.window, "shift-tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Grid);

    // Four to a row, a heading between sections: Down is the same column
    // one row on, Left and Right one tile.
    let (start, _) = cursor(&harness, cx);
    press(harness.window, "right", cx);
    assert_eq!(cursor(&harness, cx).0, start + 1);
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx).0, start + 1 + 4);
    press(harness.window, "left", cx);
    press(harness.window, "up", cx);
    assert_eq!(cursor(&harness, cx).0, start);
    // Up from the first row is the search.
    press(harness.window, "up", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);

    // From the tabs, the arrows pick a tab and Enter changes to it.
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Tabs);
    press(harness.window, "left", cx);
    press(harness.window, "left", cx);
    press(harness.window, "enter", cx);
    assert_eq!(
        tab(&harness, cx),
        Tab::Emoji,
        "left, left from Stickers is Emoji"
    );
    assert!(shows(harness.window, "emoji-grid", cx));
    press(harness.window, "right", cx);
    press(harness.window, "enter", cx);
    assert_eq!(tab(&harness, cx), Tab::Gifs);
    let _ = items;
}

#[gpui_kit::test]
fn enter_sends_the_tile_the_keyboard_is_on_and_shift_enter_keeps_the_picker_open(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    save_sticker(&harness, still(40), false);
    save_sticker(&harness, still(48), false);
    open_stickers(&harness, cx);
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Grid);
    press(harness.window, "shift-enter", cx);
    wait(&harness, cx, "queued", || {
        harness.engine.store().outbox_pending().unwrap().len() == 1
    });
    assert_eq!(
        overlay(&harness, cx),
        Overlay::EmojiPicker,
        "Shift keeps it open"
    );
    press(harness.window, "right", cx);
    press(harness.window, "enter", cx);
    wait(&harness, cx, "queued again", || {
        harness.engine.store().outbox_pending().unwrap().len() == 2
    });
    assert_eq!(
        overlay(&harness, cx),
        Overlay::None,
        "Enter sends and closes"
    );
}

#[gpui_kit::test]
fn only_the_rows_in_view_are_built(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    for n in 0..120 {
        save_sticker(&harness, still(16 + n), false);
    }
    open_stickers(&harness, cx);
    let rows = cx.update(|cx| harness.shell.read(cx).picker.grid().rows.len());
    assert!(rows > 30, "{rows} rows");
    let built = cx.update(|cx| harness.shell.read(cx).picker.built_rows());
    assert!(built.start == 0 && built.end < 12, "{built:?} of {rows}");
    // Further down, others are: the first is no longer.
    for _ in 0..6 {
        press(harness.window, "pagedown", cx);
    }
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    for _ in 0..40 {
        press(harness.window, "down", cx);
    }
    let built = cx.update(|cx| harness.shell.read(cx).picker.built_rows());
    assert!(built.start > 0 && built.len() < 12, "{built:?}");
}

#[gpui_kit::test]
fn the_picker_stays_inside_its_popover_at_every_interface_size(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    for n in 0..30 {
        save_sticker(&harness, still(20 + n), n % 3 == 0);
    }
    save_gif(
        &harness,
        mp4(1),
        "a long name for a saved gif that must be cut off",
    );
    for step in crate::theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        for (key, selectors) in [
            (
                "ctrl-shift-e",
                [
                    "picker-tabs",
                    "emoji-search",
                    "picker-bar",
                    "picker-grid",
                    "picker-strip",
                ],
            ),
            (
                "ctrl-shift-j",
                [
                    "picker-tabs",
                    "emoji-search",
                    "picker-bar",
                    "picker-grid",
                    "picker-strip",
                ],
            ),
        ] {
            focus_composer(&harness, cx);
            press(harness.window, key, cx);
            let picker = bounds(harness.window, "emoji-picker", cx);
            let window = Bounds {
                origin: gpui_kit::point(px(0.), px(0.)),
                size: gpui_kit::size(px(1240.), px(800.)),
            };
            assert!(within(picker, window), "{step}%: {picker:?}");
            for selector in selectors {
                assert!(
                    within(bounds(harness.window, selector, cx), picker),
                    "{step}% {key}: {selector}"
                );
            }
            // The tiles in view are inside the grid.
            let grid = bounds(harness.window, "picker-grid", cx);
            let tile = bounds(harness.window, "picker-tile-0", cx);
            assert!(
                tile.left() >= grid.left() && tile.right() <= grid.right(),
                "{step}%"
            );
            press(harness.window, "escape", cx);
        }
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
}

// ----- starring and arranging ------------------------------------------------------------------

#[gpui_kit::test]
fn a_key_and_a_menu_star_a_sticker_and_the_phone_hears_of_it(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let account = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .account_id
            .clone()
    });
    let a = save_sticker(&harness, still(40), false);
    let b = save_sticker(&harness, still(48), false);
    open_stickers(&harness, cx);
    harness.settle(cx);
    // The grid is under "All stickers". The keyboard goes to the first
    // tile and the key stars it.
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    let at = cursor(&harness, cx).0;
    let target = tile_ids(&harness, cx)[at].clone();
    assert!([a.id.clone(), b.id.clone()].contains(&target));
    press(harness.window, "ctrl-d", cx);
    assert!(
        harness
            .engine
            .store()
            .library_item(&target)
            .unwrap()
            .unwrap()
            .favorite
    );
    wait(&harness, cx, "the phone is told", || {
        harness
            .mock
            .favorite_sticker_ids(&account)
            .contains(&target)
    });
    // It is in Favorites now, and the key takes the star off again.
    assert_eq!(titles(&harness, cx)[0], "Favorites");
    let at = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == target)
        .unwrap();
    click(harness.window, tile_name(at), cx);
    // (A click sends; the picker is closed. Open it again.)
    open_stickers(&harness, cx);
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    press(harness.window, "ctrl-d", cx);
    wait(&harness, cx, "the phone is told again", || {
        !harness
            .mock
            .favorite_sticker_ids(&account)
            .contains(&target)
    });
    assert!(
        !harness
            .engine
            .store()
            .library_item(&target)
            .unwrap()
            .unwrap()
            .favorite
    );

    // The menu: Alt+Down on a tile offers what can be done to it.
    press(harness.window, "alt-down", cx);
    assert!(shows(harness.window, "picker-menu", cx));
    assert!(shows(harness.window, "picker-action-0", cx));
    // Send, Add to favorites, Remove: Down twice is the favorite.
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    let tiles = tile_ids(&harness, cx);
    let at = cursor(&harness, cx).0;
    let chosen = tiles[at].clone();
    assert!(
        harness
            .engine
            .store()
            .library_item(&chosen)
            .unwrap()
            .unwrap()
            .favorite
    );
    let _ = a;

    // A favorite is moved with its menu, and a right click opens it too.
    let other = tile_ids(&harness, cx)
        .into_iter()
        .find(|id| *id != chosen)
        .unwrap();
    harness.engine.set_library_favorite(&other, true).unwrap();
    cx.run_until_parked();
    let favorites: Vec<String> = harness
        .engine
        .store()
        .library_favorites(LibraryKind::Sticker)
        .unwrap()
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(favorites, vec![chosen.clone(), other.clone()]);
    harness.engine.move_library_favorite(&other, 0).unwrap();
    let favorites: Vec<String> = harness
        .engine
        .store()
        .library_favorites(LibraryKind::Sticker)
        .unwrap()
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(favorites, vec![other.clone(), chosen.clone()]);

    // Alt with the arrows does the same from the keyboard, and the
    // keyboard goes with the sticker.
    open_stickers(&harness, cx);
    let at = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == chosen)
        .unwrap();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.emoji.zone = Zone::Grid;
            shell.picker_cursor_to_for_test(at, cx)
        })
    });
    press(harness.window, "alt-left", cx);
    let favorites: Vec<String> = harness
        .engine
        .store()
        .library_favorites(LibraryKind::Sticker)
        .unwrap()
        .into_iter()
        .map(|item| item.id)
        .collect();
    assert_eq!(favorites, vec![chosen.clone(), other]);
    let (at, _) = cursor(&harness, cx);
    assert_eq!(
        tile_ids(&harness, cx)[at],
        chosen,
        "the keyboard went with it"
    );
}

#[gpui_kit::test]
fn a_right_click_opens_the_menu_of_a_tile(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    save_sticker(&harness, still(40), true);
    open_stickers(&harness, cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let at = visual
        .debug_bounds("picker-tile-1")
        .expect("a tile")
        .center();
    visual.simulate_event(gpui_kit::MouseDownEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui_kit::MouseButton::Right,
        click_count: 1,
        first_mouse: false,
    });
    visual.run_until_parked();
    assert!(shows(harness.window, "picker-menu", cx));
    // Escape closes the menu before it closes the picker.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "picker-menu", cx));
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
}

// ----- moving ------------------------------------------------------------------------------------

fn motion_on(cx: &mut TestAppContext) {
    cx.update(|cx| settings::update(cx, |settings| settings.motion = MotionChoice::On));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn only_the_tile_under_the_pointer_or_the_keyboard_moves(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    motion_on(cx);
    let moving = save_sticker(&harness, animated_webp(48, 48, &[60, 60, 60]), false);
    let other = save_sticker(&harness, animated_webp(52, 52, &[60, 60]), false);
    open_stickers(&harness, cx);
    harness.settle(cx);
    let key = |harness: &Harness, cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).picker.moving_key())
    };
    // Nothing moves while nothing is looked at.
    assert_eq!(key(&harness, cx), None);

    // The pointer on a tile: that one, after its frames are decoded.
    let ids = tile_ids(&harness, cx);
    let at = ids.iter().position(|id| *id == moving.id).unwrap();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let center = visual.debug_bounds(tile_name(at)).unwrap().center();
    visual.simulate_mouse_move(center, None, Modifiers::none());
    visual.run_until_parked();
    wait_for(&harness, cx, "its frames are decoded", |cx| {
        cx.update(|cx| harness.shell.read(cx).picker.moving_key())
            .is_some()
    });
    assert_eq!(key(&harness, cx), Some(moving.id.clone()));

    // The pointer moves to another: the first stops, the other starts,
    // never two at once.
    let at = ids.iter().position(|id| *id == other.id).unwrap();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let center = visual.debug_bounds(tile_name(at)).unwrap().center();
    visual.simulate_mouse_move(center, None, Modifiers::none());
    visual.run_until_parked();
    wait_for(&harness, cx, "the other one", |cx| {
        cx.update(|cx| harness.shell.read(cx).picker.moving_key()) == Some(other.id.clone())
    });

    // Off the grid: none.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(gpui_kit::point(px(2.), px(2.)), None, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(key(&harness, cx), None);

    // The keyboard: the tile it is on moves.
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    wait_for(&harness, cx, "the keyboard's tile", |cx| {
        cx.update(|cx| harness.shell.read(cx).picker.moving_key())
            .is_some()
    });
}

#[gpui_kit::test]
fn nothing_moves_with_the_setting_off_or_under_reduced_motion(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    motion_on(cx);
    save_sticker(&harness, animated_webp(48, 48, &[60, 60, 60]), false);
    open_stickers(&harness, cx);
    let hover_first = |harness: &Harness, cx: &mut TestAppContext| {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        let center = visual.debug_bounds("picker-tile-0").unwrap().center();
        visual.simulate_mouse_move(center, None, Modifiers::none());
        visual.run_until_parked();
        harness.settle(cx);
        cx.update(|cx| harness.shell.read(cx).picker.moving_key())
    };
    cx.update(|cx| settings::update(cx, |s| s.sticker_hover_animation = false));
    cx.run_until_parked();
    assert_eq!(hover_first(&harness, cx), None, "the setting is off");
    cx.update(|cx| {
        settings::update(cx, |s| {
            s.sticker_hover_animation = true;
            s.motion = MotionChoice::Off;
        })
    });
    cx.run_until_parked();
    // Away and back, so that it is a new hover.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(gpui_kit::point(px(2.), px(2.)), None, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(hover_first(&harness, cx), None, "reduced motion");
}

// ----- GIFs ---------------------------------------------------------------------------------------

#[gpui_kit::test]
fn the_gifs_tab_lists_saved_gifs_and_sends_one_as_a_flagged_video(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let wave = save_gif(&harness, mp4(1), "wave");
    let still_gif = save_gif(&harness, animated_gif(40, 30, 3, 80), "dance");
    open_gifs(&harness, cx);
    let ids = tile_ids(&harness, cx);
    assert_eq!(ids.len(), 2);
    assert!(shows(harness.window, "picker-tile-0", cx));
    assert!(shows(harness.window, "picker-source-saved", cx));

    // The search filters by name.
    type_text(harness.window, "wav", cx);
    assert_eq!(tile_ids(&harness, cx), vec![wave.id.clone()]);
    press(harness.window, "escape", cx);
    assert_eq!(
        tile_ids(&harness, cx).len(),
        2,
        "Escape empties the search first"
    );
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);

    // An MP4 sent from the picker is a video that plays as a GIF.
    let at = ids.iter().position(|id| *id == wave.id).unwrap();
    click(harness.window, tile_name(at), cx);
    wait(&harness, cx, "queued", || {
        !harness.engine.store().outbox_pending().unwrap().is_empty()
    });
    let (kind, _, _) = newest_media(&harness, cx).last().unwrap().clone();
    assert_eq!(kind, MediaKind::Video);
    flush(&harness);
    assert!(matches!(
        newest_sent(&harness).content,
        OutgoingContent::Media {
            kind: MediaKind::Video,
            gif: true,
            ..
        }
    ));

    // A .gif file goes as the image it is.
    open_gifs(&harness, cx);
    let ids = tile_ids(&harness, cx);
    let at = ids.iter().position(|id| *id == still_gif.id).unwrap();
    click(harness.window, tile_name(at), cx);
    wait(&harness, cx, "queued", || {
        !harness.engine.store().outbox_pending().unwrap().is_empty()
    });
    flush(&harness);
    assert!(matches!(
        newest_sent(&harness).content,
        OutgoingContent::Media {
            kind: MediaKind::Image,
            gif: false,
            ..
        }
    ));
}

#[gpui_kit::test]
fn an_mp4_has_a_labelled_tile_because_there_is_no_video_decoder(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let item = save_gif(&harness, mp4(3), "wave");
    assert!(!item.has_thumb);
    open_gifs(&harness, cx);
    let name = cx.update(|cx| harness.shell.read(cx).picker.current().map(|t| t.key()));
    assert_eq!(name, Some(item.id));
    assert!(shows(harness.window, "picker-tile-0", cx));
}

fn giphy_fixture(server_uri: &str) -> String {
    format!(
        r#"{{"data":[
            {{"id":"g1","title":"Cat waves","images":{{
                "original_mp4":{{"mp4":"{uri}/media/g1.mp4","width":"480","height":"270","mp4_size":"400"}},
                "fixed_width_small_still":{{"url":"{uri}/media/g1-still.gif"}}}}}}
        ]}}"#,
        uri = server_uri
    )
}

#[gpui_kit::test]
fn the_online_search_makes_no_request_until_it_is_turned_on_with_a_key(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let server = harness.runtime.block_on(MockServer::start());
    harness.runtime.block_on(async {
        Mock::given(path("/v1/gifs/search"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(giphy_fixture(&server.uri()), "application/json"),
            )
            .mount(&server)
            .await;
        Mock::given(path("/v1/gifs/trending"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(giphy_fixture(&server.uri()), "application/json"),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/media/g1.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(mp4(9), "video/mp4"))
            .mount(&server)
            .await;
        Mock::given(path("/media/g1-still.gif"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(animated_gif(20, 20, 2, 80), "image/gif"),
            )
            .mount(&server)
            .await;
    });
    cx.update(|cx| {
        harness
            .shell
            .update(cx, |shell, _| shell.picker.services.base = server.uri())
    });
    let requests = |harness: &Harness| {
        harness
            .runtime
            .block_on(server.received_requests())
            .unwrap()
            .len()
    };

    // Off (the default): the Giphy tab explains, and typing asks nobody.
    assert!(!cx.update(|cx| settings::get(cx).gif_online));
    open_gifs(&harness, cx);
    click(harness.window, "picker-source-online", cx);
    // (A click leaves the keyboard on the popover until a key is pressed.)
    press(harness.window, "up", cx);
    harness.settle(cx);
    type_text(harness.window, "cat", cx);
    cx.executor()
        .advance_clock(crate::ui::picker::ONLINE_DEBOUNCE * 2);
    harness.settle(cx);
    assert_eq!(requests(&harness), 0, "off means no request at all");
    let note = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .find_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
    });
    assert!(note.unwrap().contains("Online search is off"));

    // On, without a key: still nothing, and it says what is missing.
    cx.update(|cx| settings::update(cx, |s| s.gif_online = true));
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);
    open_gifs(&harness, cx);
    click(harness.window, "picker-source-online", cx);
    press(harness.window, "up", cx);
    type_text(harness.window, "cat", cx);
    cx.executor()
        .advance_clock(crate::ui::picker::ONLINE_DEBOUNCE * 2);
    harness.settle(cx);
    assert_eq!(requests(&harness), 0, "no key, no request");

    // On, with a key: the search goes to the service, with the key and the
    // words, and its results are on the tab.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            shell.picker.services.secrets.set("my-own-key").unwrap();
            shell.picker.services.forget_key();
        })
    });
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);
    open_gifs(&harness, cx);
    click(harness.window, "picker-source-online", cx);
    press(harness.window, "up", cx);
    for _ in 0..4 {
        cx.executor()
            .advance_clock(crate::ui::picker::ONLINE_DEBOUNCE * 2);
        harness.settle(cx);
    }
    // Nothing typed: the trending ones.
    let asked: Vec<String> = harness
        .runtime
        .block_on(server.received_requests())
        .unwrap()
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(
        asked
            .iter()
            .any(|url| url.contains("/v1/gifs/trending") && url.contains("api_key=my-own-key")),
        "{asked:?}"
    );
    assert_eq!(tile_ids(&harness, cx), vec!["online:g1".to_owned()]);
    type_text(harness.window, "cat", cx);
    for _ in 0..4 {
        cx.executor()
            .advance_clock(crate::ui::picker::ONLINE_DEBOUNCE * 2);
        harness.settle(cx);
    }
    let asked: Vec<String> = harness
        .runtime
        .block_on(server.received_requests())
        .unwrap()
        .iter()
        .map(|request| request.url.to_string())
        .collect();
    assert!(
        asked
            .iter()
            .any(|url| url.contains("/v1/gifs/search") && url.contains("q=cat")),
        "{asked:?}"
    );

    // Picking one downloads its MP4, keeps it in the library and sends it
    // as a GIF.
    click(harness.window, "picker-tile-0", cx);
    wait(&harness, cx, "the GIF is queued", || {
        !harness.engine.store().outbox_pending().unwrap().is_empty()
    });
    flush(&harness);
    assert!(matches!(
        newest_sent(&harness).content,
        OutgoingContent::Media {
            kind: MediaKind::Video,
            gif: true,
            ..
        }
    ));
    let saved = harness
        .engine
        .store()
        .library_items(LibraryKind::Gif)
        .unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].source, LibrarySource::Online);
    assert_eq!(saved[0].name.as_deref(), Some("Cat waves"));
}

// ----- keeping what is in a conversation ----------------------------------------------------------

fn focus_newest_message(harness: &Harness, cx: &mut TestAppContext) {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.focus_newest(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_received_sticker_is_kept_and_starred_from_its_menu_or_its_key(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let webp = still(36);
    let url = "https://media.example/sticker-1";
    harness.mock.set_media(url, webp.clone(), "image/webp");
    push_media(&harness, cx, "st1", MediaKind::Sticker, Some(url), false);
    focus_newest_message(&harness, cx);

    // Its menu has the entry, among the others.
    press(harness.window, "m", cx);
    let entries: Vec<&'static str> = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .message_menu_items()
            .iter()
            .map(|item| item.id)
            .collect()
    });
    assert!(entries.contains(&"message-save-sticker"), "{entries:?}");
    assert!(!entries.contains(&"message-save-gif"));
    press(harness.window, "escape", cx);

    // V keeps it, as its original, starred.
    focus_newest_message(&harness, cx);
    press(harness.window, "v", cx);
    wait(&harness, cx, "kept", || {
        harness
            .engine
            .store()
            .library_item(&content_id(&webp))
            .unwrap()
            .is_some()
    });
    let item = harness
        .engine
        .store()
        .library_item(&content_id(&webp))
        .unwrap()
        .unwrap();
    assert!(item.favorite);
    assert_eq!(item.source, LibrarySource::Received);
    assert_eq!(
        harness
            .engine
            .store()
            .library_file(&item.id)
            .unwrap()
            .unwrap()
            .0,
        webp
    );
    let said = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .leaving
            .notice
            .as_ref()
            .map(|n| n.text.to_string())
    });
    assert_eq!(said.as_deref(), Some("Added to your favorite stickers"));
}

#[gpui_kit::test]
fn the_star_on_a_sticker_keeps_it_from_where_it_is(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let webp = still(34);
    let url = "https://media.example/sticker-2";
    harness.mock.set_media(url, webp.clone(), "image/webp");
    push_media(&harness, cx, "st2", MediaKind::Sticker, Some(url), false);
    cache_thumbnail(&harness, cx, url, (34, 34));
    // The buttons are in its corner while the pointer is on it.
    let frame = bounds(harness.window, "media-box", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(frame.center(), None, Modifiers::none());
    visual.run_until_parked();
    cx.update_window(harness.window.into(), |_, window, _| window.refresh())
        .unwrap();
    cx.run_until_parked();
    assert!(
        shows(harness.window, "picture-favorite", cx),
        "the button is on the sticker"
    );
    click(harness.window, "picture-favorite", cx);
    wait(&harness, cx, "kept", || {
        harness
            .engine
            .store()
            .library_item(&content_id(&webp))
            .unwrap()
            .is_some_and(|item| item.favorite)
    });
}

#[gpui_kit::test]
fn a_gif_of_a_conversation_is_saved_and_a_plain_video_still_can_be(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let video = mp4(5);
    let url = "https://media.example/clip-1";
    harness.mock.set_media(url, video.clone(), "video/mp4");
    push_media(&harness, cx, "clip", MediaKind::Video, Some(url), false);
    focus_newest_message(&harness, cx);
    press(harness.window, "g", cx);
    wait(&harness, cx, "saved", || {
        harness
            .engine
            .store()
            .library_item(&content_id(&video))
            .unwrap()
            .is_some()
    });
    let item = harness
        .engine
        .store()
        .library_item(&content_id(&video))
        .unwrap()
        .unwrap();
    assert_eq!(item.kind, LibraryKind::Gif);
    assert!(!item.favorite);
    // A sticker has no GIF entry and a document neither.
    let gifs = harness
        .engine
        .store()
        .library_items(LibraryKind::Gif)
        .unwrap();
    assert_eq!(gifs.len(), 1);
}

// ----- from disk ----------------------------------------------------------------------------------

#[gpui_kit::test]
fn pictures_from_disk_become_512_stickers_in_a_pack_and_gifs_are_checked(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("Holidays");
    std::fs::create_dir_all(&folder).unwrap();
    let one = folder.join("beach.jpg");
    let two = folder.join("sunset.jpg");
    let junk = folder.join("notes.txt");
    std::fs::write(&one, jpeg(700, 300)).unwrap();
    std::fs::write(&two, jpeg(320, 480)).unwrap();
    std::fs::write(&junk, b"not a picture").unwrap();
    let answer = Rc::new(std::cell::RefCell::new(vec![
        one.clone(),
        two.clone(),
        junk.clone(),
    ]));
    let given = answer.clone();
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            pick_files: Some(Rc::new(move |_, _| {
                gpui_kit::Task::ready(Some(given.borrow().clone()))
            })),
            ..Default::default()
        },
    );
    harness.settle(cx);

    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "Make a sticker from a picture", cx);
    press(harness.window, "enter", cx);
    wait(&harness, cx, "imported", || {
        harness
            .engine
            .store()
            .library_items(LibraryKind::Sticker)
            .unwrap()
            .len()
            == 2
    });
    let items = harness
        .engine
        .store()
        .library_items(LibraryKind::Sticker)
        .unwrap();
    assert!(items
        .iter()
        .all(|item| item.size == Some((512, 512)) && item.mime == "image/webp"));
    assert!(items.iter().all(|item| item.bytes <= 100 * 1024));
    // Two at once are a pack, named for their folder.
    let packs = harness.engine.store().library_packs().unwrap();
    assert_eq!(packs.len(), 1);
    assert_eq!(packs[0].name, "Holidays");
    assert!(items
        .iter()
        .all(|item| item.pack.as_deref() == Some(packs[0].id.as_str())));
    // The one that was not a picture is said, with the others done.
    let problem = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .problem
            .clone()
            .map(|p| p.to_string())
    });
    assert!(problem.unwrap().contains("notes.txt"));

    // GIFs: a .gif and an MP4 are taken, anything else is not.
    let gif = folder.join("loop.gif");
    let clip = folder.join("clip.mp4");
    std::fs::write(&gif, animated_gif(30, 20, 3, 80)).unwrap();
    std::fs::write(&clip, mp4(2)).unwrap();
    *answer.borrow_mut() = vec![gif, clip, one];
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "Add GIFs from files", cx);
    press(harness.window, "enter", cx);
    wait(&harness, cx, "gifs imported", || {
        harness
            .engine
            .store()
            .library_items(LibraryKind::Gif)
            .unwrap()
            .len()
            == 2
    });
    let gifs = harness
        .engine
        .store()
        .library_items(LibraryKind::Gif)
        .unwrap();
    assert!(gifs.iter().any(|item| item.mime == "image/gif"));
    assert!(gifs.iter().any(|item| item.mime == "video/mp4"));
}

// ----- settings -----------------------------------------------------------------------------------

#[gpui_kit::test]
fn the_settings_section_clears_the_library_and_keeps_the_key_in_the_keychain_alone(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    harness.settle(cx);
    let kept = save_sticker(&harness, still(40), true);
    let plain = save_sticker(&harness, still(48), false);
    save_gif(&harness, mp4(1), "wave");

    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "Settings: Stickers and GIFs", cx);
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Settings);
    assert!(shows(harness.window, "stickers-summary", cx));
    let panel = bounds(harness.window, "settings-panel", cx);
    for step in crate::theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |s| s.interface_scale = step));
        cx.run_until_parked();
        let panel = bounds(harness.window, "settings-panel", cx);
        for selector in [
            "stickers-summary",
            "stickers-clear",
            "stickers-key-row",
            "stickers-online",
        ] {
            assert!(
                within(bounds(harness.window, selector, cx), panel),
                "{step}%: {selector}"
            );
        }
    }
    cx.update(|cx| settings::update(cx, |s| s.interface_scale = 100));
    let _ = panel;

    // Off until the user turns it on; the switch is the only way.
    assert!(!cx.update(|cx| settings::get(cx).gif_online));
    click(harness.window, "stickers-online", cx);
    assert!(cx.update(|cx| settings::get(cx).gif_online));

    // The key goes to the keychain's store and nowhere else: not into the
    // settings file, which holds nothing secret.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.library.key_input.update(cx, |field, cx| {
                field.set_value("top-secret-key", window, cx)
            });
        })
    })
    .unwrap();
    click(harness.window, "stickers-key-save", cx);
    let stored = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .services
            .secrets
            .get()
            .unwrap()
    });
    assert_eq!(stored.as_deref(), Some("top-secret-key"));
    let on_disk = std::fs::read_to_string(&file).unwrap();
    assert!(!on_disk.contains("top-secret-key"), "{on_disk}");
    assert!(on_disk.contains("gif_online"));
    click(harness.window, "stickers-key-remove", cx);
    assert_eq!(
        cx.update(|cx| harness
            .shell
            .read(cx)
            .picker
            .services
            .secrets
            .get()
            .unwrap()),
        None
    );

    // Clear asks twice; favorites stay unless everything is cleared.
    click(harness.window, "stickers-clear-keep", cx);
    assert!(
        harness
            .engine
            .store()
            .library_item(&plain.id)
            .unwrap()
            .is_some(),
        "asked, not done"
    );
    click(harness.window, "stickers-clear-keep", cx);
    assert!(harness
        .engine
        .store()
        .library_item(&plain.id)
        .unwrap()
        .is_none());
    assert!(harness
        .engine
        .store()
        .library_item(&kept.id)
        .unwrap()
        .is_some());
    click(harness.window, "stickers-clear-all", cx);
    click(harness.window, "stickers-clear-all", cx);
    assert_eq!(harness.engine.store().library_stats().unwrap().items, 0);

    // The animate switch is saved.
    assert!(cx.update(|cx| settings::get(cx).sticker_hover_animation));
    click(harness.window, "stickers-animate", cx);
    assert!(!cx.update(|cx| settings::get(cx).sticker_hover_animation));
    let _ = Source::Saved;
}

// ----- one send for one request ---------------------------------------------------------------

fn pending(harness: &Harness) -> Vec<client_provider::OutgoingMessage> {
    harness
        .engine
        .store()
        .outbox_pending()
        .unwrap()
        .into_iter()
        .map(|entry| entry.message)
        .collect()
}

/// Asks for a tile to be sent `times`, at once, before anything else runs.
fn send_tile(harness: &Harness, index: usize, times: usize, cx: &mut TestAppContext) {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            for _ in 0..times {
                shell.picker_send(index, true, window, cx);
            }
        })
    })
    .unwrap();
}

fn settle_well(harness: &Harness, cx: &mut TestAppContext) {
    for _ in 0..20 {
        harness.settle(cx);
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui_kit::test]
fn a_tile_asked_for_twice_before_it_is_queued_is_sent_once(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let item = save_sticker(&harness, still(40), false);
    open_stickers(&harness, cx);
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    // A double click, or Enter held down: the second lands while the
    // first is still being prepared.
    send_tile(&harness, index, 2, cx);
    wait(&harness, cx, "queued", || !pending(&harness).is_empty());
    settle_well(&harness, cx);
    assert_eq!(pending(&harness).len(), 1, "one request, one message");

    // Once it is queued the same tile can be sent again.
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    send_tile(&harness, index, 1, cx);
    wait(&harness, cx, "queued again", || {
        pending(&harness).len() == 2
    });
    settle_well(&harness, cx);
    assert_eq!(pending(&harness).len(), 2);
}

#[gpui_kit::test]
fn an_online_gif_clicked_twice_is_downloaded_and_sent_once(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let server = harness.runtime.block_on(MockServer::start());
    harness.runtime.block_on(async {
        for route in ["/v1/gifs/search", "/v1/gifs/trending"] {
            Mock::given(path(route))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_raw(giphy_fixture(&server.uri()), "application/json"),
                )
                .mount(&server)
                .await;
        }
        Mock::given(method("GET"))
            .and(path("/media/g1.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(mp4(9), "video/mp4"))
            .mount(&server)
            .await;
        Mock::given(path("/media/g1-still.gif"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(animated_gif(20, 20, 2, 80), "image/gif"),
            )
            .mount(&server)
            .await;
    });
    cx.update(|cx| {
        settings::update(cx, |s| s.gif_online = true);
        harness.shell.update(cx, |shell, _| {
            shell.picker.services.base = server.uri();
            shell.picker.services.secrets.set("my-own-key").unwrap();
            shell.picker.services.forget_key();
        })
    });
    open_gifs(&harness, cx);
    click(harness.window, "picker-source-online", cx);
    press(harness.window, "up", cx);
    wait_for(&harness, cx, "the online GIFs are listed", |cx| {
        cx.executor()
            .advance_clock(crate::ui::picker::ONLINE_DEBOUNCE * 2);
        tile_ids(&harness, cx) == vec!["online:g1".to_owned()]
    });

    send_tile(&harness, 0, 2, cx);
    wait(&harness, cx, "the GIF is queued", || {
        !pending(&harness).is_empty()
    });
    settle_well(&harness, cx);
    assert_eq!(pending(&harness).len(), 1, "one request, one message");
    let downloads = harness
        .runtime
        .block_on(server.received_requests())
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/media/g1.mp4")
        .count();
    assert_eq!(downloads, 1, "and one download");
}

#[gpui_kit::test]
fn a_sticker_queued_after_the_chat_changed_leaves_the_reply_started_there_alone(
    cx: &mut TestAppContext,
) {
    use crate::ui::message_actions::Compose;
    let harness = open_chat_one(cx);
    let item = save_sticker(&harness, still(40), false);
    let first = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let store = harness.engine.store().clone();
    // Another chat with a message to answer.
    let (other, answered) = store
        .chats(&first.account_id, None)
        .unwrap()
        .into_iter()
        .filter(|chat| chat.id != first.id)
        .find_map(|chat| {
            let newest = store.messages(&first.account_id, &chat.id, 1).ok()?.pop()?;
            Some((chat.id, newest.message))
        })
        .expect("another chat with messages");
    open_stickers(&harness, cx);
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    // The sticker is asked for, and before it is queued the user is in
    // another chat, answering a message there.
    let reply = answered.clone();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.picker_send(index, false, window, cx);
            shell.open_chat(other.clone(), Some(window), cx);
            shell.acting.compose = Compose::Reply(Box::new(reply));
        })
    })
    .unwrap();
    wait(&harness, cx, "queued", || !pending(&harness).is_empty());
    settle_well(&harness, cx);
    // It went where it was asked for, once, and the reply is still there.
    let sent = pending(&harness);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].chat_id, first.id);
    assert_eq!(sent[0].reply_to, None);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.open.as_ref().unwrap().chat.id, other);
        assert!(
            matches!(&shell.acting.compose, Compose::Reply(message) if message.id == answered.id),
            "the reply under way in the other chat was cleared"
        );
    });
}

#[gpui_kit::test]
fn a_sticker_still_being_prepared_is_not_sent_once_files_are_being_attached(
    cx: &mut TestAppContext,
) {
    use crate::ui::attach::AttachDraft;
    use crate::ui::message_actions::Compose;
    let harness = open_chat_one(cx);
    let item = save_sticker(&harness, still(40), false);
    focused_reply_target(&harness, cx);
    open_stickers(&harness, cx);
    let index = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    // The sheet opens (a drop, a paste) before the sticker is queued.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.picker_send(index, true, window, cx);
            shell.attach = Some(AttachDraft {
                items: Vec::new(),
                selected: 0,
                notes: Vec::new(),
                confirm_discard: false,
            });
        })
    })
    .unwrap();
    settle_well(&harness, cx);
    assert!(pending(&harness).is_empty(), "nothing goes out meanwhile");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(
            matches!(&shell.acting.compose, Compose::Reply(message) if message.id.as_str() == "quoted"),
            "the reply is the sheet's to send"
        );
        assert!(shell.problem.is_some(), "and it is said");
    });

    // With the sheet gone, the tile is not held back for ever.
    cx.update(|cx| harness.shell.update(cx, |shell, _| shell.attach = None));
    send_tile(&harness, index, 1, cx);
    wait(&harness, cx, "queued", || pending(&harness).len() == 1);
}

// ----- taking a favorite out ---------------------------------------------------------------------

#[gpui_kit::test]
fn a_favorite_removed_from_its_menu_loses_its_star_on_the_phone_and_stays_out(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    let account = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .account_id
            .clone()
    });
    let item = save_sticker(&harness, still(40), true);
    wait(&harness, cx, "the phone has the star", || {
        harness
            .mock
            .favorite_sticker_ids(&account)
            .contains(&item.id)
    });
    open_stickers(&harness, cx);
    let at = tile_ids(&harness, cx)
        .iter()
        .position(|id| *id == item.id)
        .unwrap();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.emoji.zone = Zone::Grid;
            shell.picker_cursor_to_for_test(at, cx)
        })
    });
    // Its menu: the last entry takes it out of the library.
    press(harness.window, "alt-down", cx);
    press(harness.window, "up", cx);
    press(harness.window, "enter", cx);
    let store = harness.engine.store().clone();
    assert!(store.library_item(&item.id).unwrap().is_none());
    wait(&harness, cx, "the phone is told", || {
        harness.mock.favorite_sticker_ids(&account).is_empty()
    });
    // Asking the phone again does not bring it back.
    harness
        .runtime
        .block_on(harness.engine.reconcile_favorites(&account))
        .unwrap();
    assert!(store.library_item(&item.id).unwrap().is_none());
    assert!(!tile_ids(&harness, cx).contains(&item.id));
}

// ----- the stickers the account already has in its chats ----------------------------------------------

/// A sticker message in the open chat, at `ts`, sent by the account (from
/// its phone, say) or received, with its thumbnail cached as the engine
/// does once it has fetched it. `side` tells one picture from another.
fn chat_sticker(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    ts: i64,
    outgoing: bool,
    side: Option<u32>,
) -> String {
    use client_provider::{ContactId, Media, MediaRef, Message};
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let url = format!("https://media.example/chat-{id}");
    let mut media = Media::new(MediaKind::Sticker);
    media.source = Some(MediaRef::new(&url));
    media.mime_type = Some("image/webp".into());
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(id),
            client_id: None,
            account_id: chat.account_id.clone(),
            chat_id: chat.id.clone(),
            sender: ContactId::new(if outgoing { "me" } else { "them" }),
            sender_name: None,
            direction: if outgoing {
                Direction::Outgoing
            } else {
                Direction::Incoming
            },
            timestamp: client_provider::Timestamp::from_millis(ts),
            content: MessageContent::Media(media),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        }))
        .unwrap();
    if let Some(side) = side {
        let store = harness.engine.store();
        let now = client_provider::Timestamp::now();
        let thumb = client_core::CachedMedia {
            bytes: png(side, side),
            mime: Some("image/png".into()),
            size: Some((side, side)),
        };
        let still = client_core::CachedMedia {
            bytes: Vec::new(),
            mime: None,
            size: None,
        };
        store
            .put_media(&client_core::thumbnail_key(&url), &thumb, now, 1 << 40)
            .unwrap();
        store
            .put_media(&client_core::animation_key(&url), &still, now, 1 << 40)
            .unwrap();
    }
    cx.run_until_parked();
    url
}

/// The demo world has stickers of its own in its chats; these tests are
/// about the ones they put there, so the first pass is taken as made over
/// what was there.
fn skip_seeded_history(harness: &Harness) {
    let newest = harness.engine.store().newest_message_pk().unwrap();
    harness
        .engine
        .store()
        .library_set_index_state(client_core::IndexState {
            high: newest,
            low: 1,
            done: true,
        })
        .unwrap();
}

fn index_chats(harness: &Harness) {
    for _ in 0..100 {
        if !harness.engine.index_stickers().unwrap() {
            return;
        }
    }
    panic!("the indexer never finished");
}

fn grid_rows(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .map(|row| match row {
                crate::ui::picker::Row::Header(_) => "header".to_owned(),
                crate::ui::picker::Row::Note(_) => "note".to_owned(),
                crate::ui::picker::Row::Tiles(range) => format!("tiles:{}", range.len()),
            })
            .collect()
    })
}

#[gpui_kit::test]
fn with_stickers_in_the_chats_the_tab_shows_them_in_its_first_rows(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    // Sent (one from the phone, as far as the store can tell) and
    // received, the same picture twice among them.
    let base = 1_700_000_000_000;
    chat_sticker(&harness, cx, "o1", base + 10, true, Some(30));
    chat_sticker(&harness, cx, "o2", base + 30, true, Some(31));
    chat_sticker(&harness, cx, "i1", base + 20, false, Some(40));
    chat_sticker(&harness, cx, "i2", base + 25, false, Some(40));
    chat_sticker(&harness, cx, "i3", base + 15, false, Some(41));
    index_chats(&harness);

    open_stickers(&harness, cx);
    // Sections with something come first; the empty one is last.
    assert_eq!(
        titles(&harness, cx),
        ["Recently used", "From your chats", "Favorites"]
    );
    assert_eq!(
        grid_rows(&harness, cx),
        ["header", "tiles:2", "header", "tiles:2", "header", "note"],
        "two sent, two received (one picture twice), nothing starred"
    );
    // The grid starts right under the first heading.
    let grid = bounds(harness.window, "picker-grid", cx);
    let heading = bounds(harness.window, "picker-heading-0", cx);
    let tile = bounds(harness.window, "picker-tile-0", cx);
    assert!(heading.top() - grid.top() < px(12.), "{heading:?} {grid:?}");
    assert!(
        tile.top() - heading.bottom() < px(12.),
        "the first tile is under its heading: {tile:?} {heading:?}"
    );
    assert!(within(tile, grid));
    // Newest sent first.
    let recent = harness
        .engine
        .store()
        .library_recent(LibraryKind::Sticker, 5);
    let recent = recent.unwrap();
    assert_eq!(recent[0].sent_at.unwrap().as_millis(), base + 30);
    assert_eq!(tile_ids(&harness, cx)[0], recent[0].id);
}

#[gpui_kit::test]
fn a_sticker_from_the_chats_is_sent_at_once_counts_as_used_and_a_star_keeps_it(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    let base = 1_700_000_000_000;
    chat_sticker(&harness, cx, "i1", base, false, Some(44));
    chat_sticker(&harness, cx, "i2", base + 5, false, Some(45));
    index_chats(&harness);
    open_stickers(&harness, cx);
    assert_eq!(titles(&harness, cx), ["From your chats", "Favorites"]);

    // Star the first with the key: it is a favorite, in its own section.
    press(harness.window, "down", cx);
    let first = tile_ids(&harness, cx)[0].clone();
    press(harness.window, "ctrl-d", cx);
    assert!(
        harness
            .engine
            .store()
            .library_item(&first)
            .unwrap()
            .unwrap()
            .favorite
    );
    assert_eq!(titles(&harness, cx), ["Favorites", "From your chats"]);

    // A click on the other sends it, at once, as a sticker.
    let other = tile_ids(&harness, cx)[1].clone();
    click(harness.window, tile_name(1), cx);
    wait(&harness, cx, "queued", || {
        harness.engine.store().outbox_pending().unwrap().len() == 1
    });
    flush(&harness);
    assert!(matches!(
        newest_sent(&harness).content,
        client_provider::OutgoingContent::Media {
            kind: MediaKind::Sticker,
            ..
        }
    ));
    let used = harness
        .engine
        .store()
        .library_item(&other)
        .unwrap()
        .unwrap();
    assert!(used.last_used.is_some(), "it counts as recently used");
    assert!(used.indexed && !used.favorite);
}

#[gpui_kit::test]
fn a_sticker_not_downloaded_yet_is_a_place_that_is_fetched_when_it_is_in_view(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    let url = chat_sticker(&harness, cx, "far", 1_700_000_000_000, false, None);
    let webp = still(48);
    harness.mock.set_media(&url, webp, "image/webp");
    let (chat, unread) = cx.update(|cx| {
        let open = harness.shell.read(cx).open.as_ref().unwrap();
        (open.chat.clone(), open.chat.unread_count)
    });
    index_chats(&harness);
    let calls = harness.mock.media_calls();
    let read_before = harness.mock.marked_read().len();

    // Opening the tab draws the place, which asks for its file.
    open_stickers(&harness, cx);
    assert!(shows(harness.window, "picker-place", cx));
    wait_for(&harness, cx, "fetched", |_| {
        harness.mock.media_calls() == calls + 1
    });
    // Looked at again and again: still one download.
    for _ in 0..5 {
        harness.settle(cx);
    }
    assert_eq!(harness.mock.media_calls(), calls + 1);
    // When it is here the place is the sticker. The download is read on
    // another thread before it is kept.
    wait_for(&harness, cx, "kept", |_| {
        let key = client_core::thumbnail_key(&url);
        matches!(harness.engine.store().media_size(&key), Ok(Some(_)))
    });
    index_chats(&harness);
    harness.settle(cx);
    cx.run_until_parked();
    assert!(!shows(harness.window, "picker-place", cx));
    let item = harness.engine.store().library_heard(1).unwrap().remove(0);
    assert!(!item.pending() && item.has_thumb);

    // Nothing was read: not here, not on the phone.
    assert_eq!(harness.mock.marked_read().len(), read_before);
    let after = harness
        .engine
        .store()
        .chat(&chat.account_id, &chat.id)
        .unwrap()
        .unwrap()
        .unread_count;
    assert_eq!(after, unread);
}

#[gpui_kit::test]
fn with_downloads_off_a_place_waits_for_a_click(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    cx.update(|cx| settings::update(cx, |s| s.media = settings::MediaChoice::Never));
    skip_seeded_history(&harness);
    let url = chat_sticker(&harness, cx, "far", 1_700_000_000_000, false, None);
    harness.mock.set_media(&url, still(48), "image/webp");
    index_chats(&harness);
    let calls = harness.mock.media_calls();
    open_stickers(&harness, cx);
    for _ in 0..5 {
        harness.settle(cx);
    }
    assert_eq!(harness.mock.media_calls(), calls, "the policy is kept");
    assert!(shows(harness.window, "picker-place", cx));
    click(harness.window, tile_name(0), cx);
    wait_for(&harness, cx, "fetched on the click", |_| {
        harness.mock.media_calls() == calls + 1
    });
    assert!(
        harness.engine.store().outbox_pending().unwrap().is_empty(),
        "a click on a place fetches, it sends nothing"
    );
}

#[gpui_kit::test]
fn only_the_rows_in_view_are_built_with_many_stickers_from_the_chats(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    for n in 0..150u32 {
        chat_sticker(
            &harness,
            cx,
            &format!("m{n}"),
            1_700_000_000_000 + n as i64,
            false,
            Some(10 + n),
        );
    }
    index_chats(&harness);
    open_stickers(&harness, cx);
    let rows = cx.update(|cx| harness.shell.read(cx).picker.grid().rows.len());
    assert!(rows > 30, "{rows} rows");
    let built = cx.update(|cx| harness.shell.read(cx).picker.built_rows());
    assert!(
        built.start == 0 && !built.is_empty() && built.end < 14,
        "{built:?} of {rows}"
    );
}

// ----- nothing is cut off, nothing is blank -----------------------------------------------------------

/// Every text of the tab that is on screen, by selector.
fn texts_on_show(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    let built = cx.update(|cx| harness.shell.read(cx).picker.built_rows());
    let rows: Vec<String> = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let grid = shell.picker.grid();
        built
            .clone()
            .filter_map(|row| match grid.rows.get(row)? {
                crate::ui::picker::Row::Note(_) => Some(format!("picker-note-{row}")),
                crate::ui::picker::Row::Header(section) => {
                    Some(format!("picker-heading-{section}"))
                }
                crate::ui::picker::Row::Tiles(_) => None,
            })
            .collect()
    });
    let mut selectors = rows;
    for fixed in ["picker-info", "picker-said", "picker-name"] {
        if shows(harness.window, fixed, cx) {
            selectors.push(fixed.to_owned());
        }
    }
    selectors
}

#[gpui_kit::test]
fn every_text_of_the_tabs_is_inside_the_popover_at_every_size_and_theme(cx: &mut TestAppContext) {
    use crate::settings::ThemeChoice;
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    // A tab with stickers, and the same with none: the texts are the
    // empty sections' and the strip's.
    for with_stickers in [false, true] {
        if with_stickers {
            let base = 1_700_000_000_000;
            for n in 0..6u32 {
                chat_sticker(
                    &harness,
                    cx,
                    &format!("m{n}"),
                    base + n as i64,
                    n % 2 == 0,
                    Some(20 + n),
                );
            }
            index_chats(&harness);
        }
        for theme in [ThemeChoice::Light, ThemeChoice::Dark] {
            cx.update(|cx| settings::update(cx, |s| s.theme = theme));
            for step in crate::theme::SCALE_STEPS {
                cx.update(|cx| settings::update(cx, |s| s.interface_scale = step));
                cx.run_until_parked();
                for key in ["ctrl-shift-e", "ctrl-shift-j"] {
                    focus_composer(&harness, cx);
                    press(harness.window, key, cx);
                    let popover = bounds(harness.window, "emoji-picker", cx);
                    let grid = bounds(harness.window, "picker-grid", cx);
                    let selectors = texts_on_show(&harness, cx);
                    assert!(!selectors.is_empty());
                    for selector in selectors {
                        let text = bounds_of(harness.window, &selector, cx);
                        assert!(
                            within(text, popover) && text.right() <= grid.right() + px(0.5),
                            "{theme:?} {step}% {key} {with_stickers}: `{selector}` {text:?} \
                             is not inside {popover:?}"
                        );
                    }
                    press(harness.window, "escape", cx);
                }
            }
        }
    }
    cx.update(|cx| {
        settings::update(cx, |s| {
            s.interface_scale = 100;
            s.theme = ThemeChoice::Light;
        })
    });
}

#[gpui_kit::test]
fn a_long_line_wraps_inside_the_popover_instead_of_being_cut(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    open_stickers(&harness, cx);
    // The empty library: Favorites and From your chats, a line each.
    assert_eq!(
        grid_rows(&harness, cx),
        ["header", "note", "header", "note"]
    );
    let grid = bounds(harness.window, "picker-grid", cx);
    let note = bounds(harness.window, "picker-note-1", cx);
    assert!(
        note.right() <= grid.right() + px(1.) && note.left() >= grid.left() - px(1.),
        "{note:?} {grid:?}"
    );
    // The text is longer than the popover is wide: it takes more than a
    // line, and is all there.
    let line = crate::theme::metrics::TEXT_SMALL() * 1.2;
    assert!(
        note.size.height > line,
        "{:?} should be taller than a line ({line:?})",
        note.size
    );
    let words: Vec<String> = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
            .collect()
    });
    assert!(words[0].contains("not available yet") || words[0].contains("Star a sticker"));
    assert!(
        words[0].len() > 40,
        "the line is long enough to need wrapping"
    );
}

#[gpui_kit::test]
fn empty_sections_are_a_heading_and_a_line_with_no_blank_gap(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    open_stickers(&harness, cx);
    let grid = bounds(harness.window, "picker-grid", cx);
    let first = bounds(harness.window, "picker-heading-0", cx);
    let note_a = bounds(harness.window, "picker-note-1", cx);
    let second = bounds(harness.window, "picker-heading-1", cx);
    let note_b = bounds(harness.window, "picker-note-3", cx);
    // The grid starts under the first heading, each line under its own,
    // and the second section right after the first.
    assert!(first.top() - grid.top() < px(12.));
    assert!(
        note_a.top() - first.bottom() < px(12.),
        "{note_a:?} {first:?}"
    );
    assert!(
        second.top() - note_a.bottom() < px(16.),
        "{second:?} {note_a:?}"
    );
    assert!(note_b.top() - second.bottom() < px(12.));
    // Both sections together are shorter than one row of stickers and a
    // half: nothing is blank.
    let used = note_b.bottom() - first.top();
    assert!(
        used < crate::ui::picker::STICKER_CELL() * 2.,
        "{used:?} for two empty sections"
    );
    assert!(note_b.bottom() <= grid.bottom());

    // With only one section filled, it is the first, at the top.
    chat_sticker(&harness, cx, "o1", 1_700_000_000_000, true, Some(30));
    index_chats(&harness);
    press(harness.window, "escape", cx);
    open_stickers(&harness, cx);
    assert_eq!(titles(&harness, cx)[0], "Recently used");
    let first = bounds(harness.window, "picker-heading-0", cx);
    let grid = bounds(harness.window, "picker-grid", cx);
    assert!(first.top() - grid.top() < px(12.));
}

#[gpui_kit::test]
fn the_gif_tab_says_how_to_turn_the_search_on_and_where_saved_ones_appear(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    skip_seeded_history(&harness);
    open_gifs(&harness, cx);
    // Saved, none yet: one line on how to add some, and where they show.
    assert_eq!(grid_rows(&harness, cx), ["note"]);
    let said = bounds(harness.window, "picker-info", cx);
    let popover = bounds(harness.window, "emoji-picker", cx);
    assert!(within(said, popover));
    let strip_text: String = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .find_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
            .unwrap()
    });
    assert!(strip_text.contains("Save a GIF"), "{strip_text}");
    // Online, switched off: one line that says where to turn it on.
    click(harness.window, "picker-source-online", cx);
    assert_eq!(grid_rows(&harness, cx), ["note"]);
    let note: String = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .find_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
            .unwrap()
    });
    assert!(
        note.contains("Settings > Stickers and GIFs") && note.contains("off"),
        "{note}"
    );
    // The strip does not say the same again.
    assert!(!shows(harness.window, "picker-said", cx));
}

#[test]
fn the_controls_say_what_they_do_with_their_shortcuts() {
    use crate::ui::picker_view::{section_tip, with_keys};
    assert_eq!(section_tip("Favorites", 0), "Favorites");
    assert_eq!(section_tip("Favorites", 1), "Favorites: 1 sticker");
    assert_eq!(
        section_tip("From your chats", 12),
        "From your chats: 12 stickers"
    );
    // With a shortcut it is printed; without, the palette is said.
    let star = with_keys("Add to favorites", Command::FavoriteSticker);
    assert!(
        star.starts_with("Add to favorites (") && star.contains('D'),
        "{star}"
    );
    let import = with_keys("Make a sticker from a picture…", Command::ImportSticker);
    assert!(
        import.ends_with("(also in the command palette)"),
        "{import}"
    );
}

fn notes(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .picker
            .grid()
            .rows
            .iter()
            .filter_map(|row| match row {
                crate::ui::picker::Row::Note(text) => Some(text.to_string()),
                _ => None,
            })
            .collect()
    })
}

/// The provider has favorite stickers and the number on screen does not
/// have them yet (the backend turns them on a number at a time). The
/// Favorites section says so in one line, nothing is shown as a failure,
/// what is starred here stays; and when the number has them, the line
/// goes and the stars reach the phone, in the same session.
#[gpui_kit::test]
fn favorites_say_when_the_phones_are_not_available_yet_for_this_number(cx: &mut TestAppContext) {
    use client_provider::Feature;
    let harness = open_chat_one(cx);
    let account = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .account_id
            .clone()
    });
    assert!(harness.engine.capabilities().sticker_favorites);
    harness
        .mock
        .set_unavailable(Some(&account), Feature::StickerFavorites, true);
    let kept = save_sticker(&harness, still(40), true);
    harness.settle(cx);
    open_stickers(&harness, cx);
    harness.settle(cx);

    let said = notes(&harness, cx);
    assert!(
        said.iter()
            .any(|note| note.contains("not available yet for this number")
                && note.contains("you star here are kept as favorites")),
        "{said:?}"
    );
    // The favorite made here is there, first, and stays a favorite.
    assert_eq!(titles(&harness, cx)[0], "Favorites");
    assert!(tile_ids(&harness, cx).contains(&kept.id));
    assert!(
        harness
            .engine
            .store()
            .library_item(&kept.id)
            .unwrap()
            .unwrap()
            .favorite
    );
    assert!(harness.mock.favorite_sticker_ids(&account).is_empty());
    // Not a failure: nothing in the problem line.
    assert!(!shows(harness.window, "problem", cx));

    // The number has them now. The next star brings every star over, and
    // the line is gone without the picker being opened again.
    harness
        .mock
        .set_unavailable(Some(&account), Feature::StickerFavorites, false);
    let second = save_sticker(&harness, still(44), true);
    super::media_out::wait(&harness, cx, "the stars reach the phone", || {
        harness.mock.favorite_sticker_ids(&account).len() == 2
    });
    cx.run_until_parked();
    let said = notes(&harness, cx);
    assert!(
        !said.iter().any(|note| note.contains("not available yet")),
        "{said:?}"
    );
    assert!(tile_ids(&harness, cx).contains(&second.id));
}

/// The phone's favorites show under Favorites when the number has them.
#[gpui_kit::test]
fn the_phones_favorites_show_in_the_favorites_section(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let account = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .account_id
            .clone()
    });
    let (one, two) = (still(44), still(52));
    harness
        .mock
        .star_sticker_on_phone(&account, "stk_1", one.clone(), "image/webp");
    harness
        .mock
        .star_sticker_on_phone(&account, "stk_2", two.clone(), "image/webp");
    open_stickers(&harness, cx);
    super::media_out::wait(&harness, cx, "the phone's favorites arrive", || {
        harness
            .engine
            .store()
            .library_favorites(LibraryKind::Sticker)
            .unwrap()
            .len()
            == 2
    });
    cx.run_until_parked();
    // The section is first and holds both, with no line about what is
    // missing.
    assert_eq!(titles(&harness, cx)[0], "Favorites");
    let tiles = tile_ids(&harness, cx);
    assert!(tiles.contains(&content_id(&one)) && tiles.contains(&content_id(&two)));
    let said = notes(&harness, cx);
    assert!(
        !said.iter().any(|note| note.contains("not available")),
        "{said:?}"
    );
    assert!(shows(harness.window, "picker-tile-0", cx));
}
