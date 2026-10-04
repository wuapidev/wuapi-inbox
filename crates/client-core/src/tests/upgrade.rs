//! Databases left by earlier builds: every schema version that reached a
//! disk is opened by this build and comes out at the current one, with
//! what it held.

use super::*;
use client_provider::{Group, GroupParticipant, GroupRole};

/// A plain database file as a build at schema `version` left it, with one
/// message in it.
fn database_at(dir: &std::path::Path, version: u32) -> std::path::PathBuf {
    let path = dir.join(format!("v{version}.db"));
    let mut raw = rusqlite::Connection::open(&path).unwrap();
    crate::migrations::migrate_to(&mut raw, version).unwrap();
    raw.execute(
        "INSERT INTO messages (account_id, chat_id, id, sender, outgoing, ts, body, content, status)
         VALUES ('acc', 'chat', 'old', 'them', 0, 1, 'from before', ?1, 'delivered')",
        [serde_json::to_string(&MessageContent::text("from before")).unwrap()],
    )
    .unwrap();
    // A text that was still waiting to be sent, as that build queued it.
    raw.execute(
        "INSERT INTO outbox (client_id, account_id, chat_id, payload, state, attempts,
                             next_attempt_at, created_at, expires_at)
         VALUES ('waiting', 'acc', 'chat', ?1, 'queued', 0, 0, 0, 9000000000000)",
        [serde_json::to_string(&outgoing("waiting", "not sent yet")).unwrap()],
    )
    .unwrap();
    if version >= 4 {
        // And, from the build that could attach files, one on its way out.
        raw.execute_batch(
            "UPDATE outbox SET upload_round = 2;
             INSERT INTO outbox_files (client_id, bytes, mime, file_name, size)
             VALUES ('waiting', x'010203', 'application/pdf', 'a.pdf', 3);",
        )
        .unwrap();
    }
    let found: u32 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(found, version);
    path
}

/// Everything the migrations after `from` added is there and works, and
/// what the file held is still read.
fn opens_at_the_current_schema(from: u32) {
    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), from);
    let store = Store::open(&path, None).unwrap();
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION, "from v{from}");
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();

    // The outbox: what waited still waits, with the upload columns.
    let waiting = store.outbox_pending().unwrap();
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].message, outgoing("waiting", "not sent yet"));
    assert_eq!(waiting[0].uploaded, None);
    let file = store.outbox_file(&ClientMessageId::new("waiting")).unwrap();
    if from >= 4 {
        assert_eq!(waiting[0].upload_round, 2);
        assert_eq!(*file.unwrap().bytes, vec![1, 2, 3]);
    } else {
        assert_eq!(waiting[0].upload_round, 0);
        assert!(file.is_none());
    }

    // Message extras: the old row reads with nothing riding along, and a
    // new one keeps what does.
    let old = store
        .message(&account(), &MessageId::new("old"))
        .unwrap()
        .unwrap();
    assert_eq!(old.content, MessageContent::text("from before"));
    assert!(old.extras.is_empty());
    let mut starred = text("new", "chat", 2, "kept", Direction::Incoming);
    starred.extras.starred = true;
    store.upsert_message(&starred).unwrap();
    let read = store.message(&account(), &starred.id).unwrap().unwrap();
    assert!(read.extras.starred);

    // Profiles and groups: the tables are there.
    let group = Group {
        id: ChatId::new("group"),
        account_id: account(),
        subject: "Family".into(),
        description: None,
        owner: None,
        created_at: None,
        community: false,
        announce: false,
        locked: false,
        join_approval: None,
        members_can_add: None,
        participants: vec![GroupParticipant {
            contact: ContactId::new("me"),
            name: None,
            role: GroupRole::Admin,
        }],
        subgroups: Vec::new(),
    };
    store.put_group(&group, Timestamp::from_millis(5)).unwrap();
    let stored = store.group(&account(), &group.id).unwrap().unwrap();
    assert_eq!(stored.group.subject, "Family");
    assert!(!store
        .is_blocked(&account(), &ContactId::new("them"))
        .unwrap());

    // Opening again migrates nothing and loses nothing.
    drop(store);
    let store = Store::open(&path, None).unwrap();
    assert_eq!(store.messages(&account(), &chat_id(), 10).unwrap().len(), 2);
}

#[test]
fn a_database_at_schema_v3_is_brought_to_the_current_one() {
    opens_at_the_current_schema(3);
}

/// The schema `main` shipped as its v4 (files on their way out): message
/// extras and the social tables come after it.
#[test]
fn a_database_at_schema_v4_with_files_on_their_way_out_is_brought_to_the_current_one() {
    opens_at_the_current_schema(4);
}

/// The schema the owner's database is at: profiles and groups. What v7
/// adds is filled from what the file already holds.
#[test]
fn a_database_at_schema_v6_is_brought_to_the_current_one() {
    opens_at_the_current_schema(6);
}

#[test]
fn the_names_on_stored_messages_become_the_names_of_their_senders() {
    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), 6);
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        let body = serde_json::to_string(&MessageContent::text("hi")).unwrap();
        // Two messages of one sender under different names (the newer
        // one counts), one of the account's own, one without a name.
        for (id, sender, name, outgoing, ts) in [
            ("a1", "lid:2018", Some("ana"), 0, 10),
            ("a2", "lid:2018", Some("Ana R."), 0, 20),
            ("a3", "lid:2018", Some("  "), 0, 30),
            ("o1", "me", Some("Myself"), 1, 40),
            ("n1", "+5842", None, 0, 50),
        ] {
            raw.execute(
                "INSERT INTO messages
                    (account_id, chat_id, id, sender, sender_name, outgoing, ts, body, content,
                     status)
                 VALUES ('acc', 'g1', ?1, ?2, ?3, ?4, ?5, 'hi', ?6, 'delivered')",
                rusqlite::params![id, sender, name, outgoing, ts, body],
            )
            .unwrap();
        }
    }
    let store = Store::open(&path, None).unwrap();
    seed_account(&store);
    let person = |id: &str| store.person(&account(), None, &ContactId::new(id)).unwrap();
    assert_eq!(person("lid:2018").shown(), Some("Ana R."));
    assert_eq!(person("me").shown(), None, "own messages name nobody");
    assert_eq!(person("+5842").shown(), None);
    // The message that was there before the upgrade is still read.
    assert_eq!(store.messages(&account(), &chat_id(), 10).unwrap().len(), 1);

    // Opening again runs nothing twice.
    drop(store);
    let store = Store::open(&path, None).unwrap();
    assert_eq!(
        store
            .person(&account(), None, &ContactId::new("lid:2018"))
            .unwrap()
            .shown(),
        Some("Ana R.")
    );
}

/// The schema before sends were measured: v8 only adds a table, and what
/// was queued by the older build is sent and measured from then on.
#[test]
fn a_database_at_schema_v7_is_brought_to_the_current_one() {
    opens_at_the_current_schema(7);
}

#[test]
fn a_database_from_before_sends_were_measured_measures_the_next_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), 7);
    // A build at v7 knows nothing of the table: reading its timings
    // without migrating says so instead of failing.
    assert_eq!(Store::read_send_timings(&path, None, 10).unwrap(), None);

    let store = Store::open(&path, None).unwrap();
    assert!(
        store.send_timings(10).unwrap().is_empty(),
        "what was queued before has no row, and needs none"
    );
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    let now = Timestamp::now();
    store
        .enqueue(&outgoing("fresh", "measured"), now, Duration::from_secs(60))
        .unwrap();
    let timings = store.send_timings(10).unwrap();
    assert_eq!(timings.len(), 1);
    assert_eq!(timings[0].client_id, ClientMessageId::new("fresh"));
    assert_eq!(timings[0].queued_at, now);
    assert_eq!(timings[0].accepted_at, None);
    drop(store);
    // The same rows, read beside the application without opening a store.
    let beside = Store::read_send_timings(&path, None, 10).unwrap().unwrap();
    assert_eq!(beside, timings);
}

#[test]
fn the_schema_is_at_v12() {
    assert_eq!(SCHEMA_VERSION, 12);
}

/// v12 added two columns (when a chat was pinned, the message a sticker
/// of the library was seen in) and changed no row. A database a build at
/// v11 left on disk, with pinned chats, a library and stories in it,
/// opens, loses nothing, and takes both from then on.
#[test]
fn a_database_at_schema_v11_is_brought_to_the_current_one() {
    opens_at_the_current_schema(11);

    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), 11);
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        // A build at v11 knows neither column.
        assert!(raw.prepare("SELECT pinned_at FROM chats").is_err());
        assert!(raw
            .prepare("SELECT origin_message FROM library_items")
            .is_err());
        raw.execute_batch(
            "INSERT INTO accounts (id, provider, display_name, connection, position)
             VALUES ('acc', 'mock', 'Work', 'connected', 0);
             INSERT INTO chats (account_id, id, kind, title, pinned, last_message_at)
             VALUES ('acc', 'old-pin', 'direct', 'Pinned before', 1, 50),
                    ('acc', 'plain', 'direct', 'Not pinned', 0, 900);
             INSERT INTO library_items
                (id, kind, mime, size, source, bytes, added_at, favorite, indexed,
                 origin_account, origin_url)
             VALUES ('kept', 'sticker', 'image/webp', 3, 'received', x'010203', 7, 1, 1,
                     'acc', 'https://wa/old');
             INSERT INTO library_remote (account_id, item_id, remote_id)
             VALUES ('acc', 'kept', 'kept');
             INSERT INTO story_mutes (account_id, author) VALUES ('acc', 'them');",
        )
        .unwrap();
    }
    let store = Store::open(&path, None).unwrap();
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 12);
    let account = AccountId::new("acc");

    // What was pinned is pinned, without a time; what the library and the
    // stories held is what they hold.
    let chats = store.chats(&account, None).unwrap();
    let ids: Vec<&str> = chats.iter().map(|chat| chat.id.as_str()).collect();
    assert_eq!(ids, ["old-pin", "plain"]);
    assert!(chats[0].pinned);
    let kept = store.library_item("kept").unwrap().unwrap();
    assert!(kept.favorite && kept.indexed);
    assert_eq!(
        kept.origin,
        Some((account.clone(), "https://wa/old".to_owned()))
    );
    assert_eq!(kept.origin_message, None, "not known for a row from before");
    assert_eq!(
        store.library_remote(&account).unwrap(),
        [("kept".to_owned(), "kept".to_owned())]
    );
    assert!(store
        .story_muted(&account, &ContactId::new("them"))
        .unwrap());

    // From now on: a pin made here is the first of the pinned, and the
    // sticker learns the message it is seen in.
    store
        .apply_chat_change(&account, &ChatId::new("plain"), ChatChange::Pinned(true))
        .unwrap();
    let ids: Vec<String> = store
        .chats(&account, None)
        .unwrap()
        .iter()
        .map(|chat| chat.id.to_string())
        .collect();
    assert_eq!(ids, ["plain", "old-pin"]);
    store
        .library_set_origin(
            "kept",
            &account,
            "https://wa/old",
            Some(&MessageId::new("m_sticker")),
        )
        .unwrap();
    assert_eq!(
        store.library_item("kept").unwrap().unwrap().origin_message,
        Some(MessageId::new("m_sticker"))
    );
}

/// The schema before the stickers of the chats (v9, the library): what
/// the library holds is read as it was (kept by the user, never indexed),
/// and the indexer starts from the messages the file already has.
#[test]
fn a_database_at_schema_v9_with_a_library_is_brought_to_the_current_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("v9.db");
    {
        let mut raw = rusqlite::Connection::open(&path).unwrap();
        crate::migrations::migrate_to(&mut raw, 9).unwrap();
        raw.execute(
            "INSERT INTO library_items (id, kind, mime, size, source, bytes, added_at, favorite)
             VALUES ('kept', 'sticker', 'image/webp', 3, 'imported', x'010203', 7, 1)",
            [],
        )
        .unwrap();
        let mut sticker = Media::new(MediaKind::Sticker);
        sticker.source = Some(client_provider::MediaRef::new("https://wa/old"));
        raw.execute(
            "INSERT INTO messages (account_id, chat_id, id, sender, outgoing, ts, body, content, status)
             VALUES ('acc', 'chat', 'old', 'me', 1, 1, NULL, ?1, 'sent')",
            [serde_json::to_string(&MessageContent::Media(sticker)).unwrap()],
        )
        .unwrap();
        let tables: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'library_index'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0, "v9 has no index of the chats' stickers");
    }
    let store = Store::open(&path, None).unwrap();
    let version: u32 = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);

    // What the user kept is what it was.
    let kept = store.library_item("kept").unwrap().unwrap();
    assert!(kept.favorite && !kept.indexed && !kept.pending());
    assert_eq!(
        (kept.sent_at, kept.heard_at, kept.origin),
        (None, None, None)
    );
    assert_eq!(store.library_stats().unwrap().items, 1);

    // The indexer has not run; its first run sees the message the file
    // had and lists a place for its file.
    assert!(store.library_index_state().unwrap().is_none());
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(store),
        Arc::new(mock),
        SyncConfig::default(),
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .handle()
            .clone(),
    );
    // The first run starts from what is there at that moment: the message
    // is the newest, so it is below the line the first run draws.
    while engine.index_stickers().unwrap() {}
    let seen = engine
        .store()
        .library_recent(LibraryKind::Sticker, 5)
        .unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].indexed && seen[0].pending());
    assert_eq!(
        seen[0].origin,
        Some((AccountId::new("acc"), "https://wa/old".to_owned()))
    );
}

#[test]
fn a_database_at_schema_v8_is_brought_to_the_current_one() {
    opens_at_the_current_schema(8);
}

/// The library (v9) and the stickers of the chats (v10) are what the
/// builds before stories left on disk.
#[test]
fn a_database_at_schema_v9_is_brought_to_the_current_one() {
    opens_at_the_current_schema(9);
}

#[test]
fn a_database_at_schema_v10_is_brought_to_the_current_one() {
    opens_at_the_current_schema(10);
}

/// Stories came with v11, and only added tables: a database from before
/// opens, loses nothing, and holds stories from then on. They came after
/// the library (v9) and the stickers of the chats (v10), so a file left
/// by any of the builds before them is brought up: v8, v9 and v10 (the
/// one a build of main without stories leaves).
#[test]
fn a_database_from_before_stories_holds_them_afterwards() {
    for from in [8, 9, 10] {
        stories_work_after_an_upgrade(from);
    }
}

fn stories_work_after_an_upgrade(from: u32) {
    use client_provider::{Story, StoryBody, StoryStyle};
    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), from);
    // A build at that version knows nothing of the tables.
    {
        let raw = rusqlite::Connection::open(&path).unwrap();
        assert!(raw.prepare("SELECT 1 FROM stories").is_err(), "v{from}");
        if from >= 10 {
            // What v10 added to the library is there and stays.
            raw.execute(
                "INSERT INTO library_items (id, kind, mime, size, source, bytes, added_at, indexed)
                 VALUES ('kept', 'sticker', 'image/webp', 3, 'imported', x'010203', 7, 0)",
                [],
            )
            .unwrap();
        }
    }
    let store = Store::open(&path, None).unwrap();
    seed_account(&store);
    let now = Timestamp::from_millis(2_000_000_000_000);
    let story = Story {
        id: MessageId::new("s1"),
        client_id: None,
        account_id: account(),
        author: ContactId::new("them"),
        author_name: Some("Them".into()),
        mine: false,
        posted_at: now,
        expires_at: None,
        body: StoryBody::Text {
            text: "after the upgrade".into(),
            style: StoryStyle::default(),
        },
        mentions: Vec::new(),
        viewed: false,
        view_count: None,
    };
    assert!(store.upsert_story(&story, now).unwrap());
    assert_eq!(store.story_feed(&account(), now).unwrap().recent.len(), 1);
    // What the file held is still read, and opening again changes nothing.
    assert_eq!(store.messages(&account(), &chat_id(), 10).unwrap().len(), 1);
    drop(store);
    let store = Store::open(&path, None).unwrap();
    assert_eq!(store.story_feed(&account(), now).unwrap().recent.len(), 1);
    if from >= 10 {
        assert!(store.library_item("kept").unwrap().is_some());
    }
}

/// Migrations only go forward. A build that meets a database from a newer
/// build (an update that was undone, a copy of the application that is
/// older than the data) refuses it, says which versions are involved, and
/// leaves every byte of meaning where it was.
#[test]
fn a_database_from_a_newer_build_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = database_at(dir.path(), SCHEMA_VERSION);
    let newer = SCHEMA_VERSION + 1;
    {
        // What a later build would have done to it.
        let raw = rusqlite::Connection::open(&path).unwrap();
        raw.execute_batch(&format!(
            "ALTER TABLE messages ADD COLUMN from_the_future TEXT;
             UPDATE messages SET from_the_future = 'kept';
             PRAGMA user_version = {newer};"
        ))
        .unwrap();
    }
    assert_eq!(Store::schema_of(&path, None).unwrap(), newer);

    let Err(error) = Store::open(&path, None) else {
        panic!("a database from a newer build must not open");
    };
    assert!(
        matches!(error, StoreError::NewerSchema { found, supported }
            if found == newer && supported == SCHEMA_VERSION),
        "{error}"
    );
    let said = error.to_string();
    assert!(
        said.contains(&newer.to_string()) && said.contains(&SCHEMA_VERSION.to_string()),
        "{said}"
    );

    // Nothing was migrated, rewritten or dropped.
    let raw = rusqlite::Connection::open(&path).unwrap();
    let version: u32 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, newer);
    let (body, future): (String, String) = raw
        .query_row(
            "SELECT body, from_the_future FROM messages WHERE id = 'old'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((body.as_str(), future.as_str()), ("from before", "kept"));
    let waiting: u32 = raw
        .query_row("SELECT count(*) FROM outbox", [], |row| row.get(0))
        .unwrap();
    assert_eq!(waiting, 1, "what waited to be sent still waits");

    // The same file opens again once the build knows its schema: asking
    // for its version changed nothing either.
    assert_eq!(Store::schema_of(&path, None).unwrap(), newer);
}
