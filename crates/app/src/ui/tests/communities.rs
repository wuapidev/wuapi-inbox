//! Communities in the window: the filter that groups the chat list under
//! each community, the keyboard over it, and what the group panel says
//! about a community and the groups it links.

use super::*;
use crate::ui::social::Target;
use client_provider::{AccountId, ChatId, Contact};
use provider_mock::{
    COMMUNITY, COMMUNITY_ACCOUNT, COMMUNITY_ANNOUNCEMENTS, COMMUNITY_GROUPS, COMMUNITY_NAME,
    COMMUNITY_UNJOINED,
};

/// The rows of the list: a community's heading by its name, a chat by
/// its id.
fn listed(harness: &Harness, cx: &mut TestAppContext) -> Vec<(&'static str, String)> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .map(|row| match row {
                ListRow::Chat(chat) => ("chat", chat.id.to_string()),
                ListRow::Community(community) => ("community", community.name.to_string()),
                ListRow::Section(name) => ("section", (*name).to_owned()),
                ListRow::Hit(_) => ("hit", String::new()),
                ListRow::Contact(contact) => ("contact", contact.id.to_string()),
            })
            .collect()
    })
}

fn filter(harness: &Harness, cx: &mut TestAppContext) -> ChatFilter {
    cx.update(|cx| harness.shell.read(cx).filter)
}

fn cursor(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_cursor
            .as_ref()
            .map(|chat| chat.to_string())
    })
}

fn open_id(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .map(|open| open.chat.id.to_string())
    })
}

/// The community's rows as the demo data has them: its heading, its
/// announcement group, then its groups by how lately they were written
/// in.
fn the_community() -> Vec<(&'static str, String)> {
    vec![
        ("community", COMMUNITY_NAME.to_owned()),
        ("chat", COMMUNITY_ANNOUNCEMENTS.to_owned()),
        ("chat", COMMUNITY_GROUPS[0].to_owned()),
        ("chat", COMMUNITY_GROUPS[1].to_owned()),
    ]
}

#[gpui_kit::test]
fn the_communities_filter_groups_the_chats_under_their_community(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // The list everybody starts on has no headings and is not regrouped.
    let all = listed(&harness, cx);
    assert!(all.iter().all(|(kind, _)| *kind == "chat"));
    assert!(!shows(harness.window, "community-heading-0", cx));

    assert!(shows(harness.window, "filter-communities", cx));
    click(harness.window, "filter-communities", cx);
    assert_eq!(filter(&harness, cx), ChatFilter::Communities);
    assert_eq!(listed(&harness, cx), the_community());

    // The heading is a row of the list like any other: same height, and
    // it names the community.
    let heading = bounds(harness.window, "community-heading-0", cx);
    let first = bounds_of(
        harness.window,
        &format!("chat-{COMMUNITY_ANNOUNCEMENTS}"),
        cx,
    );
    assert_eq!(heading.size.height, first.size.height);
    assert_eq!(heading.bottom(), first.top(), "the chats follow it");
    // The announcement group is marked as what it is.
    assert!(shows(harness.window, "row-announcements-1", cx));
    assert!(!shows(harness.window, "row-announcements-2", cx));

    // A chat of the community opens from there like anywhere else.
    click(harness.window, "row-name-2", cx);
    harness.settle(cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[0]));

    // Back on every chat, the list is as it was.
    click(harness.window, "filter-all", cx);
    assert_eq!(listed(&harness, cx), all);
}

#[gpui_kit::test]
fn the_filter_is_offered_only_when_a_chat_is_in_a_community(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::quiet();
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    for group in [
        COMMUNITY_ANNOUNCEMENTS,
        COMMUNITY_GROUPS[0],
        COMMUNITY_GROUPS[1],
    ] {
        mock.unlink_on_phone(&account, &ChatId::new(group));
    }
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    assert!(shows(harness.window, "filter-groups", cx));
    assert!(!shows(harness.window, "filter-communities", cx));

    // Tab does not stop on a filter that is not there.
    focus_composer(&harness, cx);
    press(harness.window, "escape", cx);
    for expected in [
        ChatFilter::Unread,
        ChatFilter::Groups,
        ChatFilter::Archived,
        ChatFilter::All,
    ] {
        press(harness.window, "tab", cx);
        assert_eq!(filter(&harness, cx), expected);
    }

    // A group is linked on a phone: the filter appears by itself.
    let (community, class) = (ChatId::new(COMMUNITY), ChatId::new(COMMUNITY_GROUPS[0]));
    harness.mock.link_on_phone(&account, &community, &class);
    harness
        .engine
        .apply_event(client_provider::ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community,
            groups: vec![class],
        })
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "filter-communities", cx));
    click(harness.window, "filter-communities", cx);
    assert_eq!(
        listed(&harness, cx),
        [
            ("community", COMMUNITY_NAME.to_owned()),
            ("chat", COMMUNITY_GROUPS[0].to_owned()),
        ]
    );

    // And when the last chat leaves its community while the filter is
    // on, the list goes back to every chat instead of staying empty.
    harness
        .mock
        .unlink_on_phone(&account, &ChatId::new(COMMUNITY_GROUPS[0]));
    harness
        .engine
        .apply_event(client_provider::ProviderEvent::CommunityChanged {
            account_id: account,
            community_id: ChatId::new(COMMUNITY),
            groups: vec![ChatId::new(COMMUNITY_GROUPS[0])],
        })
        .unwrap();
    harness.settle(cx);
    assert!(!shows(harness.window, "filter-communities", cx));
    assert_eq!(filter(&harness, cx), ChatFilter::All);
}

#[gpui_kit::test]
fn the_keyboard_walks_the_community_row_and_its_chats(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    focus_composer(&harness, cx);
    press(harness.window, "escape", cx);

    // Tab walks the filters: the communities come after the groups.
    for expected in [
        ChatFilter::Unread,
        ChatFilter::Groups,
        ChatFilter::Communities,
    ] {
        press(harness.window, "tab", cx);
        assert_eq!(filter(&harness, cx), expected);
    }
    assert_eq!(listed(&harness, cx), the_community());

    // Home is the community's own row, which the keyboard stops on; the
    // arrows go from row to row and stop at the ends.
    press(harness.window, "home", cx);
    assert_eq!(cursor(&harness, cx).as_deref(), Some(COMMUNITY));
    press(harness.window, "up", cx);
    assert_eq!(cursor(&harness, cx).as_deref(), Some(COMMUNITY));
    press(harness.window, "down", cx);
    assert_eq!(
        cursor(&harness, cx).as_deref(),
        Some(COMMUNITY_ANNOUNCEMENTS)
    );
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[0]));
    press(harness.window, "end", cx);
    assert_eq!(cursor(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[1]));
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[1]));
    let ring = bounds(harness.window, "list-focus", cx);
    let row = bounds_of(harness.window, &format!("chat-{}", COMMUNITY_GROUPS[1]), cx);
    assert!(within(ring, row), "{ring:?} in {row:?}");

    // Enter opens the chat the outline is on.
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[1]));

    // One more Tab is the archive, then every chat again.
    press(harness.window, "escape", cx);
    press(harness.window, "tab", cx);
    assert_eq!(filter(&harness, cx), ChatFilter::Archived);
    press(harness.window, "tab", cx);
    assert_eq!(filter(&harness, cx), ChatFilter::All);
}

/// Opens the conversation `chat` with its info panel showing.
fn open_info_of(harness: &Harness, cx: &mut TestAppContext, chat: &str) {
    let chat = ChatId::new(chat);
    cx.update(|cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.open_chat(chat, None, cx))
    });
    harness.settle(cx);
    click(harness.window, "header-info", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "group-info", cx));
}

fn panel_group(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| match harness.shell.read(cx).social.target() {
        Some(Target::Group { group, .. }) => Some(group.to_string()),
        _ => None,
    })
}

#[gpui_kit::test]
fn a_groups_panel_says_its_community_and_leads_to_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());

    // A group of a community says which one; only the announcement group
    // is marked as that.
    open_info_of(&harness, cx, COMMUNITY_GROUPS[0]);
    assert!(shows(harness.window, "group-community", cx));
    assert!(!shows(harness.window, "group-announcements", cx));
    assert!(!shows(harness.window, "group-subgroups", cx));
    press(harness.window, "escape", cx);
    open_info_of(&harness, cx, COMMUNITY_ANNOUNCEMENTS);
    assert!(shows(harness.window, "group-community", cx));
    assert!(shows(harness.window, "group-announcements", cx));

    // The community's name leads to the community, which lists the
    // groups it links.
    click(harness.window, "group-community", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert!(shows(harness.window, "group-subgroups", cx));
    assert!(!shows(harness.window, "group-community", cx));
    for name in ["subgroup-0", "subgroup-1", "subgroup-2", "subgroup-3"] {
        assert!(shows(harness.window, name, cx), "{name}");
    }

    // A group of the list that has a chat opens it.
    click(harness.window, "subgroup-1", cx);
    harness.settle(cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[0]));
    assert!(!shows(harness.window, "group-info", cx), "the panel closed");
}

#[gpui_kit::test]
fn a_plain_group_says_nothing_about_communities(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_info_of(&harness, cx, provider_mock::SHOWCASE_CHAT);
    assert!(!shows(harness.window, "group-community", cx));
    assert!(!shows(harness.window, "group-announcements", cx));
    assert!(!shows(harness.window, "group-subgroups", cx));
}

/// A group the community links that this number has no chat with is
/// listed, and a click on it opens nothing.
#[gpui_kit::test]
fn a_linked_group_without_a_chat_is_listed_and_opens_nothing(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    let community = ChatId::new(COMMUNITY);
    open_info_of(&harness, cx, COMMUNITY_ANNOUNCEMENTS);
    click(harness.window, "group-community", cx);
    // Let the panel read the community first, then store it as a later
    // read would, with a group that has no chat here.
    harness.settle(cx);
    let mut read = harness.mock.fetch_group_now(&account, &community);
    read.subgroups.push(client_provider::Subgroup {
        id: ChatId::new("group:elsewhere"),
        subject: "Teachers".into(),
        announcements: false,
    });
    harness
        .engine
        .store()
        .put_group(&read, client_provider::Timestamp::now())
        .unwrap();
    harness.settle(cx);
    assert!(shows(harness.window, "subgroup-4", cx));
    click_in_panel(&harness, "subgroup-4", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert_eq!(
        open_id(&harness, cx).as_deref(),
        Some(COMMUNITY_ANNOUNCEMENTS)
    );
}

// ----- the community's own screen ---------------------------------------------

/// Turns the wheel over the community's panel until `selector` is well
/// inside it, as a person would, then clicks it. A click on what is
/// clipped misses the panel and closes it.
fn click_in_panel(harness: &Harness, selector: &'static str, cx: &mut TestAppContext) {
    for _ in 0..12 {
        let mut visual = gpui_kit::VisualTestContext::from_window(harness.window.into(), cx);
        let at = visual
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("`{selector}` is not on screen"));
        if at.bottom() < gpui_kit::px(700.) && at.top() > gpui_kit::px(160.) {
            break;
        }
        let over = visual.debug_bounds("group-info").expect("the panel");
        visual.simulate_event(gpui_kit::ScrollWheelEvent {
            position: over.center(),
            delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                gpui_kit::px(0.),
                gpui_kit::px(-120.),
            )),
            ..Default::default()
        });
        visual.run_until_parked();
    }
    click(harness.window, selector, cx);
}

/// Opens the community from its heading in the Communities view.
fn open_community(harness: &Harness, cx: &mut TestAppContext) {
    click(harness.window, "filter-communities", cx);
    click(harness.window, "community-heading-0", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "group-info", cx));
    assert_eq!(panel_group(harness, cx).as_deref(), Some(COMMUNITY));
}

fn community_chat_of(harness: &Harness, group: &str) -> Option<String> {
    let store = harness.engine.store();
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    store
        .chat(&account, &ChatId::new(group))
        .unwrap()
        .and_then(|chat| chat.community)
        .map(|community| community.id.to_string())
}

#[gpui_kit::test]
fn a_community_heading_opens_the_community_with_all_its_groups(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_community(&harness, cx);

    // Four groups, the announcement group first; the one this number has
    // not joined says so and opens nothing.
    for name in ["subgroup-0", "subgroup-1", "subgroup-2", "subgroup-3"] {
        assert!(shows(harness.window, name, cx), "{name}");
    }
    assert!(!shows(harness.window, "subgroup-4", cx));
    assert!(shows(harness.window, "subgroup-announcements-0", cx));
    assert!(shows(harness.window, "subgroup-unjoined-3", cx));
    assert!(!shows(harness.window, "subgroup-unjoined-1", cx));
    click_in_panel(&harness, "subgroup-3", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert!(shows(harness.window, "group-info", cx));
    // One that has a chat opens it.
    click_in_panel(&harness, "subgroup-1", cx);
    harness.settle(cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(COMMUNITY_GROUPS[0]));
}

#[gpui_kit::test]
fn a_community_lists_its_members_when_asked_with_every_state_said(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_community(&harness, cx);
    assert_eq!(harness.mock.social_call_count("community_participants"), 0);

    // The call fails: said, with a way to ask again.
    harness
        .mock
        .fail_next_social([client_provider::ProviderError::Rejected {
            code: "whatsapp_error".into(),
            message: "WhatsApp failed the operation.".into(),
        }]);
    click(harness.window, "group-tab-members", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "community-members-error", cx));
    click(harness.window, "community-members-retry", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "community-members-error", cx));
    assert!(shows(harness.window, "community-member-0", cx));
    assert_eq!(harness.mock.social_call_count("community_participants"), 2);
}

#[gpui_kit::test]
fn an_admin_takes_a_group_out_of_its_community_after_asking(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_community(&harness, cx);

    // Never for the announcement group.
    assert!(!shows(harness.window, "subgroup-remove-0", cx));
    click_in_panel(&harness, "subgroup-remove-1", cx);
    assert!(shows(harness.window, "subgroup-unlink-question", cx));
    assert_eq!(harness.mock.social_call_count("unlink_subgroup"), 0);
    click_in_panel(&harness, "subgroup-unlink-cancel", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "subgroup-unlink-question", cx));
    assert_eq!(harness.mock.social_call_count("unlink_subgroup"), 0);

    click_in_panel(&harness, "subgroup-remove-1", cx);
    click_in_panel(&harness, "subgroup-unlink-confirm", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.social_call_count("unlink_subgroup"), 1);
    assert_eq!(community_chat_of(&harness, COMMUNITY_GROUPS[0]), None);
    assert!(!shows(harness.window, "subgroup-unlink-question", cx));
    assert!(
        !shows(harness.window, "subgroup-3", cx),
        "three groups are left"
    );

    // A refusal is said, and the question stays to try again.
    harness
        .mock
        .fail_next_social([client_provider::ProviderError::Rejected {
            code: client_provider::refusal::NOT_ADMIN.into(),
            message: "Only admins of the group can do that.".into(),
        }]);
    click_in_panel(&harness, "subgroup-remove-1", cx);
    click_in_panel(&harness, "subgroup-unlink-confirm", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "info-error", cx));
    assert!(shows(harness.window, "subgroup-unlink-question", cx));
}

#[gpui_kit::test]
fn an_admin_adds_a_group_they_administer_that_is_in_no_community(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_community(&harness, cx);
    click_in_panel(&harness, "community-add-group", cx);
    assert!(shows(harness.window, "group-linking", cx));
    // Only groups that are in no community are offered.
    assert!(shows(harness.window, "link-choice-0", cx));
    let offered = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .engine
            .store()
            .groups_to_link(&AccountId::new(COMMUNITY_ACCOUNT))
            .unwrap()
    });
    assert!(offered.iter().all(|(id, _)| ![
        COMMUNITY,
        COMMUNITY_ANNOUNCEMENTS,
        COMMUNITY_GROUPS[0]
    ]
    .contains(&id.as_str())));
    let first = offered[0].0.to_string();

    click_in_panel(&harness, "link-choice-0", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.social_call_count("link_subgroup"), 1);
    assert_eq!(
        community_chat_of(&harness, &first).as_deref(),
        Some(COMMUNITY)
    );
    assert!(
        !shows(harness.window, "group-linking", cx),
        "back on the community"
    );
    assert!(shows(harness.window, "subgroup-4", cx), "it is listed now");
}

#[gpui_kit::test]
fn the_picker_says_when_there_is_nothing_to_add_and_can_be_left(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_community(&harness, cx);
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    for (id, _) in harness.engine.store().groups_to_link(&account).unwrap() {
        harness
            .engine
            .store()
            .set_departed(&account, &id, true)
            .unwrap();
    }
    click_in_panel(&harness, "community-add-group", cx);
    assert!(shows(harness.window, "link-none", cx));
    assert!(!shows(harness.window, "link-choice-0", cx));
    click_in_panel(&harness, "link-cancel", cx);
    assert!(!shows(harness.window, "group-linking", cx));
    assert!(shows(harness.window, "group-subgroups", cx));
}

#[gpui_kit::test]
fn a_group_is_created_inside_the_community_or_refused_plainly(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    let mut ana = Contact::new(
        account.clone(),
        client_provider::ContactId::new("+584140000001"),
    );
    ana.phone = Some("+584140000001".into());
    ana.saved_name = Some("Ana".into());
    harness.mock.set_contacts(&account, vec![ana]);
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.run_until_parked();
    open_community(&harness, cx);

    click_in_panel(&harness, "community-new-group", cx);
    assert!(shows(harness.window, "new-group", cx));
    type_text(harness.window, "Volunteers", cx);
    harness.settle(cx);
    click(harness.window, "people-contact-0", cx);

    // The number's engine cannot do it: said, and the form stays.
    harness
        .mock
        .fail_next_social([client_provider::ProviderError::Rejected {
            code: "not_supported".into(),
            message: "This number cannot create a group inside a community yet.".into(),
        }]);
    click(harness.window, "new-group-create", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "new-group-error", cx));
    assert!(shows(harness.window, "new-group", cx));

    click(harness.window, "new-group-create", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "new-group", cx));
    // Back on the community, which lists the new group.
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert!(shows(harness.window, "subgroup-4", cx));
}

#[gpui_kit::test]
fn a_community_is_created_from_the_menu_without_participants(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-new-community", cx);
    assert!(shows(harness.window, "new-group", cx));
    // No participants to choose for a community.
    assert!(!shows(harness.window, "people-contact-0", cx));
    type_text(harness.window, "Neighbours", cx);
    click(harness.window, "new-group-create", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "new-group", cx));
    let made = cx.update(|cx| match harness.shell.read(cx).social.target() {
        Some(Target::Group { group, .. }) => Some(group.clone()),
        _ => None,
    });
    let made = made.expect("the new community is shown");
    assert_ne!(made.as_str(), COMMUNITY);
    assert!(shows(harness.window, "group-subgroups", cx));
}

#[gpui_kit::test]
fn what_only_an_admin_does_is_not_offered_to_anyone_else(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    let me = harness.mock.self_contact(&account);
    harness.mock.set_group_role(
        &account,
        &ChatId::new(COMMUNITY),
        &me,
        Some(client_provider::GroupRole::Member),
    );
    open_community(&harness, cx);
    harness.settle(cx);
    assert!(shows(harness.window, "group-subgroups", cx));
    for hidden in [
        "community-add-group",
        "community-new-group",
        "subgroup-remove-1",
    ] {
        assert!(!shows(harness.window, hidden, cx), "{hidden}");
    }
    let _ = COMMUNITY_UNJOINED;
}

// ----- the community row ----------------------------------------------------

#[test]
fn the_row_and_the_panel_say_how_many_groups_a_community_has() {
    use crate::ui::chat_list::community_subtitle;
    assert_eq!(community_subtitle(None), "Community");
    assert_eq!(community_subtitle(Some(1)), "Community · 1 group");
    assert_eq!(community_subtitle(Some(4)), "Community · 4 groups");
    use crate::ui::groups::title_line;
    assert_eq!(
        title_line("Community", Some(4), Some(30)),
        "Community · 4 groups"
    );
    assert_eq!(title_line("Community", Some(0), Some(30)), "Community");
    assert_eq!(title_line("Group", None, Some(1)), "Group · 1 participant");
    assert_eq!(title_line("Group", None, Some(5)), "Group · 5 participants");
}

#[gpui_kit::test]
fn a_community_is_a_real_row_that_opens_by_click_and_stays_selected(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "filter-communities", cx);

    // A row like a chat's, whose first line is the name: nothing empty
    // above it (the heading used to be a row tall with its label at the
    // bottom).
    let row = bounds(harness.window, "community-heading-0", cx);
    let name = bounds(harness.window, "community-name-0", cx);
    let first = bounds_of(
        harness.window,
        &format!("chat-{COMMUNITY_ANNOUNCEMENTS}"),
        cx,
    );
    assert_eq!(row.size.height, first.size.height);
    assert!(
        name.top() - row.top() < row.size.height / 2.,
        "{name:?} in {row:?}"
    );
    assert!(shows(harness.window, "community-subtitle-0", cx));
    assert!(!shows(harness.window, "community-selected-0", cx));

    click(harness.window, "community-heading-0", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert!(shows(harness.window, "community-selected-0", cx));
}

#[gpui_kit::test]
fn enter_on_the_community_row_opens_the_community(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    focus_composer(&harness, cx);
    press(harness.window, "escape", cx);
    for _ in 0..3 {
        press(harness.window, "tab", cx);
    }
    assert_eq!(filter(&harness, cx), ChatFilter::Communities);
    press(harness.window, "home", cx);
    assert!(shows(harness.window, "list-focus", cx));
    let ring = bounds(harness.window, "list-focus", cx);
    let row = bounds(harness.window, "community-heading-0", cx);
    assert!(within(ring, row), "{ring:?} in {row:?}");
    // The row is not a chat: the chat that was open stays.
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
    assert!(shows(harness.window, "community-selected-0", cx));
}

#[gpui_kit::test]
fn the_community_in_a_groups_panel_is_a_row_that_leads_to_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    open_info_of(&harness, cx, COMMUNITY_GROUPS[0]);
    let row = bounds(harness.window, "group-community", cx);
    let panel = bounds(harness.window, "group-info", cx);
    // A row across the panel, not a word in it.
    assert!(
        row.size.width > panel.size.width / 2.,
        "{row:?} in {panel:?}"
    );
    assert!(row.size.height >= gpui_kit::px(30.));
    click(harness.window, "group-community", cx);
    harness.settle(cx);
    assert_eq!(panel_group(&harness, cx).as_deref(), Some(COMMUNITY));
}
