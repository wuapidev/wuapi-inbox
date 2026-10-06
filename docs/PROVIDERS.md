# Writing a provider

A provider connects the client to a WhatsApp backend: a hosted REST API, a self-hosted gateway, a library that speaks the protocol. You implement one trait, `Provider`, from the [`client-provider`](../crates/client-provider) crate. The client does the rest: storage, search, retries, the outbox, the UI.

Three providers are in the repository. Read them alongside this page.

- [`provider-example`](../crates/provider-example): one file, about 320 lines, over an in-memory backend that says back what it is sent. The worked example of this page: copy it to start your own.
- [`provider-mock`](../crates/provider-mock): the demo data, in memory, built on `Capabilities::all()`. The place to see an optional method honoured.
- [`provider-wuapi`](../crates/provider-wuapi): a real HTTP API, reached through its generated SDK. The place to see timeouts, error mapping, paging and live updates over a network.

On this page: [a walkthrough](#write-a-provider-in-15-minutes), [every method](#every-method), [the rules](#the-trait), [the model](#the-model), [testing](#testing-a-provider), [wiring it in](#wiring-it-in), [a checklist for a pull request](#checklist-for-a-pull-request), and how the wuapi adapter is built.

## What a provider is, and is not

A provider is a translator. It turns its backend's accounts, chats and messages into the neutral model and passes sends through. It does not cache, store, retry, order or deduplicate; the client's sync engine does all of that, and it is the only caller.

```text
your backend  ◄──►  your Provider  ◄──►  sync engine  ──►  local store  ──►  UI
```

The UI never calls a provider. It reads a local database that the engine fills from your answers. So a provider can be slow, and it can fail, as long as it says truthfully how it failed.

## Write a provider in 15 minutes

The steps below are the ones `provider-example` took. Follow them with your own name where it says `example`.

**1. A crate.** Copy `crates/provider-example` to `crates/provider-yours`:

```text
crates/provider-yours/
  Cargo.toml          client-provider, async-trait, futures (and tokio, reqwest: what your backend needs)
  src/lib.rs          the provider: `impl Provider for YourProvider`
  tests/contract.rs   what the client relies on, as tests
```

Add it to the workspace in the root `Cargo.toml`: a line in `members` and a line in `[workspace.dependencies]` (`provider-yours = { path = "crates/provider-yours" }`).

**2. The nine required methods.** `id`, `capabilities`, `list_accounts`, `list_chats`, `fetch_messages`, `send`, `mark_read`, `download_media` and `subscribe`. The crate documentation of `provider-example` has the smallest implementation that compiles (it is a doc-test), and `EchoProvider` below it is one that does something. Map your backend's objects to the neutral model ([the model](#the-model)), and its failures to `ProviderError` ([rule 2](#2-errors-say-whether-to-retry)).

**3. Capabilities.** Start from `Capabilities::none()` and turn on only what is true. `provider-example` turns on two: `chat_list` (its listing is the real one) and `realtime_push` (its events are pushed). Everything else stays off, so the window offers no reactions, no attachments, no groups. Add a flag when you implement what it gates ([every method](#every-method)).

**4. Register it in the application.** Three places in `crates/app`, each marked `provider-example` for the example ([wiring it in](#wiring-it-in) has them line by line): the dependency in `Cargo.toml`, the `--provider` value in `src/cli.rs`, and the construction in `src/providers.rs`.

**5. Run it.**

```sh
cargo run -p wuapi-inbox --features provider-example -- --provider example
```

One number, one chat; what you write comes back.

**6. Test it.** `cargo test -p provider-yours`. Start from `crates/provider-example/tests/contract.rs` ([testing](#testing-a-provider)).

## Every method

`Provider` has 69 methods: 9 are required and 60 have defaults. The engine calls an optional method only when the flag beside it is on, so an adapter implements what its backend has and leaves the rest alone. The doc comment of each method in [`provider.rs`](../crates/client-provider/src/provider.rs) is its reference.

| Method | Required | Called when |
|---|---|---|
| `id` | yes | always: a short, stable, lowercase name, stored next to each account |
| `capabilities` | yes | always: cheap, no network |
| `list_accounts` | yes | at startup and on every refresh |
| `list_chats` | yes | always; `chat_list` says the listing is the real one and not a reconstruction |
| `fetch_messages` | yes | always: newest first, a page at a time |
| `send` | yes | always: idempotent on `client_id` |
| `mark_read` | yes | always; `read_receipts` says the other side is told |
| `download_media` | yes | `media_download`; answer `Unsupported` without it |
| `subscribe` | yes | always; `realtime_push` says events are pushed and not polled for |
| `unavailable`, `recheck` | no | always: what the capabilities promise and the backend does not have right now |
| `live_updates` | no | always: for the diagnostics, when there is more than one transport |
| `mention_handle` | no | `mentions` |
| `mark_read_quietly` | no | `quiet_read` |
| `update_chat` | no | `chat_state` |
| `start_chat` | no | `start_chat` |
| `fetch_media`, `fetch_media_reporting` | no | `media_download`: the defaults wrap `download_media` |
| `fetch_avatar` | no | `avatars` |
| `upload_media`, `media_upload_limit`, `media_upload_ready` | no | `media_upload` |
| `vote_poll` | no | `poll_votes` |
| `edit_message` | no | `edits` |
| `delete_message` | no | `deletes`, `delete_for_me`, `delete_received` |
| `star_message` | no | `stars` |
| `forward_messages` | no | `forward_any` (with `forward_polls`, `forward_events`) |
| `list_favorite_stickers`, `add_favorite_sticker`, `remove_favorite_sticker` | no | `sticker_favorites` |
| `list_contacts` | no | `contacts` |
| `check_numbers` | no | `number_check` |
| `lookup_contact` | no | `contact_lookup` |
| `business_profile` | no | `business_profiles` |
| `list_blocked`, `set_blocked` | no | `blocking` |
| `own_profile`, `update_profile`, `set_profile_picture` | no | `profile_edit` |
| `list_groups`, `fetch_group` | no | `group_info` |
| `create_group` | no | `group_create` |
| `update_group`, `change_participants`, `set_group_picture` | no | `group_manage` |
| `group_invite_link` | no | `group_invites` |
| `join_requests`, `answer_join_requests` | no | `group_join_requests` |
| `leave_group` | no | `group_leave` |
| `link_subgroup`, `unlink_subgroup` | no | `community_manage` |
| `community_participants` | no | `community_members` |
| `list_stories` | no | `story_list` |
| `post_story` | no | `story_post` |
| `delete_story` | no | `story_delete` |
| `view_story` | no | `story_view` |
| `story_viewers` | no | `story_viewers` |
| `reply_to_story` | no | `story_reply` |
| `react_to_story` | no | `story_react` |
| `muted_story_authors`, `set_story_muted` | no | `story_mute` |
| `story_privacy` | no | `story_privacy` |
| `set_story_privacy` | no | `story_privacy_edit` |
| `link_places`, `create_account`, `link_status` | no | `link_accounts` |
| `pairing_code` | no | `link_by_code` |
| `scan_instead` | no | `link_back_to_scan` |
| `update_account`, `reconnect_account`, `unlink_account`, `delete_account` | no | `manage_accounts` (`history_import` for that setting) |

The flags that gate no method describe what the required ones carry: `groups` (group chats are listed and can be sent to), `contact_names` (names are the address book's), `replies`, `reactions`, `forwards` and `polls` (what `send` takes), `presence` (typing is pushed), `incremental_sync`, `story_contacts`.

## The trait

```rust
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;
    fn unavailable(&self, account: &AccountId, feature: Feature) -> bool;  // optional

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>>;
    async fn list_chats(&self, account: &AccountId, cursor: Option<Cursor>)
        -> ProviderResult<Page<Chat>>;
    async fn fetch_messages(&self, account: &AccountId, chat: &ChatId,
        cursor: Option<Cursor>, limit: u32) -> ProviderResult<Page<Message>>;
    async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt>;
    async fn mark_read(&self, account: &AccountId, chat: &ChatId,
        up_to: Option<&MessageId>) -> ProviderResult<()>;
    async fn update_chat(&self, account: &AccountId, chat: &ChatId,
        change: ChatChange) -> ProviderResult<()>;          // optional
    async fn start_chat(&self, account: &AccountId, phone: &str)
        -> ProviderResult<Chat>;                            // optional
    async fn download_media(&self, account: &AccountId, media: &MediaRef)
        -> ProviderResult<MediaData>;
    async fn subscribe(&self) -> ProviderResult<EventStream>;
}
```

`fetch_avatar` (a chat's picture, with `AvatarAnswer::Unchanged` when the id the client holds is still current) and `fetch_media` (a download within a size and type limit; the default wraps `download_media`, an HTTP provider should override it to refuse from the headers) are optional too; `Capabilities::avatars` says whether pictures exist.

`unavailable` says that a part the capabilities promise is missing for now, for one account: the backend does not have its routes yet, or has not turned it on for that number (`Feature::ForwardAny`, `StickerFavorites`, `Stories`). Capabilities are what the provider is built to do and are read once; this is what, of that, does not work right now. A provider learns it from its backend's answers, answers `Unsupported` for that part meanwhile (without a request, so a missing part costs nothing), and forgets it after a while so that it is tried again. The client then says "not available yet" in that place and keeps everything else working (see "What a backend does not have yet" in ARCHITECTURE.md). It is cheap and makes no request: the client asks whenever it draws. The default is `false`. `recheck(account, feature)` is the other half: the user went to the place that offers the part, or pressed "Check again", so whatever you remembered about it is forgotten and your next call asks the backend. The default does nothing, which is right for a provider that remembers nothing.

`update_chat` (pin, mute, archive, mark as unread) and `start_chat` (the chat with a phone number) have default implementations that answer `Unsupported`; implement them and set `Capabilities::chat_state` / `start_chat` to light up the menu entries that use them. Every `ChatChange` sets a value rather than toggling one, so the engine can repeat a change whose answer was lost.

`Capabilities::mentions` says that a text can mention people: `OutgoingMessage::mentions` then lists them, and the text names each as `@` followed by what `mention_handle` answers for their id (by default the digits of the id, which is WhatsApp's way; override it when your ids are not numbers). Send the ids along with the text.

`vote_poll` (the account's vote in a poll: the names of every option it now stands for, none to take the vote back) is optional in the same way, behind `Capabilities::poll_votes`. A vote sets a value too: the client shows it at once, repeats the call while the failure is transient, and takes it back off the screen only when you refuse it. Answer the poll with its new tally when your backend returns it. `Capabilities::polls` says that `send` takes `OutgoingContent::Poll`.

`edit_message`, `delete_message` and `star_message` change a message that was sent, behind `Capabilities::edits`, `deletes` (for everyone), `delete_for_me` (the account's own messages, on its devices only), `delete_received` (other people's, for the account alone) and `stars`. Each sets a value, so each must be safe to repeat: the client shows the change at once, repeats the call while the failure is transient, and puts the message back as it was only when you refuse, with your words (WhatsApp lets a message be edited for about a quarter of an hour: answer its refusal as `Rejected`). A message still in the client's outbox never reaches these calls: it is edited or taken back there. A reaction is a message (`OutgoingContent::Reaction`, empty to take it back) and goes through `send`; so does the forward of a text the provider does not hold, which is the content sent again with `OutgoingMessage::forwarded` set (`Capabilities::forwards`). Anything the provider holds is forwarded by naming the original, a text included: `forward_messages` takes `ForwardItem`s (a client id that is the idempotency key, the message, the target chat) behind `Capabilities::forward_any`, passes the original on the way WhatsApp does (reusing the media reference, so no file passes through the client, and marking the copy as forwarded), and answers one result per item, a refusal (`Rejected`: a file WhatsApp no longer has, view-once, a kind that cannot be forwarded) failing that item alone. Say a refusal in the neutral codes of `client_provider::refusal` (`forward_deleted`, `forward_view_once`, `forward_kind`, `forward_not_sent`, `forward_no_file`, `forward_file_gone`, `forward_too_large`) and the client words it the way it words its own; any other code is shown with your message. `Capabilities::forward_polls` and `forward_events` say whether a poll and a calendar event are passed on: where they are not, the client refuses them before anything is queued. The client queues each copy in its outbox as `OutgoingContent::Forward`, with a pending bubble of what it passes on, and never gives that content to `send`. The message named may be a story that is up (a story is a message, and `Story::id` is its id): a picture, a video or a voice note of Status is passed on to a chat this way, and a text story as a text.

`Capabilities::sticker_favorites` says that the stickers starred on the account (WhatsApp's "favorite stickers", a synced list of the account) can be read and changed: `list_favorite_stickers` answers each favorite with an id and a `MediaRef` (the file is fetched with `fetch_media`, as any other, and only for a favorite the client does not hold yet), `add_favorite_sticker` takes a `StickerFile` (the client's id is the hash of its bytes; `message` names a message of the account the sticker was seen in, when there is one, so that a backend that can star by naming a message moves no file) and answers the id the favorite is listed under from then on (your backend's own, or the client's when it has none), and `remove_favorite_sticker` takes that id. All three set values, so each is safe to repeat. The client keeps its own library of stickers and GIFs (see "Stickers and GIFs" in ARCHITECTURE.md) and reconciles it with this list in both directions; without the capability favorites live on the computer alone, and the picker says so. A GIF is sent as `OutgoingContent::Media { kind: Video, gif: true, .. }`: a provider that can flag a video to play as a GIF does, and one that cannot sends the video as it is.

Profiles and groups are optional as well, each behind its own capability flag and answering `Unsupported` by default (the model is in [`social.rs`](../crates/client-provider/src/social.rs)):

| Capability | Methods |
|---|---|
| `contact_lookup` | `lookup_contact`: the About text, username and picture id of one contact, asked when their profile is opened |
| `business_profiles` | `business_profile` |
| `blocking` | `list_blocked`, `set_blocked` |
| `profile_edit` | `own_profile` (as far as it can be read), `update_profile`, `set_profile_picture` |
| `group_info` | `fetch_group`, and `list_groups` when every group can be listed with its participants (that is where "groups in common" comes from, and how the chat list knows which community a group is in: the client asks once per connected number at every refresh) |
| `group_create` | `create_group` |
| `group_manage` | `update_group`, `change_participants`, `set_group_picture` |
| `group_invites` | `group_invite_link` |
| `group_join_requests` | `join_requests`, `answer_join_requests` |
| `group_leave` | `leave_group` |
| `community_manage` | `link_subgroup`, `unlink_subgroup`, and `NewGroup::community` / `NewGroup::in_community` on `create_group` (a community, and a group made inside one) |
| `community_members` | `community_participants` |

Three rules for them. Every change sets a value or carries a `request_id` that is the same on every retry of one user action: forward it as the idempotency key, because the engine repeats a request whose answer was lost. Refusals use the neutral codes of `client_provider::refusal` (`not_admin`, `privacy_restricted`, `already_member`, `not_member`, `not_on_whatsapp`, `not_found`) where the backend's mean the same, so the client can word them. And a change to participants is answered per contact (`ParticipantOutcome`): some may go through while others are refused. When a group changes on the backend's side, emit `ProviderEvent::GroupChanged`: a hint, on which the client reads the group again (at once when somebody looked at it, else when it is looked at).

A community is a group with `Group::community` set, and the groups it links say so themselves: `Group::community_id` is the community's own group id (`None` for a group that is in none, and for a community), and `Group::announcements` marks the community's announcement group. Fill both in `list_groups` as well as in `fetch_group`: the listing is what the client groups its chat list by. `Group::subgroups` (the groups a community links, with their names) may be left empty where it costs another request, as in a listing: the client keeps the ones it holds and reads the community on its own when it is opened. `link_subgroup` and `unlink_subgroup` are admin-only (answer `not_admin` otherwise) and repeat safely: linking a group that is already in that community, or unlinking one that is not, succeeds. `community_participants` lists the contacts of all the community's groups; the client asks when the list is shown and keeps nothing. `create_group` with `NewGroup::community` makes a community (participants may be empty) and with `NewGroup::in_community` a group that is one of that community's; an engine that cannot do the second answers a `Rejected` whose message the user reads as it is, which the client does not retry. When a group is linked to a community or unlinked from it, or the account joins a group that is in one, emit `ProviderEvent::CommunityChanged` with the community and the groups: the client reads all of them again at once, whether or not it held them.

Status (stories) is optional too, one flag for each part, answering `Unsupported` by default (the model is in [`stories.rs`](../crates/client-provider/src/stories.rs)). A story is a message to the status list: its id is what a reply or a reaction quotes, and it lasts `STORY_LIFETIME` (24 hours) from `Story::posted_at` unless `expires_at` says otherwise.

| Capability | Methods and events |
|---|---|
| `story_list` | `list_stories`: the account's own stories and its contacts', the ones not expired. Called when Status is looked at and after a reconnect |
| `story_contacts` | Contacts' stories are delivered (listed, and pushed as `ProviderEvent::StoryUpserted` and `StoryRemoved`). Without it the list says so |
| `story_post` | `post_story`, idempotent on `NewStory::client_id` like `send`; a picture or video is uploaded first with `upload_media` |
| `story_delete` | `delete_story`, for everyone, safe to repeat |
| `story_view` | `view_story`: the view receipt. The client calls it once, for a story that was shown, and not at all with receipts off |
| `story_viewers` | `story_viewers`, and `ProviderEvent::StoryViewed` when somebody sees one of the account's |
| `story_reply` | `reply_to_story`: a text for the author's chat whose `reply_to` is the story |
| `story_react` | `react_to_story`: a reaction whose target is the story (an empty emoji takes it back) |
| `story_mute` | `muted_story_authors`, `set_story_muted`, `ProviderEvent::StoryMuteChanged`. Without it mutes stay on this computer |
| `story_privacy` | `story_privacy`: who sees the account's stories (`StoryAudience` and both lists) |
| `story_privacy_edit` | `set_story_privacy`. Without it the audience is shown read-only |

A reply or a reaction to a story reaches the provider through the client's message outbox, so it is retried with the same `client_id`; the provider's own copy of it may come back without the mark that says it answers a story (`MessageExtras::story_reply`), and the client keeps the mark it wrote. A provider that can read a received message's story (the id of what it quotes) sets `story_reply` in `extras` and the conversation shows it the same way.

The doc comments in [`provider.rs`](../crates/client-provider/src/provider.rs) are the reference for each method. The rules that matter most follow.

### 1. Sends are idempotent

This is the one rule you cannot bend.

Every outgoing message carries a `client_id` that the client generated and saved before the first attempt. When an attempt times out or the connection drops, the client cannot know whether the message went out, so it calls `send` again with the identical `OutgoingMessage`. It does the same after a restart.

**Any number of `send` calls with the same `client_id` must put at most one message on WhatsApp, and every successful call must return the same `message_id`.** The guarantee has to hold for at least `IDEMPOTENCY_WINDOW` (24 hours); the client gives up on a message well before that.

Ways to get there:

- The backend has idempotency keys: forward `client_id` as the key. wuapi does this with the `Idempotency-Key` header (`.idempotency_key(client_id)` on the SDK's request).
- You control the WhatsApp message id: derive it from `client_id` deterministically, so a repeat is the same message.
- Neither: keep your own table of `client_id` to receipt and consult it first. The mock does this.

Also echo the id back. When the message later shows up in history or in an event, `Message::client_id` must carry it, so the client matches the server's copy with the bubble it is already showing instead of adding a second one. wuapi does this by sending the id as message metadata, which the API returns.

### 2. Errors say whether to retry

`ProviderError` has two families, and the client treats them very differently.

| Variant | Meaning | What the client does |
|---|---|---|
| `Transient` | Timeout, dropped connection, 5xx, account reconnecting | Retries quietly with backoff. The user sees nothing. |
| `RateLimited` | The backend asked to slow down | Retries after `retry_after`. |
| `Unauthorized` | Credentials missing, expired or revoked | Stops for good, keeps what is queued, and takes the user to the sign-in. Return it only for a real 401: the client does not retry it. |
| `Rejected` | The request is wrong and will stay wrong | Shows `message` to the user. A send becomes a failed message. |
| `Unsupported` | The provider does not implement this | Should not happen for a capability reported as `true`. |
| `Protocol` | The backend answered something unreadable | Treated as terminal. |

Use `Transient` only when repeating the same request later can work. A recipient without WhatsApp is `Rejected`, not `Transient`: retrying it for hours helps nobody. A request that timed out is `Transient`, not `Rejected`: the user should not be told a message failed because a socket went quiet.

### 3. Every call is bounded

Put a timeout on every network request and return `Transient` when it fires. The engine also wraps each call in its own deadline, but a provider that hangs wastes a worker until then.

If your backend's SDK retries on its own, turn that off. The engine retries with its own backoff and keeps the order of a chat's messages while it does; a provider that sits on a failure for three attempts hides it from the one place that can act on it. The wuapi adapter builds its SDK client with `RetryPolicy::none()` for this reason.

### 4. Capabilities are honest

`capabilities()` tells the UI what to show. No reaction picker without `reactions`, no attach button without `media_upload`. A `true` that later answers `Unsupported` gives the user a button that does nothing, so when in doubt, say `false`.

Build the value with struct update syntax, because fields will be added:

```rust
fn capabilities(&self) -> Capabilities {
    Capabilities {
        groups: true,
        replies: true,
        read_receipts: true,
        ..Capabilities::none()
    }
}
```

Some capabilities describe quality rather than presence, and the engine adapts to them:

- `chat_list: false` means `list_chats` is a reconstruction (derived from recent messages, say). The engine then keeps unread counts itself instead of trusting yours. With `true`, your counts, pins, mutes and archive flags are what the client shows, and a `ChatUpdated` event is how they change.
- `realtime_push: false` means your event stream polls behind the scenes. Events arrive late; that is fine. The wuapi adapter says `true` unless it was told to poll (`--live polling`), also while an `Auto` run is polling because the stream is not there: the flag says what the adapter can do, and `Provider::live_updates` says what it is doing now.

### 5. Events are hints

`subscribe` returns a stream of `ProviderEvent`: a message was upserted, a status moved, a chat changed, somebody is typing, an account connected or dropped.

The client applies events idempotently and re-reads history after a gap, so:

- delivering an event twice is fine;
- repeating current state after a reconnect is fine;
- letting the stream end when your transport gives up is fine: the engine calls `subscribe` again with backoff.

Statuses only move forward on the client (`Pending`, `Accepted`, `Sent`, `Delivered`, `Read`). If you send `Delivered` after `Read`, it is ignored.

`send` answers with a `SendReceipt` whose `status` says how far the message is: `Accepted` when your backend only queued it (it is safe with you, and not on WhatsApp yet), `Sent` when WhatsApp already has it. Never `Pending`: that is the client's own word for "no answer yet". What happens next reaches the client through events, so a backend that queues must report the later statuses, by push or by asking (below).

Reactions are messages. Emit a `Message` whose content is `MessageContent::Reaction { target, emoji }` (empty emoji removes it); the client folds it into the target's reaction chips.

No push channel? Poll inside the provider and emit what you find. Keep the transport behind your own small trait so a push transport can replace it later; `provider-wuapi/src/events.rs` is built that way, and its stream (`stream.rs`, `live.rs`) took the place of polling without the engine noticing. A general poll is not enough for the messages the user just sent: ask about those sooner and by id, as the wuapi adapter does ("Following a sent message").

### 6. Connection states

Report `Reconnecting` for a drop you expect to recover from, and keep `Disconnected` and `LoggedOut` for states that need the user. The client holds queued messages through the former and tells the user about the latter. When an account comes back, emit `ConnectionChanged { state: Connected }`: the outbox sends what was waiting right away.

## The model

All types are in [`model.rs`](../crates/client-provider/src/model.rs) and documented there. A few notes:

- **Ids are opaque strings** you choose (`AccountId`, `ChatId`, `MessageId`, `ContactId`). They must be stable across restarts. The client never parses them.
- **`Cursor` and `MediaRef` are opaque too.** Put in them whatever you need to continue a listing or fetch a payload later: an offset, a token, a URL, a serialised descriptor.
- **`fetch_messages` is newest first.** `None` is the newest page; each page's `next_cursor` walks further back.
- **`Chat::pinned_at`** is when a chat was pinned, when your backend says: pinned chats are listed the last one pinned first. Leave it `None` otherwise; a pin made in the client keeps the moment it was made.
- **`Chat::last_message`**, when your chat listing includes it, lets the client show previews before fetching any history: the chat list is then complete without one history request, which by default waits until a chat is opened. It is also how the engine notices that a chat has news.
- **Timestamps** are milliseconds since the Unix epoch, UTC.
- **`MessageContent`** has a variant for everything a chat shows: `Text`, `Media`, `Location` (with `live`), `Contacts` (one card or several), `Poll`, `Event` (a calendar event), `System` (a notice from the chat itself: somebody joined or left, was added, removed, promoted or demoted, the name or picture changed, a call), and `Reaction`. The types are in [`content.rs`](../crates/client-provider/src/content.rs). Coordinates are a `GeoPoint` (whole numbers, so the model stays `Eq`).
- **What rides along with any message** is `Message::extras`: `forwarded` (and `forwarded_many`, WhatsApp's "Forwarded many times"), `starred`, `view_once`, `story_reply` (the story it answers: give the story's id and what you know of it; the client fills in the rest from the story it holds), `mentions`, `link` (a link's preview, as the message carries it: the client never fetches the page) and `sender_username`. A mention carries the person's id and the `handle` that follows the `@` in the text; when your backend does not say what stands in the text, give `mention_handle` of the id: the client also looks for the person's other ids. Leave `name` and `me` empty, the client fills them. A view-once file is never downloaded by the client, whatever its `source` says.
- **One person, several ids.** When your backend knows the same person under more than one id (on WhatsApp: the phone number and the hidden-number id), say so in `Contact::alt_ids`. The client keeps them together, so a message, a mention or a group participant that names the person by one id gets the name saved under the other. An id that is a phone number in E.164 (`+` and digits) is read as the person's number.
- **`Media::gif`** marks a video that is meant to play as a GIF (short, silent, looping). Leave it `false` when your backend cannot tell. An animated sticker or GIF *file* needs no flag: the client tells from the bytes.
- **A poll's `chosen`** is the account's own vote. Say `None` when your backend's tally does not tell: the client then keeps the vote it cast through you, and takes only the counts from your copy.
- **Things you cannot model** go in `MessageContent::Unsupported { description }`, with the type's name as your backend gives it in `description`: it is shown as a neutral "Unsupported message" tile that names the type. Do not drop them: a gap in a conversation is worse than a placeholder.

## What a provider must produce

| Type | From | What must be right |
|---|---|---|
| `Account` | `list_accounts` | a stable `id`, a `display_name`, the `connection` state as it is now, `self_contact` when you know the account's own id |
| `Chat` | `list_chats`, `ChatUpdated` | a stable `id`, `kind`, `title`; `last_message` when the listing has it; `unknown` for the flags your backend does not tell |
| `Message` | `fetch_messages`, `MessageUpserted` | a stable `id`, the `client_id` of a message sent from here, `direction`, `timestamp`, `content`, `status` |
| `SendReceipt` | `send` | the same `message_id` for the same `client_id`; `Accepted` or `Sent`, never `Pending` |
| `Page<T>` | the listings | `next_cursor` set until there is nothing more, whatever the length of a page |
| `ProviderEvent` | `subscribe` | full objects, not differences; delivering one twice is fine |
| `ProviderError` | every call | `Transient` or `RateLimited` only when repeating the same request can work |

## The example

[`crates/provider-example/src/lib.rs`](../crates/provider-example/src/lib.rs) is the skeleton to copy, and it is built and tested with the workspace, so it cannot fall behind the trait. Its crate documentation holds the smallest provider that compiles (the one the README shows, as a doc-test); `EchoProvider` is the same nine methods over a backend that keeps messages: a send that is idempotent, history paged newest first with a cursor, a refusal as `Rejected`, and a reply pushed through `subscribe`.

## Testing a provider

No test should need the network or a real account.

**The contract, against your provider.** [`crates/provider-example/tests/contract.rs`](../crates/provider-example/tests/contract.rs) says what the client relies on as six tests: the account and its chat are listed, a send repeated is one message with one id (and the stored copy carries the `client_id`), an event reaches whoever subscribed, history is newest first and pages back to the start, and what cannot be sent is `Rejected` and not transient. Copy the file and change the constructor. There is no shared conformance suite that takes any `Provider` yet; this file is the closest thing, and it is short on purpose.

**The mapping, as pure functions.** Follow `provider-wuapi`: keep the mapping from wire types to the model free of I/O and test it against JSON fixtures.

**Requests, paging and errors, against a local server.** The wuapi adapter's tests run it against [`wiremock`](https://crates.io/crates/wiremock) on localhost: request building, cursors, each status code to its `ProviderError`, timeouts. When you make the requests by hand, a small transport trait with a fake does the same.

**The engine's expectations.** The tests in [`client-core/src/tests.rs`](../crates/client-core/src/tests.rs) run the sync engine over `provider-mock` and over providers written for one test. Two to read before trusting your `send`: `a_lost_answer_never_sends_twice` and `an_event_that_overtakes_the_receipt_does_not_duplicate_the_message`.

**In the window.** `--provider yours --no-keychain --data-dir <a scratch directory>` keeps a trial away from your real data.

## Wiring it in

Providers are compiled into the application and chosen with `--provider`. There is no registry and nothing is loaded at runtime: adding one means editing an enum and two `match`es in `crates/app`, and rebuilding. These are all the places, with what the example added to each (search the crate for `provider-example` to see them):

| File | What to add |
|---|---|
| `Cargo.toml` (root) | the crate in `members`, and `provider-yours = { path = "crates/provider-yours" }` in `[workspace.dependencies]` |
| `crates/app/Cargo.toml` | `provider-yours.workspace = true` under `[dependencies]`. The example is `optional = true` behind a cargo feature because it must not be in a release build; a real provider is a plain dependency |
| `crates/app/src/cli.rs` | a variant of `ProviderKind`; its name in the `match` of `"--provider"` in `parse`; the name in the usage text (`--provider <mock|wuapi>`) |
| `crates/app/src/providers.rs`, `launch` | where its local database goes: an arm in the `match` on `(options.provider, &options.data_dir)`. `Some(database_in(dir, "yours")?)` keeps the chats in an encrypted file named after the provider; `None` keeps them in memory |
| `crates/app/src/providers.rs`, `launch_on` | an arm that builds the provider and starts an engine on it: `start_engine(Arc::new(YourProvider::new(..)), &storage, history, runtime)?`, returned as `Launch::Chats` |

`Launch::Chats` also says what kind of session it is (`SessionKind`), which decides what Settings offers: `Demo` has nothing to sign out of, `Environment` is a key the application cannot forget. A provider whose credentials come from an environment variable or a file fits one of those. A provider with a sign-in screen of its own needs more: wuapi's is `WuapiLogin` in the same file and the device flow in `crates/app/src/login.rs`, and today that part is written for wuapi, not generic.

Options that only make sense for one provider are refused for the others at the end of `parse` (`--live needs the wuapi provider`); add yours there.

## Checklist for a pull request

- [ ] The crate depends on `client-provider` and not on `client-core` or the application.
- [ ] `capabilities()` starts from `Capabilities::none()` and every flag that is on has its method implemented.
- [ ] `send` is idempotent on `client_id` for at least `IDEMPOTENCY_WINDOW`, and the message carries the `client_id` when it comes back in history or in an event. A test sends twice.
- [ ] Every network request has a timeout, and the backend's own retries are off.
- [ ] Errors are mapped on purpose: timeouts, dropped connections and 5xx are `Transient`, a 429 is `RateLimited` with its delay, a real 401 is `Unauthorized`, what will stay wrong is `Rejected` with words a person can read. A test per family.
- [ ] `fetch_messages` is newest first and `next_cursor` is followed to the end in a test.
- [ ] `subscribe` ends when its transport gives up (the engine subscribes again) and never blocks a call.
- [ ] No credential is logged, printed or put in a URL.
- [ ] Tests run without the network: `cargo test -p provider-yours`.
- [ ] The five places of "Wiring it in" are edited, with a test in `cli.rs` for the new `--provider` value.
- [ ] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.
- [ ] A section in this page for what your backend cannot do yet, as the wuapi adapter has.

## How the wuapi adapter is built

The adapter does not speak HTTP to `/v1` itself. It uses [`wuapi`](https://github.com/wuapidev/wuapi-rust), the Rust SDK generated from wuapi's OpenAPI spec, for requests, wire types, cursor paging and idempotency keys. What is left in the crate:

| Module | Hand-written because |
|---|---|
| `client.rs` | One method per endpoint the adapter uses, each a call to the SDK. Configures it: the request timeout, no retries, the shared HTTP client. Asks where a file the API holds is (a message's, a story's, a favorite sticker's), with the retries that takes. |
| `mapping.rs` | The SDK's types to the neutral model. Values newer than the SDK (its enums keep them as `Unknown`) get the least committal mapping instead of an error. |
| `error.rs` | The SDK's error enum to `ProviderError`. Timeouts, network errors, 408, 429, 5xx and the 409s that mean "not now" (`account_not_ready`, `idempotency_conflict`, `state_resyncing`, `message_sending`) are transient; 401 asks for a new sign-in; other 4xx are rejections; a 2xx that does not parse is a protocol error. |
| `login.rs`, `http.rs` | The browser sign-in (`/cli/device/code`, `/cli/device/token`) is not in the OpenAPI spec, so the SDK does not cover it. `DeviceLogin::request_code` and `poll_once` are the two steps; the application's sign-in screen drives them (see below). Media bytes are fetched from URLs that are not JSON and not always on the API host. Both use a small reqwest transport with the same timeouts. |
| `uploads.rs` | Sending a file: the SDK's upload calls, and the one request that is not the SDK's, the bytes to the storage URL (without the API key). |
| `forward.rs`, `stickers.rs`, `stories.rs` | Forwarding by naming a message, favorite stickers and stories: their SDK calls, their mapping, and what each refusal becomes. |
| `availability.rs` | What this deployment, or one number, does not have yet (`Provider::unavailable`), and when it is asked again. |
| `compat.rs` | Temporary: the calls whose answers carry a message, an account or a group, read leniently so that a backend older than the SDK is still understood (below). Not temporary: the two story listings, read one story at a time. |
| `social.rs` | Profiles and groups: the SDK calls, their mapping, and the API's refusals turned into the neutral codes (`403 whatsapp_forbidden` is "not an admin" on a route only admins use and "not in the group" on a read). |
| `events.rs` | Polling, behind the `EventSource` trait: the fallback, and the safety poll beside the stream. |
| `sse.rs`, `envelope.rs` | The server-sent events parser, and one stream event to the neutral model (decoding, the `evt_` window, the chats to read again). |
| `stream.rs`, `live.rs` | The connection to wuapi Streams and the drivers over it: strict (`--live stream`) and `Auto`. See "Live updates: wuapi Streams". |
| `keychain.rs` | The API key in the OS keychain. |

The SDK comes from crates.io (`wuapi` 0.14.0, see the workspace `Cargo.toml`). Every call to `/v1` goes through it (its generated methods, or, for the answers `compat.rs` reads leniently, its `RequestParts`); the two things that do not are the browser sign-in and the bytes of files (downloads, and the upload to the storage URL).

### The chat list

`list_chats` is `GET /v1/accounts/{accountId}/chats`, one page per call, with the API's cursor passed through: the engine follows it until it is `null`, never until a short page (the API may return a page shorter than `limit` with a cursor still set). No filter is sent; channels are skipped in the mapping. `mapping::chat` translates a chat, and three of the API's semantics matter there:

- `pinned`, `archived` and `muted` are `null` until wuapi knows them: since SDK 0.11.0 it takes them from what WhatsApp syncs to the linked number (after linking and after a resync), so `null` is rare and short-lived. The neutral model has no "unknown", so `null` becomes not pinned, not archived, not muted, with `Chat::unknown` saying which of them is a placeholder.
- `pinnedAt` becomes `Chat::pinned_at` for a chat that is pinned: it is what orders the pinned chats, the last one pinned first.
- `unreadCount` is wuapi's own count and can be too low (or `null`) for chats that were unread before linking; `unread: true` with a count of 0 is a chat marked as unread. The model has only a count, so a chat that is unread without a known count gets 1: the badge and the "Unread" filter work.
- `name` is already the best name (the one saved in the phone's address book, else the group's subject or the profile name), so `contact_names` is `true`.

The polling event source reads the newest page of messages as before, and the first page of an account's chats when that account has new messages, plus every account's every fourth poll (that is how a chat read or pinned on the phone is noticed). It emits `ChatUpdated` for chats whose state changed, so the provider's unread counts reach the store between refreshes. It does not page messages to rebuild chats. Only accounts that are `ready` are asked for their chats, four at a time: a number that was never linked, or is logged out, costs a poll nothing, and one whose chats cannot be read is asked again at the next poll without holding up the others. A general poll is two requests (accounts, messages), plus one per connected number when its chats are read.

Stories have no message the poll would see (contacts' are stored apart from messages) and, without the stream, nothing tells of them, so the first poll and then every eighth (about every half minute) reads the stories of each number that is connected or reconnecting (`GET …/stories/own` and `GET …/stories`, several numbers at a time; they are stored rows, so a connection that dropped does not matter, and a number that is not linked is not asked). A listing that fails in passing is asked for again at the next poll, not a sweep later. Each story of a listing is decoded apart (`compat.rs`), with the fields a backend may leave out filled in: one story the SDK cannot read is one story less, never an empty Status, and a story inside an author's group is that author's whatever it says itself. It emits `StoryUpserted` for one that is new or changed (seen on the phone, another view of an own one), `StoryRemoved` for one that is no longer listed, and `StoryMuteChanged` when `story_group.muted` changes ("muted" is said the first time too; "not muted" never first, because a mute made in the client cannot be written to the API). These are reads of stored rows: nothing is asked of WhatsApp and no author is told anything. A deployment without the routes is asked once and then left alone until it is worth asking again: after ten minutes, or at once when the user goes to Status or presses "Check again" (`recheck`).

### Live updates: wuapi Streams

wuapi Streams is the channel a client opens itself: one `GET /v1/events/stream` (server-sent events) with the API key as a bearer token, which stays open and carries the events of the organization as they happen. The adapter uses it as the live transport (`stream.rs`, `live.rs`, `envelope.rs`, `sse.rs`).

**Three modes**, chosen with `WuapiConfig::live` or `--live`:

| Mode | What it does |
|---|---|
| `Auto` (the default) | Tries the stream. Where the deployment has none (`404`, `400`, `403`, a page that is not a stream, an address that cannot be used) it polls exactly as before and asks again after ten minutes. When the stream works, the poll slows down to a safety poll. |
| `Stream` (`--live stream`) | The stream only. No poller is built, so no poll request is ever made. A stream that is not there is an error from `subscribe`; so is one that stays down for thirty minutes (the gateway's replay window). The engine paces its retries by its own backoff (1 s up to 60 s, not reset by a refresh that works), never sooner than a `Retry-After`; the provider also refuses to connect, and returns its last error, while its six-a-minute budget is spent or a `Retry-After` has not passed. |
| `Polling` (`--live polling`) | Polling only, with no stream code built: what the adapter did before the stream. |

**One connection, no gap.** A drop is absorbed inside the same `EventStream`: the link waits (the `retry:` the gateway sent, plus a jittered backoff from 1 s up to 30 s, so that clients told the same `retry:` do not all return at the same instant; at most six connects a minute, counted across every `subscribe` of the provider, not per connection), reconnects with `Last-Event-ID` and carries on, so the engine sees nothing and does not refresh. A gateway drain (`retry:` then close) is the same. Past the replay window the gateway answers with `event: reset`: the stream ends, the cursor goes with it, and the engine subscribes again and refreshes once. The same event can arrive twice (the cursor may lag), so each `evt_` id is said once (a window of 4096).

**Fallback and the safety poll (`Auto`).** After three failed connects, or twenty seconds without one, the adapter polls until a re-probe finds the stream again; one poll then closes the seam. While the stream is live, the poll runs only as a safety net, every 75 seconds (the chats every 5 minutes, stories every 10), with no follow-ups. If a safety poll finds a message older than ten seconds that the stream never said, twice in a row, the stream is treated as silent (a gateway with its log writes turned off, say) and polling goes back to full cadence, with the connection kept; the next event from the stream ends that.

**What still needs REST.** The stream carries the events, but not always the object a screen wants:

- the chats an event says changed (`chat.updated`, `contact.picture_updated`, `group.joined`, a message the adapter could not decode, `history.synced`) are read again with `GET /v1/accounts/{accountId}/chats/{chatId}`: gathered over half a second, at most four at once, two seconds apart per chat, sixty a minute, and over two hundred pending they collapse into one page of chats per account;
- the engine's refresh (a start, a reset), history, sending, media and everything else a user does remain requests to the API;
- events the client does not map (calls, presence, labels, channels, `webhook.test`, types newer than the adapter) are ignored.

**The key goes only to the API and to the stream.** The bearer is sent to the configured API origin and to the stream origin, and to no other: redirects are never followed (a `3xx` is an answer), the key is never in a URL, and a stream address must be `https://` (plain `http://` only on this machine: `localhost`, `127.0.0.0/8`, `::1`) and carry no user name or password. Without `--stream-url`, production uses `https://stream.wuapi.dev` and any other `--api-url` uses its own origin.

**What `Auto` sends before the gateway exists.** The default is `Auto`, and until the DNS record for `stream.wuapi.dev` points at the gateway, `Auto` sends `GET https://stream.wuapi.dev/v1/events/stream` with `Authorization: Bearer <the API key>` (plus `Accept`, `Cache-Control` and `User-Agent`; no `Last-Event-ID`, query, body or cookies) once per `subscribe`, and then every ten minutes while the answer is a `404`, a page or a `401` that is not the API's. It goes to whatever terminates TLS for that name today (wuapi's own web hosting), over `https` with certificate checks and redirects off; if the host answers nothing, the link retries up to six times a minute and the key travels only once TLS is up. A `401` from that host never signs the user out in `Auto` (an API-shaped one is confirmed with `GET /v1/me` first). `--live polling` avoids the request altogether. This was an accepted exposure of the default, recorded at the owner's decision; it ends when the gateway answers at that name.

**Flags.** `--live <auto|stream|polling>` and `--stream-url <URL>` (both need the wuapi provider; `--stream-url` is refused with `--live polling`). `--diagnose stream` opens no window and no database: it makes one connection without `Last-Event-ID`, listens for twenty seconds and prints the stream address, the mode, the answer to the connect (`status`, `content_type`, `took_ms`; on a refusal its `code` and `retry_after`), the `retry` the gateway asked for, whether a ping came and within how long, whether a cursor was seen, and how many events came, by type. It prints no key, event id, cursor, payload or words of the API's message, and uses one of the organization's stream connections while it runs. The window "Diagnostics" shows the transport at that moment ("Live updates": Stream, reconnecting, or Polling and why).

### Following a sent message

`POST /v1/messages` answers `202` with the message `queued` (`DeliveryStatus::Accepted`: the clock goes, a faint tick shows). `sent`, `delivered` and `read` come later, and the general poll is a poor way to hear of them: it is up to an interval late, and `GET /v1/messages` is the newest page of the whole organization in the order the rows were created, so a message leaves it as soon as a hundred others (incoming ones, of any number, imported history included) were stored after it. So the adapter keeps the outgoing messages whose ticks can still move and asks about each by id (`follow.rs`, `GET /v1/messages/{messageId}`):

| When | How often |
|---|---|
| Accepted from here, or first seen within two minutes of being sent | 0.5 s after, then 1, 2, 4, 8, 16 and every 30 s apart, until it is delivered, read or failed: at most 8 requests in the two minutes |
| Not read yet, up to 6 hours old | every eighth of its age, between 15 s (30 s while it is not delivered) and 5 minutes apart: about 90 requests over the six hours for a message that is never read |
| Still on the newest page the general poll reads | never: that poll checks it for free and puts its next time off |

Several messages of one chat that are due share one request (that chat's newest page; one it does not show is then asked for by id). Follow-ups are held to 120 requests a minute, a fifth of the key's 600, and at most 8 per step; a `429` stops them, and the general poll, for its `Retry-After`. A request that fails for a reason that may pass leaves the message where it is, to be asked about at its next time; `404` lets it go. The list is at most 300 messages and belongs to the provider, not to a stream: a send made while the stream is down is followed by the next one. It is filled by `send` (`EventSource::accepted`), by every page of history the client reads (`EventSource::listed`) and by the general poll; when a stream opens, the account's own messages of the last six hours on the newest page are reported once, because their ticks may have moved while nobody listened.

**With the stream** (wuapi Streams carries `message.sent`, `message.delivered`, `message.read`, `message.failed`) none of this is needed while it is live: the stream source maps each event to `ProviderEvent::MessageUpserted` (the events carry the whole message) and leaves `accepted` and `listed` as the no-ops they are by default. The follow-ups (`follow.rs`) and the general poll still run when the stream is not there: with `--live polling`, in `Auto` while it falls back, and in `Auto` after a silent stream (below). While it is live, the stream tells the follow list what it already said, and the list is retired. What stays is everything on the client's side: the `202` still turns the clock into the faint tick, the copy that overtakes its answer is still one bubble, and after a reconnect of the stream the client still reads the recent conversations again, since a push stream does not replay what was missed.

**Media on demand (SDK 0.7.0 and later).** Where a message's file is fetched from is decided by `media.downloaded` alone, never by what the URL looks like:

- `downloaded: true`: `media.url` is the file, and it is fetched as it is.
- `downloaded: false`: the file is still on WhatsApp and `media.url` is an API route that needs the key. The adapter does not follow that URL. The `MediaRef` names the message (`wuapi-media:<size>:<messageId>`), and `fetch_media` calls `messages().get_media(id, redirect=false)` through the SDK, on the configured API host, then downloads the file URL in the answer.
- no `url`: nothing to fetch (some history-imported messages); the tile says "not available".

The API downloads such a file through the number's proxy on first use, so that call is slow and fails in passing: each attempt has its own deadline (`WuapiConfig::media_timeout`, 45 s), a timeout, a dropped connection, a 5xx, a 429 or `409 account_not_ready` is tried again (`media_backoff`, two more attempts), and what still fails is reported as transient, so the engine asks again later instead of marking the file as lost. Only `410 media_expired` is terminal: "WhatsApp no longer has this file."

`media.size` becomes `Media::size_bytes` (shown on the tiles) and rides in the ref, so a file over the caller's limit is refused before the API is asked to fetch it.

**The key goes to the API origin only.** The get-media call carries it; the download of the answered file URL does not, unless that URL has the API's own scheme, host and port. A redirect from the API to a storage host drops it. Both are tested (`tests::pictures_and_media`).

`Account.mediaAutoDownload` (`"none"`, `"all"` or `{maxBytes, types}`) maps to `AccountSettings::server_media` and is shown per number in Settings > Account. It is the server's choice of what to store up front; this app's own "Download media automatically" setting still decides what this computer fetches.

**What the chat list does not know (SDK 0.6.0 and later).** `pinned`, `archived`, `muted`, `unread`/`unreadCount` and `pictureId` are `null` until wuapi knows them (since SDK 0.11.0 the first four come with what WhatsApp syncs after linking and after a resync, so that is seldom and brief). `null` is not `false` or `0`: `mapping::chat` says so in `Chat::unknown` (`ChatUnknown { pinned, muted, archived, unread, picture }`), and the store keeps its own value for each field that is unknown. That is why a chat pinned here stays pinned when the next refresh has nothing to say about pins, and why a chat without a count from the API still gets a badge for what arrives.

**Numbers.** `create_account` is `POST /v1/accounts` with `proxyLocation` (a pair from `GET /v1/proxy-locations`, listed by `link_places`), `historySync` and, for a pairing code, `pairingPhone`; `NewAccount::request_id` travels as `Idempotency-Key`, so a request sent again after a dropped connection finds the same account. `link_status` is `GET /v1/accounts/{id}`: `qrCodeUrl` is a PNG data URL, decoded in `mapping::link_status` (anything else in that field is not drawn and never fetched). `update_account` (`PATCH`: name, `historySync`), `reconnect_account`, `unlink_account` (`POST .../logout`, the account stays) and `delete_account` are one call each. The API's refusals (`upgrade_required`, `project_limit_reached`, `subscription_required`) arrive with a sentence written for people, which is shown as it is. `pairing_code` is `POST /v1/accounts/{id}/pairing-code`, on the account and the session that are already there: the linking screen uses it to leave the QR code for a code without creating anything. There is no call for the way back (see the gaps below), so `link_back_to_scan` is off and `scan_instead` is not implemented.

**Contacts (SDK 0.8.0).** `list_contacts` is `GET /v1/accounts/{accountId}/contacts`, a page of 100 at a time with the API's cursor; the engine copies the book into the store in the background (`SyncEngine::sync_contacts`: paced, resumable from the page that failed, pruned when a pass completes) and everything that shows a contact reads the store. `check_numbers` is `POST .../contacts/check`.

**Sending files.** `upload_media` then `send`, both repeatable (see "Files on their way out" in ARCHITECTURE.md). For wuapi (`uploads.rs`), through the SDK: `uploads().create` with the engine's upload key as `Idempotency-Key` (a file up to 3 MiB travels in that request as base64 and is ready at once), `POST {uploadUrl}` with the bytes (a plain request: the SDK only talks to the API), `uploads().complete`; the reference is `wuapi-upload:<id>`, and the message is `messages().send` with `media: {uploadId}` (`SendMediaUpload`, `SendImageMediaUpload`, `SendVideoMediaUpload`) and the client message id as `Idempotency-Key`. **The API key never goes to `uploadUrl`**: it is a storage host (tested). `413` is `too_large`, `429` waits for `Retry-After`, `404` on the completion or on the send is `upload_expired`, which the engine answers by uploading again. An image is re-encoded by the API the way the WhatsApp apps do (`media.quality`, the account's `imageQuality` by default): the adapter leaves that to the account, except for a `.gif` sent as the image it is, which goes with `quality: original` so that it stays that file. A `404` on the create means the routes are not deployed: the file waits, and `media_upload_ready` says no for ten minutes (TEMPORARY(uploads-rollout); the SDK has no call that says which routes a deployment has, so the probe is a create with an empty body, which creates nothing).

**Forwarding (SDK 0.12.0).** `forward_messages` is `messages().forward(messageId, {to})`, one request per copy with the copy's client id as `Idempotency-Key`: the API takes up to five chats in a request, all or nothing, and the client's contract is a key per copy and a refusal that fails one copy alone. The answer's message becomes the receipt and is followed for its ticks like a send. `400 not_forwardable` is said by its `details.reason` in the neutral codes (`deleted`, `view_once`, `not_sent`, `media_not_stored`; `poll`, `calendar_event`, `reaction`, `unknown`, `empty`, `story` and any reason newer than the adapter are "this kind"), `410 media_expired` is "WhatsApp no longer has this file" and `413 media_too_large` "too large to forward". The API forwards no poll and no calendar event, so `forward_polls` and `forward_events` are off and the menu says so before anything is queued.

**Favorite stickers (SDK 0.12.0).** `list_favorite_stickers` pages `favoriteStickers().list` (a hundred at a time, at most the 2,000 the API keeps), leaving out vector stickers (`lottie`), which cannot be drawn here. A favorite whose file WhatsApp no longer has stays listed (it is still starred on the phone); its file answers `410 media_expired`, which is asked once. A favorite's `MediaRef` is `wuapi-favorite:<size>:<stickerId>`: its file is asked for through `favoriteStickers().getMedia(redirect=false)` only when the engine wants it, and downloaded from the URL that answers, without the key unless it is the API's own origin. `add_favorite_sticker` names the message when the sticker has one (`{messageId}`) and uploads the file otherwise, or when that message cannot be used (`{uploadId}`, the upload under a key made of the account and the file's hash); the two never share an idempotency key. It answers the API's own id, which is what the client knows the favorite by. `remove_favorite_sticker` is the `DELETE`; one already gone is done.

**Stories (SDK 0.12.0).** `list_stories` is `stories().listOwn` (with `viewCount`) and `stories().list` (contacts', one group per author), every page; a text story comes with its colour and font, a file with its size in pixels, and a file still on WhatsApp is `wuapi-story:<size>:<storyId>`, fetched through `stories().getMedia(redirect=false)` when it is wanted. `view_story` is `stories().view`, under the key `view:<storyId>`: **the only call that tells an author anything**, made by the engine once, for a story that was shown, with receipts on. Listing, reading and downloading never send a receipt (tested: the listing and the download make `GET`s only). `story_viewers` pages `stories().listViewers`; `reply_to_story` is a text with `replyToStoryId` (and no `replyToMessageId`); `react_to_story` is `stories().react`, whose `400 not_supported` (the account's status privacy leaves the author out) is a refusal in the API's words. `delete_story` stays `DELETE /v1/messages/{id}`: a story the account posted is a message, and that route is on every deployment. `Message.replyToStoryId` becomes `MessageExtras::story_reply`.

**What is not there yet.** These three came with 0.12.0, and the backend the application talks to may be older than the application, or have a part turned off for a number (`availability.rs`):

| What the API answers | Means | What the adapter does |
|---|---|---|
| `404 not_found` with the message "Route not found." to a forward, a favorites or a stories route | The deployment does not have the route | Marks the feature missing for every number, answers `Unsupported`, asks again ten minutes later. A `404` with any other message is a message, a story or a favorite that is not there, and is said as that |
| `400 not_supported` to `POST` or `DELETE …/stickers/favorites` | Favorite stickers are not turned on for this number in the engine (`WA_STICKER_FAVORITES_SYNC`) | Marks them missing for that number: "not available yet for this number". Changes answer `Unsupported` without a request meanwhile; the list is still read (it is empty) |
| An empty `GET …/stories` | No contact has a story up, or stories are not turned on for this number in the engine (`WA_STORIES`): the API does not say which | Lists nothing |
| `404` to `GET …/stories/own` ("Route not found.") | An older deployment | The account's own stories are read as the messages of the chat `stories`, as before 0.12.0 (TEMPORARY(stories-rollout)), contacts' are "not available yet", and the poll leaves stories alone |

**Marking read.** `mark_read` sends read receipts (`POST .../chats/{id}/read`) and then clears the badge (`POST .../mark-read`); `mark_read_quietly` only clears the badge, for users who turned receipts off.

**Diagnosis.** `wuapi-inbox --provider wuapi --diagnose sends` opens no window and asks the API nothing: it prints, for the last twenty messages sent from this computer, how long after Enter the first request left (`toPost`), how long the answered request took (`post`), how many requests there were, and when the API accepted it and each tick was reached (`accepted`, `firstChange`, `sent`, `delivered`, `read`, `failed`), from the local database, which it does not change. `--diagnose chats` opens no window and no database: it prints the SDK version, the API's URL and, for each account, the first page of its chats as the API answers them (`unread`, `unreadCount`, `pinned`, `archived`, `muted`, whether there is a `pictureId`, `lastMessageAt`). `--diagnose stories` opens no window and no database either: it lists each number's stories through the call the application makes (the one behind `list_stories` and the poll) and prints, per number, `stories=available` or `not_available` (what `unavailable` says after the call), how many are its own and its contacts', how many are not seen, how many authors there are and how many of them are muted, how many stories the API answered (`answered`) and how many of those could not be read (`unread`); then a line per story with its author, `own`, `type`, `status`, `posted`, `expires`, `viewed`, where its file is (`media=none`, `downloaded`, `on_whatsapp` or `missing`) and whether the client shows it (`shown`); then an `unread` line for each story it could not read, with the field that was wrong and never its value. A listing that fails is one line with `error status=<HTTP status> code=<the API's code>`. It only reads: nothing is marked as seen and no author is told anything. Ids are cut to their last four characters; no name, text, caption, URL or key is printed (`diagnose.rs`).

**Temporary fallbacks.** The chats and contacts routes are live in production, so the fallbacks that stood in for them (chats derived from recent messages, contacts derived from the chat list) are gone: a failure of either route is said as it is. Three remain, each marked in the code: `TEMPORARY(uploads-rollout)` (the probe and the ten-minute "not yet" of the upload routes), `TEMPORARY(stories-rollout)` (the account's own stories read from the messages where `GET …/stories/own` does not exist) and `TEMPORARY(response-compat)` (`compat.rs`, below). Remove them once the backend they stand in for is live in production.

**A backend older than the SDK.** The SDK is published when the spec is merged; the backend is deployed after. The generated types read a response field the API added later as required, so against the older backend a `Message` without `forwardedManyTimes`, a `media` without `gifPlayback`, an `Account` without `imageQuality`, a `Group` without `default` or a `group.updated` without `linked` and `unlinked` does not decode, and with it no message, chat, account or group would be read. `compat.rs` makes the calls whose answers carry a `Message`, an `Account` or a `Group` (the lists of accounts, chats, messages and groups, a message by id, the sends, a vote, an edit, a star, a posted story, the account routes, and reading, creating and changing a group): the same requests the generated methods build (through the SDK's `RequestParts` and its HTTP client: the same key, timeouts and errors), decoded into the SDK's own types after those fields were given what their absence means. `tests/older_backend.rs` runs the adapter against such a backend: what worked before works, and forwarding, favorites and contacts' stories each say "not available yet".

### Signing in

The application signs in with the device authorization flow (RFC 8628). `crates/app/src/login.rs` has the state machine (`LoginMachine`: waiting, approved, expired, denied, offline, failed) and `crates/app/src/ui/login.rs` the screen: a "Connect your wuapi account" action, then the code, a button that opens the verification URL, a copy button and a countdown. The polling follows the server's `interval` (plus five seconds on `slow_down`) and stops at `expiresIn`; it runs on the Tokio runtime, never on the UI thread, and belongs to the view, so closing the window stops it. A poll that does not get through is said on screen and repeated: only the server's "expired" or "denied" end the sign-in.

The API key goes to the OS keychain (`keychain.rs`); it is never printed or logged. "Sign out" in Settings removes it and returns to the sign-in. When the keychain cannot be used (a Linux session without a Secret Service), the screen says so and what to do; the session still works and the next start asks again. `--no-keychain` asks for that behaviour on purpose, and `WUAPI_API_KEY` in the environment skips the sign-in.

## Known gaps in the wuapi adapter

They are limits of wuapi's public API today, not of the trait. Each is marked `TODO(wuapi-api)` in the code.

| Gap | Effect | What the adapter does |
|---|---|---|
| A deployment without wuapi Streams, or a key it refuses | Incoming messages are up to a poll late; no typing indicators | Polls, as `Auto` does by itself, and asks about the messages just sent by id ("Following a sent message"); it asks for the stream again every ten minutes |
| `GET /v1/messages` has no filter for "changed since", and is ordered by when a row was created, across the organization | A status that changes on a message off the newest page is not in any listing | The outgoing messages that are not read yet are asked for by id for six hours; an edit or deletion of an older message is seen when its chat is read again |
| No `since` filter on the message listing | Catching up re-reads recent pages | Reads the newest pages on each poll |
| The upload routes (`POST /v1/uploads`) may not be deployed, and no call says which routes a deployment has (a missing route and a missing resource both answer `404 not_found`) | Until they are, files cannot be sent | Probes once with a create that creates nothing (`media_upload_ready`), says "not available yet", asks again after ten minutes |
| An account that was asked for a pairing code cannot go back to its QR code: `pairingPhone` stays set, and `qrCodeUrl` hidden, until the account links or is logged out; no route clears it, and `reconnect` asks for a new code | On the linking screen, "Use QR code instead" is offered while the number is typed and not once the code was asked for | `link_back_to_scan` is off. The way from the QR code to a code works, on the same account and session. A route that clears the pairing phone of an account that is not linked (the session keeps its QR code) would be all it takes: `scan_instead` then calls it |
| `pictureId` is `null` for "none", "hidden" and "not seen yet" alike | A chat without an id is asked about once a day, like before 0.8.0; one with an id is fetched only when the id changes | Follows the id when there is one |
| Without the stream, `contact.picture_updated` is not heard | A changed picture is noticed within a day, not at once | Re-checks after 24 hours; with the stream, the event makes the chat be read again |
| `media.width` and `height` are set for a received file only when the engine reports them, and `durationSeconds` is `null` for received files | Without them an image's box is a fixed placeholder until its thumbnail is cached; a voice note's length shows once it is decoded | Uses them when present (a half pair is no size) |
| `unread` and `unreadCount` are both `null` for chats from before wuapi counted, or holding only imported history | No badge could be shown for them | The client counts what arrives for such a chat (`ChatUnknown::unread`) until the API has a count |
| Without the stream, `history.synced` is not heard | The client cannot know when imported history has arrived | Reads the chat list again 20 s, 1, 3 and 5 min after a link; with the stream, the event reads the account's chats |
| No route lists the groups a contact shares with the account | "Groups in common" has no direct answer | Derived in the store from `GET …/groups`, which carries every group's participants: one listing per number, at most every 30 minutes, when a profile is opened |
| No `GET …/profile`: the own About text cannot be read (`PATCH …/profile` answers 204) | The own profile editor may not know the current About | The display name comes from `Account.profileName`; the About is asked with a contact lookup of the account's own number, and shown as "not known" when WhatsApp does not answer it. What is set from the client is kept |
| `joinApproval` and `memberAddMode` are accepted by `PATCH …/groups/{id}` but are not in the `Group` object | Those two group settings cannot be read back | Shown as "not known" until switched from here; the value set here is kept |
| A group may list a participant (the account itself included) by `lid:<digits>` while the account object only has the phone number | The account's own role in such a group cannot be told, and "groups in common" misses that contact | The role shows as "Not known here" and the admin controls stay offered: WhatsApp refuses what the number may not do, and the refusal is said in words |
| Without the stream, `group.updated`, `group.join_request`, `blocklist.updated` and `contact.updated` are not heard | A participant added, an admin changed or a block made on the phone is not seen as it happens | A group is read again when it is opened (at most every 5 minutes) and when the chat list shows a new name; the blocklist when a profile is opened. With the stream, `group.updated` maps to `ProviderEvent::GroupChanged` and `contact.updated` to `ContactUpdated`; join requests and the blocklist are not mapped |
| `GET …/groups` gives each group's `communityId` and `default` (SDK 0.14.0) but not the groups a community links; those are `GET …/groups/{id}/subgroups` | A community that was only listed does not know the groups it links that this number is not in | `list_groups` leaves `subgroups` empty and `fetch_group` reads them; until a community is opened its panel shows the groups known to be in it |
| Without the stream, a subgroup linked or unlinked on the phone (`group.updated` with `linked`, `unlinked`) and `group.joined` are not heard | The Communities filter learns it late | The groups are listed again at every refresh (a start, a reconnect, a stream that ended). With the stream, a link or unlink maps to `ProviderEvent::CommunityChanged` (`communityId` names the community whichever group WhatsApp reported it on), and so does `group.joined` for a group that is in a community |
| The blocklist is listed by hidden-number id (`lid:`) more and more often | A chat known by its number may not be found blocked | Every id the API gives for an entry (`contactId`, `phone`, `lid`) is kept |
| `media.url` is `null` for history-imported messages | Those attachments cannot be shown | Shows "not available" |
| Whether favorite stickers are turned on for a number is not readable: the list is empty either way, and only a change answers `400 not_supported` | "Not available yet for this number" is known only after the first star or unstar made here | Learns it from that answer, remembers it for ten minutes, and never shows it as a failure |
| A favorite sticker says `animated` and `emojis` only after its file was fetched once, and has no pack | Nothing to search a favorite by before it is here | Whether it moves is told from the file |
| A received sticker carries no pack id, no emoji and no animated flag; the only pack route is `GET …/sticker-packs/{id}` for an id already known | "View pack" and "Add all to library" cannot be offered from a message | Packs are sets the user's imports made; whether a sticker moves is told from the file |
| A sent sticker is not checked or converted by the API or the engine (a PNG goes out as a PNG; a WebP goes out without width, height or thumbnail), and the limits (512 by 512, about 100 KB still, about 500 KB moving) are in no document of the API | A wrong file is sent as it is | The client makes 512 by 512 WebP stickers itself (`client_core::sticker_from_image`) |
| An `image/gif` sent as an image is not converted to a looping video, and the engine sets no thumbnail for a video | A `.gif` file reaches the other side as a picture, and a GIF sent as an MP4 has no preview frame | A saved MP4 is sent as a flagged video; a `.gif` as the image it is. Turning one into the other needs a video encoder, which this build does not have |
| A vector sticker (Lottie) arrives as `type: sticker` with `media.mimeType: application/was` | It cannot be drawn | A tile that says so; the file is not fetched |
| A received message carries no link preview (title, description, thumbnail) | Links are clickable, without a card | `MessageExtras::link` stays empty |
| A location does not say whether it is a live one, and has no later positions | Every location is a fixed place | `Location::live` is `false` |
| A poll's tally does not say which options the account chose | The vote cast from another device is not marked | `Poll::chosen` is "not known"; the client keeps the vote cast through it |
| Group changes (`group.updated`), calls (`call.*`) and poll votes (`poll.voted`) are events, not messages | No "joined", "left", "renamed" or missed-call lines in a conversation; a tally moves when the poll is read again | `MessageContent::System` is never produced; a poll is reported again when its `updatedAt` moves, or (with the stream) when `poll.voted` carries the poll |
| A contact card is reduced to a name and one number | Other numbers, the organisation and the rest of the vCard are not shown | One number per card |
| `mentions` lists the mentioned ids, with a hidden-number id already turned into the number when the engine knows it, while `text` keeps the digits of the id the sender's phone used; what stands for each mention in the text (the engine's `mentionedUsers[].token`) is not returned | A mention made by hidden-number id cannot be matched by the digits of the listed id | The client matches by every id the person goes by (`Contact.phone` and `Contact.lid` from the contact list), and pairs what is left in order when the counts agree. The backend would need `mentions: [{id, token}]` (or a `mentionTokens` array parallel to `mentions`) on `Message` |
| `GroupParticipant` has one `contactId` (number or hidden-number id), never both, and `Account` does not give the account's own hidden-number id | A participant listed by hidden-number id who is not in the address book has no number; the account's own mention by hidden-number id is not recognised as "You" | Names come from the address book's link, the participant's `name`, and the names on that person's messages; else "someone". The backend would need `phone` and `lid` on `GroupParticipant`, and `lid` on `Account` |
| A poll's voters are not listed (the tally has counts) | Nothing shows who voted | Counts only |
| `DELETE /v1/messages/{id}` is for outbound messages only | A message somebody else sent cannot be deleted, not even for the account alone | `delete_received` is off: "Delete" on such a message is shown off, with the reason |
| There is no route to pin a message in a chat | "Pin" is not offered for messages | No capability, no key, no menu entry |
| A message's object has one `status`; who received or read it in a group, and when, is not reported | "Message info" cannot list readers | It shows who sent it, when, its status and its marks |
| `POST /v1/messages/{id}/forward` takes no `metadata`, so the copy carries no `clientMessageId` | A copy that shows up in the poll before its request is answered cannot be told from somebody else's message | The engine joins the two when the answer arrives (by the id it gives); until then there may be two bubbles for a moment |
| The forward route is all or nothing for its chats, and answers one `Idempotency-Key` for all of them | One refusal would fail every chat of a request, and a retry could not be told apart per chat | One request per copy, each under its own key |
| Polls and calendar events are not forwarded (`400 not_forwardable`), and a contact's story is not either (`reason: story`) | They cannot be passed on | `forward_polls` and `forward_events` are off: the menu says "This kind of message cannot be forwarded"; a story of a contact fails its copy with that sentence |
| No route mutes or unmutes a contact's stories (`story_group.muted` is read-only), and none changes the status privacy lists | A mute made here does not reach the phone; the audience cannot be edited | `story_mute` and `story_privacy_edit` are off (`TODO(wuapi-api: stories mute write)`, `TODO(wuapi-api: stories privacy edit)`); a mute made on the phone is passed on by the poll |
| Without the stream, `story.received`, `story.deleted`, `story.viewed` and `story.reacted` are not heard | A contact's new story, or a new view of an own one, is up to half a minute late | The poll reads the story lists on its first round and every eighth, and the client asks when the user goes to Status; with the stream, each event maps to its `ProviderEvent` at once |
| A `StoryViewer` has no name, and a contact's story no mentions | Viewers are named from the address book; mentions in a story are not shown | `StoryViewer::name` and `Story::mentions` stay empty |
| `Message.replyToStoryId` names the story and nothing of what it showed | A reply to a story that is gone has only "Status" to show | The client fills in the kind and the words from the story it holds when the reply arrives |
| The SDK reads a response field added later as required (`Message.forwardedManyTimes`, `MessageMedia.gifPlayback`, `Account.imageQuality`, `Group.default`, `GroupChange.linked` and `unlinked`), and is published before the backend that sends it is deployed | Against the older backend no message, chat, account or group decodes | `compat.rs` fills those fields in before decoding (TEMPORARY(response-compat)): a group from before communities is in none, and a change from then links nothing |
| `Account.imageQuality` (and `media.quality` per image) exist; the client has no control for them | Pictures go at the account's quality | Left to the account; only a `.gif` sent as an image asks for `original` |
| The edit window is described as "about 15 minutes" and enforced by WhatsApp, not by the API | The client cannot know beforehand that an edit will be refused | It lets the API refuse and shows its reason; the old text comes back |
| No route marks a view-once file as opened, and none should fetch one | A view-once message is a tile that says to open it on the phone | Never downloaded |
