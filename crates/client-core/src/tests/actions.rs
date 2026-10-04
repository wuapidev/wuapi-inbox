//! What is done to a message after it was sent: shown at once, told to the
//! provider through any number of dropped connections, and put back only
//! when the provider refuses.

use super::*;
use client_provider::{OutgoingContent, ProviderError};

/// A text of the account's own that reached the provider: where it is,
/// and its id there.
async fn sent(engine: &SyncEngine, body: &str) -> (AccountId, ChatId, MessageId) {
    let (account, chat) = first_chat(engine).await;
    engine.send_text(&account, &chat, body, None).unwrap();
    engine.flush_outbox().await.unwrap();
    let newest = engine
        .store()
        .messages(&account, &chat, 1)
        .unwrap()
        .remove(0)
        .message;
    assert_eq!(newest.status, DeliveryStatus::Sent);
    (account, chat, newest.id)
}

fn stored(engine: &SyncEngine, account: &AccountId, id: &MessageId) -> Option<Message> {
    engine.store().message(account, id).unwrap()
}

fn problems(changes: &mut crate::ChangeListener) -> Vec<String> {
    let mut said = Vec::new();
    while let Some(change) = changes.try_next() {
        if let StoreChange::Problem { message } = change {
            said.push(message);
        }
    }
    said
}

#[tokio::test(start_paused = true)]
async fn an_edit_shows_at_once_and_survives_a_bad_connection() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id) = sent(&engine, "see you at 8").await;

    mock.fail_next_actions([
        ProviderError::Transient("eof".into()),
        ProviderError::RateLimited { retry_after: None },
    ]);
    engine
        .edit_message(&account, &chat, &id, "see you at 9".into())
        .unwrap();
    let shown = stored(&engine, &account, &id).unwrap();
    assert_eq!(shown.content, MessageContent::text("see you at 9"));
    assert!(shown.edited);
    assert!(mock.actions().is_empty(), "nothing got through yet");

    settle().await;
    assert_eq!(mock.actions(), [format!("edit {id}")], "told once");
    assert_eq!(
        mock.message(&account, &id).unwrap().content,
        MessageContent::text("see you at 9")
    );
    // The same text again is not an edit.
    engine
        .edit_message(&account, &chat, &id, "see you at 9".into())
        .unwrap();
    settle().await;
    assert_eq!(mock.actions().len(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_edit_the_provider_refuses_is_put_back_and_said() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id) = sent(&engine, "the first text").await;
    let mut changes = engine.store().subscribe();

    mock.fail_next_actions([ProviderError::Rejected {
        code: "edit_window_closed".into(),
        message: "It is too late to edit this message.".into(),
    }]);
    engine
        .edit_message(&account, &chat, &id, "a second text".into())
        .unwrap();
    settle().await;
    let back = stored(&engine, &account, &id).unwrap();
    assert_eq!(back.content, MessageContent::text("the first text"));
    assert!(!back.edited);
    let said = problems(&mut changes);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(
        said[0].contains("not edited") && said[0].contains("too late"),
        "{said:?}"
    );

    // A connection that never comes back gives up in the end, and says
    // that: the text is as it was.
    mock.fail_next_actions((0..20).map(|_| ProviderError::Transient("eof".into())));
    engine
        .edit_message(&account, &chat, &id, "a third text".into())
        .unwrap();
    settle().await;
    settle().await;
    assert_eq!(
        stored(&engine, &account, &id).unwrap().content,
        MessageContent::text("the first text")
    );
    let said = problems(&mut changes);
    assert!(
        said.last().unwrap().contains("connection kept failing"),
        "{said:?}"
    );

    // Only one's own text can be edited: anything else is left alone.
    let theirs = text("theirs", chat.as_str(), 5, "not mine", Direction::Incoming);
    let theirs = Message {
        account_id: account.clone(),
        ..theirs
    };
    engine.store().upsert_message(&theirs).unwrap();
    engine
        .edit_message(&account, &chat, &theirs.id, "mine now".into())
        .unwrap();
    settle().await;
    assert_eq!(
        stored(&engine, &account, &theirs.id).unwrap().content,
        MessageContent::text("not mine")
    );
}

#[tokio::test(start_paused = true)]
async fn editing_what_has_not_left_yet_changes_what_is_sent() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat) = first_chat(&engine).await;
    let client_id = engine.send_text(&account, &chat, "xylophne", None).unwrap();
    let local = crate::local_message_id(&client_id);

    engine
        .edit_message(&account, &chat, &local, "xylophone".into())
        .unwrap();
    let pending = engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1, "still one message waiting");
    assert_eq!(
        pending[0].message.content,
        OutgoingContent::Text {
            body: "xylophone".into()
        }
    );
    let bubble = stored(&engine, &account, &local).unwrap();
    assert_eq!(bubble.content, MessageContent::text("xylophone"));
    assert!(!bubble.edited, "nobody saw the old text");

    engine.flush_outbox().await.unwrap();
    settle().await;
    assert!(
        mock.actions().is_empty(),
        "the provider was never asked to edit"
    );
    let newest = engine
        .store()
        .messages(&account, &chat, 1)
        .unwrap()
        .remove(0)
        .message;
    assert_eq!(newest.content, MessageContent::text("xylophone"));
    assert_eq!(mock.delivered_count(), 1);
    // And it can be found by its new text.
    assert_eq!(
        engine
            .store()
            .search_messages(&account, "xylophone", 5)
            .unwrap()
            .len(),
        1
    );
    assert!(engine
        .store()
        .search_messages(&account, "xylophne", 5)
        .unwrap()
        .is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_message_is_deleted_for_everyone_or_for_the_account_alone() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, first) = sent(&engine, "one").await;
    let (_, _, second) = sent(&engine, "two").await;
    let mut changes = engine.store().subscribe();

    // For everyone: the row stays, marked as deleted.
    mock.fail_next_actions([ProviderError::Transient("eof".into())]);
    engine
        .delete_message(&account, &chat, &first, true)
        .unwrap();
    assert!(stored(&engine, &account, &first).unwrap().deleted);
    settle().await;
    assert_eq!(mock.actions(), [format!("delete-all {first}")]);
    assert!(mock.message(&account, &first).unwrap().deleted);

    // For the account alone: the row goes.
    engine
        .delete_message(&account, &chat, &second, false)
        .unwrap();
    assert!(stored(&engine, &account, &second).is_none());
    settle().await;
    assert_eq!(mock.actions()[1], format!("delete-me {second}"));
    assert!(mock.message(&account, &second).is_none());
    assert!(problems(&mut changes).is_empty());

    // A refusal brings back what was taken away.
    let (_, _, third) = sent(&engine, "three").await;
    mock.fail_next_actions([ProviderError::Rejected {
        code: "forbidden".into(),
        message: "Too late to delete.".into(),
    }]);
    engine
        .delete_message(&account, &chat, &third, false)
        .unwrap();
    assert!(stored(&engine, &account, &third).is_none());
    settle().await;
    let back = stored(&engine, &account, &third).expect("the message is back");
    assert_eq!(back.content, MessageContent::text("three"));
    let said = problems(&mut changes);
    assert!(
        said[0].contains("not deleted") && said[0].contains("Too late"),
        "{said:?}"
    );

    // What never left is taken back from the outbox: no provider.
    let before = mock.actions().len();
    let client_id = engine
        .send_text(&account, &chat, "never mind", None)
        .unwrap();
    let local = crate::local_message_id(&client_id);
    engine
        .delete_message(&account, &chat, &local, true)
        .unwrap();
    assert!(stored(&engine, &account, &local).is_none());
    assert!(engine.store().outbox_pending().unwrap().is_empty());
    settle().await;
    assert_eq!(mock.actions().len(), before);
}

#[tokio::test(start_paused = true)]
async fn a_star_is_set_and_taken_back_and_the_newest_wish_wins() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id) = sent(&engine, "keep this").await;

    engine.star_message(&account, &chat, &id, true).unwrap();
    assert!(stored(&engine, &account, &id).unwrap().extras.starred);
    settle().await;
    assert_eq!(mock.actions(), [format!("star {id}")]);

    // Starred and unstarred again while the connection is down: the last
    // one stands, whatever happens to the first.
    mock.fail_next_actions([ProviderError::Transient("eof".into())]);
    engine.star_message(&account, &chat, &id, false).unwrap();
    engine.star_message(&account, &chat, &id, true).unwrap();
    settle().await;
    assert!(stored(&engine, &account, &id).unwrap().extras.starred);
    assert!(mock.message(&account, &id).unwrap().extras.starred);
}

#[tokio::test(start_paused = true)]
async fn a_reaction_and_a_forward_are_messages_of_the_outbox() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id) = sent(&engine, "we won").await;

    engine.react(&account, &chat, &id, "🎉").unwrap();
    let reactions = engine
        .store()
        .messages(&account, &chat, 1)
        .unwrap()
        .remove(0)
        .reactions;
    assert_eq!(reactions.len(), 1, "shown at once");
    assert!(reactions[0].from_me && reactions[0].emoji == "🎉");
    // An empty one takes it back.
    engine.react(&account, &chat, &id, "").unwrap();
    assert!(engine
        .store()
        .messages(&account, &chat, 1)
        .unwrap()
        .remove(0)
        .reactions
        .is_empty());

    let other = engine.store().chats(&account, None).unwrap().remove(1).id;
    engine
        .forward_text(&account, &other, "we won".into())
        .unwrap();
    let pending = engine.store().outbox_pending().unwrap();
    let forward = pending.last().unwrap();
    assert!(forward.message.forwarded);
    assert_eq!(forward.message.chat_id, other);
    engine.flush_outbox().await.unwrap();
    let delivered = mock
        .fetch_messages(&account, &other, None, 1)
        .await
        .unwrap();
    assert!(delivered.items[0].extras.forwarded);
}
