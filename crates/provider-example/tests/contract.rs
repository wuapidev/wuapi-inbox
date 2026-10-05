//! What the client relies on, said as tests. A provider of your own needs
//! the same ones: copy this file and change the constructor.

use client_provider::{
    AccountId, ChatId, ClientMessageId, DeliveryStatus, Direction, MessageContent, MessageId,
    OutgoingContent, OutgoingMessage, Provider, ProviderError, ProviderEvent,
};
use futures::StreamExt as _;
use provider_example::EchoProvider;

fn text(client_id: &str, body: &str) -> OutgoingMessage {
    OutgoingMessage {
        client_id: ClientMessageId::new(client_id),
        account_id: AccountId::new("echo"),
        chat_id: ChatId::new("echo"),
        content: OutgoingContent::Text { body: body.into() },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    }
}

async fn history(provider: &EchoProvider) -> Vec<client_provider::Message> {
    provider
        .fetch_messages(&AccountId::new("echo"), &ChatId::new("echo"), None, 50)
        .await
        .unwrap()
        .items
}

#[tokio::test]
async fn the_account_and_its_chat_are_listed_with_a_preview() {
    let provider = EchoProvider::default();
    let accounts = provider.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    let chats = provider.list_chats(&accounts[0].id, None).await.unwrap();
    assert_eq!(chats.items.len(), 1);
    assert!(chats.next_cursor.is_none(), "one page is all there is");
    assert!(chats.items[0].last_message.is_some());
}

#[tokio::test]
async fn a_send_repeated_is_one_message_with_one_id() {
    let provider = EchoProvider::default();
    let first = provider.send(text("c1", "hello")).await.unwrap();
    let again = provider.send(text("c1", "hello")).await.unwrap();
    assert_eq!(first.message_id, again.message_id);
    assert_eq!(first.status, DeliveryStatus::Sent);

    let sent: Vec<_> = history(&provider)
        .await
        .into_iter()
        .filter(|message| message.direction == Direction::Outgoing)
        .collect();
    assert_eq!(sent.len(), 1);
    // The client's id comes back on the stored copy.
    assert_eq!(sent[0].client_id, Some(ClientMessageId::new("c1")));
    assert_eq!(sent[0].id, first.message_id);
}

#[tokio::test]
async fn the_reply_is_pushed_to_whoever_subscribed() {
    let provider = EchoProvider::default();
    let mut events = provider.subscribe().await.unwrap();
    provider.send(text("c1", "ping")).await.unwrap();
    let Some(ProviderEvent::MessageUpserted(reply)) = events.next().await else {
        panic!("expected the reply");
    };
    assert_eq!(reply.direction, Direction::Incoming);
    assert_eq!(reply.content, MessageContent::text("ping"));
}

#[tokio::test]
async fn history_is_newest_first_and_pages_back_to_the_start() {
    let provider = EchoProvider::default();
    for index in 0..3 {
        provider
            .send(text(&format!("c{index}"), &format!("message {index}")))
            .await
            .unwrap();
    }
    let (account, chat) = (AccountId::new("echo"), ChatId::new("echo"));
    let mut ids: Vec<MessageId> = Vec::new();
    let mut cursor = None;
    loop {
        let page = provider
            .fetch_messages(&account, &chat, cursor, 2)
            .await
            .unwrap();
        assert!(page.items.len() <= 2);
        ids.extend(page.items.into_iter().map(|message| message.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    // The greeting, then a sent message and its reply three times over.
    assert_eq!(ids.len(), 7);
    assert_eq!(ids[0], MessageId::new("reply-c2"));
    assert_eq!(ids[6], MessageId::new("hello"));
}

#[tokio::test]
async fn what_cannot_be_sent_is_refused_for_good_not_retried() {
    let provider = EchoProvider::default();
    let mut reaction = text("c1", "");
    reaction.content = OutgoingContent::Reaction {
        target: MessageId::new("hello"),
        emoji: "👍".into(),
    };
    let error = provider.send(reaction).await.unwrap_err();
    assert!(matches!(error, ProviderError::Rejected { .. }));
    assert!(!error.is_transient());
    assert!(
        !provider.capabilities().reactions,
        "and the window never offers it"
    );
}

/// The README shows the smallest provider; the same lines are a doc-test
/// of this crate, so they compile. This keeps the two alike.
#[test]
fn the_readme_shows_the_example_that_is_compiled() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let source =
        std::fs::read_to_string(format!("{root}/crates/provider-example/src/lib.rs")).unwrap();
    let readme = std::fs::read_to_string(format!("{root}/README.md")).unwrap();
    let example: Vec<&str> = source
        .lines()
        .skip_while(|line| *line != "//! ```rust")
        .skip(1)
        .take_while(|line| *line != "//! ```")
        .map(|line| line.strip_prefix("//! ").unwrap_or(""))
        .collect();
    assert!(example.len() > 20, "the example was found");
    let example = example.join("\n");
    assert!(
        readme.replace("\r\n", "\n").contains(&example),
        "README.md and the doc-test of provider-example differ"
    );
}
