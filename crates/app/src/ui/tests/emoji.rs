//! The emoji picker in the real window: over the composer and in the
//! reaction sheet, with the mouse and with the keyboard, on top of the
//! mock provider.

use super::super::emoji_picker::{Zone, COLUMNS, LONG_PRESS, SEARCH_DEBOUNCE};
use super::*;
use crate::emoji::data::{Lang, Tone};
use crate::emoji::locale::LanguageChoice;
use crate::emoji::prefs;
use client_provider::{AccountId, ChatId, ContactId, Message, MessageId, ProviderEvent, Timestamp};
use gpui_kit::MouseButton;

fn open_chat_one(cx: &mut TestAppContext) -> Harness {
    open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

/// Lets the search's wait pass, and what it found reach the window.
fn settle_search(cx: &mut TestAppContext) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(SEARCH_DEBOUNCE + Duration::from_millis(10));
    cx.run_until_parked();
}

/// Types into whatever has the keyboard and waits for the search.
fn search(harness: &Harness, text: &str, cx: &mut TestAppContext) {
    type_text(harness.window, text, cx);
    settle_search(cx);
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn composer_text(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string())
}

/// The emoji of the grid, in its order.
fn cells(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .emoji
            .cells()
            .iter()
            .map(|cell| cell.text.to_string())
            .collect()
    })
}

/// The cell the keyboard is on, and the part of the picker it is in.
fn cursor(harness: &Harness, cx: &mut TestAppContext) -> (usize, Zone) {
    cx.update(|cx| {
        let emoji = &harness.shell.read(cx).emoji;
        (emoji.cursor, emoji.zone)
    })
}

fn preview(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| harness.shell.read(cx).emoji.preview_name())
        .unwrap_or_default()
}

fn cell_name(index: usize) -> &'static str {
    Box::leak(format!("emoji-cell-{index}").into_boxed_str())
}

/// Opens the composer's picker with its shortcut and waits for the tables.
fn open_picker(harness: &Harness, cx: &mut TestAppContext) {
    focus_composer(harness, cx);
    press(harness.window, "ctrl-e", cx);
    cx.run_until_parked();
    assert_eq!(overlay(harness, cx), Overlay::EmojiPicker);
}

/// A message from the other side in the open chat, with the keyboard on it.
fn focused_message(harness: &Harness, cx: &mut TestAppContext, id: &str) {
    let (account, chat): (AccountId, ChatId) = cx.update(|cx| {
        let chat = &harness.shell.read(cx).open.as_ref().unwrap().chat;
        (chat.account_id.clone(), chat.id.clone())
    });
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(id),
            client_id: None,
            account_id: account,
            chat_id: chat,
            sender: ContactId::new("them"),
            sender_name: None,
            direction: Direction::Incoming,
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000),
            content: MessageContent::text("we won!"),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        }))
        .unwrap();
    cx.run_until_parked();
    focus_composer(harness, cx);
    press(harness.window, "up", cx);
}

/// The reactions of a message, as the window holds them: (emoji, mine).
fn reactions(harness: &Harness, cx: &mut TestAppContext, id: &str) -> Vec<(String, bool)> {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) if row.stored.message.id.as_str() == id => Some(
                    row.stored
                        .reactions
                        .iter()
                        .map(|reaction| (reaction.emoji.clone(), reaction.from_me))
                        .collect(),
                ),
                _ => None,
            })
            .unwrap_or_default()
    })
}

// ----- the composer ------------------------------------------------------------

#[gpui_kit::test]
fn the_button_opens_the_picker_and_a_click_puts_the_emoji_at_the_caret(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    focus_composer(&harness, cx);
    type_text(harness.window, "ab", cx);
    press(harness.window, "left", cx);

    click(harness.window, "emoji", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    // Above its button, inside the window, with the search and the grid.
    let picker = bounds(harness.window, "emoji-picker", cx);
    let button = bounds(harness.window, "emoji", cx);
    assert!(picker.bottom() <= button.top(), "{picker:?} {button:?}");
    assert!(within(bounds(harness.window, "emoji-search", cx), picker));
    assert!(within(bounds(harness.window, "emoji-grid", cx), picker));
    assert!(within(bounds(harness.window, "emoji-preview", cx), picker));
    // The nine groups of the keyboard, in its order; nothing was used yet.
    let sections = cx.update(|cx| harness.shell.read(cx).emoji.sections());
    assert_eq!(
        sections,
        [
            "Smileys & Emotion",
            "People & Body",
            "Animals & Nature",
            "Food & Drink",
            "Travel & Places",
            "Activities",
            "Objects",
            "Symbols",
            "Flags"
        ]
    );
    let all = cells(&harness, cx);
    assert_eq!(all.len(), 1923, "every emoji is offered");
    assert_eq!(all[0], "😀");

    // A click inserts where the caret was, and the picker stays for more.
    click(harness.window, cell_name(0), cx);
    assert_eq!(composer_text(&harness, cx), "a😀b");
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    click(harness.window, cell_name(1), cx);
    assert_eq!(composer_text(&harness, cx), format!("a😀{}b", all[1]));

    // Escape closes it and typing goes on where the emoji went in.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    type_text(harness.window, "!", cx);
    assert_eq!(composer_text(&harness, cx), format!("a😀{}!b", all[1]));

    // A click outside closes it too, and nothing was sent by any of this.
    click(harness.window, "emoji", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_click(gpui_kit::point(px(1200.), px(120.)), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
}

#[gpui_kit::test]
fn the_shortcut_opens_and_closes_it_and_is_in_the_registry(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    press(harness.window, "ctrl-e", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    // From a message in focus as well.
    press(harness.window, "up", cx);
    press(harness.window, "ctrl-e", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    press(harness.window, "escape", cx);

    let binding = crate::keys::binding(crate::keys::Command::EmojiPicker).unwrap();
    assert_eq!(binding.label, "Emoji");
    assert_eq!(binding.chords[0].label_for(false), "Ctrl+E");
    // The sheet of shortcuts lists it.
    assert!(super::super::palette::shortcut_rows()
        .iter()
        .flat_map(|(_, rows)| rows)
        .any(|(binding, _)| binding.command == crate::keys::Command::EmojiPicker));
}

#[gpui_kit::test]
fn without_a_chat_there_is_nothing_to_open_it_over(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    press(harness.window, "ctrl-e", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

// ----- the keyboard ------------------------------------------------------------

#[gpui_kit::test]
fn the_arrows_walk_the_grid_across_rows_and_categories(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    assert_eq!(cursor(&harness, cx), (0, Zone::Search));

    // With nothing typed the arrows are the grid's.
    press(harness.window, "right", cx);
    assert_eq!(cursor(&harness, cx), (1, Zone::Grid));
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx), (1 + COLUMNS, Zone::Grid));
    press(harness.window, "left", cx);
    assert_eq!(cursor(&harness, cx).0, COLUMNS);
    // Left from the start of a row is the end of the row above.
    press(harness.window, "left", cx);
    assert_eq!(cursor(&harness, cx), (COLUMNS - 1, Zone::Grid));
    // Above the first row is the search field.
    press(harness.window, "up", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);

    // Page Down jumps a category, Page Up comes back.
    let second = cx
        .update(|cx| harness.shell.read(cx).emoji.section_start(1))
        .unwrap();
    press(harness.window, "pagedown", cx);
    assert_eq!(cursor(&harness, cx), (second, Zone::Grid));
    assert_eq!(cells(&harness, cx)[second], "👋");
    assert!(
        shows(harness.window, cell_name(second), cx),
        "it is in view"
    );
    // Up from a category's first row is the last row of the one before,
    // and Down comes back across the heading.
    press(harness.window, "up", cx);
    let above = cursor(&harness, cx).0;
    assert!(above < second && above + COLUMNS >= second, "{above}");
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx).0, second);
    // Left steps back into the category before.
    press(harness.window, "left", cx);
    assert_eq!(cursor(&harness, cx).0, second - 1);
    press(harness.window, "pagedown", cx);
    press(harness.window, "pageup", cx);
    assert_eq!(cursor(&harness, cx).0, 0);

    // The name under the grid is the emoji's the keyboard is on.
    assert_eq!(preview(&harness, cx), "grinning face");
    press(harness.window, "right", cx);
    assert_eq!(preview(&harness, cx), "grinning face with big eyes");
}

#[gpui_kit::test]
fn tab_goes_from_the_search_to_the_tabs_to_the_categories_to_the_grid(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    // The bar of tabs (Emoji, GIF, Stickers) is between the search and
    // the categories.
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Tabs);
    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Categories);
    // In the bar the arrows pick a category and Enter goes there.
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    press(harness.window, "enter", cx);
    let third = cx
        .update(|cx| harness.shell.read(cx).emoji.section_start(2))
        .unwrap();
    assert_eq!(cursor(&harness, cx), (third, Zone::Grid));
    assert_eq!(
        overlay(&harness, cx),
        Overlay::EmojiPicker,
        "nothing picked"
    );
    assert_eq!(composer_text(&harness, cx), "");

    press(harness.window, "tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);
    press(harness.window, "shift-tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Grid);
    press(harness.window, "shift-tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Categories);
    press(harness.window, "shift-tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Tabs);
    press(harness.window, "shift-tab", cx);
    assert_eq!(cursor(&harness, cx).1, Zone::Search);

    // Typing goes to the search wherever the keyboard was.
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    search(&harness, "pizza", cx);
    assert_eq!(cells(&harness, cx)[0], "🍕");
    assert_eq!(cursor(&harness, cx), (0, Zone::Search));
}

#[gpui_kit::test]
fn enter_picks_shift_enter_keeps_it_open_and_escape_empties_the_search_first(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    search(&harness, "fire", cx);
    assert_eq!(cells(&harness, cx)[0], "🔥");
    assert_eq!(preview(&harness, cx), "fire");
    assert!(shows(harness.window, "emoji-found", cx));

    // Shift+Enter picks and stays.
    press(harness.window, "shift-enter", cx);
    assert_eq!(composer_text(&harness, cx), "🔥");
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);

    // Escape empties the search and shows the categories again; the
    // second one closes.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    assert_eq!(
        cx.update(|cx| harness
            .shell
            .read(cx)
            .emoji
            .search
            .read(cx)
            .value()
            .to_string()),
        ""
    );
    assert!(cells(&harness, cx).len() > 1000);
    assert!(!shows(harness.window, "emoji-found", cx));

    // Down into the results, then Enter picks and closes.
    search(&harness, "thumbs", cx);
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx), (0, Zone::Grid));
    press(harness.window, "right", cx);
    let second = cells(&harness, cx)[1].clone();
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(composer_text(&harness, cx), format!("🔥{second}"));
    // The keyboard is the composer's again.
    type_text(harness.window, "!", cx);
    assert_eq!(composer_text(&harness, cx), format!("🔥{second}!"));

    // With a word typed, Left and Right move in the word.
    open_picker(&harness, cx);
    search(&harness, "fir", cx);
    press(harness.window, "left", cx);
    assert_eq!(cursor(&harness, cx), (0, Zone::Search));
    search(&harness, "x", cx);
    assert_eq!(
        cx.update(|cx| harness
            .shell
            .read(cx)
            .emoji
            .search
            .read(cx)
            .value()
            .to_string()),
        "fixr"
    );
    // Nothing is called that: the strip says so, and Enter picks nothing.
    assert!(cells(&harness, cx).is_empty());
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

// ----- the language ------------------------------------------------------------

#[gpui_kit::test]
fn spanish_words_find_emoji_and_name_them_once_spanish_is_picked(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    cx.update(|cx| {
        prefs::update(cx, |chosen| {
            chosen.language = LanguageChoice::Lang(Lang::Es)
        })
    });
    open_picker(&harness, cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).emoji.language())
            .map(|language| language.lang),
        Some(Lang::Es)
    );
    assert_eq!(preview(&harness, cx), "cara sonriendo");

    search(&harness, "fuego", cx);
    assert_eq!(cells(&harness, cx)[0], "🔥");
    assert_eq!(preview(&harness, cx), "fuego");
    press(harness.window, "escape", cx);
    // Without the accent, in capitals.
    search(&harness, "CORAZON", cx);
    assert_eq!(cells(&harness, cx)[0], "❤️");
    assert_eq!(preview(&harness, cx), "corazón rojo");
    press(harness.window, "escape", cx);
    search(&harness, "risa", cx);
    assert!(cells(&harness, cx).contains(&"😂".to_owned()));
    press(harness.window, "escape", cx);
    // English still finds it.
    search(&harness, "heart", cx);
    assert_eq!(cells(&harness, cx)[0], "❤️");
    press(harness.window, "escape", cx);
    // And a shortcode, and an emoticon.
    search(&harness, ":thumbsup:", cx);
    assert_eq!(cells(&harness, cx)[0], "👍");
    press(harness.window, "escape", cx);
    search(&harness, ":)", cx);
    assert_eq!(cells(&harness, cx)[0], "🙂");
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);

    // Back to the system's language (English, in a test): the Spanish
    // word finds nothing, and the names are English.
    cx.update(|cx| {
        prefs::update(cx, |chosen| {
            chosen.language = LanguageChoice::Lang(Lang::En)
        })
    });
    open_picker(&harness, cx);
    assert_eq!(preview(&harness, cx), "grinning face");
    search(&harness, "fuego", cx);
    assert!(!cells(&harness, cx).contains(&"🔥".to_owned()));
}

#[gpui_kit::test]
fn the_language_is_picked_in_the_settings(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    assert_eq!(
        cx.update(|cx| prefs::language(cx)),
        LanguageChoice::System,
        "the system's, until one is picked"
    );
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.run_command(crate::keys::Command::SettingsAppearance, window, cx);
        })
    })
    .unwrap();
    cx.run_until_parked();
    // It is in the panel without scrolling, at every interface size.
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        let panel = bounds(harness.window, "settings-panel", cx);
        let row = bounds(harness.window, "emoji-language", cx);
        assert!(within(row, panel), "{step}%: {row:?} outside {panel:?}");
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
    cx.run_until_parked();

    // The arrows step through System and the languages there are names
    // for, in both directions.
    click(harness.window, "emoji-language-next", cx);
    assert_eq!(
        cx.update(|cx| prefs::language(cx)),
        LanguageChoice::Lang(Lang::En)
    );
    click(harness.window, "emoji-language-next", cx);
    assert_eq!(
        cx.update(|cx| prefs::language(cx)),
        LanguageChoice::Lang(Lang::Es)
    );
    click(harness.window, "emoji-language-previous", cx);
    click(harness.window, "emoji-language-previous", cx);
    assert_eq!(cx.update(|cx| prefs::language(cx)), LanguageChoice::System);
    click(harness.window, "emoji-language-previous", cx);
    assert_eq!(
        cx.update(|cx| prefs::language(cx)),
        LanguageChoice::Lang(*Lang::ALL.last().unwrap())
    );
    assert_eq!(overlay(&harness, cx), Overlay::Settings, "the panel stays");
}

// ----- skin tones --------------------------------------------------------------

#[gpui_kit::test]
fn a_skin_tone_is_picked_from_the_keyboard_and_remembered(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    search(&harness, "waving hand", cx);
    assert_eq!(cells(&harness, cx)[0], "👋");

    // Alt+Down offers its tones: the emoji itself, then the five.
    press(harness.window, "alt-down", cx);
    let tones = cx.update(|cx| harness.shell.read(cx).emoji.tone_options());
    assert_eq!(tones, ["👋", "👋🏻", "👋🏼", "👋🏽", "👋🏾", "👋🏿"]);
    assert!(shows(harness.window, "emoji-tones", cx));
    assert!(shows(harness.window, "emoji-tone-5", cx));
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    press(harness.window, "enter", cx);
    assert_eq!(composer_text(&harness, cx), "👋🏽");
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(cx.update(|cx| prefs::get(cx)).tone, Some(Tone::Medium));

    // From now on every emoji that has tones is offered in that one, and
    // the others are as they were.
    open_picker(&harness, cx);
    let all = cells(&harness, cx);
    assert!(all.contains(&"👍🏽".to_owned()));
    assert!(!all.contains(&"👍".to_owned()));
    assert!(all.contains(&"🔥".to_owned()));
    search(&harness, "waving hand", cx);
    assert_eq!(cells(&harness, cx)[0], "👋🏽");
    // The row opens on the tone in use; Escape closes the row only.
    press(harness.window, "alt-down", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-tones", cx));
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    // The first option takes the tone away again.
    press(harness.window, "alt-down", cx);
    click(harness.window, "emoji-tone-0", cx);
    assert_eq!(composer_text(&harness, cx), "👋🏽👋");
    assert_eq!(cx.update(|cx| prefs::get(cx)).tone, None);

    // An emoji without tones offers none.
    press(harness.window, "escape", cx);
    search(&harness, "fire", cx);
    press(harness.window, "alt-down", cx);
    assert!(!shows(harness.window, "emoji-tones", cx));
}

#[gpui_kit::test]
fn a_right_click_or_a_long_press_offers_the_skin_tones(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    search(&harness, "thumbs up", cx);
    assert_eq!(cells(&harness, cx)[0], "👍");
    let cell = bounds(harness.window, cell_name(0), cx).center();

    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(cell, MouseButton::Right, Modifiers::none());
    visual.simulate_mouse_up(cell, MouseButton::Right, Modifiers::none());
    visual.run_until_parked();
    assert!(shows(harness.window, "emoji-tones", cx));
    assert_eq!(
        composer_text(&harness, cx),
        "",
        "a right click picks nothing"
    );
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-tones", cx));

    // Held down long enough, the same; letting go then picks nothing.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(cell, MouseButton::Left, Modifiers::none());
    visual.run_until_parked();
    assert!(!shows(harness.window, "emoji-tones", cx), "not yet");
    cx.executor()
        .advance_clock(LONG_PRESS + Duration::from_millis(20));
    cx.run_until_parked();
    assert!(shows(harness.window, "emoji-tones", cx));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_up(cell, MouseButton::Left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(composer_text(&harness, cx), "");
    assert!(shows(harness.window, "emoji-tones", cx));
    // A click on a tone picks it; from the composer's picker it stays open.
    click(harness.window, "emoji-tone-5", cx);
    assert_eq!(composer_text(&harness, cx), "👍🏿");
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);

    // A short press is a click.
    press(harness.window, "escape", cx);
    search(&harness, "fire", cx);
    click(harness.window, cell_name(0), cx);
    assert_eq!(composer_text(&harness, cx), "👍🏿🔥");
}

// ----- the emoji used most -----------------------------------------------------

#[gpui_kit::test]
fn the_emoji_used_most_come_first_and_are_kept_next_to_the_settings(cx: &mut TestAppContext) {
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
    open_picker(&harness, cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).emoji.sections())[0],
        "Smileys & Emotion",
        "nothing was used yet"
    );
    for word in ["fire", "pizza", "fire"] {
        search(&harness, word, cx);
        press(harness.window, "shift-enter", cx);
        press(harness.window, "escape", cx);
    }
    // The grid does not move under the pointer while it is open.
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).emoji.sections())[0],
        "Smileys & Emotion"
    );
    press(harness.window, "escape", cx);
    assert_eq!(composer_text(&harness, cx), "🔥🍕🔥");

    // Open again: the used ones lead, the most used first.
    open_picker(&harness, cx);
    let sections = cx.update(|cx| harness.shell.read(cx).emoji.sections());
    assert_eq!(sections[0], "Frequently used");
    assert_eq!(sections.len(), 10);
    assert_eq!(cells(&harness, cx)[..3], ["🔥", "🍕", "😀"]);
    assert!(shows(harness.window, "emoji-category-9", cx));

    // On disk, beside the settings; a new session reads it.
    let kept = prefs::EmojiPrefs::load(&dir.path().join(prefs::FILE_NAME));
    assert_eq!(kept.frequent(5), ["🔥", "🍕"]);
    assert_eq!(kept.used[0].count, 2);
}

// ----- drawing only what is in view --------------------------------------------

#[gpui_kit::test]
fn only_the_rows_in_view_are_built(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    open_picker(&harness, cx);
    let (rows, built) = cx.update(|cx| {
        let emoji = &harness.shell.read(cx).emoji;
        (emoji.row_count(), emoji.built.take())
    });
    assert!(rows > 200, "{rows} rows of emoji");
    assert_eq!(built.start, 0);
    assert!(built.len() <= 12, "{built:?} built for 8 rows in view");
    assert!(shows(harness.window, cell_name(0), cx));
    assert!(!shows(harness.window, cell_name(COLUMNS * 12), cx));
    assert!(!shows(harness.window, cell_name(1500), cx));

    // Far down the list it is still a handful of rows, other ones.
    for _ in 0..6 {
        press(harness.window, "pagedown", cx);
    }
    let (at, _) = cursor(&harness, cx);
    assert!(at > 1000, "{at}");
    let built = cx.update(|cx| harness.shell.read(cx).emoji.built.take());
    assert!(built.start > 100 && built.len() <= 12, "{built:?}");
    assert!(shows(harness.window, cell_name(at), cx));
    assert!(!shows(harness.window, cell_name(0), cx));
}

#[gpui_kit::test]
fn it_fits_at_every_interface_size_in_both_themes(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    for appearance in [ThemeChoice::Light, ThemeChoice::Dark] {
        cx.update(|cx| settings::update(cx, |settings| settings.theme = appearance));
        for step in theme::SCALE_STEPS {
            cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
            cx.run_until_parked();
            open_picker(&harness, cx);
            let window = Bounds {
                origin: Point::default(),
                size: size(px(1240.), px(800.)),
            };
            let picker = bounds(harness.window, "emoji-picker", cx);
            assert!(within(picker, window), "{step}%: {picker:?}");
            let grid = bounds(harness.window, "emoji-grid", cx);
            assert!(within(grid, picker), "{step}%");
            // Nine square cells fill a row of the grid, to the pixel the
            // layout rounds to.
            let close = |a: gpui_kit::Pixels, b: gpui_kit::Pixels| (a - b).abs() <= px(1.);
            let first = bounds(harness.window, cell_name(0), cx);
            let last = bounds(harness.window, cell_name(COLUMNS - 1), cx);
            assert!(close(first.size.width, first.size.height), "{step}%");
            assert!(close(first.left(), grid.left()), "{step}%");
            assert!(
                close(last.right(), grid.right()),
                "{step}%: {last:?} {grid:?}"
            );
            assert!(close(first.top(), last.top()), "{step}%");
            // The next row starts under the first.
            let below = bounds(harness.window, cell_name(COLUMNS), cx);
            assert!(close(below.top(), first.bottom()), "{step}%");
            assert!(close(below.left(), first.left()), "{step}%");
            for part in ["emoji-search", "emoji-categories", "emoji-preview"] {
                assert!(
                    within(bounds(harness.window, part, cx), picker),
                    "{step}%: {part}"
                );
            }
            press(harness.window, "escape", cx);
            assert_eq!(overlay(&harness, cx), Overlay::None);
        }
    }
}

// ----- reactions ---------------------------------------------------------------

#[gpui_kit::test]
fn plus_opens_every_emoji_in_the_reaction_sheet_and_a_pick_is_the_reaction(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    focused_message(&harness, cx, "e1");
    type_text(harness.window, "+", cx);
    assert_eq!(overlay(&harness, cx), Overlay::React);
    // The six, the way to the rest, and the search; no grid yet.
    let sheet = bounds(harness.window, "react-sheet", cx);
    assert!(within(bounds(harness.window, "reaction-more", cx), sheet));
    assert!(within(bounds(harness.window, "reaction-input", cx), sheet));
    assert!(!shows(harness.window, "emoji-grid", cx));

    click(harness.window, "reaction-more", cx);
    assert_eq!(overlay(&harness, cx), Overlay::React);
    let sheet = bounds(harness.window, "react-sheet", cx);
    assert!(within(bounds(harness.window, "emoji-grid", cx), sheet));
    let window = Bounds {
        origin: Point::default(),
        size: size(px(1240.), px(800.)),
    };
    assert!(within(sheet, window), "{sheet:?}");
    assert!(shows(harness.window, "reaction-0", cx), "the six stay");

    // The arrows are the grid's now; Enter reacts with what they are on.
    press(harness.window, "right", cx);
    let picked = cells(&harness, cx)[1].clone();
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(reactions(&harness, cx, "e1"), [(picked.clone(), true)]);
    // Through the outbox, like the six.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(
        pending[0].message.content,
        OutgoingContent::Reaction {
            target: MessageId::new("e1"),
            emoji: picked.clone()
        }
    );
    // And the provider takes it, sequence or not.
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    assert_eq!(reactions(&harness, cx, "e1"), [(picked, true)]);
}

#[gpui_kit::test]
fn typing_in_the_reaction_sheet_searches_right_away(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    focused_message(&harness, cx, "e2");
    type_text(harness.window, "+", cx);
    assert_eq!(overlay(&harness, cx), Overlay::React);

    search(&harness, "fire", cx);
    assert!(shows(harness.window, "emoji-grid", cx), "the rest opened");
    assert_eq!(cells(&harness, cx)[0], "🔥");
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(reactions(&harness, cx, "e2"), [("🔥".to_owned(), true)]);

    // A sequence of several characters, in a skin tone, by a click.
    type_text(harness.window, "+", cx);
    search(&harness, "firefighter", cx);
    let at = cells(&harness, cx)
        .iter()
        .position(|emoji| emoji == "👩‍🚒")
        .expect("the woman firefighter is found");
    press(harness.window, "down", cx);
    for _ in 0..at {
        press(harness.window, "right", cx);
    }
    press(harness.window, "alt-down", cx);
    click(harness.window, "emoji-tone-2", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(reactions(&harness, cx, "e2"), [("👩🏼‍🚒".to_owned(), true)]);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    assert_eq!(reactions(&harness, cx, "e2"), [("👩🏼‍🚒".to_owned(), true)]);
    // It is among the used ones the next time, in either picker.
    open_picker(&harness, cx);
    assert_eq!(cells(&harness, cx)[..2], ["👩🏼‍🚒", "🔥"]);
}

// ----- a shortcode typed in the composer ---------------------------------------

#[gpui_kit::test]
fn a_shortcode_typed_in_the_composer_offers_its_emoji(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    focus_composer(&harness, cx);
    let before = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().rows.len());
    type_text(harness.window, "on :fir", cx);
    settle_search(cx);
    // The tables were not at hand yet: the list comes when they are.
    settle_search(cx);
    let offered = cx.update(|cx| harness.shell.read(cx).emoji.completions());
    assert_eq!(offered[0], ("🔥".to_owned(), ":fire:".to_owned()));
    assert!(offered.len() > 1 && offered.len() <= 6, "{offered:?}");
    let list = bounds(harness.window, "emoji-completion", cx);
    let composer = bounds(harness.window, "composer", cx);
    assert!(within(list, composer));

    // Enter puts the emoji in place of what was typed, and sends nothing.
    press(harness.window, "enter", cx);
    assert_eq!(composer_text(&harness, cx), "on 🔥");
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().rows.len()),
        before
    );
    // The caret is after it.
    type_text(harness.window, " :piz", cx);
    settle_search(cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).emoji.completions())[0].0,
        "🍕"
    );
    // The arrows move in the list and Tab picks.
    press(harness.window, "down", cx);
    press(harness.window, "up", cx);
    press(harness.window, "tab", cx);
    assert_eq!(composer_text(&harness, cx), "on 🔥 🍕");

    // Escape closes the list and leaves the text; Enter then sends it.
    type_text(harness.window, " :smi", cx);
    settle_search(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert_eq!(composer_text(&harness, cx), "on 🔥 🍕 :smi");
    press(harness.window, "enter", cx);
    assert_eq!(composer_text(&harness, cx), "");
    assert_eq!(
        cx.update(|cx| newest_message(harness.shell.read(cx)))
            .map(|(text, _, _)| text),
        Some("on 🔥 🍕 :smi".to_owned())
    );

    // A time or a link is no shortcode.
    type_text(harness.window, "at 10:30", cx);
    settle_search(cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
}

// ----- the composer's helpers, together -------------------------------------------

#[gpui_kit::test]
fn the_emoji_button_the_two_lists_and_the_way_to_the_chats_do_not_get_in_each_others_way(
    cx: &mut TestAppContext,
) {
    // A group, so that "@" has people to offer.
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(
                client_provider::ChatId::new(provider_mock::SHOWCASE_CHAT),
                None,
                cx,
            );
        })
    });
    harness.settle(cx);
    focus_composer(&harness, cx);
    let on_list = |cx: &mut TestAppContext| {
        cx.update_window(harness.window.into(), |_, window, cx| {
            harness.shell.read(cx).list_has_keyboard(window)
        })
        .unwrap()
    };
    let open_chat = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).open.is_some());

    // A shortcode: its list is up, the people's is not; Escape closes it
    // and nothing else, and the text stays.
    type_text(harness.window, "see :fir", cx);
    settle_search(cx);
    settle_search(cx);
    assert!(shows(harness.window, "emoji-completion", cx));
    assert!(!shows(harness.window, "mention-picker", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert_eq!(composer_text(&harness, cx), "see :fir");
    assert!(!on_list(cx) && open_chat(cx));

    // "@": the people's list has the keys, and Escape closes that one.
    type_text(harness.window, " @", cx);
    settle_search(cx);
    assert!(shows(harness.window, "mention-picker", cx));
    assert!(!shows(harness.window, "emoji-completion", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "mention-picker", cx));
    assert!(!on_list(cx) && open_chat(cx));
    // With something typed, Escape goes nowhere.
    press(harness.window, "escape", cx);
    assert!(!on_list(cx));
    assert_eq!(composer_text(&harness, cx), "see :fir @");

    // The button opens the picker over the composer; a pick lands at the
    // caret; Escape closes the picker and the keyboard is the composer's.
    click(harness.window, "emoji", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    let (picker, composer) = (
        bounds(harness.window, "emoji-picker", cx),
        bounds(harness.window, "composer", cx),
    );
    assert!(picker.bottom() <= composer.bottom());
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(!on_list(cx) && open_chat(cx));
    type_text(harness.window, "!", cx);
    assert_eq!(composer_text(&harness, cx), "see :fir @!");

    // Ctrl+E from the chat list opens it too, for the composer; Ctrl+L
    // from the composer still goes to the list.
    press(harness.window, "ctrl-l", cx);
    assert!(on_list(cx));
    press(harness.window, "ctrl-e", cx);
    assert_eq!(overlay(&harness, cx), Overlay::EmojiPicker);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // Emptied, the composer's Escape is the way to the chats again, and
    // the conversation stays open.
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-a", cx);
    press(harness.window, "backspace", cx);
    settle_search(cx);
    press(harness.window, "escape", cx);
    assert!(on_list(cx) && open_chat(cx));
    // The emoji button and the attach button are where they were.
    assert!(shows(harness.window, "emoji", cx) && shows(harness.window, "attach", cx));
}
