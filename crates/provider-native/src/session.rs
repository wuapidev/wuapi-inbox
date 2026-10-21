//! One linked number's connection to WhatsApp, as the provider sees it.
//!
//! The provider never names the protocol library beyond its addresses and
//! its message type: it talks to a [`Session`] and hears [`Inbound`]s. The
//! real one is [`crate::live`]; the tests have their own, so that nothing
//! here needs the network or a phone.

use crate::mapping::MediaPointer;
use async_trait::async_trait;
use client_provider::{
    AvatarAnswer, BusinessProfile, ChatChange, Group, GroupChange, JoinRequest, NewGroup,
    ParticipantChange, ParticipantOutcome, PresenceState, StoryPrivacy, SystemEvent,
};
use std::sync::Arc;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::Jid;

/// Why a session could not do what it was asked.
#[derive(Clone, Debug, thiserror::Error)]
pub enum SessionError {
    /// Not now: the connection is down, the phone did not answer, WhatsApp
    /// timed out. The same request can work later.
    #[error("{0}")]
    NotNow(String),
    /// WhatsApp asked to slow down.
    #[error("WhatsApp asked to slow down")]
    SlowDown,
    /// No, and it will stay no. `code` is one of
    /// [`client_provider::refusal`] where it means the same.
    #[error("{message}")]
    Refused {
        /// Machine-readable reason.
        code: String,
        /// Words for the user.
        message: String,
    },
    /// The number is not linked (any more).
    #[error("this number is not linked to WhatsApp")]
    NotLinked,
    /// This session cannot do that.
    #[error("not supported: {0}")]
    Unsupported(&'static str),
}

impl SessionError {
    /// A refusal with a code and words.
    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Self::Refused {
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

/// Shorthand for a session's results.
pub type SessionResult<T> = Result<T, SessionError>;

/// Who a session is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// Its number.
    pub pn: Option<Jid>,
    /// Its hidden-number id.
    pub lid: Option<Jid>,
    /// The name the account gave itself.
    pub push_name: String,
}

/// A message, addressed: what WhatsApp needs to act on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The chat it is in.
    pub chat: Jid,
    /// Its id.
    pub id: String,
    /// Who wrote it, when it was not the account.
    pub sender: Option<Jid>,
    /// The account wrote it.
    pub from_me: bool,
    /// When it was sent, in milliseconds.
    pub timestamp_ms: i64,
}

/// A message that reached this device.
#[derive(Clone, Debug, PartialEq)]
pub struct Incoming {
    /// The chat.
    pub chat: Jid,
    /// The other address the chat's person goes by, when the message says.
    pub chat_alt: Option<Jid>,
    /// Who wrote it.
    pub sender: Jid,
    /// The other address the sender goes by, when the message says.
    pub sender_alt: Option<Jid>,
    /// The account wrote it (from here or another of its devices).
    pub from_me: bool,
    /// Its id.
    pub id: String,
    /// The name it came with.
    pub push_name: Option<String>,
    /// When, in milliseconds.
    pub timestamp_ms: i64,
    /// The message.
    pub message: Arc<wa::Message>,
    /// How far it got, when the source says (history does), as
    /// [`client_provider::DeliveryStatus::rank`].
    pub status: Option<u8>,
    /// The account starred it.
    pub starred: bool,
}

/// A chat as the phone handed it over.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryChat {
    /// The chat.
    pub chat: Jid,
    /// The other address its person goes by.
    pub chat_alt: Option<Jid>,
    /// Its name: a group's subject, or the name the phone shows.
    pub name: Option<String>,
    /// Unread messages.
    pub unread: Option<u32>,
    /// It is flagged as unread.
    pub marked_unread: bool,
    /// It is archived.
    pub archived: bool,
    /// Pinned, with the moment in milliseconds when the phone says (0
    /// when it does not).
    pub pinned_at_ms: Option<i64>,
    /// Muted until, in milliseconds; -1 for "until unmuted".
    pub muted_until_ms: Option<i64>,
    /// Its messages, in any order.
    pub messages: Vec<Incoming>,
}

/// How far a receipt says messages got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptLevel {
    /// On the other phone.
    Delivered,
    /// Read (or, for a voice note, played).
    Read,
}

/// Where a call is, as WhatsApp signals it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallSignal {
    /// Somebody is calling.
    Ringing {
        /// With video.
        video: bool,
        /// A group call.
        group: bool,
    },
    /// The call is connected: both sides hear each other.
    Connected,
    /// It ended; `missed` when nobody on this account answered.
    Ended {
        /// Nobody answered here.
        missed: bool,
        /// Why, when WhatsApp says.
        reason: Option<String>,
    },
}

/// What a session tells the provider.
#[derive(Clone, Debug, PartialEq)]
pub enum Inbound {
    /// A QR code to scan, as its text.
    Qr(String),
    /// The phone accepted; the link is being finished.
    Paired,
    /// Linking stopped.
    LinkFailed(String),
    /// Online.
    Connected(Identity),
    /// Dropped, coming back by itself.
    Reconnecting,
    /// Offline, not coming back by itself.
    Disconnected(Option<String>),
    /// Unlinked from the phone, or banned.
    LoggedOut(Option<String>),
    /// Messages, as they happened.
    Messages(Vec<Incoming>),
    /// A part of the account's history: chats with their messages, names
    /// and which addresses are one person. `asked` is set for the answer
    /// to [`Session::request_history`].
    History {
        /// The chats.
        chats: Vec<HistoryChat>,
        /// Names people gave themselves.
        names: Vec<(Jid, String)>,
        /// Hidden-number ids with their numbers.
        aliases: Vec<(Jid, Jid)>,
        /// It answers a request made from here.
        asked: bool,
    },
    /// Messages got further.
    Receipt {
        /// The chat.
        chat: Jid,
        /// Whose device says so.
        from: Jid,
        /// The messages.
        ids: Vec<String>,
        /// How far.
        level: ReceiptLevel,
        /// When, in milliseconds.
        timestamp_ms: i64,
    },
    /// A chat was pinned or unpinned, on any of the account's devices.
    Pinned {
        /// The chat.
        chat: Jid,
        /// Pinned now.
        pinned: bool,
        /// When, in milliseconds.
        at_ms: i64,
    },
    /// A chat was muted or unmuted.
    Muted {
        /// The chat.
        chat: Jid,
        /// Until when, in milliseconds; -1 until unmuted; `None` unmuted.
        until_ms: Option<i64>,
    },
    /// A chat was archived or taken out of the archive.
    Archived {
        /// The chat.
        chat: Jid,
        /// Archived now.
        archived: bool,
    },
    /// A chat was marked as read or as unread.
    MarkedRead {
        /// The chat.
        chat: Jid,
        /// Read now.
        read: bool,
    },
    /// A chat was deleted on another device.
    ChatDeleted {
        /// The chat.
        chat: Jid,
    },
    /// A message was starred or unstarred.
    Starred {
        /// The chat.
        chat: Jid,
        /// The message.
        id: String,
        /// Starred now.
        starred: bool,
    },
    /// A message was deleted for the account alone, on another device.
    DeletedForMe {
        /// The chat.
        chat: Jid,
        /// The message.
        id: String,
    },
    /// An entry of the address book: a name saved, or removed.
    Contact {
        /// Who.
        jid: Jid,
        /// Their other address.
        alt: Option<Jid>,
        /// The saved name; `None` when the entry was removed.
        saved_name: Option<String>,
        /// Their username.
        username: Option<String>,
    },
    /// A name somebody gave themselves.
    ProfileName {
        /// Who.
        jid: Jid,
        /// The name.
        name: String,
    },
    /// Somebody is typing, recording, or stopped.
    Activity {
        /// The chat.
        chat: Jid,
        /// Who.
        from: Jid,
        /// What.
        state: PresenceState,
    },
    /// A picture changed.
    Picture {
        /// Whose: a person's or a group's.
        jid: Jid,
        /// Its new id; `None` when it was removed.
        picture_id: Option<String>,
    },
    /// Something about a group changed.
    GroupChanged {
        /// The group.
        group: Jid,
        /// The line a conversation shows for it, when it has one, with
        /// the id and time it is kept under.
        notice: Option<(String, i64, SystemEvent)>,
        /// Its new subject, when that is what changed.
        subject: Option<String>,
        /// The community it was linked to or unlinked from, or made in.
        community: Option<Jid>,
        /// When it was made, when that is what happened: its chat is
        /// listed from then on.
        created_ms: Option<i64>,
    },
    /// A vote in a poll, decrypted.
    PollVote {
        /// The chat.
        chat: Jid,
        /// The poll.
        poll: String,
        /// Who voted.
        voter: Jid,
        /// The account itself.
        mine: bool,
        /// The hashes of the options they now stand for.
        hashes: Vec<Vec<u8>>,
        /// When, in milliseconds.
        timestamp_ms: i64,
    },
    /// A contact's stories were muted or unmuted on another device.
    StoryMuted {
        /// The author.
        author: Jid,
        /// Muted now.
        muted: bool,
    },
    /// Who sees the account's stories, as the phone has it.
    StoryAudience(StoryPrivacy),
    /// A call changed state.
    Call {
        /// The call.
        id: String,
        /// Who it is with.
        peer: Jid,
        /// What happened.
        signal: CallSignal,
        /// When, in milliseconds.
        timestamp_ms: i64,
    },
}

/// Where a session sends what it hears: the session's number in the
/// protocol store, and the news.
pub type Sink = tokio::sync::mpsc::UnboundedSender<(i32, Inbound)>;

/// What a person says about themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lookup {
    /// Their "About" text.
    pub about: Option<String>,
    /// The id of their picture.
    pub picture_id: Option<String>,
    /// Their hidden-number id or number, whichever was not asked by.
    pub alt: Option<Jid>,
    /// A business's verified name.
    pub business_name: Option<String>,
}

/// A number looked up on WhatsApp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OnWhatsApp {
    /// The number as it was asked, digits only.
    pub digits: String,
    /// Its address, when it has WhatsApp.
    pub jid: Option<Jid>,
}

/// A story to put up.
#[derive(Clone, Debug, PartialEq)]
pub struct StatusPost {
    /// The message.
    pub message: wa::Message,
    /// Who it is shown to.
    pub recipients: Vec<Jid>,
    /// The id it goes out with.
    pub id: String,
}

fn unsupported<T>(what: &'static str) -> SessionResult<T> {
    Err(SessionError::Unsupported(what))
}

/// One linked number.
///
/// The first block is what every session does. The rest answers
/// "not supported" by default, as the provider's own optional methods do.
#[async_trait]
pub trait Session: Send + Sync + 'static {
    /// Who the session is, as far as it knows yet.
    fn identity(&self) -> Identity;

    /// Sends a message under an id chosen by the caller. Sending the same
    /// id again is the same message.
    async fn send(&self, to: &Jid, message: wa::Message, id: &str) -> SessionResult<()>;

    /// Tells the senders that messages were read.
    async fn mark_read(
        &self,
        chat: &Jid,
        sender: Option<&Jid>,
        ids: &[String],
    ) -> SessionResult<()>;

    /// Downloads and decrypts a file.
    async fn download(&self, pointer: &MediaPointer) -> SessionResult<Vec<u8>>;

    /// Encrypts and uploads a file; `class` is what WhatsApp stores it as.
    async fn upload(&self, bytes: Vec<u8>, class: &str) -> SessionResult<MediaPointer>;

    /// Asks the phone for messages older than `oldest`. The answer comes
    /// as [`Inbound::History`] with `asked` set, when the phone is online.
    async fn request_history(&self, oldest: &Target, count: u32) -> SessionResult<()>;

    /// Asks for a code to type on the phone, instead of scanning.
    async fn pair_code(&self, phone: &str) -> SessionResult<String>;

    /// Gives up on the code; the QR codes go on.
    async fn cancel_pair_code(&self) -> SessionResult<()>;

    /// Connects again now, without waiting for the backoff.
    async fn reconnect(&self);

    /// Unlinks this device from the phone.
    async fn logout(&self) -> SessionResult<()>;

    /// Stops the session, keeping its link.
    async fn shutdown(&self);

    // ----- messages -------------------------------------------------------

    /// Reacts to a message; an empty emoji takes the reaction back.
    /// Answers the id the reaction went out with.
    async fn react(&self, target: &Target, emoji: &str) -> SessionResult<String> {
        let _ = (target, emoji);
        unsupported("reactions")
    }

    /// Replaces a message's content.
    async fn edit(&self, target: &Target, replacement: wa::Message) -> SessionResult<()> {
        let _ = (target, replacement);
        unsupported("editing a message")
    }

    /// Takes a message back for everyone.
    async fn revoke(&self, target: &Target) -> SessionResult<()> {
        let _ = target;
        unsupported("deleting a message")
    }

    /// Deletes a message on the account's devices alone.
    async fn delete_for_me(&self, target: &Target) -> SessionResult<()> {
        let _ = target;
        unsupported("deleting a message for me")
    }

    /// Stars or unstars a message.
    async fn star(&self, target: &Target, starred: bool) -> SessionResult<()> {
        let _ = (target, starred);
        unsupported("starring a message")
    }

    /// Pins a message for everyone in its chat, or unpins it (`None`).
    async fn pin(
        &self,
        target: &Target,
        pin: Option<client_provider::PinFor>,
    ) -> SessionResult<()> {
        let _ = (target, pin);
        unsupported("pinning a message")
    }

    /// Casts the account's vote in a poll; `poll` is the poll's own
    /// message.
    async fn vote(
        &self,
        target: &Target,
        poll: &wa::Message,
        options: &[String],
    ) -> SessionResult<()> {
        let _ = (target, poll, options);
        unsupported("voting in a poll")
    }

    // ----- chats ----------------------------------------------------------

    /// Pins, mutes or archives a chat on every device of the account.
    /// `last` is the chat's newest message, which WhatsApp wants named.
    async fn change_chat(
        &self,
        chat: &Jid,
        change: ChatChange,
        last: Option<&Target>,
    ) -> SessionResult<()> {
        let _ = (chat, change, last);
        unsupported("changing a chat's state")
    }

    /// Marks a chat read or unread on every device of the account,
    /// telling nobody else.
    async fn set_chat_read(
        &self,
        chat: &Jid,
        read: bool,
        last: Option<&Target>,
    ) -> SessionResult<()> {
        let _ = (chat, read, last);
        unsupported("marking a chat read quietly")
    }

    /// Asks to be told when somebody is online or typing.
    async fn watch(&self, who: &Jid) -> SessionResult<()> {
        let _ = who;
        unsupported("presence")
    }

    // ----- people ---------------------------------------------------------

    /// Which of these numbers have WhatsApp.
    async fn on_whatsapp(&self, digits: &[String]) -> SessionResult<Vec<OnWhatsApp>> {
        let _ = digits;
        unsupported("checking numbers")
    }

    /// What a person says about themselves.
    async fn lookup(&self, who: &Jid) -> SessionResult<Lookup> {
        let _ = who;
        unsupported("looking a contact up")
    }

    /// What a business says about itself.
    async fn business(&self, who: &Jid) -> SessionResult<Option<BusinessProfile>> {
        let _ = who;
        unsupported("business profiles")
    }

    /// A person's or a group's picture, unless it is the one `known`.
    async fn picture(&self, whose: &Jid, known: Option<&str>) -> SessionResult<AvatarAnswer> {
        let _ = (whose, known);
        unsupported("profile pictures")
    }

    /// The people the account blocked.
    async fn blocked(&self) -> SessionResult<Vec<Jid>> {
        unsupported("the blocklist")
    }

    /// Blocks or unblocks somebody.
    async fn set_blocked(&self, who: &Jid, blocked: bool) -> SessionResult<()> {
        let _ = (who, blocked);
        unsupported("blocking a contact")
    }

    /// The account's own "About" text.
    async fn own_about(&self) -> SessionResult<Option<String>> {
        unsupported("reading the own profile")
    }

    /// Changes the name others see.
    async fn set_name(&self, name: &str) -> SessionResult<()> {
        let _ = name;
        unsupported("changing the own name")
    }

    /// Changes the "About" text.
    async fn set_about(&self, about: &str) -> SessionResult<()> {
        let _ = about;
        unsupported("changing the own About")
    }

    /// Sets or removes a picture: the account's own, or a group's.
    /// Answers the new picture's id.
    async fn set_picture(
        &self,
        group: Option<&Jid>,
        jpeg: Option<&[u8]>,
    ) -> SessionResult<Option<String>> {
        let _ = (group, jpeg);
        unsupported("changing a picture")
    }

    // ----- groups ---------------------------------------------------------

    /// Every group the account is in, with its participants.
    async fn groups(&self) -> SessionResult<Vec<Group>> {
        unsupported("listing groups")
    }

    /// One group, read from WhatsApp now.
    async fn group(&self, group: &Jid) -> SessionResult<Group> {
        let _ = group;
        unsupported("group details")
    }

    /// Makes a group, a community, or a group inside a community.
    async fn create_group(
        &self,
        new: &NewGroup,
        participants: &[Jid],
        parent: Option<&Jid>,
    ) -> SessionResult<Group> {
        let _ = (new, participants, parent);
        unsupported("creating a group")
    }

    /// Changes a group's details or settings.
    async fn change_group(&self, group: &Jid, change: &GroupChange) -> SessionResult<()> {
        let _ = (group, change);
        unsupported("changing a group")
    }

    /// Adds, removes, promotes or demotes participants, answered one by
    /// one.
    async fn change_participants(
        &self,
        group: &Jid,
        change: ParticipantChange,
        who: &[Jid],
    ) -> SessionResult<Vec<ParticipantOutcome>> {
        let _ = (group, change, who);
        unsupported("changing a group's participants")
    }

    /// A group's invite link, made anew when `reset`.
    async fn invite_link(&self, group: &Jid, reset: bool) -> SessionResult<String> {
        let _ = (group, reset);
        unsupported("group invite links")
    }

    /// Who asks to join a group.
    async fn join_requests(&self, group: &Jid) -> SessionResult<Vec<JoinRequest>> {
        let _ = group;
        unsupported("requests to join a group")
    }

    /// Approves or rejects requests to join.
    async fn answer_join_requests(
        &self,
        group: &Jid,
        approve: bool,
        who: &[Jid],
    ) -> SessionResult<Vec<ParticipantOutcome>> {
        let _ = (group, approve, who);
        unsupported("answering requests to join")
    }

    /// Leaves a group.
    async fn leave_group(&self, group: &Jid) -> SessionResult<()> {
        let _ = group;
        unsupported("leaving a group")
    }

    /// Links a group to a community.
    async fn link_subgroup(&self, community: &Jid, group: &Jid) -> SessionResult<()> {
        let _ = (community, group);
        unsupported("linking a group to a community")
    }

    /// Takes a group out of a community.
    async fn unlink_subgroup(&self, community: &Jid, group: &Jid) -> SessionResult<()> {
        let _ = (community, group);
        unsupported("taking a group out of a community")
    }

    /// The people of all of a community's groups.
    async fn community_participants(&self, community: &Jid) -> SessionResult<Vec<Jid>> {
        let _ = community;
        unsupported("a community's participants")
    }

    // ----- stories --------------------------------------------------------

    /// Puts a story up.
    async fn post_status(&self, post: StatusPost) -> SessionResult<()> {
        let _ = post;
        unsupported("posting a story")
    }

    /// Takes a story down, for the people it was shown to.
    async fn revoke_status(&self, id: &str, recipients: &[Jid]) -> SessionResult<()> {
        let _ = (id, recipients);
        unsupported("deleting a story")
    }

    /// Mutes or unmutes somebody's stories on every device.
    async fn mute_stories(&self, author: &Jid, muted: bool) -> SessionResult<()> {
        let _ = (author, muted);
        unsupported("muting stories")
    }

    // ----- calls ----------------------------------------------------------

    /// Starts a call. Answers its id.
    async fn start_call(&self, peer: &Jid, video: bool) -> SessionResult<String> {
        let _ = (peer, video);
        unsupported("calls")
    }

    /// Answers a call that is ringing.
    async fn accept_call(&self, id: &str) -> SessionResult<()> {
        let _ = id;
        unsupported("calls")
    }

    /// Declines a call that is ringing, or hangs up one that is on.
    async fn end_call(&self, id: &str) -> SessionResult<()> {
        let _ = id;
        unsupported("calls")
    }

    /// Silences or restores the microphone of a call.
    async fn mute_call(&self, id: &str, muted: bool) -> SessionResult<()> {
        let _ = (id, muted);
        unsupported("calls")
    }

    /// Asks somebody else into a call that is on.
    async fn invite_to_call(&self, id: &str, guest: &Jid) -> SessionResult<()> {
        let _ = (id, guest);
        unsupported("calls with several people")
    }

    /// Turns the camera off or on again in a call with video.
    async fn set_call_camera(&self, id: &str, on: bool) -> SessionResult<()> {
        let _ = (id, on);
        unsupported("video calls")
    }

    /// The pictures of a call with video.
    fn call_video(&self, id: &str) -> Option<client_provider::CallVideo> {
        let _ = id;
        None
    }
}

/// How a session is opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    /// Import the history the phone hands over when it links.
    pub history: bool,
}

/// The sessions of this machine: where they are kept and how one starts.
#[async_trait]
pub trait Sessions: Send + Sync + 'static {
    /// Starts the session kept under `device`, or a new one that has to be
    /// linked when `device` is `None`. What it hears goes to `sink` under
    /// the number this answers.
    async fn open(
        &self,
        device: Option<i32>,
        options: OpenOptions,
        sink: Sink,
    ) -> SessionResult<(i32, Arc<dyn Session>)>;

    /// Forgets a session: its keys are deleted from this machine.
    async fn remove(&self, device: i32) -> SessionResult<()>;
}
