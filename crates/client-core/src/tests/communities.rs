//! Communities: which community a group is linked to, in the store and in
//! what the chat list reads, and how the engine comes to know it without
//! opening each group.

use super::{account, chat, engine_for, seed_account, settle};
use crate::*;
use client_provider::{
    AccountId, ChatId, ChatKind, ContactId, Group, GroupParticipant, GroupRole, ProviderError,
    ProviderEvent, Subgroup, Timestamp,
};
use provider_mock::{
    MockProvider, COMMUNITY, COMMUNITY_ACCOUNT, COMMUNITY_ANNOUNCEMENTS, COMMUNITY_GROUPS,
    COMMUNITY_NAME,
};

fn a_group(id: &str, subject: &str) -> Group {
    Group {
        id: ChatId::new(id),
        account_id: account(),
        subject: subject.into(),
        description: None,
        owner: None,
        created_at: None,
        community: false,
        community_id: None,
        announcements: false,
        announce: false,
        locked: false,
        join_approval: None,
        members_can_add: None,
        participants: vec![GroupParticipant {
            contact: ContactId::new("me"),
            name: None,
            role: GroupRole::Member,
        }],
        subgroups: Vec::new(),
    }
}

fn a_community(id: &str, subject: &str) -> Group {
    Group {
        community: true,
        announce: true,
        ..a_group(id, subject)
    }
}

fn in_community(community: &str, id: &str, subject: &str, announcements: bool) -> Group {
    Group {
        community_id: Some(ChatId::new(community)),
        announcements,
        ..a_group(id, subject)
    }
}

fn group_chat(id: &str, title: &str) -> client_provider::Chat {
    client_provider::Chat {
        kind: ChatKind::Group,
        ..chat(id, title)
    }
}

fn linked(id: &str, subject: &str, announcements: bool) -> Subgroup {
    Subgroup {
        id: ChatId::new(id),
        subject: subject.into(),
        announcements,
    }
}

/// A store with a community, its announcement group and one more group,
/// each of the two with a chat, beside a plain group and a person.
fn store_with_a_community() -> Store {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    for chat in [
        group_chat("news", "Announcements"),
        group_chat("class", "Class 3B"),
        group_chat("family", "Family"),
        chat("ana", "Ana"),
    ] {
        store.upsert_chat(&chat, false).unwrap();
    }
    store
        .put_groups(
            &[
                a_community("school", "School"),
                in_community("school", "news", "Announcements", true),
                in_community("school", "class", "Class 3B", false),
                a_group("family", "Family"),
            ],
            Timestamp::from_millis(5),
        )
        .unwrap();
    store
}

fn community_of(store: &Store, chat: &str) -> Option<ChatCommunity> {
    store
        .chat(&account(), &ChatId::new(chat))
        .unwrap()
        .unwrap()
        .community
}

/// Whether the chat list was told to read again since the last look.
fn chats_changed(changes: &mut ChangeListener) -> bool {
    let mut changed = false;
    while let Some(change) = changes.try_next() {
        changed |= matches!(change, StoreChange::Chats { .. } | StoreChange::Everything);
    }
    changed
}

// ----- the store ------------------------------------------------------------

/// A listing of every group does not say which groups a community links.
/// It must not empty the ones a read of the community stored.
#[test]
fn a_listing_of_every_group_keeps_the_groups_a_community_links() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let read = Group {
        subgroups: vec![
            linked("news", "Announcements", true),
            linked("elsewhere", "A group this number is not in", false),
        ],
        ..a_community("school", "School")
    };
    store.put_group(&read, Timestamp::from_millis(5)).unwrap();

    store
        .put_groups(
            &[a_community("school", "School")],
            Timestamp::from_millis(9),
        )
        .unwrap();
    let stored = store.group(&account(), &read.id).unwrap().unwrap();
    assert_eq!(stored.group.subgroups, read.subgroups);
    assert_eq!(stored.fetched_at, Timestamp::from_millis(9));

    // A read of the community that says them replaces them.
    let fewer = Group {
        subgroups: vec![linked("news", "Announcements", true)],
        ..read
    };
    store.put_group(&fewer, Timestamp::from_millis(12)).unwrap();
    let stored = store.group(&account(), &fewer.id).unwrap().unwrap();
    assert_eq!(stored.group.subgroups, fewer.subgroups);
}

#[test]
fn a_group_is_kept_with_its_community_and_the_chat_list_reads_it() {
    let store = store_with_a_community();
    let school = ChatId::new("school");

    let stored = store
        .group(&account(), &ChatId::new("news"))
        .unwrap()
        .unwrap();
    assert_eq!(stored.group.community_id.as_ref(), Some(&school));
    assert!(stored.group.announcements);
    let plain = store
        .group(&account(), &ChatId::new("family"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (plain.group.community_id, plain.group.announcements),
        (None, false)
    );

    let of = |id: &str, announcements: bool| {
        Some(ChatCommunity {
            id: ChatId::new(id),
            name: "School".into(),
            announcements,
        })
    };
    assert_eq!(community_of(&store, "news"), of("school", true));
    assert_eq!(community_of(&store, "class"), of("school", false));
    assert_eq!(community_of(&store, "family"), None);
    assert_eq!(community_of(&store, "ana"), None);

    // The list says the same, and so does the archive.
    let listed: Vec<(String, Option<ChatCommunity>)> = store
        .chats(&account(), None)
        .unwrap()
        .into_iter()
        .map(|chat| (chat.id.to_string(), chat.community))
        .collect();
    assert_eq!(listed.len(), 4);
    for (id, community) in listed {
        assert_eq!(community, community_of(&store, &id), "{id}");
    }
    store
        .apply_chat_change(
            &account(),
            &ChatId::new("class"),
            client_provider::ChatChange::Archived(true),
        )
        .unwrap();
    let archived = store.archived_chats(&account()).unwrap();
    assert_eq!(archived.len(), 1);
    assert_eq!(archived[0].community, of("school", false));
}

/// A group may be known before its community is: the chat is in it all
/// the same, under a name that is not known yet.
#[test]
fn a_community_that_was_not_read_yet_has_no_name() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store
        .upsert_chat(&group_chat("class", "Class 3B"), false)
        .unwrap();
    store
        .put_group(
            &in_community("school", "class", "Class 3B", false),
            Timestamp::from_millis(5),
        )
        .unwrap();
    assert_eq!(
        community_of(&store, "class"),
        Some(ChatCommunity {
            id: ChatId::new("school"),
            name: String::new(),
            announcements: false,
        })
    );
}

/// The chat list groups by community and shows its name: both are part
/// of what it reads, so a change of either tells it to read again.
#[test]
fn a_change_of_community_or_of_its_name_tells_the_chat_list() {
    let store = store_with_a_community();
    let mut changes = store.subscribe();
    let at = Timestamp::from_millis(9);

    // The same again is no change.
    store
        .put_groups(
            &[
                a_community("school", "School"),
                in_community("school", "news", "Announcements", true),
                a_group("family", "Family"),
            ],
            at,
        )
        .unwrap();
    assert!(!chats_changed(&mut changes));

    // A plain group is linked to the community.
    store
        .put_group(&in_community("school", "family", "Family", false), at)
        .unwrap();
    assert!(chats_changed(&mut changes));
    assert!(community_of(&store, "family").is_some());

    // The community is renamed.
    store
        .put_group(&a_community("school", "Riverside School"), at)
        .unwrap();
    assert!(chats_changed(&mut changes));
    assert_eq!(
        community_of(&store, "class").unwrap().name,
        "Riverside School"
    );

    // A group is unlinked.
    store.put_group(&a_group("class", "Class 3B"), at).unwrap();
    assert!(chats_changed(&mut changes));
    assert_eq!(community_of(&store, "class"), None);

    // A plain group that is renamed where it has no chat changes no list.
    store.put_group(&a_group("nochat", "No chat"), at).unwrap();
    assert!(!chats_changed(&mut changes));
}

/// A community that was only ever listed still shows the groups known to
/// be in it, its announcement group first.
#[test]
fn a_community_that_was_only_listed_names_the_groups_known_to_be_in_it() {
    let store = store_with_a_community();
    let stored = store
        .group(&account(), &ChatId::new("school"))
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.group.subgroups,
        [
            linked("news", "Announcements", true),
            linked("class", "Class 3B", false),
        ]
    );
    // A plain group links nothing.
    let plain = store
        .group(&account(), &ChatId::new("family"))
        .unwrap()
        .unwrap();
    assert!(plain.group.subgroups.is_empty());
}

// ----- the engine -----------------------------------------------------------

fn mock_ids() -> (AccountId, ChatId, ChatId, ChatId) {
    (
        AccountId::new(COMMUNITY_ACCOUNT),
        ChatId::new(COMMUNITY),
        ChatId::new(COMMUNITY_ANNOUNCEMENTS),
        ChatId::new(COMMUNITY_GROUPS[0]),
    )
}

fn community_in(store: &Store, account: &AccountId, chat: &ChatId) -> Option<ChatCommunity> {
    store.chat(account, chat).unwrap().unwrap().community
}

/// One listing of every group at refresh, and no read of any group: that
/// is all it takes for the chat list to know its communities.
#[tokio::test(start_paused = true)]
async fn a_refresh_learns_every_groups_community_from_one_listing() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store();
    let (account, community, announcements, class) = mock_ids();

    assert_eq!(
        mock.social_calls(),
        ["list_groups", "list_groups"],
        "one listing per number, and nothing per group"
    );
    assert_eq!(
        community_in(store, &account, &announcements),
        Some(ChatCommunity {
            id: community.clone(),
            name: COMMUNITY_NAME.into(),
            announcements: true,
        })
    );
    assert_eq!(
        community_in(store, &account, &class),
        Some(ChatCommunity {
            id: community,
            name: COMMUNITY_NAME.into(),
            announcements: false,
        })
    );
    let in_one = store
        .chats(&account, None)
        .unwrap()
        .iter()
        .filter(|chat| chat.community.is_some())
        .count();
    assert_eq!(in_one, 3);
    assert!(engine.groups_listed(&account));
}

/// The listing fails in passing (the number is offline): the refresh is
/// not a failure for that, and the chats are there.
#[tokio::test(start_paused = true)]
async fn a_listing_that_fails_does_not_fail_the_refresh() {
    let mock = MockProvider::quiet();
    mock.fail_next_social([
        ProviderError::Transient("offline".into()),
        ProviderError::Transient("offline".into()),
    ]);
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store();
    let (account, _, announcements, _) = mock_ids();
    assert_eq!(community_in(store, &account, &announcements), None);
    assert!(!engine.groups_listed(&account));

    // The next refresh lists them.
    engine.refresh().await.unwrap();
    assert!(community_in(store, &account, &announcements).is_some());
}

/// A provider without groups is not asked for them.
#[tokio::test(start_paused = true)]
async fn a_provider_that_has_no_group_details_is_not_asked_to_list_groups() {
    let mut config = provider_mock::MockConfig::quiet();
    config.capabilities = Some(client_provider::Capabilities {
        group_info: false,
        ..client_provider::Capabilities::all()
    });
    let mock = MockProvider::new(config);
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    assert!(mock.social_calls().is_empty());
}

/// The listing may leave a community out. It is then read on its own,
/// once, so that its name is known; never one read per group.
#[tokio::test(start_paused = true)]
async fn a_community_missing_from_the_listing_is_read_once_for_its_name() {
    let mock = MockProvider::quiet();
    let (account, community, announcements, _) = mock_ids();
    mock.unlist_group(&account, &community);
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store();
    assert_eq!(mock.social_call_count("fetch_group"), 1);
    assert_eq!(
        community_in(store, &account, &announcements).unwrap().name,
        COMMUNITY_NAME
    );

    // It is held now: the next refresh does not read it again.
    engine.refresh().await.unwrap();
    assert_eq!(mock.social_call_count("fetch_group"), 1);
}

/// A listing does not pass for a read of a community: opening it still
/// reads the groups it links, and the next listing leaves them alone.
#[tokio::test(start_paused = true)]
async fn a_community_is_read_when_looked_at_although_it_was_just_listed() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, _, class) = mock_ids();

    // A plain group was read by the listing: looking reads nothing.
    engine.want_group(&account, &class);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 0);

    engine.want_group(&account, &community);
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 1);
    let read = mock.fetch_group_now(&account, &community);
    assert_eq!(read.subgroups.len(), 4);
    let stored = store.group(&account, &community).unwrap().unwrap();
    assert_eq!(stored.group.subgroups, read.subgroups);

    // The mock no longer has one of them; a listing does not say so, and
    // what the read stored stays until the community is read again.
    mock.unlink_on_phone(&account, &class);
    engine.refresh().await.unwrap();
    let stored = store.group(&account, &community).unwrap().unwrap();
    assert_eq!(stored.group.subgroups, read.subgroups);
}

/// A link or an unlink reads the community and the groups again, whether
/// or not they were ever stored.
#[tokio::test(start_paused = true)]
async fn linking_and_unlinking_read_the_community_and_its_groups_again() {
    let mock = MockProvider::quiet();
    // The listing fails: no group is stored.
    mock.fail_next_social([
        ProviderError::Transient("offline".into()),
        ProviderError::Transient("offline".into()),
    ]);
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, _, class) = mock_ids();
    assert!(store.group(&account, &class).unwrap().is_none());
    let changed = ProviderEvent::CommunityChanged {
        account_id: account.clone(),
        community_id: community.clone(),
        groups: vec![class.clone()],
    };
    let mut changes = store.subscribe();

    engine.apply_event(changed.clone()).unwrap();
    settle().await;
    assert_eq!(
        mock.social_call_count("fetch_group"),
        2,
        "the community and the group, nothing else"
    );
    assert_eq!(
        community_in(&store, &account, &class),
        Some(ChatCommunity {
            id: community.clone(),
            name: COMMUNITY_NAME.into(),
            announcements: false,
        })
    );
    assert!(chats_changed(&mut changes));

    // Unlinked on a phone: the chat is in no community any more, and the
    // community no longer lists it.
    mock.unlink_on_phone(&account, &class);
    engine.apply_event(changed).unwrap();
    settle().await;
    assert_eq!(mock.social_call_count("fetch_group"), 4);
    assert_eq!(community_in(&store, &account, &class), None);
    assert!(chats_changed(&mut changes));
    let parent = store.group(&account, &community).unwrap().unwrap();
    assert!(parent
        .group
        .subgroups
        .iter()
        .all(|linked| linked.id != class));
}

/// The running engine lists the groups behind the refresh, so its events
/// do not wait for the listing.
#[tokio::test(start_paused = true)]
async fn the_running_engine_learns_the_communities_by_itself() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let store = engine.store().clone();
    let (account, _, announcements, _) = mock_ids();
    engine.start();
    super::wait_for_account_sync(|| {
        matches!(
            store.chat(&account, &announcements),
            Ok(Some(chat)) if chat.community.is_some()
        )
    })
    .await;
    assert_eq!(mock.social_call_count("fetch_group"), 0);
    engine.shutdown();
}

// ----- managing a community ---------------------------------------------------

#[tokio::test(start_paused = true)]
async fn an_unlinked_group_is_linked_again_and_the_chat_list_follows() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, _, class) = mock_ids();
    let mut changes = store.subscribe();

    engine
        .unlink_subgroup(&account, &community, &class)
        .await
        .unwrap();
    assert_eq!(community_in(&store, &account, &class), None);
    assert!(chats_changed(&mut changes));
    let parent = store.group(&account, &community).unwrap().unwrap();
    assert!(parent
        .group
        .subgroups
        .iter()
        .all(|linked| linked.id != class));

    engine
        .link_subgroup(&account, &community, &class, "link-1")
        .await
        .unwrap();
    assert_eq!(
        community_in(&store, &account, &class).map(|c| c.id),
        Some(community.clone())
    );
    assert!(chats_changed(&mut changes));
    let parent = store.group(&account, &community).unwrap().unwrap();
    assert!(parent
        .group
        .subgroups
        .iter()
        .any(|linked| linked.id == class));
}

#[tokio::test(start_paused = true)]
async fn a_refusal_to_link_or_unlink_changes_nothing_and_says_why() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, announcements, class) = mock_ids();

    mock.fail_next_social([ProviderError::Rejected {
        code: client_provider::refusal::NOT_ADMIN.into(),
        message: "Only admins of the group can do that.".into(),
    }]);
    let refused = engine
        .unlink_subgroup(&account, &community, &class)
        .await
        .unwrap_err();
    assert_eq!(
        failure_sentence(&refused),
        "Only admins of the group can do that."
    );
    assert_eq!(
        community_in(&store, &account, &class).map(|c| c.id),
        Some(community.clone())
    );
    // The announcement group stays; the provider says so.
    assert!(engine
        .unlink_subgroup(&account, &community, &announcements)
        .await
        .is_err());
}

#[tokio::test(start_paused = true)]
async fn a_communitys_participants_are_asked_for_when_looked_for() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, community, ..) = mock_ids();
    let people = engine
        .community_participants(&account, &community)
        .await
        .unwrap();
    assert!(!people.is_empty());
    assert_eq!(mock.social_call_count("community_participants"), 1);
}

#[tokio::test(start_paused = true)]
async fn a_group_is_created_in_a_community_and_a_community_without_a_chat() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, ..) = mock_ids();
    let people = vec![ContactId::new("+15550000001")];

    let inside = engine
        .create_group_in(
            &account,
            "Volunteers",
            people.clone(),
            None,
            "req-1",
            GroupPlace::InCommunity(community.clone()),
        )
        .await
        .unwrap();
    assert!(!inside.community);
    let chat = store.chat(&account, &inside.chat).unwrap().unwrap();
    assert_eq!(chat.community.map(|c| c.id), Some(community.clone()));
    let parent = store.group(&account, &community).unwrap().unwrap();
    assert!(parent
        .group
        .subgroups
        .iter()
        .any(|linked| linked.id == inside.chat));

    // A community has no chat: what comes back is the community itself.
    let made = engine
        .create_group_in(
            &account,
            "Neighbours",
            Vec::new(),
            None,
            "req-2",
            GroupPlace::Community,
        )
        .await
        .unwrap();
    assert!(made.community);
    assert!(store.chat(&account, &made.chat).unwrap().is_none());
    assert!(
        store
            .group(&account, &made.chat)
            .unwrap()
            .unwrap()
            .group
            .community
    );
}

#[tokio::test(start_paused = true)]
async fn only_groups_an_admin_could_link_are_offered_to_link() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let (account, community, announcements, class) = mock_ids();

    let offered: Vec<ChatId> = store
        .groups_to_link(&account)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert!(!offered.is_empty());
    // Not the community, and none that is in one already.
    for not in [&community, &announcements, &class] {
        assert!(!offered.contains(not), "{not}");
    }
    // Unlinked, a group is offered; one the account has left is not.
    engine
        .unlink_subgroup(&account, &community, &class)
        .await
        .unwrap();
    let offered = store.groups_to_link(&account).unwrap();
    assert!(offered.iter().any(|(id, _)| *id == class));
    store.set_departed(&account, &class, true).unwrap();
    let offered = store.groups_to_link(&account).unwrap();
    assert!(offered.iter().all(|(id, _)| *id != class));
}
