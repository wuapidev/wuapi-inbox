//! Profiles and groups: the store's tables and the engine's reads and
//! changes, against the mock provider.

use super::{engine_for, settle};
use crate::*;
use client_provider::{
    refusal, AccountId, BusinessProfile, ChatId, ChatKind, Contact, ContactId, Group, GroupChange,
    GroupParticipant, GroupRole, JoinRequest, OwnProfile, ParticipantChange, ProfileChange,
    Provider, ProviderError, ProviderEvent, Timestamp,
};
use provider_mock::MockProvider;

fn rejected(code: &str, message: &str) -> ProviderError {
    ProviderError::Rejected {
        code: code.into(),
        message: message.into(),
    }
}

fn dropped() -> ProviderError {
    ProviderError::Transient("eof".into())
}

/// What the user was told since `changes` was last drained.
fn said(changes: &mut ChangeListener) -> Vec<String> {
    let mut messages = Vec::new();
    while let Some(change) = changes.try_next() {
        if let StoreChange::Problem { message } = change {
            messages.push(message);
        }
    }
    messages
}

/// Longer than a group's details are taken to be current: what a refresh
/// listed is read again after it.
const STALE: std::time::Duration = std::time::Duration::from_secs(6 * 60);

/// An engine over the mock's world, with the account and one of its
/// groups.
async fn with_group(mock: &MockProvider) -> (SyncEngine, AccountId, ChatId) {
    let engine = engine_for(mock);
    engine.refresh().await.unwrap();
    let store = engine.store();
    let account = store.accounts().unwrap().remove(0).id;
    let group = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Group)
        .expect("the mock has groups")
        .id;
    (engine, account, group)
}

fn member(mock: &MockProvider, account: &AccountId, group: &ChatId, role: GroupRole) -> ContactId {
    mock.group(account, group)
        .unwrap()
        .participants
        .into_iter()
        .find(|p| p.role == role && p.contact != mock.self_contact(account))
        .expect("the group has such a participant")
        .contact
}

fn jpeg() -> Vec<u8> {
    let image = image::RgbImage::from_pixel(200, 120, image::Rgb([200, 30, 30]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    profile_picture(&out).unwrap()
}

// ----- the store ------------------------------------------------------------

fn a_group(account: &AccountId) -> Group {
    let participant = |id: &str, name: Option<&str>, role| GroupParticipant {
        contact: ContactId::new(id),
        name: name.map(str::to_owned),
        role,
    };
    Group {
        id: ChatId::new("g1"),
        account_id: account.clone(),
        subject: "Family".into(),
        description: Some("Sunday lunch".into()),
        owner: Some(ContactId::new("+5841")),
        created_at: Some(Timestamp::from_millis(1_700_000_000_000)),
        community: false,
        community_id: None,
        announcements: false,
        announce: false,
        locked: true,
        join_approval: None,
        members_can_add: None,
        participants: vec![
            participant("+5843", Some("zed"), GroupRole::Member),
            participant("me", None, GroupRole::Admin),
            participant("+5841", Some("olga"), GroupRole::Owner),
            participant("+5842", None, GroupRole::Member),
        ],
        subgroups: Vec::new(),
    }
}

#[test]
fn a_group_is_kept_with_its_participants_named_from_the_address_book() {
    let store = Store::open_in_memory().unwrap();
    super::seed_account(&store);
    let account = super::account();
    let mut saved = Contact::new(account.clone(), ContactId::new("+5842"));
    saved.saved_name = Some("Ana".into());
    saved.phone = Some("+5842".into());
    store.upsert_contact(&saved).unwrap();

    let group = a_group(&account);
    store.put_group(&group, Timestamp::from_millis(5)).unwrap();
    let stored = store.group(&account, &group.id).unwrap().unwrap();
    assert_eq!(stored.group.subject, "Family");
    assert_eq!(stored.group.description.as_deref(), Some("Sunday lunch"));
    assert_eq!(stored.participant_count, 4);
    assert_eq!(stored.my_role, Some(GroupRole::Admin));
    assert!(stored.group.locked && !stored.departed);
    assert_eq!(stored.group.join_approval, None, "not readable: not known");

    // The account first, then the owner, then by name: the saved name
    // wins over the one the provider gave.
    let listed = store
        .group_participants(&account, &group.id, None, 10)
        .unwrap();
    let names: Vec<(&str, bool, GroupRole)> = listed
        .iter()
        .map(|p| (p.name.as_str(), p.me, p.role))
        .collect();
    assert_eq!(
        names,
        [
            ("me", true, GroupRole::Admin),
            ("olga", false, GroupRole::Owner),
            ("Ana", false, GroupRole::Member),
            ("zed", false, GroupRole::Member),
        ]
    );
    assert!(listed[2].saved && !listed[3].saved);
    let found = store
        .group_participants(&account, &group.id, Some("an"), 10)
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].contact.as_str(), "+5842");

    // Groups in common come from the same table.
    let common = store
        .groups_in_common(&account, &ContactId::new("+5842"))
        .unwrap();
    assert_eq!(common, vec![(ChatId::new("g1"), "Family".to_owned())]);
    assert!(store
        .groups_in_common(&account, &ContactId::new("+5899"))
        .unwrap()
        .is_empty());

    // A newer copy replaces the participants; a setting that cannot be
    // read keeps what was set from here, and so does the invite link.
    store
        .apply_group_change(&account, &group.id, &GroupChange::JoinApproval(true))
        .unwrap();
    store
        .set_invite_link(&account, &group.id, Some("https://chat.whatsapp.com/x"))
        .unwrap();
    let mut smaller = group.clone();
    smaller.participants.truncate(2);
    store
        .put_group(&smaller, Timestamp::from_millis(9))
        .unwrap();
    let stored = store.group(&account, &group.id).unwrap().unwrap();
    assert_eq!(stored.participant_count, 2);
    assert_eq!(stored.group.join_approval, Some(true));
    assert_eq!(
        stored.invite_link.as_deref(),
        Some("https://chat.whatsapp.com/x")
    );
    assert_eq!(stored.fetched_at, Timestamp::from_millis(9));

    // A list without the account says nothing about having left: the
    // provider may name it by an id unknown here. Leaving does.
    let mut without_me = group.clone();
    without_me
        .participants
        .retain(|p| p.contact.as_str() != "me");
    store
        .put_group(&without_me, Timestamp::from_millis(10))
        .unwrap();
    let stored = store.group(&account, &group.id).unwrap().unwrap();
    assert!(!stored.departed);
    assert_eq!(stored.my_role, None);
    store.put_group(&group, Timestamp::from_millis(11)).unwrap();
    store.set_departed(&account, &group.id, true).unwrap();
    let stored = store.group(&account, &group.id).unwrap().unwrap();
    assert!(stored.departed);
    assert_eq!(stored.my_role, None);
    assert_eq!(stored.participant_count, 3);

    // Forgetting the account forgets all of it.
    store.remove_account(&account).unwrap();
    assert!(store.group(&account, &group.id).unwrap().is_none());
    assert!(store
        .group_participants(&account, &group.id, None, 10)
        .unwrap()
        .is_empty());
}

#[test]
fn a_change_to_a_group_answers_how_to_undo_it() {
    let store = Store::open_in_memory().unwrap();
    super::seed_account(&store);
    let account = super::account();
    let mut chat = super::chat("g1", "Family");
    chat.kind = ChatKind::Group;
    store.upsert_chat(&chat, false).unwrap();
    let group = a_group(&account);
    store.put_group(&group, Timestamp::from_millis(1)).unwrap();

    let undo = store
        .apply_group_change(&account, &group.id, &GroupChange::Subject("Clan".into()))
        .unwrap();
    assert_eq!(undo, Some(GroupChange::Subject("Family".into())));
    assert_eq!(
        store.chat(&account, &group.id).unwrap().unwrap().title,
        "Clan"
    );
    let undo = store
        .apply_group_change(
            &account,
            &group.id,
            &GroupChange::Description(String::new()),
        )
        .unwrap();
    assert_eq!(undo, Some(GroupChange::Description("Sunday lunch".into())));
    assert_eq!(
        store
            .group(&account, &group.id)
            .unwrap()
            .unwrap()
            .group
            .description,
        None
    );
    // A setting whose old value was never known has no undo: it is
    // forgotten instead.
    let change = GroupChange::MembersCanAdd(false);
    assert_eq!(
        store
            .apply_group_change(&account, &group.id, &change)
            .unwrap(),
        None
    );
    store
        .forget_group_setting(&account, &group.id, &change)
        .unwrap();
    assert_eq!(
        store
            .group(&account, &group.id)
            .unwrap()
            .unwrap()
            .group
            .members_can_add,
        None
    );
    // A group the store never saw: nothing to change.
    assert_eq!(
        store
            .apply_group_change(&account, &ChatId::new("nope"), &GroupChange::Locked(true))
            .unwrap(),
        None
    );
}

#[test]
fn the_blocklist_requests_and_profiles_are_kept() {
    let store = Store::open_in_memory().unwrap();
    super::seed_account(&store);
    let account = super::account();
    let (ana, bruno) = (ContactId::new("+5842"), ContactId::new("lid:77"));

    store
        .put_blocked(&account, &[ana.clone(), bruno.clone()])
        .unwrap();
    assert!(store.is_blocked(&account, &ana).unwrap());
    store.set_blocked(&account, &ana, false).unwrap();
    assert!(!store.is_blocked(&account, &ana).unwrap());
    assert!(store.is_blocked(&account, &bruno).unwrap());

    let group = ChatId::new("g1");
    let request = JoinRequest {
        contact: ana.clone(),
        requested_at: Some(Timestamp::from_millis(3)),
    };
    store
        .put_join_requests(&account, &group, std::slice::from_ref(&request))
        .unwrap();
    assert_eq!(
        store.join_requests(&account, &group).unwrap(),
        vec![request.clone()]
    );
    store
        .set_join_request(&account, &group, &request, false)
        .unwrap();
    assert!(store.join_requests(&account, &group).unwrap().is_empty());

    let profile = BusinessProfile {
        description: Some("Bread".into()),
        websites: vec!["https://bakery.example".into()],
        ..Default::default()
    };
    store
        .put_business_profile(&account, &ana, Some(&profile), Timestamp::from_millis(1))
        .unwrap();
    assert_eq!(
        store.business_profile(&account, &ana).unwrap(),
        Some(profile)
    );
    store
        .put_business_profile(&account, &ana, None, Timestamp::from_millis(2))
        .unwrap();
    assert_eq!(store.business_profile(&account, &ana).unwrap(), None);

    // The own profile: a field nobody knows is `None`, and a read that
    // does not carry a field keeps what is held.
    assert_eq!(store.own_profile(&account).unwrap(), OwnProfile::default());
    let named = OwnProfile {
        name: Some("Vic".into()),
        about: None,
    };
    store.put_own_profile(&account, &named, false).unwrap();
    let about = OwnProfile {
        name: None,
        about: Some("Busy".into()),
    };
    store.put_own_profile(&account, &about, false).unwrap();
    let both = store.own_profile(&account).unwrap();
    assert_eq!(
        (both.name.as_deref(), both.about.as_deref()),
        (Some("Vic"), Some("Busy"))
    );
    store.put_own_profile(&account, &named, true).unwrap();
    assert_eq!(store.own_profile(&account).unwrap().about, None);
}

// ----- the engine: reading ------------------------------------------------

#[tokio::test(start_paused = true)]
async fn a_group_is_read_when_it_is_opened_and_again_when_it_changes() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    // The refresh listed every group: this one is held, and opening its
    // chat reads nothing while that copy is fresh.
    let stored = store
        .group(&account, &group)
        .unwrap()
        .expect("the group was listed");
    assert_eq!(stored.my_role, Some(GroupRole::Admin));
    assert!(stored.participant_count >= 3);
    engine.open_chat(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 0);

    // Once it is not, opening the chat reads the group, behind the
    // messages.
    tokio::time::sleep(STALE).await;
    engine.open_chat(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 1);

    // Looking again does not ask again while the copy is fresh.
    engine.want_group(&account, &group);
    engine.open_chat(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 1);

    // Someone is made an admin on a phone: the provider says the group
    // changed, and it is read again at once.
    let someone = member(&mock, &account, &group, GroupRole::Member);
    mock.set_group_role(&account, &group, &someone, Some(GroupRole::Admin));
    engine
        .apply_event(ProviderEvent::GroupChanged {
            account_id: account.clone(),
            group_id: group.clone(),
        })
        .unwrap();
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 2);
    assert_eq!(
        store.participant_role(&account, &group, &someone).unwrap(),
        Some(GroupRole::Admin)
    );

    // A provider that only polls shows a rename through its chat list:
    // that is a group change too.
    mock.rename_group(&account, &group, "Renamed on the phone");
    engine
        .apply_event(ProviderEvent::ChatUpdated(
            mock.chat(&account, &group).unwrap(),
        ))
        .unwrap();
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 3);
    assert_eq!(
        store
            .group(&account, &group)
            .unwrap()
            .unwrap()
            .group
            .subject,
        "Renamed on the phone"
    );

    // A change to a group nobody looked at reads nothing: it is read
    // when it is opened.
    let other = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Group && chat.id != group)
        .unwrap()
        .id;
    engine
        .apply_event(ProviderEvent::GroupChanged {
            account_id: account.clone(),
            group_id: other,
        })
        .unwrap();
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 3);
}

#[tokio::test(start_paused = true)]
async fn a_read_that_drops_is_tried_again_and_a_group_left_elsewhere_is_marked() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    // What the refresh listed is no longer fresh.
    tokio::time::sleep(STALE).await;

    mock.fail_next_social([dropped()]);
    engine.want_group(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 1);
    // Not fresh: the next look asks again.
    engine.want_group(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 2);
    assert!(store.group(&account, &group).unwrap().is_some());

    // The account was removed from the group on a phone.
    mock.set_group_role(&account, &group, &mock.self_contact(&account), None);
    engine
        .apply_event(ProviderEvent::GroupChanged {
            account_id: account.clone(),
            group_id: group.clone(),
        })
        .unwrap();
    settle().await;
    let stored = store.group(&account, &group).unwrap().unwrap();
    assert!(stored.departed);
    assert_eq!(stored.my_role, None);
}

#[tokio::test(start_paused = true)]
async fn groups_in_common_come_from_one_listing_of_every_group() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    let someone = member(&mock, &account, &group, GroupRole::Member);
    // The refresh listed the groups of each number, once.
    let numbers = store.accounts().unwrap().len();
    assert_eq!(mock.social_call_count("list_groups"), numbers);
    assert!(engine.groups_listed(&account));

    engine.want_groups(&account);
    engine.want_groups(&account);
    settle().await;
    assert_eq!(
        mock.social_call_count("list_groups"),
        numbers,
        "not again per look while that is fresh"
    );
    let common = store.groups_in_common(&account, &someone).unwrap();
    assert!(common.iter().any(|(id, _)| id == &group));
    // Each group was just read: opening one does not read it again.
    engine.want_group(&account, &group);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 0);
}

#[tokio::test(start_paused = true)]
async fn a_profile_is_looked_up_once_when_it_is_opened() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = with_group(&mock).await;
    let store = engine.store().clone();
    let (person, shop) = (
        ContactId::new("contact:person"),
        ContactId::new("contact:shop"),
    );
    mock.set_about(&person, "At the beach");
    mock.set_business(
        &shop,
        "Corner Bakery",
        BusinessProfile {
            address: Some("Main St 1".into()),
            categories: vec!["Bakery".into()],
            ..Default::default()
        },
    );
    mock.set_blocked(&account, &shop, true, "seed")
        .await
        .unwrap();

    engine.want_profile(&account, &person);
    engine.want_profile(&account, &person);
    engine.want_profile(&account, &shop);
    settle().await;
    assert_eq!(
        mock.social_call_count("lookup_contact"),
        2,
        "one per contact"
    );
    assert_eq!(mock.social_call_count("list_blocked"), 1);
    // Only the business is asked for a business profile.
    assert_eq!(mock.social_call_count("business_profile"), 1);
    let known = store.contact(&account, &person).unwrap().unwrap();
    assert_eq!(known.about.as_deref(), Some("At the beach"));
    assert!(store.business_profile(&account, &person).unwrap().is_none());
    let business = store.contact(&account, &shop).unwrap().unwrap();
    assert_eq!(business.business_name.as_deref(), Some("Corner Bakery"));
    let profile = store.business_profile(&account, &shop).unwrap().unwrap();
    assert_eq!(profile.address.as_deref(), Some("Main St 1"));
    assert!(store.is_blocked(&account, &shop).unwrap());
    assert!(!store.is_blocked(&account, &person).unwrap());
}

// ----- the engine: changing -----------------------------------------------

#[tokio::test(start_paused = true)]
async fn promoting_shows_at_once_survives_a_drop_and_is_put_back_when_refused() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    engine.refresh_group(&account, &group).await.unwrap();
    let someone = member(&mock, &account, &group, GroupRole::Member);
    let role = |of: &ContactId| store.participant_role(&account, &group, of).unwrap();
    let theirs = |of: &ContactId| {
        mock.group(&account, &group)
            .unwrap()
            .participants
            .into_iter()
            .find(|p| &p.contact == of)
            .map(|p| p.role)
    };

    // The connection drops twice: shown regardless, and it gets through
    // once, not three times.
    mock.fail_next_social([dropped(), ProviderError::RateLimited { retry_after: None }]);
    engine.change_participant(&account, &group, &someone, ParticipantChange::Promote);
    assert_eq!(role(&someone), Some(GroupRole::Admin));
    assert_eq!(theirs(&someone), Some(GroupRole::Member));
    settle().await;
    assert_eq!(theirs(&someone), Some(GroupRole::Admin));
    assert_eq!(role(&someone), Some(GroupRole::Admin));
    assert_eq!(mock.social_call_count("change_participants"), 3);

    // Not an admin any more: the provider refuses, the old role is back
    // and the user is told why.
    mock.set_group_role(
        &account,
        &group,
        &mock.self_contact(&account),
        Some(GroupRole::Member),
    );
    let mut changes = store.subscribe();
    engine.change_participant(&account, &group, &someone, ParticipantChange::Demote);
    assert_eq!(role(&someone), Some(GroupRole::Member));
    settle().await;
    assert_eq!(role(&someone), Some(GroupRole::Admin), "put back");
    let told = said(&mut changes);
    assert_eq!(told.len(), 1, "{told:?}");
    assert!(
        told[0].contains("Only admins of the group can do that"),
        "{told:?}"
    );

    // A connection that never comes back: put back, and "try again", not
    // a permanent failure.
    mock.set_group_role(
        &account,
        &group,
        &mock.self_contact(&account),
        Some(GroupRole::Admin),
    );
    mock.fail_next_social(std::iter::repeat_with(dropped).take(8));
    engine.change_participant(&account, &group, &someone, ParticipantChange::Remove);
    assert_eq!(role(&someone), None);
    settle().await;
    assert_eq!(role(&someone), Some(GroupRole::Admin));
    let told = said(&mut changes);
    assert!(told[0].contains("Try again"), "{told:?}");

    // The owner cannot be demoted: the refusal is per participant.
    let owner = member(&mock, &account, &group, GroupRole::Owner);
    engine.change_participant(&account, &group, &owner, ParticipantChange::Demote);
    settle().await;
    assert_eq!(role(&owner), Some(GroupRole::Owner));
    assert_eq!(said(&mut changes).len(), 1);

    // Removing works, and the list here follows.
    engine.change_participant(&account, &group, &someone, ParticipantChange::Remove);
    settle().await;
    assert_eq!(role(&someone), None);
    assert_eq!(theirs(&someone), None);
    assert!(said(&mut changes).is_empty());
}

#[tokio::test(start_paused = true)]
async fn adding_answers_per_person_and_privacy_is_said_in_words() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    engine.refresh_group(&account, &group).await.unwrap();
    let (open, private) = (
        ContactId::new("contact:newcomer"),
        ContactId::new("contact:shy"),
    );
    let already = member(&mock, &account, &group, GroupRole::Member);
    mock.restrict_adds(&private);

    // The answer is lost once: the same request is sent again, and adds
    // nobody twice.
    mock.fail_next_social([dropped()]);
    let outcomes = engine
        .add_participants(
            &account,
            &group,
            &[open.clone(), private.clone(), already.clone()],
            "request-1",
        )
        .await
        .unwrap();
    assert_eq!(mock.social_call_count("change_participants"), 2);
    assert!(outcomes[0].worked());
    assert_eq!(outcomes[1].error.as_deref(), Some(refusal::PRIVACY));
    assert_eq!(outcomes[2].error.as_deref(), Some(refusal::ALREADY_MEMBER));
    assert_eq!(
        store.participant_role(&account, &group, &open).unwrap(),
        Some(GroupRole::Member)
    );
    assert_eq!(
        store.participant_role(&account, &group, &private).unwrap(),
        None
    );
    let sentence = outcome_sentence("Shy", ParticipantChange::Add, &outcomes[1]).unwrap();
    assert!(sentence.contains("privacy settings") && sentence.contains("invite link"));
    assert!(outcome_sentence("New", ParticipantChange::Add, &outcomes[0]).is_none());

    // A refusal of the whole request reaches the dialog as a sentence; a
    // dead connection as "try again".
    mock.fail_next_social([rejected(refusal::NOT_ADMIN, "forbidden")]);
    let refused = engine
        .add_participants(&account, &group, std::slice::from_ref(&open), "request-2")
        .await
        .unwrap_err();
    assert_eq!(
        failure_sentence(&refused),
        "Only admins of the group can do that."
    );
    mock.fail_next_social(std::iter::repeat_with(dropped).take(3));
    let lost = engine
        .add_participants(&account, &group, &[open], "request-3")
        .await
        .unwrap_err();
    assert!(lost.is_transient());
    assert_eq!(
        failure_sentence(&lost),
        "Could not reach the provider. Try again."
    );
}

#[tokio::test(start_paused = true)]
async fn creating_a_group_twice_over_a_dropped_connection_makes_one_group() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = with_group(&mock).await;
    let store = engine.store().clone();
    let (ana, shy) = (ContactId::new("contact:ana"), ContactId::new("contact:shy"));
    mock.restrict_adds(&shy);
    let before = mock.list_groups(&account).await.unwrap().len();

    mock.fail_next_social([dropped()]);
    let created = engine
        .create_group(
            &account,
            "  Road trip ",
            vec![ana.clone(), shy.clone()],
            Some(jpeg()),
            "create-1",
        )
        .await
        .unwrap();
    // The dialog asks again with the same id (a double click, a retry).
    let again = engine
        .create_group(
            &account,
            "Road trip",
            vec![ana.clone(), shy.clone()],
            None,
            "create-1",
        )
        .await
        .unwrap();
    assert_eq!(created.chat, again.chat);
    assert_eq!(mock.list_groups(&account).await.unwrap().len(), before + 1);
    assert_eq!(created.missing, vec![shy], "who could not be added is said");

    // The chat and the group are in the store at once: nothing to wait for.
    let chat = store.chat(&account, &created.chat).unwrap().unwrap();
    assert_eq!(
        (chat.kind, chat.title.as_str()),
        (ChatKind::Group, "Road trip")
    );
    let stored = store.group(&account, &created.chat).unwrap().unwrap();
    assert_eq!(stored.my_role, Some(GroupRole::Owner));
    assert_eq!(stored.participant_count, 2);
    // The picture follows in the background.
    settle().await;
    assert_eq!(mock.social_call_count("set_group_picture"), 1);
    let picture = store.avatar(&account, &created.chat).unwrap().unwrap();
    assert!(picture.image.is_some());
    assert_ne!(picture.picture_id.as_deref(), Some("pending"));

    // A refusal says why and creates nothing here.
    mock.fail_next_social([rejected("rate_limited_groups", "too many groups today")]);
    let refused = engine
        .create_group(&account, "Another", vec![ana], None, "create-2")
        .await
        .unwrap_err();
    assert_eq!(failure_sentence(&refused), "Too many groups today");
}

#[tokio::test(start_paused = true)]
async fn a_groups_details_settings_and_picture_change_at_once_and_roll_back() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    engine.refresh_group(&account, &group).await.unwrap();
    let was = store.group(&account, &group).unwrap().unwrap().group;
    let details = || store.group(&account, &group).unwrap().unwrap().group;

    mock.fail_next_social([dropped()]);
    engine.update_group(&account, &group, GroupChange::Subject("New name".into()));
    engine.update_group(&account, &group, GroupChange::Announce(true));
    assert_eq!(details().subject, "New name");
    assert_eq!(
        store.chat(&account, &group).unwrap().unwrap().title,
        "New name"
    );
    // The chat list still says the old name meanwhile: that is not a
    // change made elsewhere, and reads nothing.
    engine
        .apply_event(ProviderEvent::ChatUpdated(
            mock.chat(&account, &group).unwrap(),
        ))
        .unwrap();
    settle().await;
    let theirs = mock.group(&account, &group).unwrap();
    assert_eq!(theirs.subject, "New name");
    assert!(theirs.announce && details().announce);

    let mut changes = store.subscribe();
    mock.fail_next_social([
        rejected(refusal::NOT_ADMIN, "forbidden"),
        rejected(refusal::NOT_ADMIN, "forbidden"),
    ]);
    engine.update_group(&account, &group, GroupChange::Description("Rules".into()));
    engine.update_group(&account, &group, GroupChange::Locked(!was.locked));
    assert_eq!(details().description.as_deref(), Some("Rules"));
    settle().await;
    assert_eq!(details().description, was.description);
    assert_eq!(details().locked, was.locked);
    let told = said(&mut changes);
    assert_eq!(told.len(), 2, "{told:?}");
    assert!(
        told.iter().all(|line| line.contains("Only admins")),
        "{told:?}"
    );

    // The picture: shown at once, kept under the provider's id.
    engine.set_group_picture(&account, &group, Some(jpeg()));
    settle().await;
    let picture = store.avatar(&account, &group).unwrap().unwrap();
    assert!(picture.image.is_some());
    let id = picture.picture_id.clone().unwrap();
    assert_ne!(id, "pending");
    // Removing it is refused: the picture is back.
    mock.fail_next_social([rejected(refusal::NOT_ADMIN, "forbidden")]);
    engine.set_group_picture(&account, &group, None);
    settle().await;
    let back = store.avatar(&account, &group).unwrap().unwrap();
    assert_eq!(back.picture_id.as_deref(), Some(id.as_str()));
    assert_eq!(back.image, picture.image);
    assert!(said(&mut changes)[0].starts_with("The picture was not changed"));
    // A file that is no image never reaches the provider.
    let calls = mock.social_call_count("set_group_picture");
    engine.set_group_picture(&account, &group, Some(b"not an image".to_vec()));
    settle().await;
    assert_eq!(mock.social_call_count("set_group_picture"), calls);
    assert_eq!(
        store.avatar(&account, &group).unwrap().unwrap().image,
        picture.image
    );
    assert!(said(&mut changes)[0].contains("not an image"));
}

#[tokio::test(start_paused = true)]
async fn the_invite_link_requests_to_join_and_leaving() {
    let mock = MockProvider::quiet();
    let (engine, account, group) = with_group(&mock).await;
    let store = engine.store().clone();
    engine.refresh_group(&account, &group).await.unwrap();

    let link = engine
        .invite_link(&account, &group, false, "l1")
        .await
        .unwrap();
    assert!(link.starts_with("https://chat.whatsapp.com/"));
    assert_eq!(
        store.group(&account, &group).unwrap().unwrap().invite_link,
        Some(link.clone())
    );
    mock.fail_next_social([dropped()]);
    let reset = engine
        .invite_link(&account, &group, true, "l2")
        .await
        .unwrap();
    assert_ne!(reset, link, "the old link stops working");
    assert_eq!(
        store.group(&account, &group).unwrap().unwrap().invite_link,
        Some(reset)
    );

    // Two people ask to join: one is let in, one is turned away.
    let (ana, bruno) = (
        ContactId::new("contact:asker1"),
        ContactId::new("contact:asker2"),
    );
    mock.add_join_request(&account, &group, &ana);
    mock.add_join_request(&account, &group, &bruno);
    engine.want_join_requests(&account, &group);
    settle().await;
    let pending = store.join_requests(&account, &group).unwrap();
    assert_eq!(pending.len(), 2);
    engine.answer_join_request(&account, &group, &pending[0], true);
    assert_eq!(
        store.join_requests(&account, &group).unwrap().len(),
        1,
        "at once"
    );
    settle().await;
    assert_eq!(
        store.participant_role(&account, &group, &ana).unwrap(),
        Some(GroupRole::Member)
    );
    // The answer is refused: the request is back in the list, and said.
    let mut changes = store.subscribe();
    mock.fail_next_social([rejected(refusal::NOT_ADMIN, "forbidden")]);
    engine.answer_join_request(&account, &group, &pending[1], false);
    settle().await;
    assert_eq!(store.join_requests(&account, &group).unwrap().len(), 1);
    assert!(said(&mut changes)[0].contains("Only admins"));
    engine.answer_join_request(&account, &group, &pending[1], false);
    settle().await;
    assert!(store.join_requests(&account, &group).unwrap().is_empty());
    assert_eq!(
        store.participant_role(&account, &group, &bruno).unwrap(),
        None
    );

    // Leaving: a drop is retried, a dead connection says "try again" and
    // leaves nothing half done.
    mock.fail_next_social(std::iter::repeat_with(dropped).take(3));
    let lost = engine
        .leave_group(&account, &group, "leave-1")
        .await
        .unwrap_err();
    assert!(lost.is_transient());
    assert!(!store.group(&account, &group).unwrap().unwrap().departed);
    mock.fail_next_social([dropped()]);
    engine
        .leave_group(&account, &group, "leave-1")
        .await
        .unwrap();
    let stored = store.group(&account, &group).unwrap().unwrap();
    assert!(stored.departed);
    assert_eq!(stored.my_role, None);
    assert!(mock
        .group(&account, &group)
        .unwrap()
        .participants
        .iter()
        .all(|p| p.contact != mock.self_contact(&account)));
}

#[tokio::test(start_paused = true)]
async fn blocking_shows_at_once_and_a_stale_blocklist_does_not_undo_it() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = with_group(&mock).await;
    let store = engine.store().clone();
    let ana = ContactId::new("contact:ana");

    // The block is on its way (the connection dropped) when the blocklist
    // is read: the list does not have her yet, and she stays blocked here.
    engine.set_blocked(&account, &ana, true);
    assert!(store.is_blocked(&account, &ana).unwrap());
    engine.refresh_blocklist(&account).await.unwrap();
    assert!(store.is_blocked(&account, &ana).unwrap());
    settle().await;
    assert_eq!(mock.blocked(&account), vec![ana.clone()]);

    // A refusal puts it back and says so.
    let mut changes = store.subscribe();
    mock.fail_next_social([rejected("whatsapp_error", "WhatsApp failed the operation")]);
    engine.set_blocked(&account, &ana, false);
    assert!(!store.is_blocked(&account, &ana).unwrap());
    settle().await;
    assert!(store.is_blocked(&account, &ana).unwrap());
    let told = said(&mut changes);
    assert!(
        told[0].contains("was not unblocked") && told[0].contains("WhatsApp failed"),
        "{told:?}"
    );

    // A drop is not a failure: it goes through, and nothing is said.
    mock.fail_next_social([dropped(), dropped()]);
    engine.set_blocked(&account, &ana, false);
    settle().await;
    assert!(mock.blocked(&account).is_empty());
    assert!(!store.is_blocked(&account, &ana).unwrap());
    assert!(said(&mut changes).is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_own_profile_is_edited_at_once_and_what_cannot_be_read_is_kept() {
    let mock = MockProvider::quiet();
    let (engine, account, _) = with_group(&mock).await;
    let store = engine.store().clone();

    engine.want_own_profile(&account);
    settle().await;
    let read = store.own_profile(&account).unwrap();
    assert_eq!(read.name.as_deref(), Some("Personal"));
    assert_eq!(read.about.as_deref(), Some("Available"));

    mock.fail_next_social([dropped()]);
    engine.update_profile(&account, ProfileChange::About("On holiday".into()));
    assert_eq!(
        store.own_profile(&account).unwrap().about.as_deref(),
        Some("On holiday")
    );
    settle().await;
    assert_eq!(
        mock.own_profile_of(&account).about.as_deref(),
        Some("On holiday")
    );

    // Refused: the old text is back, and the user is told.
    let mut changes = store.subscribe();
    mock.fail_next_social([rejected("invalid_request", "the name is too long")]);
    engine.update_profile(&account, ProfileChange::Name("x".repeat(40)));
    settle().await;
    assert_eq!(
        store.own_profile(&account).unwrap().name.as_deref(),
        Some("Personal")
    );
    let told = said(&mut changes);
    assert!(
        told[0].starts_with("Your name was not changed") && told[0].contains("too long"),
        "{told:?}"
    );

    // A provider that cannot read the profile back: what was set from
    // here is what is shown, not blanked by the read.
    mock.unreadable_profile(true);
    engine.refresh_own_profile(&account).await.unwrap();
    let kept = store.own_profile(&account).unwrap();
    assert_eq!(kept.about.as_deref(), Some("On holiday"));
    assert_eq!(kept.name.as_deref(), Some("Personal"));

    // The picture, under the account's own number.
    let subject = engine.own_subject(&account).unwrap();
    engine.set_profile_picture(&account, Some(jpeg()));
    settle().await;
    assert!(store
        .avatar(&account, &subject)
        .unwrap()
        .unwrap()
        .image
        .is_some());
    mock.fail_next_social([rejected("whatsapp_error", "no")]);
    engine.set_profile_picture(&account, None);
    settle().await;
    assert!(
        store
            .avatar(&account, &subject)
            .unwrap()
            .unwrap()
            .image
            .is_some(),
        "a refused removal leaves the picture"
    );
    engine.set_profile_picture(&account, None);
    settle().await;
    assert!(store
        .avatar(&account, &subject)
        .unwrap()
        .unwrap()
        .image
        .is_none());
}

#[test]
fn a_notice_names_people_as_the_group_panel_does() {
    use client_provider::{
        Chat, ChatKind, Direction, MessageContent, Party, SystemEvent, SystemKind,
    };
    let store = Store::open_in_memory().unwrap();
    super::seed_account(&store);
    let account = super::account();
    // "+5842" is saved as Ana; "+5843" and "+5841" go by the names the
    // group knows them by ("zed", "olga"), whatever they call themselves.
    let mut saved = Contact::new(account.clone(), ContactId::new("+5842"));
    saved.saved_name = Some("Ana".into());
    store.upsert_contact(&saved).unwrap();
    let mut profile = Contact::new(account.clone(), ContactId::new("+5841"));
    profile.profile_name = Some("Olga R.".into());
    store.upsert_contact(&profile).unwrap();
    let group = a_group(&account);
    store.put_group(&group, Timestamp::from_millis(5)).unwrap();
    store
        .upsert_chat(
            &Chat {
                id: group.id.clone(),
                kind: ChatKind::Group,
                ..super::chat(group.id.as_str(), "Family")
            },
            false,
        )
        .unwrap();

    let party = |id: &str, name: Option<&str>| Party {
        id: ContactId::new(id),
        name: name.map(str::to_owned),
    };
    let mut notice = super::text("n1", group.id.as_str(), 10, "", Direction::Incoming);
    notice.content = MessageContent::System(SystemEvent {
        kind: SystemKind::Added,
        // The provider calls her by the name she gave herself.
        actor: Some(party("+5842", Some("ana ~"))),
        targets: vec![party("+5843", Some("zed")), party("+5841", None)],
        detail: None,
    });
    store.upsert_message(&notice).unwrap();

    let panel: std::collections::HashMap<String, String> = store
        .group_participants(&account, &group.id, None, 10)
        .unwrap()
        .into_iter()
        .map(|participant| (participant.contact.to_string(), participant.name))
        .collect();
    let read = store.messages(&account, &group.id, 10).unwrap();
    let MessageContent::System(event) = &read[0].message.content else {
        panic!("a notice");
    };
    let named = |party: &Party| party.name.clone().unwrap();
    assert_eq!(named(event.actor.as_ref().unwrap()), "Ana");
    for party in event.actor.iter().chain(&event.targets) {
        assert_eq!(
            Some(&named(party)),
            panel.get(party.id.as_str()),
            "{} reads the same in the notice and in the panel",
            party.id
        );
    }
    assert_eq!(system_line(event), "Ana added zed and olga");
    // And in the chat list's preview.
    let listed = store.chat(&account, &group.id).unwrap().unwrap();
    assert_eq!(listed.last_message.unwrap().text, "Ana added zed and olga");

    // Somebody who is not in the group any more and not in the address
    // book keeps the name the notice came with.
    let mut left = super::text("n2", group.id.as_str(), 11, "", Direction::Incoming);
    left.content = MessageContent::System(SystemEvent {
        kind: SystemKind::Left,
        actor: None,
        targets: vec![party("+5849", Some("Rui"))],
        detail: None,
    });
    store.upsert_message(&left).unwrap();
    let read = store.message(&account, &left.id).unwrap().unwrap();
    let MessageContent::System(event) = &read.content else {
        panic!("a notice");
    };
    assert_eq!(system_line(event), "Rui left");
}
