//! One community in the demo data: a parent that links an announcement
//! group and two more groups, so that the grouped chat list and the
//! details of a community can be looked at without a real account.
//!
//! The community itself has no chat, as on WhatsApp: what is written to
//! it goes to its announcement group.

use crate::seed::{contact_for, self_contact, World};
use client_provider::{
    AccountId, Chat, ChatId, ChatKind, DeliveryStatus, Direction, Message, MessageContent,
    MessageId, Timestamp,
};

/// The account whose demo data holds the community.
pub const COMMUNITY_ACCOUNT: &str = "acc_personal";
/// The community: a group that links the ones below and has no chat.
pub const COMMUNITY: &str = "group:riversideschool";
/// The community's name.
pub const COMMUNITY_NAME: &str = "Riverside School";
/// Its announcement group.
pub const COMMUNITY_ANNOUNCEMENTS: &str = "group:riversideschoolannouncements";
/// The other groups it links.
pub const COMMUNITY_GROUPS: [&str; 2] = ["group:class3bparents", "group:schoolfootballteam"];

/// A group the community links that the account has not joined: it has no
/// chat, and the account is not in it.
pub const COMMUNITY_UNJOINED: &str = "group:schoolteachers";
/// Its name.
pub const COMMUNITY_UNJOINED_NAME: &str = "Teachers";

const DAY: i64 = 24 * 60 * 60 * 1000;

/// The community's chats: id, title, who writes in it, what was written.
const CHATS: [(&str, &str, &[&str], &[&str]); 3] = [
    (
        COMMUNITY_ANNOUNCEMENTS,
        "Announcements",
        &["Mrs. Okafor"],
        &[
            "Welcome to the Riverside School community.",
            "The spring fair is on the 14th, gates at ten.",
        ],
    ),
    (
        COMMUNITY_GROUPS[0],
        "Class 3B parents",
        &["Mrs. Okafor", "Tomasz Zielinski", "Ines Carvalho"],
        &[
            "Does anyone have the reading list?",
            "Sending a photo of it tonight.",
            "Thank you both.",
        ],
    ),
    (
        COMMUNITY_GROUPS[1],
        "School football team",
        &["Tomasz Zielinski", "Ines Carvalho"],
        &["Training moves to the upper pitch.", "Bibs are washed."],
    ),
];

/// Everyone in the community: the people of its groups.
pub(crate) const MEMBERS: [&str; 3] = ["Mrs. Okafor", "Tomasz Zielinski", "Ines Carvalho"];

/// Adds the community's chats to the world, quiet and older than every
/// other chat, so that the list above them reads as it did.
pub(crate) fn add(world: &mut World, now: Timestamp) {
    let account = AccountId::new(COMMUNITY_ACCOUNT);
    for (index, (id, title, members, lines)) in CHATS.into_iter().enumerate() {
        let chat_id = ChatId::new(id);
        let last_at = now.as_millis() - (30 + index as i64) * DAY;
        let count = lines.len() as i64;
        let thread: Vec<Message> = lines
            .iter()
            .enumerate()
            .map(|(at, line)| {
                let sender = members[at % members.len()];
                let id = world.next_message;
                world.next_message += 1;
                Message {
                    id: MessageId::new(format!("m{id}")),
                    client_id: None,
                    account_id: account.clone(),
                    chat_id: chat_id.clone(),
                    sender: contact_for(sender),
                    sender_name: Some(sender.to_owned()),
                    direction: Direction::Incoming,
                    timestamp: Timestamp::from_millis(last_at - (count - 1 - at as i64) * 60_000),
                    content: MessageContent::text(*line),
                    reply_to: None,
                    status: DeliveryStatus::Read,
                    edited: false,
                    deleted: false,
                    extras: Default::default(),
                }
            })
            .collect();
        world.chats.push(Chat {
            id: chat_id.clone(),
            account_id: account.clone(),
            kind: ChatKind::Group,
            title: title.to_owned(),
            avatar: None,
            unread_count: 0,
            pinned: false,
            muted: false,
            archived: false,
            last_message: thread.last().cloned(),
            unknown: Default::default(),
            picture_id: None,
            pinned_at: None,
        });
        world.members.insert(
            (account.clone(), chat_id.clone()),
            members.iter().map(|name| (*name).to_owned()).collect(),
        );
        world.messages.insert((account.clone(), chat_id), thread);
    }
}

/// The id the account goes by in the community.
pub(crate) fn me() -> client_provider::ContactId {
    self_contact(&AccountId::new(COMMUNITY_ACCOUNT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MockConfig, MockProvider};
    use client_provider::{Provider, ProviderEvent};
    use futures::StreamExt;

    fn ids() -> (AccountId, ChatId, ChatId, ChatId) {
        (
            AccountId::new(COMMUNITY_ACCOUNT),
            ChatId::new(COMMUNITY),
            ChatId::new(COMMUNITY_ANNOUNCEMENTS),
            ChatId::new(COMMUNITY_GROUPS[0]),
        )
    }

    #[tokio::test]
    async fn the_demo_data_has_a_community_with_its_announcement_group_and_two_more() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, announcements, class) = ids();

        // Every group is listed with the community it is linked to; the
        // listing does not say which groups a community links.
        let groups = provider.list_groups(&account).await.unwrap();
        let of = |id: &ChatId| groups.iter().find(|group| &group.id == id).unwrap();
        let parent = of(&community);
        assert!(parent.community && parent.community_id.is_none());
        assert_eq!(parent.subject, COMMUNITY_NAME);
        assert!(parent.subgroups.is_empty());
        assert_eq!(of(&announcements).community_id.as_ref(), Some(&community));
        assert!(of(&announcements).announcements);
        assert_eq!(of(&class).community_id.as_ref(), Some(&community));
        assert!(!of(&class).announcements);
        let plain = groups
            .iter()
            .filter(|group| !group.community && group.community_id.is_none())
            .count();
        assert_eq!(plain, 6, "the groups that were there are in no community");

        // Read on its own, the community says which groups it links.
        let read = provider.fetch_group(&account, &community).await.unwrap();
        let linked: Vec<(&str, bool)> = read
            .subgroups
            .iter()
            .map(|linked| (linked.subject.as_str(), linked.announcements))
            .collect();
        assert_eq!(
            linked,
            [
                ("Announcements", true),
                ("Class 3B parents", false),
                ("School football team", false),
                (COMMUNITY_UNJOINED_NAME, false),
            ]
        );

        // Its groups have chats; the community itself has none.
        let mut chats = Vec::new();
        let mut cursor = None;
        loop {
            let page = provider.list_chats(&account, cursor).await.unwrap();
            chats.extend(page.items.into_iter().map(|chat| chat.id));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert!(chats.contains(&announcements) && chats.contains(&class));
        assert!(!chats.contains(&community));
    }

    #[tokio::test]
    async fn linking_and_unlinking_a_group_is_announced() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, _, class) = ids();
        let mut events = provider.subscribe().await.unwrap();
        let changed = ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community.clone(),
            groups: vec![class.clone()],
        };

        provider.unlink_on_phone(&account, &class);
        assert_eq!(events.next().await, Some(changed.clone()));
        let read = provider.fetch_group(&account, &class).await.unwrap();
        assert_eq!(read.community_id, None);
        let parent = provider.fetch_group(&account, &community).await.unwrap();
        assert_eq!(parent.subgroups.len(), 3);

        provider.link_on_phone(&account, &community, &class);
        assert_eq!(events.next().await, Some(changed));
        let read = provider.fetch_group(&account, &class).await.unwrap();
        assert_eq!(read.community_id, Some(community));
    }

    #[tokio::test]
    async fn the_community_links_a_group_the_account_has_not_joined() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, ..) = ids();
        let unjoined = ChatId::new(COMMUNITY_UNJOINED);

        let read = provider.fetch_group(&account, &community).await.unwrap();
        assert_eq!(read.subgroups.len(), 4);
        let last = read.subgroups.last().unwrap();
        assert_eq!(
            (last.id.clone(), last.subject.as_str()),
            (unjoined.clone(), COMMUNITY_UNJOINED_NAME)
        );
        // The account is an admin of the community, not a member of that
        // group, which has no chat and is not listed.
        let me = provider.self_contact(&account);
        let admin = read.participants.iter().find(|p| p.contact == me).unwrap();
        assert!(admin.role.is_admin());
        assert!(provider.fetch_group(&account, &unjoined).await.is_err());
        let listed = provider.list_groups(&account).await.unwrap();
        assert!(listed.iter().all(|group| group.id != unjoined));
    }

    #[tokio::test]
    async fn an_admin_links_and_unlinks_a_group_and_every_change_is_announced() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, announcements, class) = ids();
        let mut events = provider.subscribe().await.unwrap();
        let plain = ChatId::new(crate::SHOWCASE_CHAT);
        let changed = |group: &ChatId| ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community.clone(),
            groups: vec![group.clone()],
        };

        provider
            .unlink_subgroup(&account, &community, &class)
            .await
            .unwrap();
        assert_eq!(events.next().await, Some(changed(&class)));
        // Unlinking what is not linked is done already.
        provider
            .unlink_subgroup(&account, &community, &class)
            .await
            .unwrap();
        // The announcement group stays.
        let refused = provider
            .unlink_subgroup(&account, &community, &announcements)
            .await
            .unwrap_err();
        assert!(matches!(
            refused,
            client_provider::ProviderError::Rejected { .. }
        ));

        provider
            .link_subgroup(&account, &community, &class, "link-1")
            .await
            .unwrap();
        assert_eq!(events.next().await, Some(changed(&class)));
        provider
            .link_subgroup(&account, &community, &class, "link-1")
            .await
            .unwrap();
        let read = provider.fetch_group(&account, &class).await.unwrap();
        assert_eq!(read.community_id.as_ref(), Some(&community));

        // A community is not linked to a community.
        let refused = provider
            .link_subgroup(&account, &community, &community, "link-2")
            .await
            .unwrap_err();
        assert!(matches!(
            refused,
            client_provider::ProviderError::Rejected { .. }
        ));
        provider
            .link_subgroup(&account, &community, &plain, "link-3")
            .await
            .unwrap();
        assert_eq!(
            provider
                .fetch_group(&account, &community)
                .await
                .unwrap()
                .subgroups
                .len(),
            5
        );
    }

    #[tokio::test]
    async fn a_group_is_created_inside_the_community_and_a_community_is_created() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, ..) = ids();
        let new = |subject: &str, request: &str| client_provider::NewGroup {
            subject: subject.into(),
            participants: vec![client_provider::ContactId::new("+15550000001")],
            request_id: request.into(),
            community: false,
            in_community: None,
        };

        let mut inside = new("Volunteers", "r-1");
        inside.in_community = Some(community.clone());
        let made = provider.create_group(&account, &inside).await.unwrap();
        assert_eq!(made.community_id.as_ref(), Some(&community));
        let again = provider.create_group(&account, &inside).await.unwrap();
        assert_eq!(again.id, made.id, "a repeat finds the same group");
        let read = provider.fetch_group(&account, &community).await.unwrap();
        assert!(read.subgroups.iter().any(|linked| linked.id == made.id));

        // Not in a group that is no community.
        let mut wrong = new("Nope", "r-2");
        wrong.in_community = Some(ChatId::new(crate::SHOWCASE_CHAT));
        assert!(provider.create_group(&account, &wrong).await.is_err());

        // A community needs no participants and brings its announcement
        // group with it.
        let mut fresh = new("Neighbours", "r-3");
        fresh.community = true;
        fresh.participants.clear();
        let parent = provider.create_group(&account, &fresh).await.unwrap();
        assert!(parent.community);
        let read = provider.fetch_group(&account, &parent.id).await.unwrap();
        assert_eq!(read.subgroups.len(), 1);
        assert!(read.subgroups[0].announcements);
    }

    #[tokio::test]
    async fn the_community_lists_the_people_of_all_its_groups() {
        let provider = MockProvider::new(MockConfig::default());
        let (account, community, ..) = ids();
        let people = provider
            .community_participants(&account, &community)
            .await
            .unwrap();
        assert_eq!(people.len(), MEMBERS.len() + 1);
        assert!(people.contains(&provider.self_contact(&account)));
    }
}
