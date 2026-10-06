//! The profile and group panels, driven with clicks and keys on top of
//! the mock provider. No file dialog is ever opened: pictures come from a
//! fake picker.

use super::{click, open, png, press, shows, type_text, until, Harness};
use crate::pictures::tests::FakePicker;
use crate::ui::shell::{Overlay, ShellOptions};
use crate::ui::social::{GroupTab, Target};
use client_core::StoredParticipant;
use client_provider::{
    AccountId, ChatId, ChatKind, Contact, ContactId, GroupRole, ProviderError, ProviderEvent,
};
use gpui_kit::{AppContext as _, TestAppContext};
use std::rc::Rc;

fn rejected(code: &str, message: &str) -> ProviderError {
    ProviderError::Rejected {
        code: code.into(),
        message: message.into(),
    }
}

fn account(harness: &Harness) -> AccountId {
    harness.engine.store().accounts().unwrap()[0].id.clone()
}

/// Opens the conversation `chat` in the window.
fn open_chat(harness: &Harness, cx: &mut TestAppContext, chat: &ChatId) {
    cx.update(|cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.open_chat(chat.clone(), None, cx))
    });
    harness.settle(cx);
}

/// The first group of the first number, opened in the window with its
/// info panel showing.
fn open_group_info(harness: &Harness, cx: &mut TestAppContext) -> (AccountId, ChatId) {
    let account = account(harness);
    let group = harness
        .engine
        .store()
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Group)
        .expect("the mock has groups")
        .id;
    open_chat(harness, cx, &group);
    click(harness.window, "header-info", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "group-info", cx));
    (account, group)
}

fn participants(harness: &Harness, account: &AccountId, group: &ChatId) -> Vec<StoredParticipant> {
    harness
        .engine
        .store()
        .group_participants(account, group, None, 100)
        .unwrap()
}

/// What the window is telling the user did not work, if anything.
fn problem(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .problem
            .as_ref()
            .map(|problem| problem.to_string())
            .unwrap_or_default()
    })
}

/// Turns the wheel over the element `selector`: the panel under it scrolls
/// down by `pixels`.
fn scroll_down(harness: &Harness, selector: &'static str, pixels: f32, cx: &mut TestAppContext) {
    let mut visual = gpui_kit::VisualTestContext::from_window(harness.window.into(), cx);
    let over = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is not on screen"));
    visual.simulate_event(gpui_kit::ScrollWheelEvent {
        position: over.center(),
        delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
            gpui_kit::px(0.),
            gpui_kit::px(-pixels),
        )),
        ..Default::default()
    });
    visual.run_until_parked();
}

fn copied(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

/// Three people in the first number's address book.
fn with_contacts(harness: &Harness, cx: &mut TestAppContext) -> AccountId {
    let account = account(harness);
    let person = |phone: &str, name: &str| {
        let mut contact = Contact::new(account.clone(), ContactId::new(phone));
        contact.phone = Some(phone.to_owned());
        contact.saved_name = Some(name.to_owned());
        contact
    };
    harness.mock.set_contacts(
        &account,
        vec![
            person("+584140000001", "Ana Pérez"),
            person("+584140000002", "Bruno"),
            person("+584140000003", "Carla"),
        ],
    );
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.run_until_parked();
    account
}

#[gpui_kit::test]
fn a_profile_opens_from_the_header_and_shows_what_whatsapp_and_the_store_know(
    cx: &mut TestAppContext,
) {
    let harness = open(cx, ShellOptions::default());
    let account = with_contacts(&harness, cx);
    let ana = ContactId::new("+584140000001");
    harness.mock.set_about(&ana, "At the beach");
    let chat = harness
        .engine
        .chat_with_contact(
            &harness
                .engine
                .store()
                .contact(&account, &ana)
                .unwrap()
                .unwrap(),
        )
        .unwrap();
    open_chat(&harness, cx, &chat);

    // The header's name (and its picture) open the profile. What the
    // store holds is there in the first frame; WhatsApp is asked behind.
    click(harness.window, "header-info", cx);
    assert!(shows(harness.window, "contact-info", cx));
    assert!(shows(harness.window, "profile-number", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::ContactInfo);
        assert_eq!(
            shell.social.target(),
            Some(&Target::Contact {
                account: account.clone(),
                contact: ana.clone()
            })
        );
    });
    assert_eq!(harness.mock.social_call_count("lookup_contact"), 0);
    harness.settle(cx);
    assert_eq!(harness.mock.social_call_count("lookup_contact"), 1);
    let known = harness
        .engine
        .store()
        .contact(&account, &ana)
        .unwrap()
        .unwrap();
    assert_eq!(known.about.as_deref(), Some("At the beach"));
    assert_eq!(
        known.saved_name.as_deref(),
        Some("Ana Pérez"),
        "the name stays"
    );
    assert!(shows(harness.window, "profile-about", cx));

    // Copy the number: as it is dialled, not as it is displayed.
    click(harness.window, "profile-copy", cx);
    assert_eq!(copied(cx).as_deref(), Some("+584140000001"));
    assert!(shows(harness.window, "info-note", cx));

    // Search in chat closes the panel and opens the chat's search.
    click(harness.window, "profile-search", cx);
    assert!(!shows(harness.window, "contact-info", cx));
    assert!(shows(harness.window, "thread-search", cx));

    // The picture opens it too; Escape and a look again ask nobody.
    click(harness.window, "header-avatar", cx);
    assert!(shows(harness.window, "contact-info", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "contact-info", cx));
    click(harness.window, "header-info", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.social_call_count("lookup_contact"), 1);
}

#[gpui_kit::test]
fn groups_in_common_lead_to_the_group_and_back(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = account(&harness);
    let store = harness.engine.store().clone();
    // Someone the number both chats with and shares a group with.
    let groups = harness
        .runtime
        .block_on(async {
            use client_provider::Provider as _;
            harness.mock.list_groups(&account).await
        })
        .unwrap();
    let person = groups
        .iter()
        .flat_map(|group| &group.participants)
        .map(|participant| participant.contact.clone())
        .find(|contact| {
            matches!(
                store.chat(&account, &ChatId::new(contact.as_str())),
                Ok(Some(_))
            )
        })
        .expect("someone in a group has a chat of their own");
    open_chat(&harness, cx, &ChatId::new(person.as_str()));

    click(harness.window, "header-info", cx);
    // The refresh listed this number's groups: no waiting for them.
    harness.settle(cx);
    assert!(shows(harness.window, "common-group-0", cx));
    assert!(!shows(harness.window, "common-groups-none", cx));

    // The group opens in the same panel, and the arrow goes back.
    click(harness.window, "common-group-0", cx);
    assert!(shows(harness.window, "group-info", cx));
    assert!(!shows(harness.window, "profile", cx));
    click(harness.window, "info-back", cx);
    assert!(shows(harness.window, "profile", cx));
    // Escape leaves the panel from its first page.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "contact-info", cx));
}

#[gpui_kit::test]
fn blocking_asks_first_shows_at_once_and_a_refusal_puts_it_back(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = account(&harness);
    let store = harness.engine.store().clone();
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Direct)
        .unwrap()
        .id;
    let contact = ContactId::new(chat.as_str());
    open_chat(&harness, cx, &chat);
    click(harness.window, "header-info", cx);
    harness.settle(cx);

    // Never at once: the panel asks, and Cancel leaves everything as is.
    click(harness.window, "profile-block", cx);
    assert!(shows(harness.window, "profile-block-question", cx));
    assert!(!store.is_blocked(&account, &contact).unwrap());
    click(harness.window, "profile-block-cancel", cx);
    assert!(!shows(harness.window, "profile-block-question", cx));
    assert_eq!(harness.mock.social_call_count("set_blocked"), 0);

    // Escape answers the question too, without closing the panel.
    click(harness.window, "profile-block", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "profile-block-question", cx));
    assert!(shows(harness.window, "contact-info", cx));

    // Confirmed: blocked on screen before the provider has heard of it.
    click(harness.window, "profile-block", cx);
    click(harness.window, "profile-block-confirm", cx);
    assert!(store.is_blocked(&account, &contact).unwrap());
    assert!(shows(harness.window, "profile-blocked", cx));
    assert!(harness.mock.blocked(&account).is_empty());
    harness.settle(cx);
    assert_eq!(harness.mock.blocked(&account), vec![contact.clone()]);

    // Unblocking is refused: she is blocked again, and the window says why.
    harness
        .mock
        .fail_next_social([rejected("whatsapp_error", "WhatsApp failed the operation.")]);
    click(harness.window, "profile-block", cx);
    click(harness.window, "profile-block-confirm", cx);
    assert!(!store.is_blocked(&account, &contact).unwrap());
    harness.settle(cx);
    assert!(store.is_blocked(&account, &contact).unwrap());
    assert!(shows(harness.window, "profile-blocked", cx));
    let said = problem(&harness, cx);
    assert!(
        said.contains("was not unblocked") && said.contains("WhatsApp failed"),
        "{said}"
    );
}

#[gpui_kit::test]
fn a_group_is_created_from_the_menu_with_people_from_the_address_book(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = with_contacts(&harness, cx);
    let picker = FakePicker::default();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            shell.set_picture_picker(Rc::new(picker.clone()))
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let photo = dir.path().join("photo.png");
    std::fs::write(&photo, png(320, 200)).unwrap();

    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-new-group", cx);
    assert!(shows(harness.window, "new-group", cx));
    // Nothing typed, nobody chosen: said, and nobody is asked.
    click(harness.window, "new-group-create", cx);
    assert!(shows(harness.window, "new-group-error", cx));
    assert_eq!(harness.mock.social_call_count("create_group"), 0);

    // The name goes where the keyboard already is.
    type_text(harness.window, "Road trip", cx);
    // The address book is searched as it is typed in, from the store.
    assert!(shows(harness.window, "people-contact-2", cx));
    click(harness.window, "people-contact-0", cx);
    click(harness.window, "people-contact-1", cx);
    assert!(shows(harness.window, "people-chosen", cx));
    // A second click takes someone out again.
    click(harness.window, "people-contact-1", cx);
    click(harness.window, "people-contact-2", cx);
    let chosen = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let form = harness.shell.read(cx).social.new_group.as_ref().unwrap();
            form.people
                .chosen
                .iter()
                .map(|contact| contact.display_name())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(chosen(cx), ["Ana Pérez", "Carla"]);

    // A picture: the dialog is dismissed once (nothing changes), then
    // answers a file, which is shown in the form.
    picker.answer(None);
    click(harness.window, "new-group-picture", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "new-group-picture-shown", cx));
    picker.answer(Some(photo));
    click(harness.window, "new-group-picture", cx);
    until(&harness, cx, |shell| {
        shell
            .social
            .new_group
            .as_ref()
            .is_some_and(|form| form.picture.is_some())
    });
    assert!(shows(harness.window, "new-group-picture-shown", cx));
    assert_eq!(picker.asked(), 2);

    // The provider refuses: said in the form, which keeps what was typed.
    harness
        .mock
        .fail_next_social([rejected("whatsapp_error", "WhatsApp failed the operation.")]);
    click(harness.window, "new-group-create", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "new-group-error", cx));
    assert_eq!(chosen(cx), ["Ana Pérez", "Carla"]);

    // Again: one group, opened as a conversation at once.
    click(harness.window, "new-group-create", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.social_call_count("create_group"), 2);
    let created = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.social.new_group.is_none());
        let open = shell.open.as_ref().expect("the new group is open");
        assert_eq!(open.chat.title, "Road trip");
        assert_eq!(open.chat.kind, ChatKind::Group);
        open.chat.id.clone()
    });
    let theirs = harness.mock.group(&account, &created).unwrap();
    assert_eq!(theirs.subject, "Road trip");
    assert_eq!(
        theirs.participants.len(),
        3,
        "the number and the two chosen"
    );
    let stored = harness
        .engine
        .store()
        .group(&account, &created)
        .unwrap()
        .unwrap();
    assert_eq!(stored.my_role, Some(GroupRole::Owner));
    // The picture follows in the background.
    until(&harness, cx, |_| {
        harness.mock.social_call_count("set_group_picture") == 1
    });
    harness.settle(cx);
    let picture = harness
        .engine
        .store()
        .avatar(&account, &created)
        .unwrap()
        .unwrap();
    assert!(picture.image.is_some());
}

#[gpui_kit::test]
fn a_participant_is_promoted_at_once_and_what_only_admins_do_is_only_offered_to_admins(
    cx: &mut TestAppContext,
) {
    let harness = open(cx, ShellOptions::default());
    let (account, group) = open_group_info(&harness, cx);
    let store = harness.engine.store().clone();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.social.tab, GroupTab::Overview);
    });
    assert!(shows(harness.window, "group-role", cx));
    assert!(shows(harness.window, "group-tab-manage", cx));

    // Participants: the number first, then the owner, each with a badge.
    click(harness.window, "group-tab-participants", cx);
    let people = participants(&harness, &account, &group);
    assert!(people[0].me && people[0].role == GroupRole::Admin);
    assert_eq!(people[1].role, GroupRole::Owner);
    assert!(shows(harness.window, "participant-admin-0", cx));
    assert!(shows(harness.window, "participant-admin-1", cx));
    assert!(!shows(harness.window, "participant-admin-2", cx));
    let member = people[2].contact.clone();
    assert_eq!(people[2].role, GroupRole::Member);

    // The owner cannot be demoted or removed: only looked at.
    click(harness.window, "participant-1", cx);
    assert!(shows(harness.window, "participant-profile", cx));
    assert!(!shows(harness.window, "participant-demote", cx));
    assert!(!shows(harness.window, "participant-remove", cx));

    // A member is made an admin: the badge is there before the provider
    // has answered.
    click(harness.window, "participant-2", cx);
    click(harness.window, "participant-promote", cx);
    assert_eq!(
        store.participant_role(&account, &group, &member).unwrap(),
        Some(GroupRole::Admin)
    );
    assert!(shows(harness.window, "participant-admin-2", cx));
    assert_eq!(harness.mock.social_call_count("change_participants"), 0);
    harness.settle(cx);
    let theirs = |contact: &ContactId| {
        harness
            .mock
            .group(&account, &group)
            .unwrap()
            .participants
            .into_iter()
            .find(|p| &p.contact == contact)
            .map(|p| p.role)
    };
    assert_eq!(theirs(&member), Some(GroupRole::Admin));

    // The number stopped being an admin on the phone, and this window has
    // not heard yet. The provider refuses: the admin is an admin again,
    // and the window says why.
    let me = harness.mock.self_contact(&account);
    harness
        .mock
        .set_group_role(&account, &group, &me, Some(GroupRole::Member));
    click(harness.window, "participant-2", cx);
    click(harness.window, "participant-demote", cx);
    assert_eq!(
        store.participant_role(&account, &group, &member).unwrap(),
        Some(GroupRole::Member)
    );
    harness.settle(cx);
    assert_eq!(
        store.participant_role(&account, &group, &member).unwrap(),
        Some(GroupRole::Admin),
        "put back"
    );
    assert!(problem(&harness, cx).contains("Only admins of the group can do that"));

    // The provider says the group changed: it is read again, and what
    // only admins can do is no longer offered.
    harness
        .engine
        .apply_event(ProviderEvent::GroupChanged {
            account_id: account.clone(),
            group_id: group.clone(),
        })
        .unwrap();
    harness.settle(cx);
    assert!(!shows(harness.window, "group-tab-manage", cx));
    assert!(!shows(harness.window, "group-add-people", cx));
    let at = participants(&harness, &account, &group)
        .iter()
        .position(|person| person.contact == member)
        .unwrap();
    let row: &'static str = Box::leak(format!("participant-{at}").into_boxed_str());
    click(harness.window, row, cx);
    assert!(shows(harness.window, "participant-profile", cx));
    assert!(!shows(harness.window, "participant-demote", cx));
    assert!(!shows(harness.window, "participant-remove", cx));

    // A participant's profile opens from the list, and Escape comes back.
    click(harness.window, "participant-profile", cx);
    assert!(shows(harness.window, "profile", cx));
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "group-info", cx));
}

#[gpui_kit::test]
fn a_group_is_managed_people_are_added_and_requests_answered(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let contacts_of = with_contacts(&harness, cx);
    let asker = ContactId::new("+584140000003");
    let shy = ContactId::new("+584140000002");
    harness.mock.restrict_adds(&shy);
    let (account, group) = {
        let account = contacts_of;
        let group = harness
            .engine
            .store()
            .chats(&account, None)
            .unwrap()
            .into_iter()
            .find(|chat| chat.kind == ChatKind::Group)
            .unwrap()
            .id;
        harness.mock.add_join_request(&account, &group, &asker);
        open_group_info(&harness, cx)
    };
    let store = harness.engine.store().clone();
    let details = || store.group(&account, &group).unwrap().unwrap();
    let before = details().participant_count;

    // The name is edited in place: Enter saves, and the chat list and the
    // header follow at once.
    click(harness.window, "group-tab-manage", cx);
    click(harness.window, "group-edit-subject", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Renamed here", cx);
    press(harness.window, "enter", cx);
    assert_eq!(details().group.subject, "Renamed here");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.open.as_ref().unwrap().chat.title, "Renamed here");
        assert!(shell.social.edit.is_none());
    });
    harness.settle(cx);
    assert_eq!(
        harness.mock.group(&account, &group).unwrap().subject,
        "Renamed here"
    );

    // Settings switch at once, and reach the provider behind.
    // (Further down the page: the panel scrolls.)
    scroll_down(&harness, "contact-info", 300., cx);
    assert!(!details().group.announce);
    click(harness.window, "setting-announce", cx);
    assert!(details().group.announce);
    harness.settle(cx);
    assert!(harness.mock.group(&account, &group).unwrap().announce);

    // The invite link is asked for on a click, copied, and reset only
    // after the question.
    click(harness.window, "invite-show", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "invite-link", cx));
    let link = details().invite_link.unwrap();
    click(harness.window, "invite-copy", cx);
    assert_eq!(copied(cx), Some(link.clone()));
    click(harness.window, "invite-reset", cx);
    assert!(shows(harness.window, "invite-reset-question", cx));
    assert_eq!(details().invite_link, Some(link.clone()));
    click(harness.window, "invite-reset-question-confirm", cx);
    harness.settle(cx);
    assert_ne!(details().invite_link, Some(link));
    assert!(!shows(harness.window, "invite-reset-question", cx));

    // Someone asked to join: approved from the list.
    click(harness.window, "group-tab-participants", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "join-approve-0", cx));
    click(harness.window, "join-approve-0", cx);
    assert!(!shows(harness.window, "join-requests", cx), "gone at once");
    harness.settle(cx);
    assert_eq!(
        store.participant_role(&account, &group, &asker).unwrap(),
        Some(GroupRole::Member)
    );

    // Adding: the address book, without who is already in. One of the
    // two does not let themselves be added, which is said by name.
    click(harness.window, "group-add-people", cx);
    assert!(shows(harness.window, "group-adding", cx));
    assert!(shows(harness.window, "people-contact-1", cx));
    assert!(
        !shows(harness.window, "people-contact-2", cx),
        "Carla is in the group already"
    );
    click(harness.window, "people-contact-0", cx);
    click(harness.window, "people-contact-1", cx);
    click(harness.window, "people-submit", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "group-adding", cx));
    assert_eq!(
        store
            .participant_role(&account, &group, &ContactId::new("+584140000001"))
            .unwrap(),
        Some(GroupRole::Member)
    );
    assert_eq!(
        store.participant_role(&account, &group, &shy).unwrap(),
        None
    );
    assert_eq!(details().participant_count, before + 2);
    assert!(shows(harness.window, "info-error", cx));
    cx.update(|cx| {
        let said = harness.shell.read(cx).social.error.clone().unwrap();
        assert!(
            said.contains("Bruno") && said.contains("privacy settings"),
            "{said}"
        );
    });

    // Removing asks first.
    let at = participants(&harness, &account, &group)
        .iter()
        .position(|person| person.contact == asker)
        .unwrap();
    let row: &'static str = Box::leak(format!("participant-{at}").into_boxed_str());
    click(harness.window, row, cx);
    click(harness.window, "participant-remove", cx);
    assert!(shows(harness.window, "participant-remove-question", cx));
    assert!(store
        .participant_role(&account, &group, &asker)
        .unwrap()
        .is_some());
    click(harness.window, "participant-remove-confirm", cx);
    assert_eq!(
        store.participant_role(&account, &group, &asker).unwrap(),
        None
    );
    harness.settle(cx);
    assert!(harness
        .mock
        .group(&account, &group)
        .unwrap()
        .participants
        .iter()
        .all(|p| p.contact != asker));
}

#[gpui_kit::test]
fn leaving_a_group_asks_first_and_a_failure_leaves_nothing_half_done(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let (account, group) = open_group_info(&harness, cx);
    let store = harness.engine.store().clone();
    let departed = || store.group(&account, &group).unwrap().unwrap().departed;

    click(harness.window, "group-leave", cx);
    assert!(shows(harness.window, "group-leave-question", cx));
    assert_eq!(harness.mock.social_call_count("leave_group"), 0);
    // Escape answers "no": the panel stays, the group too.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "group-leave-question", cx));
    assert!(shows(harness.window, "group-info", cx));

    // The provider fails: said in the panel, and nothing is left.
    harness
        .mock
        .fail_next_social([rejected("whatsapp_error", "WhatsApp failed the operation.")]);
    click(harness.window, "group-leave", cx);
    click(harness.window, "group-leave-confirm", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "info-error", cx));
    assert!(!departed());
    assert!(
        shows(harness.window, "group-leave-question", cx),
        "to try again"
    );

    // Again: gone. The chat stays in the list, the panel says so, and
    // there is nothing left to leave or manage.
    click(harness.window, "group-leave-confirm", cx);
    harness.settle(cx);
    assert!(departed());
    assert!(!shows(harness.window, "info-error", cx));
    assert!(!shows(harness.window, "group-leave", cx));
    assert!(!shows(harness.window, "group-tab-manage", cx));
    let me = harness.mock.self_contact(&account);
    assert!(harness
        .mock
        .group(&account, &group)
        .unwrap()
        .participants
        .iter()
        .all(|p| p.contact != me));
    assert!(store.chat(&account, &group).unwrap().is_some());
}

#[gpui_kit::test]
fn each_number_has_its_own_whatsapp_profile_edited_from_settings(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let accounts = harness.engine.store().accounts().unwrap();
    let (first, second) = (accounts[0].id.clone(), accounts[1].id.clone());
    let store = harness.engine.store().clone();
    let picker = FakePicker::default();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            shell.set_picture_picker(Rc::new(picker.clone()))
        })
    });
    let dir = tempfile::tempdir().unwrap();
    let photo = dir.path().join("me.png");
    std::fs::write(&photo, png(240, 240)).unwrap();

    // Settings > Account lists each number with its own profile.
    press(harness.window, "ctrl-,", cx);
    assert!(shows(harness.window, "number-profile-0", cx));
    assert!(shows(harness.window, "number-profile-1", cx));
    click(harness.window, "number-profile-0", cx);
    assert!(shows(harness.window, "own-profile", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::OwnProfile);
        assert_eq!(shell.social.own.as_ref().unwrap().account, first);
    });
    // What the provider can read arrives behind the panel.
    harness.settle(cx);
    assert_eq!(
        store.own_profile(&first).unwrap().about.as_deref(),
        Some("Available")
    );

    // The About text: edited in place, shown at once.
    click(harness.window, "own-edit-about", cx);
    assert!(shows(harness.window, "own-about-input", cx));
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "On holiday", cx);
    press(harness.window, "enter", cx);
    assert_eq!(
        store.own_profile(&first).unwrap().about.as_deref(),
        Some("On holiday")
    );
    assert!(!shows(harness.window, "own-about-input", cx));
    harness.settle(cx);
    assert_eq!(
        harness.mock.own_profile_of(&first).about.as_deref(),
        Some("On holiday")
    );
    assert_eq!(
        harness.mock.own_profile_of(&second).about,
        None,
        "one number only"
    );

    // A name WhatsApp does not take never leaves this computer; one the
    // provider refuses is put back, and said.
    click(harness.window, "own-edit-name", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(
        harness.window,
        "A name that is much too long for WhatsApp",
        cx,
    );
    click(harness.window, "edit-save", cx);
    assert!(shows(harness.window, "edit-error", cx));
    assert_eq!(harness.mock.social_call_count("update_profile"), 1);
    // The keyboard is back in the field, to correct it.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Vic", cx);
    harness
        .mock
        .fail_next_social([rejected("whatsapp_error", "WhatsApp failed the operation.")]);
    click(harness.window, "edit-save", cx);
    assert_eq!(
        store.own_profile(&first).unwrap().name.as_deref(),
        Some("Vic")
    );
    harness.settle(cx);
    assert_eq!(
        store.own_profile(&first).unwrap().name.as_deref(),
        Some("Personal")
    );
    let said = problem(&harness, cx);
    assert!(said.starts_with("Your name was not changed"), "{said}");

    // The picture, through the dialog (a fake one here), and its removal
    // after a question.
    let subject = harness.engine.own_subject(&first).unwrap();
    assert!(!shows(harness.window, "own-picture-remove", cx));
    picker.answer(Some(photo));
    click(harness.window, "own-picture-change", cx);
    until(&harness, cx, |_| {
        matches!(store.avatar(&first, &subject), Ok(Some(stored)) if stored.image.is_some())
            && harness.mock.social_call_count("set_profile_picture") == 1
    });
    harness.settle(cx);
    assert!(shows(harness.window, "own-picture-remove", cx));
    click(harness.window, "own-picture-remove", cx);
    assert!(shows(harness.window, "own-picture-question", cx));
    click(harness.window, "own-picture-remove-confirm", cx);
    harness.settle(cx);
    assert!(store
        .avatar(&first, &subject)
        .unwrap()
        .unwrap()
        .image
        .is_none());
    assert!(!shows(harness.window, "own-picture-remove", cx));

    // A file that is no picture is said, and nothing is sent.
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "not a picture").unwrap();
    picker.answer(Some(notes));
    click(harness.window, "own-picture-change", cx);
    until(&harness, cx, |shell| shell.social.error.is_some());
    assert!(shows(harness.window, "info-error", cx));
    assert_eq!(harness.mock.social_call_count("set_profile_picture"), 2);

    // Escape goes back to the settings it came from.
    press(harness.window, "escape", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Settings));
    press(harness.window, "escape", cx);

    // Opened from anywhere else (the entry point other menus use), it
    // closes to where it was opened from.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_own_profile(second.clone(), window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "own-profile", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.social.own.as_ref().unwrap().account, second);
    });
    press(harness.window, "escape", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}

#[gpui_kit::test]
fn a_provider_that_cannot_read_the_own_profile_shows_what_is_known_and_says_so(
    cx: &mut TestAppContext,
) {
    let harness = open(cx, ShellOptions::default());
    let first = account(&harness);
    harness.mock.unreadable_profile(true);
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_own_profile(first.clone(), window, cx)
        })
    })
    .unwrap();
    harness.settle(cx);
    // Nothing is invented: the fields say they are not known, and can
    // still be set.
    let profile = harness.engine.store().own_profile(&first).unwrap();
    assert_eq!((profile.name, profile.about), (None, None));
    assert!(shows(harness.window, "own-name", cx));
    assert!(shows(harness.window, "own-edit-about", cx));
    click(harness.window, "own-edit-about", cx);
    type_text(harness.window, "Busy", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(
        harness
            .engine
            .store()
            .own_profile(&first)
            .unwrap()
            .about
            .as_deref(),
        Some("Busy")
    );
}
