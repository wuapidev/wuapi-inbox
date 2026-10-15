//! Tasks: kept, listed in the order they are wanted, done and undone,
//! edited, and gone with the number they came from.

use super::*;

fn draft(title: &str, due: Option<i64>) -> TaskDraft {
    TaskDraft {
        title: title.into(),
        notes: None,
        due_at: due.map(Timestamp::from_millis),
    }
}

fn source(message: &str) -> TaskSource {
    TaskSource {
        account_id: account(),
        chat_id: chat_id(),
        message_id: MessageId::new(message),
        quote: "Send the contract by Friday".into(),
    }
}

fn titles(store: &Store) -> Vec<String> {
    store
        .tasks()
        .unwrap()
        .into_iter()
        .map(|task| task.title)
        .collect()
}

#[test]
fn tasks_are_listed_due_first_then_undated_then_done() {
    let store = Store::open_in_memory().unwrap();
    let now = Timestamp::from_millis(1_000);
    store.add_task(&draft("no day", None), None, now).unwrap();
    store
        .add_task(&draft("later", Some(9_000)), None, now)
        .unwrap();
    let soon = store
        .add_task(&draft("soon", Some(5_000)), None, now)
        .unwrap()
        .unwrap();
    let done = store
        .add_task(&draft("done already", Some(1_000)), None, now)
        .unwrap()
        .unwrap();
    store
        .set_task_done(&done.id, true, Timestamp::from_millis(2_000))
        .unwrap();
    assert_eq!(titles(&store), ["soon", "later", "no day", "done already"]);

    // Undone, it is back among the others, in its place.
    store.set_task_done(&done.id, false, now).unwrap();
    assert_eq!(titles(&store), ["done already", "soon", "later", "no day"]);

    // Edited: the title is trimmed, empty notes are none, and an empty
    // title is refused.
    let edited = TaskDraft {
        title: "  soon, really ".into(),
        notes: Some("   ".into()),
        due_at: None,
    };
    assert!(store.edit_task(&soon.id, &edited, now).unwrap());
    let soon = store.task(&soon.id).unwrap().unwrap();
    assert_eq!(
        (soon.title.as_str(), soon.notes, soon.due_at),
        ("soon, really", None, None)
    );
    assert!(!store.edit_task(&soon.id, &draft(" ", None), now).unwrap());
    assert!(store
        .add_task(&draft("", None), None, now)
        .unwrap()
        .is_none());

    store.delete_task(&soon.id).unwrap();
    assert!(store.task(&soon.id).unwrap().is_none());
}

#[test]
fn a_task_from_a_message_names_it_and_leaves_with_its_number() {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    let now = Timestamp::now();
    let mut changes = store.subscribe();
    let task = store
        .add_task(&draft("Contract", None), Some(&source("m1")), now)
        .unwrap()
        .unwrap();
    assert_eq!(task.source, Some(source("m1")));
    assert_eq!(changes.try_next(), Some(StoreChange::Tasks));
    let free = store
        .add_task(&draft("Call the bank", None), None, now)
        .unwrap()
        .unwrap();

    // The chat knows which of its messages a task not done is made from.
    let marked = store.tasked_messages(&account(), &chat_id()).unwrap();
    assert!(marked.contains(&MessageId::new("m1")));
    store.set_task_done(&task.id, true, now).unwrap();
    assert!(store
        .tasked_messages(&account(), &chat_id())
        .unwrap()
        .is_empty());

    // The number goes, and the task made from its message with it; the
    // one written on its own stays.
    store.remove_account(&account()).unwrap();
    assert!(store.task(&task.id).unwrap().is_none());
    assert!(store.task(&free.id).unwrap().is_some());
}
