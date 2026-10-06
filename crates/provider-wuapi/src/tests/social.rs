//! Profiles and groups: the mapping of the spec's examples and the calls,
//! against a mock API on localhost.

use super::{header, on, parse, provider, reply, requests, target, ACCOUNT, GROUP};
use crate::social;
use base64::Engine as _;
use client_provider::{
    refusal, AccountId, ChatId, ContactId, GroupChange, GroupRole, NewGroup, ParticipantChange,
    ProfileChange, Provider, ProviderError, Timestamp,
};
use wiremock::MockServer;
use wuapi::types as api;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

fn account() -> AccountId {
    AccountId::new(ACCOUNT)
}

fn group() -> ChatId {
    ChatId::new(GROUP)
}

fn error(code: &str, message: &str) -> String {
    format!(r#"{{"code":"{code}","message":"{message}"}}"#)
}

fn body(request: &wiremock::Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

fn at(tail: &str) -> String {
    format!("/v1/accounts/{ACCOUNT}{tail}")
}

fn group_at(tail: &str) -> String {
    // Ids travel percent-encoded.
    at(&format!("/groups/{}{tail}", GROUP.replace('@', "%40")))
}

// ----- mapping ------------------------------------------------------------

#[test]
fn maps_the_spec_example_group() {
    let wire: api::Group = parse(fixture!("group"));
    let mapped = social::group(&wire, Vec::new());
    assert_eq!(mapped.id.as_str(), GROUP);
    assert_eq!(mapped.account_id.as_str(), ACCOUNT);
    assert_eq!(mapped.subject, "Launch team");
    assert_eq!(
        mapped.description.as_deref(),
        Some("Coordination for the September launch.")
    );
    assert_eq!(
        mapped.owner.as_ref().map(ContactId::as_str),
        Some("+584121234567")
    );
    assert_eq!(
        mapped.created_at,
        Some(Timestamp::from_millis(1_788_264_000_000))
    );
    assert!(!mapped.community && !mapped.locked && !mapped.announce);
    // The Group object does not carry these two: not known, not "off".
    assert_eq!((mapped.join_approval, mapped.members_can_add), (None, None));
    let roles: Vec<(&str, Option<&str>, GroupRole)> = mapped
        .participants
        .iter()
        .map(|p| (p.contact.as_str(), p.name.as_deref(), p.role))
        .collect();
    assert_eq!(
        roles,
        [
            ("+584121234567", Some("Acme Support"), GroupRole::Owner),
            ("+584245550199", Some("Maria"), GroupRole::Member),
            ("lid:200055501000001", None, GroupRole::Member),
        ]
    );

    // A role newer than the SDK is a member, not an error.
    let newer = fixture!("group").replace(r#""role": "owner""#, r#""role": "moderator""#);
    let wire: api::Group = parse(&newer);
    assert_eq!(
        social::group(&wire, Vec::new()).participants[0].role,
        GroupRole::Member
    );
}

#[test]
fn maps_participant_results_business_profiles_and_the_rest_of_the_examples() {
    let results: api::ParticipantResultList = parse(fixture!("participant_result_list"));
    let outcomes: Vec<_> = results.items.iter().map(social::outcome).collect();
    assert!(outcomes[0].worked());
    assert_eq!(outcomes[1].error.as_deref(), Some(refusal::PRIVACY));
    assert_eq!(outcomes[1].invite_code.as_deref(), Some("AbCdEf123456"));
    assert!(!outcomes[1].worked());
    assert_eq!(outcomes[2].error.as_deref(), Some(refusal::ALREADY_MEMBER));
    assert_eq!(outcomes[2].contact.as_str(), "lid:200055501000001");

    let wire: api::BusinessProfile = parse(fixture!("business_profile"));
    let profile = social::business(&wire);
    assert_eq!(
        profile.address.as_deref(),
        Some("Av. Principal 12, Caracas")
    );
    assert_eq!(profile.email.as_deref(), Some("hola@example.com"));
    assert_eq!(profile.categories, ["Shopping & retail"]);
    assert_eq!(profile.time_zone.as_deref(), Some("America/Caracas"));
    assert_eq!(profile.hours.len(), 1);
    assert_eq!(
        (
            profile.hours[0].day.as_str(),
            profile.hours[0].open.as_deref()
        ),
        ("mon", Some("09:00"))
    );
    assert!(profile.websites.is_empty() && profile.description.is_none());

    // The other examples decode with the SDK's types.
    let _: api::GroupInviteLink = parse(fixture!("group_invite_link"));
    let _: api::GroupJoinRequestList = parse(fixture!("group_join_request_list"));
    let _: api::BlockedContactList = parse(fixture!("blocked_contact_list"));
    let _: api::SubgroupList = parse(fixture!("subgroup_list"));
    let _: api::ContactList = parse(fixture!("contact_lookup"));
}

// ----- contacts -----------------------------------------------------------

#[tokio::test]
async fn a_lookup_asks_whatsapp_about_one_contact_and_keeps_the_asked_id() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/contacts/lookup"),
        vec![reply(200, fixture!("contact_lookup"))],
    )
    .await;
    let provider = provider(&server);
    // Asked by its hidden-number id; the answer names the number.
    let asked = ContactId::new("lid:200055501000001");
    let found = provider.lookup_contact(&account(), &asked).await.unwrap();
    assert_eq!(found.id, asked, "stored under the id the chat goes by");
    assert_eq!(found.about.as_deref(), Some("Available"));
    assert_eq!(found.username.as_deref(), Some("maria.g"));
    assert_eq!(found.business_name.as_deref(), Some("Acme Bakery"));
    assert_eq!(found.picture_id.as_deref(), Some("1727164540"));
    // A lookup knows no names: nothing here may overwrite a saved one.
    assert_eq!(
        (found.saved_name, found.profile_name, found.name),
        (None, None, None)
    );
    let sent = requests(&server).await;
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({"contactIds": ["lid:200055501000001"]})
    );
}

#[tokio::test]
async fn a_business_profile_is_read_and_a_person_has_none() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/contacts/%2B584245550199/business-profile"),
        vec![
            reply(200, fixture!("business_profile")),
            reply(
                404,
                &error("business_profile_not_found", "No business profile."),
            ),
            reply(
                502,
                &error("whatsapp_error", "WhatsApp failed the operation."),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let contact = ContactId::new("+584245550199");
    let profile = provider
        .business_profile(&account(), &contact)
        .await
        .unwrap();
    assert_eq!(profile.unwrap().email.as_deref(), Some("hola@example.com"));
    assert_eq!(
        provider
            .business_profile(&account(), &contact)
            .await
            .unwrap(),
        None
    );
    // A hiccup is not "no profile".
    assert!(provider
        .business_profile(&account(), &contact)
        .await
        .unwrap_err()
        .is_transient());
}

#[tokio::test]
async fn blocking_calls_its_routes_with_the_request_id_and_lists_every_id() {
    let server = MockServer::start().await;
    let contact = ContactId::new("+584245550199");
    on(
        &server,
        "POST",
        &at("/contacts/%2B584245550199/block"),
        vec![
            reply(409, fixture!("error_not_ready")),
            wiremock::ResponseTemplate::new(204),
        ],
    )
    .await;
    on(
        &server,
        "POST",
        &at("/contacts/%2B584245550199/unblock"),
        vec![wiremock::ResponseTemplate::new(204)],
    )
    .await;
    on(
        &server,
        "GET",
        &at("/blocklist"),
        vec![reply(200, fixture!("blocked_contact_list"))],
    )
    .await;
    let provider = provider(&server);

    // The number is reconnecting: "not now", to be offered again with the
    // same key.
    let first = provider
        .set_blocked(&account(), &contact, true, "block-1")
        .await;
    assert!(first.unwrap_err().is_transient());
    provider
        .set_blocked(&account(), &contact, true, "block-1")
        .await
        .unwrap();
    provider
        .set_blocked(&account(), &contact, false, "block-2")
        .await
        .unwrap();
    let sent = requests(&server).await;
    assert_eq!(header(&sent[0], "idempotency-key"), Some("block-1"));
    assert_eq!(header(&sent[1], "idempotency-key"), Some("block-1"));
    assert_eq!(header(&sent[2], "idempotency-key"), Some("block-2"));

    // WhatsApp lists by hidden-number id: the number rides along, so a
    // chat known by its number is found blocked.
    let blocked = provider.list_blocked(&account()).await.unwrap();
    let ids: Vec<&str> = blocked.iter().map(ContactId::as_str).collect();
    assert_eq!(
        ids,
        ["lid:200055501000001", "+584245550199", "+584140000009"]
    );
}

// ----- the own profile ----------------------------------------------------

#[tokio::test]
async fn the_own_profile_is_the_accounts_name_and_a_lookup_of_its_own_number() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at(""),
        vec![
            reply(200, fixture!("account")),
            reply(200, fixture!("account")),
        ],
    )
    .await;
    on(
        &server,
        "POST",
        &at("/contacts/lookup"),
        vec![
            reply(200, fixture!("contact_lookup")),
            reply(
                404,
                &error("whatsapp_not_found", "WhatsApp does not know the target."),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let profile = provider.own_profile(&account()).await.unwrap();
    assert_eq!(profile.name.as_deref(), Some("Acme Support"));
    assert_eq!(profile.about.as_deref(), Some("Available"));
    let sent = requests(&server).await;
    assert_eq!(
        body(&sent[1]),
        serde_json::json!({"contactIds": ["+584121234567"]})
    );

    // WhatsApp does not answer for the own number: the About is not
    // known, and that is not an error.
    let profile = provider.own_profile(&account()).await.unwrap();
    assert_eq!(profile.name.as_deref(), Some("Acme Support"));
    assert_eq!(profile.about, None);
}

#[tokio::test]
async fn the_profile_is_patched_one_field_at_a_time_and_the_picture_goes_as_base64() {
    let server = MockServer::start().await;
    on(
        &server,
        "PATCH",
        &at("/profile"),
        vec![
            wiremock::ResponseTemplate::new(204),
            wiremock::ResponseTemplate::new(204),
            reply(400, fixture!("error_invalid")),
        ],
    )
    .await;
    on(
        &server,
        "PUT",
        &at("/profile/picture"),
        vec![reply(200, fixture!("picture"))],
    )
    .await;
    on(
        &server,
        "DELETE",
        &at("/profile/picture"),
        vec![wiremock::ResponseTemplate::new(204)],
    )
    .await;
    let provider = provider(&server);
    let about = ProfileChange::About("At the beach".into());
    provider.update_profile(&account(), &about).await.unwrap();
    let name = ProfileChange::Name("Vic".into());
    provider.update_profile(&account(), &name).await.unwrap();
    let refused = provider
        .update_profile(&account(), &name)
        .await
        .unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { .. }));

    let jpeg = [0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3];
    let id = provider
        .set_profile_picture(&account(), Some(&jpeg))
        .await
        .unwrap();
    assert_eq!(id.as_deref(), Some("1727164540"));
    assert_eq!(
        provider
            .set_profile_picture(&account(), None)
            .await
            .unwrap(),
        None
    );

    let sent = requests(&server).await;
    assert_eq!(body(&sent[0]), serde_json::json!({"about": "At the beach"}));
    assert_eq!(body(&sent[1]), serde_json::json!({"name": "Vic"}));
    let encoded = base64::engine::general_purpose::STANDARD.encode(jpeg);
    assert_eq!(body(&sent[3]), serde_json::json!({ "base64": encoded }));
    assert_eq!(sent[4].method.as_str(), "DELETE");
}

// ----- groups -------------------------------------------------------------

#[tokio::test]
async fn a_group_is_read_with_the_groups_a_community_links() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &group_at(""),
        vec![
            reply(200, fixture!("group")),
            reply(200, fixture!("group_community")),
            reply(403, &error("whatsapp_forbidden", "WhatsApp refused.")),
            reply(404, &error("group_not_found", "No such group.")),
            reply(409, fixture!("error_not_ready")),
        ],
    )
    .await;
    on(
        &server,
        "GET",
        &group_at("/subgroups"),
        vec![reply(200, fixture!("subgroup_list"))],
    )
    .await;
    let provider = provider(&server);
    let plain = provider.fetch_group(&account(), &group()).await.unwrap();
    assert_eq!(plain.participants.len(), 3);
    assert!(plain.subgroups.is_empty());
    assert_eq!(
        requests(&server).await.len(),
        1,
        "a plain group is one request"
    );

    let community = provider.fetch_group(&account(), &group()).await.unwrap();
    assert!(community.community && community.announce);
    let linked: Vec<(&str, bool)> = community
        .subgroups
        .iter()
        .map(|linked| (linked.subject.as_str(), linked.announcements))
        .collect();
    assert_eq!(linked, [("Announcements", true), ("Volunteers", false)]);

    // Refused: the number is not in the group. Gone: WhatsApp does not
    // know it. Offline: not now.
    let code = |error: ProviderError| match error {
        ProviderError::Rejected { code, .. } => code,
        other => panic!("{other:?}"),
    };
    let out = provider
        .fetch_group(&account(), &group())
        .await
        .unwrap_err();
    assert_eq!(code(out), refusal::NOT_MEMBER);
    let gone = provider
        .fetch_group(&account(), &group())
        .await
        .unwrap_err();
    assert_eq!(code(gone), refusal::NOT_FOUND);
    let offline = provider
        .fetch_group(&account(), &group())
        .await
        .unwrap_err();
    assert!(offline.is_transient());
}

#[tokio::test]
async fn every_group_is_listed_with_its_participants() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/groups"),
        vec![reply(200, fixture!("group_list"))],
    )
    .await;
    let groups = provider(&server).list_groups(&account()).await.unwrap();
    assert!(!groups.is_empty());
    assert_eq!(groups[0].id.as_str(), GROUP);
    assert_eq!(groups[0].participants.len(), 3);
}

/// The community of the community fixtures, and two of its groups.
const ANNOUNCEMENTS: &str = "120363055512345678@g.us";
const VOLUNTEERS: &str = "120363055512345679@g.us";

#[test]
fn maps_the_community_a_group_is_linked_to() {
    let wire: api::Group = parse(fixture!("group_subgroup"));
    let mapped = social::group(&wire, Vec::new());
    assert_eq!(
        mapped.community_id.as_ref().map(ChatId::as_str),
        Some(GROUP)
    );
    assert!(!mapped.community && !mapped.announcements);

    // A plain group is in none, and a community is not in itself.
    for plain in [fixture!("group"), fixture!("group_community")] {
        let wire: api::Group = parse(plain);
        let mapped = social::group(&wire, Vec::new());
        assert_eq!(mapped.community_id, None);
        assert!(!mapped.announcements);
    }
}

#[tokio::test]
async fn every_group_is_listed_with_the_community_it_is_linked_to() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/groups"),
        vec![reply(200, fixture!("group_list_community"))],
    )
    .await;
    let groups = provider(&server).list_groups(&account()).await.unwrap();
    let listed: Vec<(&str, bool, Option<&str>, bool)> = groups
        .iter()
        .map(|group| {
            (
                group.subject.as_str(),
                group.community,
                group.community_id.as_ref().map(ChatId::as_str),
                group.announcements,
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("Neighbours", true, None, false),
            ("Announcements", false, Some(GROUP), true),
            ("Volunteers", false, Some(GROUP), false),
            ("Book club", false, None, false),
        ]
    );
    assert_eq!(groups[1].id.as_str(), ANNOUNCEMENTS);
    assert_eq!(groups[2].id.as_str(), VOLUNTEERS);
    // The listing does not say which groups a community links.
    assert!(groups[0].subgroups.is_empty());
    assert_eq!(requests(&server).await.len(), 1, "one request for all");
}

#[tokio::test]
async fn creating_a_group_sends_the_request_id_as_the_idempotency_key() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/groups"),
        vec![
            reply(503, &error("engine_unavailable", "Retry shortly.")),
            reply(201, fixture!("group")),
        ],
    )
    .await;
    let provider = provider(&server);
    let new = NewGroup {
        subject: "Launch team".into(),
        participants: vec![
            ContactId::new("+584245550199"),
            ContactId::new("lid:200055501000001"),
        ],
        request_id: "create-7".into(),
        community: false,
        in_community: None,
    };
    assert!(provider
        .create_group(&account(), &new)
        .await
        .unwrap_err()
        .is_transient());
    let created = provider.create_group(&account(), &new).await.unwrap();
    assert_eq!(created.id.as_str(), GROUP);
    let sent = requests(&server).await;
    assert_eq!(sent.len(), 2, "the adapter itself does not retry");
    for request in &sent {
        assert_eq!(header(request, "idempotency-key"), Some("create-7"));
        assert_eq!(
            body(request),
            serde_json::json!({
                "name": "Launch team",
                "participants": ["+584245550199", "lid:200055501000001"]
            })
        );
    }
}

#[tokio::test]
async fn participants_are_changed_on_their_routes_and_answered_one_by_one() {
    let server = MockServer::start().await;
    for route in ["add", "remove", "promote"] {
        on(
            &server,
            "POST",
            &group_at(&format!("/participants/{route}")),
            vec![reply(200, fixture!("participant_result_list"))],
        )
        .await;
    }
    on(
        &server,
        "POST",
        &group_at("/participants/demote"),
        vec![
            reply(403, &error("whatsapp_forbidden", "WhatsApp refused.")),
            ResponseTemplate429::slow_down(),
        ],
    )
    .await;
    let provider = provider(&server);
    let people = [
        ContactId::new("+584245550199"),
        ContactId::new("+584140000002"),
    ];
    let (account, group) = (account(), group());
    let change = |change, key: &'static str| {
        provider.change_participants(&account, &group, change, &people, key)
    };
    let added = change(ParticipantChange::Add, "k-add").await.unwrap();
    assert_eq!(added.len(), 3);
    assert!(added[0].worked());
    assert_eq!(added[1].error.as_deref(), Some(refusal::PRIVACY));
    change(ParticipantChange::Remove, "k-remove").await.unwrap();
    change(ParticipantChange::Promote, "k-promote")
        .await
        .unwrap();

    // Not an admin: said in words, with the neutral code. A rate limit:
    // to be retried after what the API asked for.
    match change(ParticipantChange::Demote, "k-demote")
        .await
        .unwrap_err()
    {
        ProviderError::Rejected { code, message } => {
            assert_eq!(code, refusal::NOT_ADMIN);
            assert_eq!(message, "Only admins of the group can do that.");
        }
        other => panic!("{other:?}"),
    }
    let limited = change(ParticipantChange::Demote, "k-demote")
        .await
        .unwrap_err();
    assert_eq!(
        limited.retry_after(),
        Some(std::time::Duration::from_secs(7))
    );

    let sent = requests(&server).await;
    let asked: Vec<(String, Option<&str>)> = sent
        .iter()
        .map(|request| (target(request), header(request, "idempotency-key")))
        .collect();
    assert_eq!(
        asked,
        [
            (group_at("/participants/add"), Some("k-add")),
            (group_at("/participants/remove"), Some("k-remove")),
            (group_at("/participants/promote"), Some("k-promote")),
            (group_at("/participants/demote"), Some("k-demote")),
            (group_at("/participants/demote"), Some("k-demote")),
        ]
    );
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({"contactIds": ["+584245550199", "+584140000002"]})
    );
}

/// A `429` with `Retry-After`.
struct ResponseTemplate429;

impl ResponseTemplate429 {
    fn slow_down() -> wiremock::ResponseTemplate {
        reply(429, &error("rate_limited", "Too many requests.")).insert_header("retry-after", "7")
    }
}

#[tokio::test]
async fn a_groups_details_and_settings_are_patched_one_field_at_a_time() {
    let server = MockServer::start().await;
    let mut replies = vec![reply(200, fixture!("group")); 6];
    replies.push(reply(
        403,
        &error("whatsapp_forbidden", "WhatsApp refused."),
    ));
    on(&server, "PATCH", &group_at(""), replies).await;
    let provider = provider(&server);
    let changes = [
        (
            GroupChange::Subject("Launch".into()),
            serde_json::json!({"name": "Launch"}),
        ),
        (
            GroupChange::Description("Rules".into()),
            serde_json::json!({"description": "Rules"}),
        ),
        (
            GroupChange::Announce(true),
            serde_json::json!({"announce": true}),
        ),
        (
            GroupChange::Locked(false),
            serde_json::json!({"locked": false}),
        ),
        (
            GroupChange::JoinApproval(true),
            serde_json::json!({"joinApproval": true}),
        ),
        (
            GroupChange::MembersCanAdd(false),
            serde_json::json!({"memberAddMode": "admins"}),
        ),
    ];
    for (change, _) in &changes {
        provider
            .update_group(&account(), &group(), change)
            .await
            .unwrap();
    }
    let refused = provider
        .update_group(&account(), &group(), &GroupChange::MembersCanAdd(true))
        .await
        .unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { code, .. } if code == refusal::NOT_ADMIN));
    let sent = requests(&server).await;
    for (request, (_, expected)) in sent.iter().zip(&changes) {
        assert_eq!(&body(request), expected);
    }
    assert_eq!(
        body(&sent[6]),
        serde_json::json!({"memberAddMode": "all_members"})
    );
}

#[tokio::test]
async fn the_picture_the_invite_link_the_requests_to_join_and_leaving() {
    let server = MockServer::start().await;
    on(
        &server,
        "PUT",
        &group_at("/picture"),
        vec![reply(200, fixture!("picture"))],
    )
    .await;
    on(
        &server,
        "DELETE",
        &group_at("/picture"),
        vec![reply(
            403,
            &error("whatsapp_forbidden", "WhatsApp refused."),
        )],
    )
    .await;
    on(
        &server,
        "GET",
        &group_at("/invite-link"),
        vec![reply(200, fixture!("group_invite_link"))],
    )
    .await;
    on(
        &server,
        "POST",
        &group_at("/invite-link/reset"),
        vec![reply(200, fixture!("group_invite_link"))],
    )
    .await;
    on(
        &server,
        "GET",
        &group_at("/join-requests"),
        vec![reply(200, fixture!("group_join_request_list"))],
    )
    .await;
    for answer in ["approve", "reject"] {
        on(
            &server,
            "POST",
            &group_at(&format!("/join-requests/{answer}")),
            vec![reply(200, fixture!("participant_result_list"))],
        )
        .await;
    }
    on(
        &server,
        "POST",
        &group_at("/leave"),
        vec![
            wiremock::ResponseTemplate::new(204),
            reply(403, &error("whatsapp_forbidden", "WhatsApp refused.")),
        ],
    )
    .await;
    let provider = provider(&server);
    let (account, group) = (account(), group());

    let id = provider
        .set_group_picture(&account, &group, Some(&[0xFF, 0xD8]))
        .await
        .unwrap();
    assert_eq!(id.as_deref(), Some("1727164540"));
    let refused = provider
        .set_group_picture(&account, &group, None)
        .await
        .unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { code, .. } if code == refusal::NOT_ADMIN));

    let link = provider
        .group_invite_link(&account, &group, false, "l-1")
        .await
        .unwrap();
    assert_eq!(link, "https://chat.whatsapp.com/AbCdEf123456");
    provider
        .group_invite_link(&account, &group, true, "l-2")
        .await
        .unwrap();

    let pending = provider.join_requests(&account, &group).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].contact.as_str(), "+584245550199");
    assert_eq!(
        pending[0].requested_at,
        Some(Timestamp::from_millis(1_790_237_740_000))
    );
    let who = [pending[0].contact.clone()];
    let approved = provider
        .answer_join_requests(&account, &group, true, &who, "j-1")
        .await
        .unwrap();
    assert!(approved[0].worked());
    provider
        .answer_join_requests(&account, &group, false, &who, "j-2")
        .await
        .unwrap();

    provider
        .leave_group(&account, &group, "leave-1")
        .await
        .unwrap();
    // Leaving a group the number is not in: said as such, so the client
    // can take it as done.
    let out = provider
        .leave_group(&account, &group, "leave-1")
        .await
        .unwrap_err();
    assert!(matches!(out, ProviderError::Rejected { code, .. } if code == refusal::NOT_MEMBER));

    let sent = requests(&server).await;
    let keyed: Vec<(String, Option<&str>)> = sent
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .map(|request| (target(request), header(request, "idempotency-key")))
        .collect();
    assert_eq!(
        keyed,
        [
            (group_at("/invite-link/reset"), Some("l-2")),
            (group_at("/join-requests/approve"), Some("j-1")),
            (group_at("/join-requests/reject"), Some("j-2")),
            (group_at("/leave"), Some("leave-1")),
            (group_at("/leave"), Some("leave-1")),
        ]
    );
}

#[test]
fn the_adapter_says_it_does_profiles_and_groups() {
    let server_less = crate::provider::WuapiProvider::new(
        crate::config::WuapiConfig::new("test-agent/1.0"),
        crate::config::ApiKey::new(super::KEY),
    )
    .unwrap();
    let caps = server_less.capabilities();
    assert!(caps.contact_lookup && caps.business_profiles && caps.blocking && caps.profile_edit);
    assert!(caps.group_info && caps.group_create && caps.group_manage);
    assert!(caps.group_invites && caps.group_join_requests && caps.group_leave);
}

// ----- communities ----------------------------------------------------------

fn sub(id: &str) -> ChatId {
    ChatId::new(id)
}

#[tokio::test]
async fn a_group_is_linked_and_unlinked_on_the_communitys_subgroup_route() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &group_at("/subgroups"),
        vec![
            reply(503, &error("engine_unavailable", "Retry shortly.")),
            wiremock::ResponseTemplate::new(204),
            reply(403, &error("whatsapp_forbidden", "WhatsApp refused.")),
        ],
    )
    .await;
    on(
        &server,
        "DELETE",
        &group_at("/subgroups/120363055512345679%40g.us"),
        vec![
            wiremock::ResponseTemplate::new(204),
            reply(403, &error("whatsapp_forbidden", "WhatsApp refused.")),
        ],
    )
    .await;
    let provider = provider(&server);
    let (account, community, linked) = (account(), group(), sub("120363055512345679@g.us"));

    assert!(provider
        .link_subgroup(&account, &community, &linked, "link-1")
        .await
        .unwrap_err()
        .is_transient());
    provider
        .link_subgroup(&account, &community, &linked, "link-1")
        .await
        .unwrap();
    let refused = provider
        .link_subgroup(&account, &community, &linked, "link-2")
        .await
        .unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { code, .. } if code == refusal::NOT_ADMIN));

    provider
        .unlink_subgroup(&account, &community, &linked)
        .await
        .unwrap();
    let refused = provider
        .unlink_subgroup(&account, &community, &linked)
        .await
        .unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { code, .. } if code == refusal::NOT_ADMIN));

    let sent = requests(&server).await;
    let posts: Vec<_> = sent
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .collect();
    assert_eq!(posts.len(), 3);
    for (post, key) in posts.iter().zip(["link-1", "link-1", "link-2"]) {
        assert_eq!(header(post, "idempotency-key"), Some(key));
        assert_eq!(
            body(post),
            serde_json::json!({ "groupId": "120363055512345679@g.us" })
        );
    }
}

#[tokio::test]
async fn a_communitys_participants_are_every_contact_in_its_pages() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &group_at("/community-participants"),
        vec![reply(200, fixture!("community_participant_list"))],
    )
    .await;
    let people = provider(&server)
        .community_participants(&account(), &group())
        .await
        .unwrap();
    let people: Vec<&str> = people.iter().map(ContactId::as_str).collect();
    assert_eq!(people, ["+584121234567", "lid:200055501000001"]);
}

#[tokio::test]
async fn a_group_is_created_in_a_community_and_a_community_is_created_empty() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/groups"),
        vec![
            reply(201, fixture!("group")),
            reply(201, fixture!("group_community")),
            reply(
                400,
                &error("not_supported", "Not available for this number yet."),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let mut new = NewGroup {
        subject: "Volunteers".into(),
        participants: vec![ContactId::new("+584245550199")],
        request_id: "create-8".into(),
        community: false,
        in_community: Some(group()),
    };
    provider.create_group(&account(), &new).await.unwrap();
    new = NewGroup {
        subject: "Neighbours".into(),
        participants: Vec::new(),
        request_id: "create-9".into(),
        community: true,
        in_community: None,
    };
    let made = provider.create_group(&account(), &new).await.unwrap();
    assert!(made.community);
    // The engine cannot do it for this number: said plainly, and not
    // something to try again.
    new.in_community = Some(group());
    new.community = false;
    let refused = provider.create_group(&account(), &new).await.unwrap_err();
    assert!(!refused.is_transient());
    assert!(matches!(
        &refused,
        ProviderError::Rejected { code, message }
            if code == "not_supported" && message.contains("community")
    ));

    let sent = requests(&server).await;
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({
            "name": "Volunteers",
            "participants": ["+584245550199"],
            "communityId": GROUP
        })
    );
    assert_eq!(
        body(&sent[1]),
        serde_json::json!({ "name": "Neighbours", "community": true })
    );
}

#[test]
fn the_adapter_says_it_does_communities() {
    let server_less = crate::provider::WuapiProvider::new(
        crate::config::WuapiConfig::new("test-agent/1.0"),
        crate::config::ApiKey::new(super::KEY),
    )
    .unwrap();
    let caps = server_less.capabilities();
    assert!(caps.community_manage && caps.community_members);
}
