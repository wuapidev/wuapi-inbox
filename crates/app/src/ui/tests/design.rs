//! The conversation pane's design, measured in the real window: who a
//! message is from (the name line, the avatar column, runs), the pills on
//! the page, the unread divider and where a chat opens, the wallpaper, the
//! header's second line, the composer, the bubble styles and the voice
//! note. Colours are checked where they are defined (`theme.rs`); here it
//! is where things are.

use super::*;
use crate::settings::WallpaperChoice;
use crate::theme::{metrics, BubbleStyle, Palette};
use client_provider::{
    AccountId, ChatId, ContactId, Message, MessageId, ProviderEvent, ReplyRef, Timestamp,
};
use provider_mock::SHOWCASE_CHAT;

const LEO: &str = "+5491133344455";
const LEO_HIDDEN: &str = "lid:777000111222333";
const ZOE: &str = "+34600111222";

/// A text message for a chat, `at` seconds from now.
fn text(
    account: &AccountId,
    chat: &ChatId,
    id: &str,
    from: Option<(&str, &str)>,
    body: &str,
    at: i64,
) -> Message {
    Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: account.clone(),
        chat_id: chat.clone(),
        sender: ContactId::new(from.map_or("me", |(id, _)| id)),
        sender_name: from.map(|(_, name)| name.to_owned()),
        direction: if from.is_some() {
            Direction::Incoming
        } else {
            Direction::Outgoing
        },
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000 + at * 1_000),
        content: MessageContent::text(body),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

/// Stores a message as the engine does when the provider reports it.
fn arrive(harness: &Harness, cx: &mut TestAppContext, message: Message) {
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

/// The open chat: (account, chat).
fn open_ids(harness: &Harness, cx: &mut TestAppContext) -> (AccountId, ChatId) {
    cx.update(|cx| {
        let chat = &harness.shell.read(cx).open.as_ref().unwrap().chat;
        (chat.account_id.clone(), chat.id.clone())
    })
}

/// Somebody says something in the open chat.
fn say(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    from: Option<(&str, &str)>,
    body: &str,
    at: i64,
) {
    let (account, chat) = open_ids(harness, cx);
    arrive(harness, cx, text(&account, &chat, id, from, body, at));
}

fn open_group(cx: &mut TestAppContext) -> Harness {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    harness.settle(cx);
    harness
}

/// What the window holds about a message's row: (first of its run, last
/// of it, the sender's key and colour, written as their own name).
fn row_of(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
) -> (bool, bool, Option<(String, usize, bool)>) {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) if row.stored.message.id.as_str() == id => Some((
                    row.first_of_run,
                    row.last_of_run,
                    row.sender
                        .as_ref()
                        .map(|sender| (sender.key.to_string(), sender.tone, sender.own_name)),
                )),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no row for {id}"))
    })
}

fn centre_x(bounds: Bounds<gpui_kit::Pixels>) -> gpui_kit::Pixels {
    bounds.left() + bounds.size.width / 2.
}

fn near(a: gpui_kit::Pixels, b: gpui_kit::Pixels) -> bool {
    (a - b).abs() <= px(1.)
}

fn set_scale(cx: &mut TestAppContext, step: u16) {
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
    cx.run_until_parked();
}

// ----- who a message is from ------------------------------------------------

#[gpui_kit::test]
fn in_a_group_the_name_is_a_line_of_its_own_and_the_avatar_stands_beside_the_run(
    cx: &mut TestAppContext,
) {
    let harness = open_group(cx);
    for step in theme::SCALE_STEPS {
        set_scale(cx, step);
        let id = format!("leo-{step}");
        // Somebody else in between, so each step starts a run of Leo's.
        say(
            &harness,
            cx,
            &format!("zoe-{step}"),
            Some((ZOE, "Zoe")),
            "ok",
            i64::from(step) * 10,
        );
        say(
            &harness,
            cx,
            &id,
            Some((LEO, "Leo")),
            "Landing at nine, I will take the metro",
            i64::from(step) * 10 + 1,
        );

        let thread = bounds(harness.window, "thread", cx);
        let bubble = bounds(harness.window, "bubble", cx);
        let line = bounds(harness.window, "sender-line", cx);
        let name = bounds(harness.window, "sender-name", cx);
        let body = bounds(harness.window, "message-text", cx);
        let avatar = bounds(harness.window, "sender-avatar", cx);

        // The name: inside the bubble, on a line of its own above the
        // text, starting where the text starts, in a smaller size.
        assert!(within(line, bubble), "{step}%: {line:?} outside {bubble:?}");
        assert!(within(name, line), "{step}%");
        assert!(
            name.bottom() <= body.top(),
            "{step}%: the name {name:?} runs into the text {body:?}"
        );
        assert!(near(name.left(), body.left()), "{step}%");
        assert!(
            name.size.height < metrics::LINE_BODY(),
            "{step}%: the name's line is shorter than a line of text"
        );
        // An unsaved person goes by the name they gave themselves, with
        // their number after it.
        let number = bounds(harness.window, "sender-number", cx);
        assert!(
            within(number, line) && number.left() >= name.right(),
            "{step}%"
        );

        // The avatar: a circle of the token's size, in its column left of
        // the bubble, level with the bubble's foot, inside the pane.
        assert_eq!(avatar.size.width, metrics::AVATAR_SMALL(), "{step}%");
        assert_eq!(avatar.size.height, metrics::AVATAR_SMALL(), "{step}%");
        assert!(avatar.left() >= thread.left(), "{step}%");
        assert!(
            near(bubble.left() - avatar.right(), metrics::AVATAR_GAP()),
            "{step}%: {avatar:?} / {bubble:?}"
        );
        assert!(near(avatar.bottom(), bubble.bottom()), "{step}%");
        assert!(!intersects(avatar, bubble), "{step}%");
    }
    set_scale(cx, 100);
}

#[gpui_kit::test]
fn a_run_is_grouped_the_name_on_its_first_bubble_and_the_avatar_on_its_last(
    cx: &mut TestAppContext,
) {
    let harness = open_group(cx);
    say(&harness, cx, "z1", Some((ZOE, "Zoe")), "before", 0);
    say(&harness, cx, "a1", Some((LEO, "Leo")), "one", 1);
    let alone = bounds(harness.window, "row-a1", cx);
    say(&harness, cx, "a2", Some((LEO, "Leo")), "two", 2);
    say(&harness, cx, "a3", Some((LEO, "Leo")), "three", 3);

    let (first_of_run, last_of_run, _) = row_of(&harness, cx, "a1");
    assert!(first_of_run && !last_of_run);
    assert_eq!(
        (row_of(&harness, cx, "a2").0, row_of(&harness, cx, "a2").1),
        (false, false)
    );
    assert_eq!(
        (row_of(&harness, cx, "a3").0, row_of(&harness, cx, "a3").1),
        (false, true)
    );

    let first = bounds(harness.window, "row-a1", cx);
    let second = bounds(harness.window, "row-a2", cx);
    let third = bounds(harness.window, "row-a3", cx);
    // The rows follow each other; the first carries the name and the room
    // above a run, the others only the small gap.
    assert_eq!(second.top(), first.bottom());
    assert_eq!(third.top(), second.bottom());
    assert!(first.size.height > second.size.height + px(12.));
    assert_eq!(second.size.height, third.size.height);
    // Becoming the first of three did not change the first row's size.
    assert_eq!(first.size, alone.size);

    // The newest bubble is the third: tight under the second.
    let bubble = bounds(harness.window, "bubble", cx);
    assert!(within(bubble, third));
    assert!(near(bubble.top() - third.top(), metrics::RUN_JOINT()));
    // The name was drawn once, on the first; the avatar once, on the last.
    let name = bounds(harness.window, "sender-name", cx);
    assert!(within(name, first), "{name:?} not in {first:?}");
    let avatar = bounds(harness.window, "sender-avatar", cx);
    assert!(within(avatar, third), "{avatar:?} not in {third:?}");
    assert!(near(avatar.bottom(), bubble.bottom()));
    // The bubbles of a run share their left edge, beside the column.
    assert!(near(bubble.left() - avatar.right(), metrics::AVATAR_GAP()));

    // The account's own bubbles have no column and no name: they end at
    // the pane's right gutter.
    say(&harness, cx, "mine", None, "on my way", 4);
    let mine = bounds(harness.window, "row-mine", cx);
    let own = bounds(harness.window, "bubble", cx);
    let thread = bounds(harness.window, "thread", cx);
    assert!(within(own, mine));
    assert!(near(own.top() - mine.top(), metrics::RUN_GAP()));
    assert!(own.right() < thread.right() && own.left() > centre_x(thread));
    // The run before it still ends with its avatar.
    let third = bounds(harness.window, "row-a3", cx);
    assert!(within(bounds(harness.window, "sender-avatar", cx), third));
    assert_eq!(mine.top(), third.bottom());
}

#[gpui_kit::test]
fn one_person_has_one_colour_and_one_run_under_any_of_their_ids(cx: &mut TestAppContext) {
    let harness = open_group(cx);
    let (account, _) = open_ids(&harness, cx);
    say(&harness, cx, "a1", Some((LEO, "Leo")), "by number", 1);
    say(
        &harness,
        cx,
        "h1",
        Some((LEO_HIDDEN, "Leo")),
        "by hidden id",
        2,
    );
    say(&harness, cx, "z1", Some((ZOE, "Zoe")), "somebody else", 3);

    // Before the two ids are known to be one person: two people.
    let (_, _, by_number) = row_of(&harness, cx, "a1");
    let (first_of_run, _, by_hidden) = row_of(&harness, cx, "h1");
    let (by_number, by_hidden) = (by_number.unwrap(), by_hidden.unwrap());
    assert_eq!(by_number.0, LEO);
    assert_eq!(by_hidden.0, LEO_HIDDEN);
    assert!(first_of_run, "a stranger starts a run of their own");

    // The address book says they are one: one key, one colour, one run,
    // on the rows already drawn.
    harness
        .engine
        .store()
        .link_ids(&account, &[ContactId::new(LEO), ContactId::new(LEO_HIDDEN)])
        .unwrap();
    cx.run_until_parked();
    let (_, last, by_number) = row_of(&harness, cx, "a1");
    let (first_of_run, _, by_hidden) = row_of(&harness, cx, "h1");
    let (by_number, by_hidden) = (by_number.unwrap(), by_hidden.unwrap());
    assert_eq!(by_number.0, LEO);
    assert_eq!(by_hidden.0, LEO, "the number stands for both ids");
    assert_eq!(by_number.1, by_hidden.1, "one colour");
    assert_eq!(by_number.1, crate::ui::senders::tone_of(LEO));
    assert!(!first_of_run && !last, "one run across the two ids");
    // Not saved, not named by the group: written as their own name.
    assert!(by_number.2);

    // Somebody else is somebody else.
    let (first_of_run, _, zoe) = row_of(&harness, cx, "z1");
    assert!(first_of_run);
    assert_eq!(zoe.unwrap().0, ZOE);

    // The avatar and the name both lead to the profile.
    click(harness.window, "sender-avatar", cx);
    assert!(shows(harness.window, "profile", cx), "the avatar opens it");
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "profile", cx));
    click(harness.window, "sender-name", cx);
    assert!(shows(harness.window, "profile", cx), "the name opens it");
}

#[gpui_kit::test]
fn a_quote_is_in_the_colour_of_whoever_wrote_it(cx: &mut TestAppContext) {
    use crate::ui::senders::Quoted;
    let harness = open_group(cx);
    let (account, chat) = open_ids(&harness, cx);
    say(
        &harness,
        cx,
        "a1",
        Some((LEO, "Leo")),
        "who brings the map?",
        1,
    );
    say(&harness, cx, "mine", None, "I do", 2);
    let quoting = |id: &str, quoted: &str, name: Option<&str>, at: i64| {
        let mut message = text(&account, &chat, id, Some((ZOE, "Zoe")), "this one", at);
        message.reply_to = Some(ReplyRef {
            message_id: MessageId::new(quoted),
            sender_name: name.map(str::to_owned),
            preview: Some("…".into()),
        });
        message
    };
    arrive(&harness, cx, quoting("q1", "a1", Some("Leo"), 3));
    arrive(&harness, cx, quoting("q2", "mine", None, 4));
    arrive(
        &harness,
        cx,
        quoting("q3", "long-gone", Some("Somebody"), 5),
    );
    let quoted = |cx: &mut TestAppContext, id: &str| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            shell
                .open
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .find_map(|row| match row {
                    Row::Message(row) if row.stored.message.id.as_str() == id => Some(row.quoted),
                    _ => None,
                })
                .unwrap()
        })
    };
    let leo = row_of(&harness, cx, "a1").2.unwrap().1;
    assert_eq!(quoted(cx, "q1"), Quoted::Person(leo));
    assert_eq!(quoted(cx, "q2"), Quoted::Me);
    assert_eq!(quoted(cx, "q3"), Quoted::Unknown);
    // The block is inside the bubble, above the text.
    let bubble = bounds(harness.window, "bubble", cx);
    let block = bounds(harness.window, "quote", cx);
    let body = bounds(harness.window, "message-text", cx);
    assert!(within(block, bubble) && block.bottom() <= body.top());
}

#[gpui_kit::test]
fn a_chat_with_one_person_has_no_avatar_column(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    say(&harness, cx, "d1", Some(("them", "Them")), "hello", 1);
    say(&harness, cx, "d2", Some(("them", "Them")), "there", 2);
    assert!(!shows(harness.window, "sender-avatar", cx));
    assert!(!shows(harness.window, "sender-name", cx));
    assert_eq!(row_of(&harness, cx, "d2").2, None);
    // Still a run: tight, and the bubbles start at the pane's gutter.
    let (first, second) = (
        bounds(harness.window, "row-d1", cx),
        bounds(harness.window, "row-d2", cx),
    );
    assert_eq!(second.top(), first.bottom());
    let bubble = bounds(harness.window, "bubble", cx);
    assert!(near(bubble.top() - second.top(), metrics::RUN_JOINT()));
    assert!(bubble.left() > bounds(harness.window, "thread", cx).left() + px(20.));
}

// ----- pills and the unread divider -------------------------------------------

/// A quiet chat of the list (everything read), with `count` messages from
/// the other side arriving in it, `unread` of which the provider counts
/// as unread. Returns the chat's id.
fn unread_chat(
    harness: &Harness,
    cx: &mut TestAppContext,
    at: usize,
    count: usize,
    unread: u32,
    prefix: &str,
) -> ChatId {
    let account = stored_accounts(harness)[0].id.clone();
    let id = cx.update(|cx| chat_rows(harness.shell.read(cx))[at].0.clone());
    let chat = ChatId::new(id);
    for n in 0..count {
        arrive(
            harness,
            cx,
            text(
                &account,
                &chat,
                &format!("{prefix}{n}"),
                Some((chat.as_str(), "Them")),
                &format!("message number {n}"),
                n as i64,
            ),
        );
    }
    harness.mock.set_unread(&account, &chat, unread);
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    chat
}

/// The message the unread divider stands above, and its count.
fn divider(harness: &Harness, cx: &mut TestAppContext) -> Option<(String, u32)> {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) => row
                .unread_above
                .map(|count| (row.stored.message.id.to_string(), count)),
            Row::Day(_) => None,
        })
    })
}

fn open_from_list(harness: &Harness, cx: &mut TestAppContext, chat: &ChatId) {
    let leaked: &'static str = Box::leak(format!("chat-{chat}").into_boxed_str());
    click(harness.window, leaked, cx);
}

#[gpui_kit::test]
fn a_chat_with_unread_messages_opens_at_the_divider_which_stays_until_it_is_left(
    cx: &mut TestAppContext,
) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-read-all", cx);
    harness.settle(cx);

    // Forty messages arrive, thirty of them unread: more than a screen.
    let chat = unread_chat(&harness, cx, 3, 40, 30, "u");
    open_from_list(&harness, cx, &chat);
    assert_eq!(divider(&harness, cx), Some(("u10".to_owned(), 30)));

    // The list starts at the divider, not at the end: the divider is at
    // the top of the thread, centred, and the newest message is below
    // the fold.
    let thread = bounds(harness.window, "thread", cx);
    let line = bounds(harness.window, "unread-divider", cx);
    let pill = bounds(harness.window, "unread-pill", cx);
    assert!(
        // Under the room the list keeps for the keyboard's outline.
        near(line.top(), thread.top() + metrics::FOCUS_ROOM()),
        "the divider is at {:?}, the thread starts at {:?}",
        line.top(),
        thread.top()
    );
    assert!(within(pill, line) && near(centre_x(pill), centre_x(thread)));
    assert_eq!(pill.size.height, metrics::PILL());
    // The first unread message is right under it, in the same row.
    let first = bounds(harness.window, "row-u10", cx);
    assert_eq!(first.top(), line.bottom());
    assert!(
        !shows(harness.window, "row-u39", cx),
        "the end is not on screen"
    );
    assert!(cx.update(|cx| {
        let shell = harness.shell.read(cx);
        !shell.open.as_ref().unwrap().list.is_following_tail()
    }));
    // Opening it marked it read, as before.
    assert!(cx.update(|cx| chat_rows(harness.shell.read(cx))
        .iter()
        .all(|(_, _, unread)| *unread == 0)));

    // It stays while the chat is open, where it was, whatever arrives or
    // is sent.
    let account = stored_accounts(&harness)[0].id.clone();
    arrive(
        &harness,
        cx,
        text(
            &account,
            &chat,
            "later",
            Some((chat.as_str(), "Them")),
            "one more",
            100,
        ),
    );
    assert_eq!(divider(&harness, cx), Some(("u10".to_owned(), 30)));
    harness
        .engine
        .send_text(&account, &chat, "reading now".to_owned(), None)
        .unwrap();
    cx.run_until_parked();
    assert_eq!(divider(&harness, cx), Some(("u10".to_owned(), 30)));

    // Leaving the chat is what ends it.
    press(harness.window, "ctrl-w", cx);
    open_from_list(&harness, cx, &chat);
    assert_eq!(divider(&harness, cx), None);
    assert!(!shows(harness.window, "unread-divider", cx));
}

#[gpui_kit::test]
fn a_few_unread_messages_show_the_divider_with_the_end_of_the_chat(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-read-all", cx);
    harness.settle(cx);

    let chat = unread_chat(&harness, cx, 3, 6, 2, "f");
    open_from_list(&harness, cx, &chat);
    assert_eq!(divider(&harness, cx), Some(("f4".to_owned(), 2)));
    // Both the divider and the newest message are on screen, the newest
    // where it always is: above the composer.
    let thread = bounds(harness.window, "thread", cx);
    let line = bounds(harness.window, "unread-divider", cx);
    let newest = bounds(harness.window, "row-f5", cx);
    assert!(within(line, thread) && line.bottom() <= newest.top());
    // The message under the divider shares its row with it, and is still
    // one line tall.
    let under = bounds(harness.window, "bubble-f4", cx);
    assert!(under.size.height < px(40.), "{under:?}");
    assert!(near(
        newest.bottom(),
        thread.bottom() - metrics::THREAD_INSET()
    ));

    // What the account answered after is not unread, whatever the count
    // says: nothing before one's own last message gets a divider.
    press(harness.window, "ctrl-w", cx);
    let account = stored_accounts(&harness)[0].id.clone();
    arrive(
        &harness,
        cx,
        text(&account, &chat, "answer", None, "got it", 50),
    );
    harness.mock.set_unread(&account, &chat, 3);
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    open_from_list(&harness, cx, &chat);
    assert_eq!(divider(&harness, cx), None);
}

#[gpui_kit::test]
fn the_day_and_the_notices_are_centred_pills(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    // A message of another day, so its pill is on screen whatever the
    // hour the demo's messages fall on.
    say(
        &harness,
        cx,
        "tomorrow",
        Some(("them", "Them")),
        "see you",
        26 * 3600,
    );
    for step in theme::SCALE_STEPS {
        set_scale(cx, step);
        let thread = bounds(harness.window, "thread", cx);
        let day = bounds(harness.window, "day-pill", cx);
        assert_eq!(day.size.height, metrics::PILL(), "{step}%");
        assert!(near(centre_x(day), centre_x(thread)), "{step}%: {day:?}");
        assert!(day.size.width > day.size.height * 2., "{step}%: a pill");
        assert!(day.size.width < thread.size.width / 2., "{step}%");
    }
    set_scale(cx, 100);
}

// ----- the wallpaper ------------------------------------------------------------

#[gpui_kit::test]
fn the_wallpaper_fills_the_thread_and_moves_nothing(cx: &mut TestAppContext) {
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
    // (Dated tomorrow: its day's pill is on screen whatever the hour the
    // demo's own messages fall on, which follow the wall clock.)
    say(
        &harness,
        cx,
        "w1",
        Some(("them", "Them")),
        "hello",
        26 * 3600,
    );
    let measure = |cx: &mut TestAppContext| {
        [
            bounds(harness.window, "thread", cx),
            bounds(harness.window, "row-w1", cx),
            bounds(harness.window, "bubble", cx),
            bounds(harness.window, "day-pill", cx),
            bounds(harness.window, "composer", cx),
        ]
    };

    // On by default: behind the whole thread, exactly.
    assert_eq!(
        cx.update(|cx| settings::get(cx).wallpaper),
        WallpaperChoice::Pattern
    );
    let with = measure(cx);
    assert_eq!(bounds(harness.window, "wallpaper", cx), with[0]);

    // Off, from Settings > Appearance: gone, and nothing has moved.
    click(harness.window, "settings", cx);
    click(harness.window, "settings-appearance", cx);
    click(harness.window, "wallpaper-plain", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "wallpaper", cx));
    assert_eq!(measure(cx), with);
    assert_eq!(Settings::load(&file).wallpaper, WallpaperChoice::Plain);

    // And back, in both themes.
    cx.update(|cx| {
        settings::update(cx, |settings| {
            settings.wallpaper = WallpaperChoice::Pattern;
            settings.theme = ThemeChoice::Dark;
        })
    });
    cx.run_until_parked();
    assert_eq!(bounds(harness.window, "wallpaper", cx), with[0]);
    assert_eq!(measure(cx), with);
}

// ----- bubble styles ------------------------------------------------------------

#[gpui_kit::test]
fn the_bubble_style_is_chosen_in_settings_and_only_changes_colours(cx: &mut TestAppContext) {
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
    say(&harness, cx, "mine", None, "a message of my own", 1);
    let bubble = bounds(harness.window, "bubble", cx);
    let own = |cx: &mut TestAppContext| cx.update(|cx| theme::palette(cx).bubble_out);

    // Brand is what a new install has.
    assert_eq!(
        cx.update(|cx| settings::get(cx).bubbles),
        BubbleStyle::Brand
    );
    assert_eq!(
        own(cx),
        Palette::light().with_bubbles(BubbleStyle::Brand).bubble_out
    );

    click(harness.window, "settings", cx);
    click(harness.window, "settings-appearance", cx);
    for (button, style) in [
        ("bubbles-lime", BubbleStyle::Lime),
        ("bubbles-neutral", BubbleStyle::Neutral),
        ("bubbles-brand", BubbleStyle::Brand),
        ("bubbles-lime", BubbleStyle::Lime),
    ] {
        click(harness.window, button, cx);
        assert_eq!(own(cx), Palette::light().with_bubbles(style).bubble_out);
        assert_eq!(Settings::load(&file).bubbles, style);
    }
    press(harness.window, "escape", cx);
    assert_eq!(
        bounds(harness.window, "bubble", cx),
        bubble,
        "nothing moved"
    );

    // The style holds across themes, and is what the next start uses.
    cx.update(|cx| settings::update(cx, |settings| settings.theme = ThemeChoice::Dark));
    cx.run_until_parked();
    assert_eq!(
        own(cx),
        Palette::dark().with_bubbles(BubbleStyle::Lime).bubble_out
    );
    cx.update(|cx| {
        theme::set_bubbles(BubbleStyle::Brand);
        settings::init(Some(file.clone()), Some(Appearance::Light), cx);
        assert_eq!(theme::bubbles(), BubbleStyle::Lime);
    });
}

// ----- the header -----------------------------------------------------------------

#[gpui_kit::test]
fn the_header_says_who_is_in_a_group_and_a_contacts_number(cx: &mut TestAppContext) {
    let harness = open_group(cx);
    let subtitle = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            shell
                .open
                .as_ref()
                .unwrap()
                .subtitle
                .clone()
                .map(|line| line.to_string())
        })
    };
    // A group: its participants by name, the account last, as "You".
    let people = subtitle(cx).expect("a group has a second line");
    assert!(people.ends_with(", You"), "{people}");
    assert!(people.split(", ").count() >= 3, "{people}");
    assert!(!people.contains("contact:"), "never an id: {people}");

    for step in theme::SCALE_STEPS {
        set_scale(cx, step);
        let info = bounds(harness.window, "header-info", cx);
        let title = bounds(harness.window, "header-title", cx);
        let line = bounds(harness.window, "header-subtitle", cx);
        let avatar = bounds(harness.window, "header-avatar", cx);
        assert!(within(title, info) && within(line, info), "{step}%");
        assert!(title.bottom() <= line.top() + px(1.), "{step}%");
        assert!(avatar.right() <= info.left(), "{step}%");
        assert_eq!(avatar.size.height, metrics::AVATAR_MEDIUM(), "{step}%");
        assert!(info.bottom() <= metrics::HEADER_HEIGHT(), "{step}%");
    }
    set_scale(cx, 100);

    // A person: their number, when the chat goes by a name. Nothing is
    // said about when they were last seen: the store does not know.
    let direct = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(chat) if chat.kind == client_provider::ChatKind::Direct => {
                    Some(chat.clone())
                }
                _ => None,
            })
            .expect("a direct chat")
    });
    cx.update(|cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.open_chat(direct.id.clone(), None, cx))
    });
    cx.run_until_parked();
    let known = harness
        .engine
        .store()
        .person(
            &direct.account_id,
            None,
            &ContactId::new(direct.id.as_str()),
        )
        .unwrap();
    match known.phone {
        Some(phone) => {
            assert_eq!(subtitle(cx), Some(crate::format::phone(&phone)));
            assert!(shows(harness.window, "header-subtitle", cx));
        }
        None => assert_eq!(subtitle(cx), None),
    }
}

// ----- the composer ---------------------------------------------------------------

#[gpui_kit::test]
fn the_composer_is_one_rounded_field_with_its_controls_on_one_line(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let input: ElementId = ("input", composer.entity_id()).into();
    for step in theme::SCALE_STEPS {
        set_scale(cx, step);
        // Not focused yet at the first step; then it is.
        let selector = if shows(harness.window, "composer-field", cx) {
            "composer-field"
        } else {
            "composer-field-focused"
        };
        let bar = bounds(harness.window, "composer", cx);
        let field = bounds(harness.window, selector, cx);
        let attach = bounds(harness.window, "attach", cx);
        let emoji = bounds(harness.window, "emoji", cx);
        let typed = bounds(harness.window, "composer-text", cx);
        let voice = bounds(harness.window, "voice", cx);
        assert!(within(field, bar), "{step}%");
        for (what, part) in [
            ("attach", attach),
            ("emoji", emoji),
            ("text", typed),
            ("voice", voice),
        ] {
            assert!(
                within(part, field),
                "{step}%: {what} {part:?} outside {field:?}"
            );
            assert!(
                near(centre_y(part), centre_y(field)),
                "{step}%: {what} {part:?} is off the middle of {field:?}"
            );
        }
        assert!(
            attach.right() <= emoji.left() && emoji.right() <= typed.left(),
            "{step}%"
        );
        assert!(typed.right() <= voice.left(), "{step}%");
        // A comfortable height: the control and room around it. The
        // corners are rounder than half of that would need a square.
        assert!(
            field.size.height >= metrics::CONTROL() + px(8.),
            "{step}%: {field:?}"
        );
        assert!(
            metrics::COMPOSER_RADIUS() * 2. <= field.size.height,
            "{step}%"
        );
        // The same room left and right of the controls.
        assert!(
            near(attach.left() - field.left(), field.right() - voice.right()),
            "{step}%"
        );

        // Focus puts the accent on the field's outline and moves nothing.
        cx.update_window(harness.window.into(), |_, window, cx| {
            window.click(input.clone(), cx)
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(
            bounds(harness.window, "composer-field-focused", cx),
            field,
            "{step}%"
        );

        // With text, the send button takes the microphone's place.
        type_text(harness.window, "hello", cx);
        let send = bounds(harness.window, "send", cx);
        assert_eq!(send, voice, "{step}%");
        assert!(!shows(harness.window, "voice", cx), "{step}%");

        // Several lines: the field grows, the controls stay on its last
        // line.
        for _ in 0..3 {
            press(harness.window, "shift-enter", cx);
            type_text(harness.window, "more", cx);
        }
        let grown = bounds(harness.window, "composer-field-focused", cx);
        let (attach, send) = (
            bounds(harness.window, "attach", cx),
            bounds(harness.window, "send", cx),
        );
        assert!(
            grown.size.height > field.size.height + metrics::LINE_BODY() * 2.,
            "{step}%: {field:?} -> {grown:?}"
        );
        assert!(near(attach.bottom(), send.bottom()), "{step}%");
        assert!(
            near(
                grown.bottom() - send.bottom(),
                field.bottom() - voice.bottom()
            ),
            "{step}%: the controls stay at the foot"
        );
        assert!(within(send, grown) && within(attach, grown), "{step}%");
        press(harness.window, "ctrl-a", cx);
        press(harness.window, "backspace", cx);
    }
    set_scale(cx, 100);
}

// ----- voice notes ------------------------------------------------------------------

#[gpui_kit::test]
fn a_voice_note_is_one_compact_row_at_every_interface_size(cx: &mut TestAppContext) {
    let (harness, _tape, _notes) = with_voice_notes(cx);
    let mut widths = Vec::new();
    for step in theme::SCALE_STEPS {
        set_scale(cx, step);
        let bubble = bounds(harness.window, "bubble", cx);
        let row = bounds(harness.window, "audio-row", cx);
        let button = bounds(harness.window, "audio-button", cx);
        let wave = bounds(harness.window, "audio-wave", cx);
        let time = bounds(harness.window, "audio-time", cx);
        let mic = bounds(harness.window, "audio-mic", cx);
        // The button, then the waveform, on one line, centred on it.
        assert_eq!(button.size.width, metrics::AUDIO_BUTTON(), "{step}%");
        assert_eq!(button.size.height, metrics::AUDIO_BUTTON(), "{step}%");
        assert!(near(centre_y(button), centre_y(row)), "{step}%");
        assert!(near(centre_y(wave), centre_y(row)), "{step}%");
        assert!(button.right() < wave.left(), "{step}%");
        for (what, part) in [
            ("row", row),
            ("button", button),
            ("wave", wave),
            ("time", time),
        ] {
            assert!(
                within(part, bubble),
                "{step}%: {what} {part:?} outside {bubble:?}"
            );
        }
        // Under them, the length's line with the microphone of a voice
        // note, starting at the bubble's content edge.
        assert!(time.top() >= row.bottom(), "{step}%");
        assert!(within(mic, time) && near(mic.left(), row.left()), "{step}%");
        // A fixed width, the same padding on both sides.
        assert_eq!(row.size.width, metrics::AUDIO_WIDTH(), "{step}%");
        assert!(
            near(row.left() - bubble.left(), bubble.right() - row.right()),
            "{step}%"
        );
        // Every voice note of the chat is as wide: the three rows end at
        // the same edge.
        for n in 1..=3 {
            let line = bounds_of(harness.window, &format!("row-m-voice-{n}"), cx);
            assert!(line.size.height > metrics::AUDIO_BUTTON(), "{step}%");
        }
        widths.push(bubble.size.width);
    }
    // It follows the interface size.
    for pair in widths.windows(2) {
        assert!(pair[1] > pair[0], "{widths:?}");
    }
    set_scale(cx, 100);
}

#[gpui_kit::test]
fn the_real_waveform_takes_the_place_of_the_placeholder_without_moving_anything(
    cx: &mut TestAppContext,
) {
    let (harness, _tape, notes) = with_voice_notes(cx);
    let last = url_of(&notes[2]);
    // Not here yet: the button offers the download, the waveform is the
    // even placeholder, and no length is made up.
    assert!(shows(harness.window, "audio-download", cx));
    assert!(shows(harness.window, "audio-wave-idle", cx));
    assert!(!shows(harness.window, "audio-wave-real", cx));
    assert!(!shows(harness.window, "audio-knob", cx));
    assert!(!shows(harness.window, "audio-speed", cx));
    let before = [
        bounds(harness.window, "bubble", cx),
        bounds(harness.window, "audio-row", cx),
        bounds(harness.window, "audio-button", cx),
        bounds(harness.window, "audio-wave", cx),
        bounds(harness.window, "row-m-voice-3", cx),
    ];
    let idle = bounds(harness.window, "audio-wave-idle", cx);

    // A click fetches, decodes and plays: the same box, now with the
    // real waveform, the position on it and the speed beside it.
    click(harness.window, "audio-button", cx);
    until(&harness, cx, |shell| {
        matches!(shell.audio.player.playback(&last), Playback::Playing(_))
    });
    let after = [
        bounds(harness.window, "bubble", cx),
        bounds(harness.window, "audio-row", cx),
        bounds(harness.window, "audio-button", cx),
        bounds(harness.window, "audio-wave", cx),
        bounds(harness.window, "row-m-voice-3", cx),
    ];
    assert_eq!(after, before, "nothing changed size or place");
    assert_eq!(bounds(harness.window, "audio-wave-real", cx), idle);
    assert!(shows(harness.window, "audio-pause", cx));
    let wave = after[3];
    let knob = bounds(harness.window, "audio-knob", cx);
    assert!(near(centre_y(knob), centre_y(wave)));
    assert!(
        knob.left() >= wave.left() - knob.size.width
            && knob.right() <= wave.right() + knob.size.width
    );
    let speed = bounds(harness.window, "audio-speed", cx);
    assert!(within(speed, after[1]) && speed.left() >= wave.right());
    assert!(near(centre_y(speed), centre_y(wave)));
    assert!(near(speed.right(), after[1].right()));
}

#[gpui_kit::test]
fn in_a_group_a_sticker_stands_beside_its_senders_avatar_without_a_name(cx: &mut TestAppContext) {
    use client_provider::{Media, MediaKind, MediaRef};
    let harness = open_group(cx);
    let (account, chat) = open_ids(&harness, cx);
    say(&harness, cx, "z1", Some((ZOE, "Zoe")), "before", 0);
    // An animated sticker and a still one take the same box: what moves
    // is painted inside it (`tests/animated.rs`).
    let mut media = Media::new(MediaKind::Sticker);
    media.mime_type = Some("image/webp".into());
    media.source = Some(MediaRef::new("https://media.example/group-sticker.webp"));
    let mut message = text(&account, &chat, "st1", Some((LEO, "Leo")), "", 1);
    message.content = MessageContent::Media(media);
    arrive(&harness, cx, message);

    let row = bounds(harness.window, "row-st1", cx);
    let column = bounds(harness.window, "sticker", cx);
    let frame = bounds(harness.window, "media-box", cx);
    let avatar = bounds(harness.window, "sender-avatar", cx);
    assert!(within(column, row) && within(avatar, row) && within(frame, column));
    assert_eq!(frame.size.width, frame.size.height, "the sticker's square");
    assert_eq!(avatar.size.width, metrics::AVATAR_SMALL());
    assert!(near(column.left() - avatar.right(), metrics::AVATAR_GAP()));
    assert!(near(avatar.bottom(), column.bottom()));
    assert!(near(column.top() - row.top(), metrics::RUN_GAP()));
    // No bubble, so no name line: the avatar says who.
    let named =
        VisualTestContext::from_window(harness.window.into(), cx).debug_bounds("sender-line");
    assert!(named.is_none_or(|line| !intersects(line, row)));
    assert!(row_of(&harness, cx, "st1").2.is_some());
}

#[gpui_kit::test]
fn in_a_group_the_newest_bubble_is_as_large_as_any_other(cx: &mut TestAppContext) {
    let harness = open_group(cx);
    let one = "So I built this, maybe it brings people.";
    let three = "So I built this, maybe it brings people. It took a while, and there is \
                 more to do, but the part that matters is there and it works well enough \
                 to show around, which is what I wanted before the weekend anyway.";
    let ana = Some(("ana@example", "Ana"));
    let luis = Some(("luis@example", "Luis"));
    for (n, (from, body)) in [(None, one), (ana, one), (None, three), (ana, three)]
        .into_iter()
        .enumerate()
    {
        let at = 500 + n as i64 * 10;
        let (newest, after) = (format!("g{n}"), format!("h{n}"));
        say(&harness, cx, &newest, from, body, at);
        let as_newest = bounds_of(harness.window, &format!("bubble-{newest}"), cx);
        // Somebody else answers: it is one message among others.
        say(&harness, cx, &after, luis, "ok", at + 5);
        let as_other = bounds_of(harness.window, &format!("bubble-{newest}"), cx);
        assert!(
            (as_newest.size.height - as_other.size.height).abs() <= px(1.)
                && (as_newest.size.width - as_other.size.width).abs() <= px(1.),
            "message {n}: {as_newest:?} as the newest, {as_other:?} after"
        );
        // One line of text, under the sender's name when it is not ours.
        let lines = if from.is_some() { 2. } else { 1. };
        if body == one {
            assert!(
                as_newest.size.height <= px(21.) * lines + px(18.),
                "message {n}: {as_newest:?}"
            );
        }
        let thread = bounds(harness.window, "thread", cx);
        assert!(
            as_newest.size.width < thread.size.width * 0.8,
            "message {n}"
        );
    }
}
