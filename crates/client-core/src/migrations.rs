//! Schema migrations.
//!
//! The schema version lives in SQLite's `user_version`. Each entry of
//! [`MIGRATIONS`] moves the database one version forward and runs inside a
//! transaction together with the version bump, so a crash mid-migration
//! leaves the previous version intact. Never edit an entry that has shipped:
//! append a new one.

use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[
    // v1: accounts, chats, messages with full-text search, reactions,
    // contacts and the outbox.
    r#"
    CREATE TABLE accounts (
        id            TEXT PRIMARY KEY,
        provider      TEXT NOT NULL,
        display_name  TEXT NOT NULL,
        phone         TEXT,
        self_contact  TEXT,
        connection    TEXT NOT NULL,
        position      INTEGER NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE chats (
        account_id       TEXT NOT NULL,
        id               TEXT NOT NULL,
        kind             TEXT NOT NULL,
        title            TEXT NOT NULL,
        avatar           TEXT,
        unread_count     INTEGER NOT NULL DEFAULT 0,
        pinned           INTEGER NOT NULL DEFAULT 0,
        muted            INTEGER NOT NULL DEFAULT 0,
        archived         INTEGER NOT NULL DEFAULT 0,
        last_message_at  INTEGER,
        -- Where the next page of older history starts, and whether there
        -- is any. `history_loaded` is set once the newest page is in.
        history_cursor   TEXT,
        history_loaded   INTEGER NOT NULL DEFAULT 0,
        history_complete INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;

    CREATE TABLE messages (
        pk            INTEGER PRIMARY KEY,
        account_id    TEXT NOT NULL,
        chat_id       TEXT NOT NULL,
        -- The provider's id, or `local:<client_id>` until it assigns one.
        id            TEXT NOT NULL,
        client_id     TEXT,
        sender        TEXT NOT NULL,
        sender_name   TEXT,
        outgoing      INTEGER NOT NULL,
        ts            INTEGER NOT NULL,
        -- Searchable text: the body, or a media caption.
        body          TEXT,
        -- The MessageContent, as JSON.
        content       TEXT NOT NULL,
        -- The ReplyRef, as JSON.
        reply_to      TEXT,
        status        TEXT NOT NULL,
        status_reason TEXT,
        edited        INTEGER NOT NULL DEFAULT 0,
        deleted       INTEGER NOT NULL DEFAULT 0,
        UNIQUE (account_id, id)
    );
    CREATE UNIQUE INDEX messages_by_client_id
        ON messages (account_id, client_id) WHERE client_id IS NOT NULL;
    CREATE INDEX messages_by_chat ON messages (account_id, chat_id, ts, pk);

    CREATE VIRTUAL TABLE messages_fts USING fts5(
        body,
        content = 'messages',
        content_rowid = 'pk',
        tokenize = 'unicode61 remove_diacritics 2'
    );
    CREATE TRIGGER messages_fts_insert AFTER INSERT ON messages BEGIN
        INSERT INTO messages_fts (rowid, body) VALUES (new.pk, new.body);
    END;
    CREATE TRIGGER messages_fts_delete AFTER DELETE ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body)
        VALUES ('delete', old.pk, old.body);
    END;
    CREATE TRIGGER messages_fts_update AFTER UPDATE OF body ON messages BEGIN
        INSERT INTO messages_fts (messages_fts, rowid, body)
        VALUES ('delete', old.pk, old.body);
        INSERT INTO messages_fts (rowid, body) VALUES (new.pk, new.body);
    END;

    -- One row per (message, person): a new reaction replaces the old one.
    CREATE TABLE reactions (
        account_id TEXT NOT NULL,
        chat_id    TEXT NOT NULL,
        target_id  TEXT NOT NULL,
        sender     TEXT NOT NULL,
        emoji      TEXT NOT NULL,
        from_me    INTEGER NOT NULL,
        ts         INTEGER NOT NULL,
        PRIMARY KEY (account_id, target_id, sender)
    ) WITHOUT ROWID;

    CREATE TABLE contacts (
        account_id TEXT NOT NULL,
        id         TEXT NOT NULL,
        name       TEXT,
        phone      TEXT,
        avatar     TEXT,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;

    -- Messages waiting to be handed to a provider. A row is deleted once
    -- the provider accepts the message.
    CREATE TABLE outbox (
        client_id       TEXT PRIMARY KEY,
        account_id      TEXT NOT NULL,
        chat_id         TEXT NOT NULL,
        -- The OutgoingMessage, as JSON, resubmitted unchanged on retries.
        payload         TEXT NOT NULL,
        -- queued | sending | failed
        state           TEXT NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER NOT NULL,
        created_at      INTEGER NOT NULL,
        expires_at      INTEGER NOT NULL,
        last_error      TEXT
    ) WITHOUT ROWID;
    CREATE INDEX outbox_by_state ON outbox (state, created_at);
    "#,
    // v2: profile pictures and cached media. They live in the database,
    // not in files next to it, so they are encrypted with everything else
    // and go when the database goes.
    r#"
    CREATE TABLE avatars (
        account_id  TEXT NOT NULL,
        subject     TEXT NOT NULL,
        -- The provider's id of the picture; NULL with a NULL image means
        -- "there is no picture", which is remembered too.
        picture_id  TEXT,
        image       BLOB,
        checked_at  INTEGER NOT NULL,
        PRIMARY KEY (account_id, subject)
    );

    CREATE TABLE media_cache (
        key        TEXT PRIMARY KEY,
        bytes      BLOB NOT NULL,
        mime       TEXT,
        width      INTEGER,
        height     INTEGER,
        size       INTEGER NOT NULL,
        last_used  INTEGER NOT NULL
    );
    CREATE INDEX media_cache_by_use ON media_cache (last_used);
    "#,
    // v3: what the provider does on its side for a number (history import
    // at link time, which media it downloads up front), as JSON; and the
    // id of each chat's picture, so that a picture is fetched only when
    // it changed.
    r#"
    ALTER TABLE accounts ADD COLUMN settings TEXT;
    -- The id of the chat's picture, as the provider's chat list gives it.
    ALTER TABLE chats ADD COLUMN picture_id TEXT;
    -- The address book: the names a contact goes by, and when the row was
    -- last seen in the provider's list (NULL: it came from an event), so
    -- that a contact removed from the phone goes from here too.
    ALTER TABLE contacts ADD COLUMN saved_name TEXT;
    ALTER TABLE contacts ADD COLUMN profile_name TEXT;
    ALTER TABLE contacts ADD COLUMN business_name TEXT;
    ALTER TABLE contacts ADD COLUMN username TEXT;
    ALTER TABLE contacts ADD COLUMN about TEXT;
    ALTER TABLE contacts ADD COLUMN picture_id TEXT;
    ALTER TABLE contacts ADD COLUMN listed_at INTEGER;
    -- What the picker sorts and searches by.
    ALTER TABLE contacts ADD COLUMN sort_name TEXT;
    CREATE INDEX contacts_by_name ON contacts (account_id, sort_name);
    "#,
    // v4: files on their way out. The bytes are copied here when a
    // message with a file is queued, so the send survives a restart and
    // does not depend on where the file came from; `uploaded` is the
    // provider's reference once the upload step is done, and
    // `upload_round` counts how often it had to be done again.
    r#"
    ALTER TABLE outbox ADD COLUMN uploaded TEXT;
    ALTER TABLE outbox ADD COLUMN upload_round INTEGER NOT NULL DEFAULT 0;
    CREATE TABLE outbox_files (
        client_id  TEXT PRIMARY KEY,
        bytes      BLOB NOT NULL,
        mime       TEXT NOT NULL,
        file_name  TEXT,
        size       INTEGER NOT NULL
    );
    "#,
    // v5 (message extras): what rides along with a message of any type
    // (forwarded, starred, view once, mentions, a link preview), as JSON;
    // NULL when there is nothing.
    r#"
    ALTER TABLE messages ADD COLUMN extras TEXT;
    "#,
    // v6: profiles and groups. Self-contained: it only creates tables of
    // its own. A group's details and participants as the provider last
    // gave them, the requests to join it, the blocklist, what a business
    // says about itself, and the account's own profile.
    r#"
    CREATE TABLE groups (
        account_id      TEXT NOT NULL,
        id              TEXT NOT NULL,
        subject         TEXT NOT NULL,
        description     TEXT,
        owner           TEXT,
        created_at      INTEGER,
        community       INTEGER NOT NULL DEFAULT 0,
        announce        INTEGER NOT NULL DEFAULT 0,
        locked          INTEGER NOT NULL DEFAULT 0,
        -- NULL: the provider cannot read the setting.
        join_approval   INTEGER,
        members_can_add INTEGER,
        -- The groups a community links, as JSON.
        subgroups       TEXT,
        invite_link     TEXT,
        -- The account left the group, or was removed from it.
        departed        INTEGER NOT NULL DEFAULT 0,
        fetched_at      INTEGER NOT NULL,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;

    CREATE TABLE group_participants (
        account_id TEXT NOT NULL,
        group_id   TEXT NOT NULL,
        contact_id TEXT NOT NULL,
        name       TEXT,
        -- member | admin | owner
        role       TEXT NOT NULL,
        PRIMARY KEY (account_id, group_id, contact_id)
    ) WITHOUT ROWID;
    CREATE INDEX group_participants_by_contact
        ON group_participants (account_id, contact_id);

    CREATE TABLE group_join_requests (
        account_id   TEXT NOT NULL,
        group_id     TEXT NOT NULL,
        contact_id   TEXT NOT NULL,
        requested_at INTEGER,
        PRIMARY KEY (account_id, group_id, contact_id)
    ) WITHOUT ROWID;

    CREATE TABLE blocked_contacts (
        account_id TEXT NOT NULL,
        contact_id TEXT NOT NULL,
        PRIMARY KEY (account_id, contact_id)
    ) WITHOUT ROWID;

    -- What was asked about a contact when its profile was opened.
    CREATE TABLE contact_profiles (
        account_id   TEXT NOT NULL,
        contact_id   TEXT NOT NULL,
        -- The BusinessProfile, as JSON; NULL: not a business.
        business     TEXT,
        looked_up_at INTEGER NOT NULL,
        PRIMARY KEY (account_id, contact_id)
    ) WITHOUT ROWID;

    -- The account's own profile: what the provider could read, or what
    -- was last set from this client. NULL: not known.
    CREATE TABLE own_profiles (
        account_id  TEXT PRIMARY KEY,
        name        TEXT,
        about       TEXT
    ) WITHOUT ROWID;
    "#,
    // v7: one person, several ids. WhatsApp names the same person by
    // their number in one place and by a hidden-number id in another;
    // `identities` keeps the ids of one person together (`person` is the
    // key they share), as the provider's address book says they belong.
    // `seen_names` is the name each sender's own messages came with, the
    // newest one, filled here from the messages already stored. Only new
    // tables: nothing that exists is changed.
    r#"
    CREATE TABLE identities (
        account_id TEXT NOT NULL,
        id         TEXT NOT NULL,
        person     TEXT NOT NULL,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX identities_by_person ON identities (account_id, person);

    CREATE TABLE seen_names (
        account_id   TEXT NOT NULL,
        id           TEXT NOT NULL,
        profile_name TEXT,
        username     TEXT,
        seen_at      INTEGER NOT NULL,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;

    -- With MAX(ts), SQLite takes the other columns from the newest row.
    INSERT INTO seen_names (account_id, id, profile_name, seen_at)
    SELECT account_id, sender, sender_name, MAX(ts) FROM messages
    WHERE outgoing = 0 AND sender_name IS NOT NULL AND TRIM(sender_name) <> ''
    GROUP BY account_id, sender;
    "#,
    // v8: how long each message sent from here took, for measuring the
    // path from Enter to the ticks (`--diagnose sends`). Times are
    // milliseconds since the epoch; nothing of the message itself is
    // kept. Only a new table: nothing that exists is changed.
    r#"
    CREATE TABLE send_timings (
        client_id       TEXT PRIMARY KEY,
        account_id      TEXT NOT NULL,
        chat_id         TEXT NOT NULL,
        -- text | media | poll | reaction
        kind            TEXT NOT NULL,
        -- When the user sent it.
        queued_at       INTEGER NOT NULL,
        -- When the first request for it left, and how many were made.
        post_started_at INTEGER,
        attempts        INTEGER NOT NULL DEFAULT 0,
        -- When the provider said it has the message, and how long the
        -- request that was answered took.
        accepted_at     INTEGER,
        post_ms         INTEGER,
        -- When the bubble first left the clock, and each tick after.
        first_change_at INTEGER,
        sent_at         INTEGER,
        delivered_at    INTEGER,
        read_at         INTEGER,
        failed_at       INTEGER
    ) WITHOUT ROWID;
    CREATE INDEX send_timings_by_time ON send_timings (queued_at);
    "#,
    // v9: the sticker and GIF library: files the user keeps, with what is
    // needed to show them (a small still), find them (name, pack, last
    // use) and send them again. The id is the hash of the bytes, so the
    // same file is one row however it came. `favorite` and `position` are
    // the user's starred ones in their order; `library_remote` says which
    // of them the provider knew as starred on an account when last
    // synced. Only new tables: nothing that exists is changed.
    r#"
    CREATE TABLE library_items (
        id         TEXT PRIMARY KEY,
        -- sticker | gif
        kind       TEXT NOT NULL,
        mime       TEXT NOT NULL,
        animated   INTEGER NOT NULL DEFAULT 0,
        width      INTEGER,
        height     INTEGER,
        size       INTEGER NOT NULL,
        -- received | sent | imported | phone | online
        source     TEXT NOT NULL,
        name       TEXT,
        pack       TEXT,
        bytes      BLOB NOT NULL,
        -- A still of the first frame, PNG or JPEG; none for a video.
        thumb      BLOB,
        thumb_mime TEXT,
        added_at   INTEGER NOT NULL,
        last_used  INTEGER,
        favorite   INTEGER NOT NULL DEFAULT 0,
        position   INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX library_by_kind ON library_items (kind, favorite, position);
    CREATE INDEX library_by_use ON library_items (kind, last_used);
    CREATE TABLE library_packs (
        id         TEXT PRIMARY KEY,
        name       TEXT NOT NULL,
        created_at INTEGER NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE library_remote (
        account_id TEXT NOT NULL,
        item_id    TEXT NOT NULL,
        remote_id  TEXT NOT NULL,
        PRIMARY KEY (account_id, item_id)
    ) WITHOUT ROWID;
    "#,
    // v10: stickers the account already has in its chats. The library
    // lists them without the user saving anything: a row made by the
    // indexer (`indexed`) is a sticker seen in a message, with when the
    // account last sent it (`sent_at`, from this computer or its phone)
    // and when it last received it (`heard_at`), and the media reference
    // it came from (`origin_*`). A sticker whose file is not cached yet
    // is a row with no bytes (`size` 0) that the engine completes when
    // the file arrives. `library_index` is where the indexer has got to
    // in the messages: everything above `high` is new, everything below
    // `low` is not looked at yet. Only added columns and a table: no row
    // that exists changes.
    r#"
    ALTER TABLE library_items ADD COLUMN indexed INTEGER NOT NULL DEFAULT 0;
    ALTER TABLE library_items ADD COLUMN sent_at INTEGER;
    ALTER TABLE library_items ADD COLUMN heard_at INTEGER;
    ALTER TABLE library_items ADD COLUMN origin_account TEXT;
    ALTER TABLE library_items ADD COLUMN origin_url TEXT;
    CREATE INDEX library_by_origin ON library_items (origin_url)
        WHERE origin_url IS NOT NULL;
    CREATE TABLE library_index (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        high INTEGER NOT NULL,
        low  INTEGER NOT NULL,
        done INTEGER NOT NULL DEFAULT 0
    );
    "#,
    // v11: stories (WhatsApp's status). Only new tables: nothing that
    // exists is changed. A story lives for a day (`expires_at`); the
    // rows and the media cached for them go when it does.
    //
    // `stories` holds contacts' stories and the account's own. `body` is
    // the story's `StoryBody` as JSON; `viewed_at` is when it was shown
    // here (NULL: not yet), `provider_viewed` that the provider says the
    // account saw it elsewhere. `story_viewers` are the people who saw
    // one of the account's own. `story_mutes` are the authors whose
    // stories are hidden from the Recent list. `story_privacy` is the
    // last audience the provider answered, as JSON. `story_posts` is the
    // outbox of stories (with `story_post_files` for their bytes), and
    // `story_receipts` the view receipts that are owed to authors.
    r#"
    CREATE TABLE stories (
        account_id      TEXT NOT NULL,
        id              TEXT NOT NULL,
        client_id       TEXT,
        author          TEXT NOT NULL,
        author_name     TEXT,
        mine            INTEGER NOT NULL,
        posted_at       INTEGER NOT NULL,
        expires_at      INTEGER NOT NULL,
        -- text | image | video | voice
        kind            TEXT NOT NULL,
        body            TEXT NOT NULL,
        mentions        TEXT,
        viewed_at       INTEGER,
        provider_viewed INTEGER NOT NULL DEFAULT 0,
        view_count      INTEGER,
        PRIMARY KEY (account_id, id)
    ) WITHOUT ROWID;
    CREATE INDEX stories_by_author ON stories (account_id, author, posted_at);
    CREATE INDEX stories_by_expiry ON stories (expires_at);
    CREATE INDEX stories_by_client ON stories (account_id, client_id);

    CREATE TABLE story_viewers (
        account_id TEXT NOT NULL,
        story_id   TEXT NOT NULL,
        contact    TEXT NOT NULL,
        name       TEXT,
        viewed_at  INTEGER NOT NULL,
        reaction   TEXT,
        PRIMARY KEY (account_id, story_id, contact)
    ) WITHOUT ROWID;

    CREATE TABLE story_mutes (
        account_id TEXT NOT NULL,
        author     TEXT NOT NULL,
        PRIMARY KEY (account_id, author)
    ) WITHOUT ROWID;

    CREATE TABLE story_privacy (
        account_id TEXT PRIMARY KEY,
        privacy    TEXT NOT NULL,
        fetched_at INTEGER NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE story_posts (
        client_id       TEXT PRIMARY KEY,
        account_id      TEXT NOT NULL,
        -- The NewStory as JSON; a picture or video carries the reference
        -- of its upload once there is one.
        payload         TEXT NOT NULL,
        -- queued | uploading | posting | failed
        state           TEXT NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER NOT NULL,
        created_at      INTEGER NOT NULL,
        expires_at      INTEGER NOT NULL,
        last_error      TEXT,
        uploaded        TEXT,
        upload_round    INTEGER NOT NULL DEFAULT 0,
        local_kind      TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE INDEX story_posts_by_time ON story_posts (next_attempt_at);

    CREATE TABLE story_post_files (
        client_id TEXT PRIMARY KEY,
        bytes     BLOB NOT NULL,
        mime      TEXT NOT NULL,
        file_name TEXT,
        size      INTEGER NOT NULL
    ) WITHOUT ROWID;

    CREATE TABLE story_receipts (
        account_id      TEXT NOT NULL,
        story_id        TEXT NOT NULL,
        author          TEXT NOT NULL,
        attempts        INTEGER NOT NULL DEFAULT 0,
        next_attempt_at INTEGER NOT NULL,
        created_at      INTEGER NOT NULL,
        PRIMARY KEY (account_id, story_id)
    ) WITHOUT ROWID;
    "#,
    // v12: when a chat was pinned (`pinned_at`, NULL when it is not or
    // the provider does not say), which orders the pinned chats, and the
    // message a sticker of the library was seen in (`origin_message`,
    // next to `origin_account`), so that it can be starred on the phone
    // by naming that message. Only added columns: no row that exists
    // changes.
    r#"
    ALTER TABLE chats ADD COLUMN pinned_at INTEGER;
    ALTER TABLE library_items ADD COLUMN origin_message TEXT;
    "#,
];

/// The schema version this build expects.
pub const SCHEMA_VERSION: u32 = MIGRATIONS.len() as u32;

/// Brings the database up to [`SCHEMA_VERSION`]. Returns the version it
/// found.
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32, crate::StoreError> {
    let found: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(crate::StoreError::NewerSchema {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    apply(conn, found, SCHEMA_VERSION)?;
    Ok(found)
}

/// Runs the migrations that take the database from version `from` to
/// version `to`, each in its own transaction.
fn apply(conn: &mut Connection, from: u32, to: u32) -> Result<(), crate::StoreError> {
    let steps = MIGRATIONS
        .iter()
        .enumerate()
        .take(to as usize)
        .skip(from as usize);
    for (index, sql) in steps {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index as u32 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

/// Builds the schema as it was at `version` in an empty database: what a
/// build that knew no later migration left on disk. For the tests of the
/// upgrade paths.
#[cfg(test)]
pub(crate) fn migrate_to(conn: &mut Connection, version: u32) -> Result<(), crate::StoreError> {
    apply(conn, 0, version)
}
