//! The real sessions: whatsapp-rust, speaking WhatsApp's own protocol from
//! this machine.
//!
//! Everything here is translation between the library and
//! [`crate::session`]: its events become [`Inbound`]s, and each thing the
//! provider asks is one or two calls on the library's client. The sessions'
//! keys live in the library's own SQLite store, encrypted under the key the
//! application gives.

use crate::call_audio::{self, AudioLink};
use crate::call_video::{self, VideoLink};
use crate::ids::{chat_id, contact_id};
use crate::mapping::MediaPointer;
use crate::session::{
    CallSignal, HistoryChat, Identity, Inbound, Incoming, Lookup, OnWhatsApp, OpenOptions,
    ReceiptLevel, Session, SessionError, SessionResult, Sessions, Sink, StatusPost, Target,
};
use async_trait::async_trait;
use client_provider::{
    refusal, AccountId, Avatar, AvatarAnswer, BusinessHours, BusinessProfile, ChatChange,
    ContactId, Group, GroupChange, GroupParticipant, GroupRole, JoinRequest, NewGroup,
    ParticipantChange, ParticipantOutcome, Party, PresenceState, StoryAudience, StoryPrivacy,
    Subgroup, SystemEvent, SystemKind, Timestamp,
};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use whatsapp_rust::bot::{Bot, BotHandle};
use whatsapp_rust::client::Client;
use whatsapp_rust::download::{DownloadParams, MediaType};
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::request::IqError;
use whatsapp_rust::send::{EditRequest, SendError, SendOptions, SendRequest};
use whatsapp_rust::store::{SqliteDatabase, SqliteDatabaseConfig};
use whatsapp_rust::types::events::Event;
use whatsapp_rust::upload::UploadOptions;
use whatsapp_rust::voip::CallHandle;
use whatsapp_rust::wacore::iq::contacts::{ProfilePictureLookup, ProfilePictureType};
use whatsapp_rust::wacore::iq::groups::{
    GroupCreateOptions, GroupDescription, GroupParticipantOptions, GroupSubject,
    MembershipApprovalMode, ParticipantChangeResponse, ParticipantType,
};
use whatsapp_rust::wacore::stanza::groups::{GroupNotificationAction, GroupParticipantInfo};
use whatsapp_rust::wacore::types::call::{CallAction, IncomingCall};
use whatsapp_rust::wacore::types::presence::{ChatActivity, PresenceStatus, ReceiptType};
use whatsapp_rust::wacore::types::wire_enums::MemberAddMode;
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::{
    GroupMetadata, Jid, MessageId, MessageRef, MessageSecret, PollRef, PreviousDescription,
    ProfilePictureRequest, ProfilePictureTarget, StatusSendOptions,
};

/// What the phone calls this device in its list of linked devices.
const DEVICE_NAME: &str = "Wuapi";

/// How long a request to WhatsApp may take before it counts as "not now".
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a file may take, either way.
const MEDIA_TIMEOUT: Duration = Duration::from_secs(180);
/// How many chats of the phone's history travel in one piece.
const HISTORY_CHUNK: usize = 40;

// ----- errors ---------------------------------------------------------------

fn iq_error(error: &IqError) -> SessionError {
    match error {
        IqError::ServerError { code, text, .. } => match *code {
            429 => SessionError::SlowDown,
            500..=599 => SessionError::NotNow(format!("WhatsApp answered {code}")),
            401 | 403 => SessionError::refused(
                refusal::NOT_ADMIN,
                "WhatsApp does not allow this number to do that.",
            ),
            404 => {
                SessionError::refused(refusal::NOT_FOUND, "WhatsApp does not know it (any more).")
            }
            _ => SessionError::refused(
                &format!("whatsapp_{code}"),
                format!("WhatsApp refused ({code} {text})."),
            ),
        },
        other => SessionError::NotNow(other.to_string()),
    }
}

fn send_error(error: SendError) -> SessionError {
    match &error {
        SendError::NotLoggedIn => SessionError::NotLinked,
        SendError::Iq(iq) => iq_error(iq),
        SendError::InvalidRequest(_) | SendError::MessageRef(_) | SendError::InvalidSecret(_) => {
            SessionError::refused("invalid", error.to_string())
        }
        SendError::NoRecipientDevice(_) => SessionError::refused(
            refusal::NOT_ON_WHATSAPP,
            "This number has no WhatsApp to receive the message.",
        ),
        SendError::PrimaryDeviceRejected(_) => {
            SessionError::refused("rejected", "The phone refused this message.")
        }
        _ => classify(&error),
    }
}

/// Any other failure of the library: what WhatsApp answered when the
/// failure carries an answer, "not now" when it reads as the connection,
/// and a refusal with the library's words otherwise.
fn classify(error: &(dyn std::error::Error + 'static)) -> SessionError {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = cause {
        if let Some(iq) = current.downcast_ref::<IqError>() {
            return iq_error(iq);
        }
        cause = current.source();
    }
    let words = error.to_string();
    let lower = words.to_lowercase();
    let waiting = [
        "not connected",
        "timed out",
        "timeout",
        "socket",
        "disconnect",
        "channel closed",
    ];
    if waiting.iter().any(|sign| lower.contains(sign)) {
        SessionError::NotNow(words)
    } else if lower.contains("not logged in") {
        SessionError::NotLinked
    } else {
        SessionError::refused("failed", words)
    }
}

fn failed<E: std::error::Error + 'static>(error: E) -> SessionError {
    classify(&error)
}

fn anyhow_failed(error: whatsapp_rust::anyhow::Error) -> SessionError {
    classify(error.as_ref())
}

fn invalid(error: impl std::fmt::Display) -> SessionError {
    SessionError::refused("invalid", error.to_string())
}

/// Bounds a request: one that never answers is "not now", not a hang.
async fn bounded<T>(
    limit: Duration,
    request: impl std::future::Future<Output = SessionResult<T>>,
) -> SessionResult<T> {
    match tokio::time::timeout(limit, request).await {
        Ok(answer) => answer,
        Err(_) => Err(SessionError::NotNow(
            "WhatsApp did not answer in time".into(),
        )),
    }
}

// ----- the store ------------------------------------------------------------

/// The sessions of this machine, in one encrypted file.
pub struct LiveSessions {
    database: SqliteDatabase,
}

impl LiveSessions {
    /// Opens the file the sessions' keys are kept in, encrypted under
    /// `key_hex` (64 hexadecimal digits) when one is given.
    pub async fn open(path: &Path, key_hex: Option<String>) -> Result<Self, String> {
        let mut config = SqliteDatabaseConfig::default();
        if let Some(key) = key_hex.filter(|key| key.chars().all(|c| c.is_ascii_hexdigit())) {
            config = config.with_connection_init(move |connection| {
                use diesel::RunQueryDsl as _;
                // Hex digits only, so it is safe in SQL text, which is the
                // only way SQLCipher takes a raw key.
                diesel::sql_query(format!("PRAGMA key = \"x'{key}'\";")).execute(connection)?;
                // A wrong key shows here and not at the first write.
                diesel::sql_query("SELECT count(*) FROM sqlite_master;").execute(connection)?;
                Ok(())
            });
        }
        let url = path.to_string_lossy().into_owned();
        let database = SqliteDatabase::open(&url, config)
            .await
            .map_err(|error| format!("cannot open the sessions' keys: {error}"))?;
        Ok(Self { database })
    }
}

#[async_trait]
impl Sessions for LiveSessions {
    async fn open(
        &self,
        device: Option<i32>,
        options: OpenOptions,
        sink: Sink,
    ) -> SessionResult<(i32, Arc<dyn Session>)> {
        let held = match device {
            Some(id) => self.database.device_exists(id).await.unwrap_or(false),
            None => false,
        };
        let store = match device {
            Some(id) if held => self.database.store(id),
            _ => self.database.create_device().await.map_err(|error| {
                SessionError::NotNow(format!("cannot keep a new session: {error}"))
            })?,
        };
        let number = store.device_id();
        let calls = Arc::new(Calls::default());
        let hearing = Hearing {
            device: number,
            sink: sink.clone(),
            calls: calls.clone(),
        };
        // How the phone lists this device under "Linked devices": by its
        // name, as a desktop application. Left to the library it is a
        // device of unknown kind ("Other device"). It says what it is and
        // does not pass for a browser. Only read when a number is linked.
        let props = whatsapp_rust::wacore::store::DevicePropsOverride::new()
            .with_os(DEVICE_NAME)
            .with_platform_type(
                whatsapp_rust::waproto::whatsapp::device_props::PlatformType::DESKTOP,
            );
        let mut builder = Bot::builder()
            .with_backend(store)
            .with_device_props(props)
            .on_event(move |event, client| {
                let hearing = hearing.clone();
                async move { hearing.hear(event, client).await }
            });
        if !options.history {
            builder = builder.skip_history_sync();
        }
        let bot = builder
            .build()
            .await
            .map_err(|error| SessionError::NotNow(format!("the session did not start: {error}")))?;
        let client = bot.client();
        let handle = bot.spawn();
        let session = LiveSession {
            client,
            handle: tokio::sync::Mutex::new(Some(handle)),
            hearing: Hearing {
                device: number,
                sink,
                calls,
            },
        };
        Ok((number, Arc::new(session)))
    }

    async fn remove(&self, device: i32) -> SessionResult<()> {
        if !self.database.device_exists(device).await.unwrap_or(false) {
            return Ok(());
        }
        self.database.remove_device(device).await.map_err(|error| {
            SessionError::NotNow(format!("cannot delete the session's keys: {error}"))
        })
    }
}

// ----- what is heard --------------------------------------------------------

/// A call that is on.
struct ActiveCall {
    handle: Arc<CallHandle>,
    audio: AudioLink,
    /// The camera and the pictures, in a call with video.
    video: Option<Arc<VideoLink>>,
}

/// Opens the camera chosen for calls. With `wait` it answers once the
/// camera gave its first picture, so it is kept off the threads that
/// carry everything else.
async fn open_video(wait: bool) -> SessionResult<(Arc<VideoLink>, VideoUnits)> {
    let camera = call_audio::chosen().camera;
    let opened = tokio::task::spawn_blocking(move || call_video::open(camera.as_deref(), wait))
        .await
        .map_err(|error| SessionError::refused("no_video", error.to_string()))?;
    let (link, units) = opened.map_err(|reason| SessionError::refused("no_video", reason))?;
    Ok((Arc::new(link), units))
}

/// The camera's encoded pictures, as the library takes them.
type VideoUnits = whatsapp_rust::async_channel::Receiver<Vec<u8>>;

/// A call that rings here.
struct RingingCall {
    offer: IncomingCall,
    /// With video, the camera, open while it rings so that the user sees
    /// themselves before answering. Answering goes on with it; anything
    /// else lets it go.
    preview: Option<(Arc<VideoLink>, VideoUnits)>,
    /// The camera chosen when the preview was opened.
    camera: Option<String>,
}

#[derive(Default)]
struct Calls {
    ringing: Mutex<HashMap<String, RingingCall>>,
    /// Held while a preview is opened on another camera: one at a time.
    switching: Mutex<()>,
    active: Mutex<HashMap<String, ActiveCall>>,
}

impl Calls {
    fn ringing(&self) -> std::sync::MutexGuard<'_, HashMap<String, RingingCall>> {
        self.ringing
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn active(&self) -> std::sync::MutexGuard<'_, HashMap<String, ActiveCall>> {
        self.active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `camera`, for a call with video that rings here, opened without
    /// waiting for it: its pictures show when it is ready. Not while
    /// another call films, which has the camera.
    fn preview_camera(&self, camera: &Option<String>) -> Option<(Arc<VideoLink>, VideoUnits)> {
        let filming = self.active().values().any(|call| call.video.is_some());
        if filming || !call_video::available() {
            return None;
        }
        match call_video::open(camera.as_deref(), false) {
            Ok((link, units)) => Some((Arc::new(link), units)),
            Err(reason) => {
                tracing::warn!(%reason, "no camera to show while a call rings");
                None
            }
        }
    }

    /// The calls with video that ring here show the camera chosen now:
    /// one whose preview is of another camera opens it again. Off the
    /// caller's thread, as a camera takes a moment to let go.
    fn follow_camera(self: &Arc<Self>) {
        let ids: Vec<String> = self
            .ringing()
            .iter()
            .filter(|(_, held)| {
                matches!(held.offer.action, CallAction::Offer { is_video: true, .. })
            })
            .map(|(id, _)| id.clone())
            .collect();
        if ids.is_empty() {
            return;
        }
        let calls = self.clone();
        let spawned = std::thread::Builder::new()
            .name("call-preview".into())
            .spawn(move || {
                let _one = calls
                    .switching
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                for id in ids {
                    calls.preview_on_chosen(&id);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "the camera of a call that rings could not be changed");
        }
    }

    /// Opens the preview of `id` on the camera chosen now, until it is on
    /// it: the choice can change again while a camera opens.
    fn preview_on_chosen(&self, id: &str) {
        loop {
            let wanted = call_audio::chosen().camera;
            let old = {
                let mut ringing = self.ringing();
                let Some(held) = ringing.get_mut(id) else {
                    return;
                };
                if held.camera == wanted {
                    return;
                }
                held.camera = wanted.clone();
                held.preview.take()
            };
            // The old camera goes before the new one opens: a device is
            // filmed by one program at a time.
            drop(old);
            let opened = self.preview_camera(&wanted);
            let mut ringing = self.ringing();
            match ringing.get_mut(id) {
                Some(held) if held.camera == wanted => held.preview = opened,
                // Answered, or over, or another camera chosen meanwhile.
                _ => {
                    drop(ringing);
                    drop(opened);
                }
            }
        }
    }
}

/// Where a session's events go.
#[derive(Clone)]
struct Hearing {
    device: i32,
    sink: Sink,
    calls: Arc<Calls>,
}

fn millis(at: whatsapp_rust::chrono::DateTime<whatsapp_rust::chrono::Utc>) -> i64 {
    at.timestamp_millis()
}

fn parse(address: &Option<String>) -> Option<Jid> {
    address
        .as_deref()
        .filter(|text| !text.is_empty())?
        .parse()
        .ok()
}

/// A time WhatsApp gives in seconds or in milliseconds, in milliseconds.
fn to_millis(value: i64) -> i64 {
    if value.abs() < 100_000_000_000 {
        value.saturating_mul(1000)
    } else {
        value
    }
}

fn party(info: &GroupParticipantInfo) -> Party {
    Party {
        id: contact_id(info.phone_number.as_ref().unwrap_or(&info.jid)),
        name: info.display_name.clone(),
    }
}

impl Hearing {
    fn tell(&self, inbound: Inbound) {
        let _ = self.sink.send((self.device, inbound));
    }

    fn call(&self, id: &str, peer: &Jid, signal: CallSignal, at_ms: Option<i64>) {
        self.tell(Inbound::Call {
            id: id.to_owned(),
            peer: peer.to_non_ad(),
            signal,
            timestamp_ms: at_ms.unwrap_or_else(|| Timestamp::now().as_millis()),
        });
    }

    async fn hear(&self, event: Arc<Event>, client: Arc<Client>) {
        match &*event {
            Event::PairingQrCode(qr) => self.tell(Inbound::Qr(qr.code.clone())),
            Event::PairSuccess(_) => self.tell(Inbound::Paired),
            Event::PairError(error) => self.tell(Inbound::LinkFailed(format!(
                "The phone could not finish linking ({}).",
                error.error
            ))),
            Event::PairingQrCodesExhausted(_) => self.tell(Inbound::LinkFailed(
                "Nobody scanned the code in time. Start again to get a new one.".into(),
            )),
            Event::Connected(_) => self.tell(Inbound::Connected(identity(&client))),
            Event::Disconnected(_) => self.tell(Inbound::Reconnecting),
            Event::LoggedOut(_) => self.tell(Inbound::LoggedOut(None)),
            Event::StreamReplaced(_) => self.tell(Inbound::Disconnected(Some(
                "This number's session was opened somewhere else.".into(),
            ))),
            Event::TemporaryBan(ban) => self.tell(Inbound::Disconnected(Some(format!(
                "WhatsApp restricted this number for {} minutes.",
                ban.expire.num_minutes().max(1)
            )))),
            Event::ClientOutdated(_) => self.tell(Inbound::Disconnected(Some(
                "This version is too old for WhatsApp. Update the application.".into(),
            ))),
            Event::Messages(batch) => {
                let messages = batch
                    .messages
                    .iter()
                    .map(|inbound| {
                        let source = &inbound.info.source;
                        let direct = !source.is_group;
                        Incoming {
                            chat: source.chat.to_non_ad(),
                            chat_alt: direct
                                .then(|| {
                                    if source.is_from_me {
                                        source.recipient_alt.clone()
                                    } else {
                                        source.sender_alt.clone()
                                    }
                                })
                                .flatten()
                                .map(Jid::into_non_ad),
                            sender: source.sender.to_non_ad(),
                            sender_alt: source.sender_alt.clone().map(Jid::into_non_ad),
                            from_me: source.is_from_me,
                            id: inbound.info.id.as_str().to_owned(),
                            push_name: Some(inbound.info.push_name.to_string())
                                .filter(|name| !name.is_empty()),
                            timestamp_ms: millis(inbound.info.timestamp),
                            message: inbound.message.clone(),
                            status: None,
                            starred: false,
                        }
                    })
                    .collect();
                self.tell(Inbound::Messages(messages));
            }
            Event::Receipt(receipt) => {
                let chat = receipt.source.chat.to_non_ad();
                let level = match receipt.r#type {
                    ReceiptType::Delivered => ReceiptLevel::Delivered,
                    ReceiptType::Read | ReceiptType::Played => ReceiptLevel::Read,
                    ReceiptType::ReadSelf => {
                        // Read on another of the account's devices.
                        self.tell(Inbound::MarkedRead {
                            chat,
                            read: true,
                            at_ms: millis(receipt.timestamp),
                        });
                        return;
                    }
                    _ => return,
                };
                self.tell(Inbound::Receipt {
                    chat,
                    from: receipt.source.sender.to_non_ad(),
                    ids: receipt
                        .message_ids
                        .iter()
                        .map(|id| id.as_str().to_owned())
                        .collect(),
                    level,
                    timestamp_ms: millis(receipt.timestamp),
                });
            }
            Event::ChatPresence(update) => self.tell(Inbound::Activity {
                chat: update.source.chat.to_non_ad(),
                from: update.source.sender.to_non_ad(),
                state: match update.state {
                    ChatActivity::Typing => PresenceState::Typing,
                    ChatActivity::RecordingAudio => PresenceState::Recording,
                    _ => PresenceState::Idle,
                },
            }),
            Event::Presence(update) => self.tell(Inbound::Activity {
                chat: update.from.to_non_ad(),
                from: update.from.to_non_ad(),
                state: match update.status {
                    PresenceStatus::Available => PresenceState::Online,
                    _ => PresenceState::Idle,
                },
            }),
            Event::PictureUpdate(update) => self.tell(Inbound::Picture {
                jid: update.jid.to_non_ad(),
                picture_id: update.picture_id.clone().filter(|_| !update.removed),
            }),
            Event::ContactUpdate(update) => {
                let action = &update.action;
                let alt = if update.jid.server.is_lid_family() {
                    parse(&action.pn_jid)
                } else {
                    parse(&action.lid_jid)
                };
                self.tell(Inbound::Contact {
                    jid: update.jid.to_non_ad(),
                    alt,
                    saved_name: action
                        .full_name
                        .clone()
                        .or_else(|| action.first_name.clone())
                        .filter(|name| !name.trim().is_empty()),
                    username: action.username.clone().filter(|name| !name.is_empty()),
                });
            }
            Event::PinUpdate(update) => self.tell(Inbound::Pinned {
                chat: update.jid.to_non_ad(),
                pinned: update.action.pinned.unwrap_or(false),
                at_ms: millis(update.action_timestamp.unwrap_or(update.timestamp)),
            }),
            Event::MuteUpdate(update) => {
                let muted = update.action.muted.unwrap_or(false);
                let until = update.action.mute_end_timestamp.filter(|end| *end > 0);
                self.tell(Inbound::Muted {
                    chat: update.jid.to_non_ad(),
                    until_ms: muted.then(|| until.map(to_millis).unwrap_or(-1)),
                });
            }
            Event::ArchiveUpdate(update) => self.tell(Inbound::Archived {
                chat: update.jid.to_non_ad(),
                archived: update.action.archived.unwrap_or(false),
            }),
            Event::MarkChatAsReadUpdate(update) => self.tell(Inbound::MarkedRead {
                chat: update.jid.to_non_ad(),
                read: update.action.read.unwrap_or(true),
                at_ms: millis(update.timestamp),
            }),
            Event::DeleteChatUpdate(update) => self.tell(Inbound::ChatDeleted {
                chat: update.jid.to_non_ad(),
            }),
            Event::StarUpdate(update) => self.tell(Inbound::Starred {
                chat: update.chat_jid.to_non_ad(),
                id: update.message_id.clone(),
                starred: update.action.starred.unwrap_or(false),
            }),
            Event::DeleteMessageForMeUpdate(update) => self.tell(Inbound::DeletedForMe {
                chat: update.chat_jid.to_non_ad(),
                id: update.message_id.clone(),
            }),
            Event::UserStatusMuteUpdate(update) => self.tell(Inbound::StoryMuted {
                author: update.jid.to_non_ad(),
                muted: update.muted,
            }),
            Event::StatusPrivacyUpdate(update) => {
                if let Some(privacy) = story_audience(&update.action) {
                    self.tell(Inbound::StoryAudience(privacy));
                }
            }
            Event::GroupUpdate(update) => self.group_changed(update),
            Event::HistorySync(_) => {
                // Unpacking a history is work for a thread, not for the
                // one that reads the socket.
                let (hearing, event) = (self.clone(), event.clone());
                let _ = tokio::task::spawn_blocking(move || {
                    if let Event::HistorySync(history) = &*event {
                        hearing.history(history);
                    }
                })
                .await;
            }
            Event::IncomingCall(incoming) => self.signalled(incoming),
            Event::MissedCall(missed) => {
                self.calls.ringing().remove(&missed.call_id);
                self.call(
                    &missed.call_id,
                    &missed.from,
                    CallSignal::Ended {
                        missed: true,
                        reason: None,
                    },
                    Some(millis(missed.timestamp)),
                );
            }
            Event::CallEndedElsewhere(ended) => {
                self.calls.ringing().remove(&ended.call_id);
                self.call(
                    &ended.call_id,
                    &ended.from,
                    CallSignal::Ended {
                        missed: false,
                        reason: Some("Answered on another device.".into()),
                    },
                    Some(millis(ended.timestamp)),
                );
            }
            _ => {}
        }
    }

    /// What WhatsApp signals about a call.
    fn signalled(&self, incoming: &IncomingCall) {
        let at = incoming.timestamp.map(millis);
        match &incoming.action {
            CallAction::Offer {
                call_id,
                call_creator,
                is_video,
                group_jid,
                ..
            } => {
                // A call that was offered while this device was away is
                // over by the time it is heard.
                if incoming.offline {
                    return;
                }
                // An offer heard twice keeps the camera it opened.
                let heard = self.calls.ringing().remove(call_id);
                let camera = call_audio::chosen().camera;
                let (preview, camera) = match heard {
                    Some(held) => (held.preview, held.camera),
                    None if *is_video => (self.calls.preview_camera(&camera), camera),
                    None => (None, camera),
                };
                self.calls.ringing().insert(
                    call_id.clone(),
                    RingingCall {
                        offer: incoming.clone(),
                        preview,
                        camera,
                    },
                );
                self.call(
                    call_id,
                    call_creator,
                    CallSignal::Ringing {
                        video: *is_video,
                        group: group_jid.is_some(),
                    },
                    at,
                );
            }
            CallAction::Accept { call_id, .. } => {
                self.call(call_id, &incoming.from, CallSignal::Connected, at);
            }
            CallAction::Reject {
                call_id, reason, ..
            }
            | CallAction::Terminate {
                call_id, reason, ..
            } => {
                let rang = self.calls.ringing().remove(call_id).is_some();
                let on = self.calls.active().remove(call_id).is_some();
                self.call(
                    call_id,
                    &incoming.from,
                    CallSignal::Ended {
                        missed: rang && !on,
                        reason: reason.clone(),
                    },
                    at,
                );
            }
            _ => {}
        }
    }

    fn group_changed(&self, update: &whatsapp_rust::wacore::types::events::GroupUpdate) {
        let actor = update
            .participant_pn
            .as_ref()
            .or(update.participant.as_ref())
            .map(|jid| Party {
                id: contact_id(jid),
                name: None,
            });
        let at_ms = millis(update.timestamp);
        let people = |participants: &[GroupParticipantInfo]| {
            participants.iter().map(party).collect::<Vec<_>>()
        };
        let mut subject = None;
        let mut community = None;
        let mut created_ms = None;
        let line = match &*update.action {
            // A group just made, here, on the phone or by whoever added the
            // account: its chat is listed before anybody writes in it. A
            // community has no chat; the groups made with it do.
            GroupNotificationAction::Create { raw } => {
                if let Some(group) = raw
                    .get_optional_child("group")
                    .filter(|group| group.get_optional_child("parent").is_none())
                {
                    subject = group
                        .attrs()
                        .optional_string("subject")
                        .map(|subject| subject.into_owned());
                    community = group
                        .get_optional_child("linked_parent")
                        .and_then(|linked| linked.attrs().optional_jid("jid"));
                    created_ms = Some(at_ms);
                }
                None
            }
            GroupNotificationAction::Add { participants, .. } => {
                let targets = people(participants);
                // Somebody who added themselves joined.
                let joined = actor
                    .as_ref()
                    .is_some_and(|actor| targets.iter().any(|t| t.id == actor.id));
                Some((
                    if joined {
                        SystemKind::Joined
                    } else {
                        SystemKind::Added
                    },
                    targets,
                    None,
                ))
            }
            GroupNotificationAction::Remove { participants, .. } => {
                let targets = people(participants);
                let left = actor
                    .as_ref()
                    .is_some_and(|actor| targets.iter().any(|t| t.id == actor.id));
                Some((
                    if left {
                        SystemKind::Left
                    } else {
                        SystemKind::Removed
                    },
                    targets,
                    None,
                ))
            }
            GroupNotificationAction::Promote { participants } => {
                Some((SystemKind::Promoted, people(participants), None))
            }
            GroupNotificationAction::Demote { participants } => {
                Some((SystemKind::Demoted, people(participants), None))
            }
            GroupNotificationAction::Subject { subject: new, .. } => {
                subject = Some(new.clone());
                Some((SystemKind::SubjectChanged, Vec::new(), Some(new.clone())))
            }
            GroupNotificationAction::Link { .. } | GroupNotificationAction::Unlink { .. } => {
                community = Some(update.group_jid.to_non_ad());
                None
            }
            _ => None,
        };
        let notice = line.map(|(kind, targets, detail)| {
            let id = update
                .notification_id
                .clone()
                .map(|id| format!("notice-{id}-{}", update.action_index))
                .unwrap_or_else(|| format!("notice-{at_ms}-{}", update.action_index));
            let joined = matches!(kind, SystemKind::Joined | SystemKind::Left);
            let event = SystemEvent {
                kind,
                actor: if joined { None } else { actor.clone() },
                targets,
                detail,
            };
            (id, at_ms, event)
        });
        self.tell(Inbound::GroupChanged {
            group: update.group_jid.to_non_ad(),
            notice,
            subject,
            community,
            created_ms,
        });
    }

    /// A piece of the account's history, unpacked and passed on a few
    /// chats at a time.
    fn history(&self, history: &whatsapp_rust::wacore::types::events::LazyHistorySync) {
        let asked = history.peer_data_request_session_id().is_some();
        let mut stream = history.stream();
        let mut chats = Vec::new();
        let send = |chats: &mut Vec<HistoryChat>, last: bool| {
            if chats.is_empty() && !last {
                return;
            }
            self.tell(Inbound::History {
                chats: std::mem::take(chats),
                names: Vec::new(),
                aliases: Vec::new(),
                asked: asked && last,
            });
        };
        loop {
            match stream.next_conversation() {
                Ok(Some(conversation)) => {
                    if let Some(chat) = history_chat(&conversation) {
                        chats.push(chat);
                    }
                    if chats.len() >= HISTORY_CHUNK {
                        send(&mut chats, false);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!("a piece of history could not be read: {error}");
                    break;
                }
            }
        }
        let mut names = Vec::new();
        let mut aliases = Vec::new();
        if let Ok(rest) = stream.remainder() {
            for name in &rest.pushnames {
                if let (Some(jid), Some(name)) = (parse(&name.id), name.pushname.clone()) {
                    if !name.trim().is_empty() {
                        names.push((jid.into_non_ad(), name));
                    }
                }
            }
            for mapping in &rest.phone_number_to_lid_mappings {
                if let (Some(pn), Some(lid)) = (parse(&mapping.pn_jid), parse(&mapping.lid_jid)) {
                    aliases.push((lid.into_non_ad(), pn.into_non_ad()));
                }
            }
            let stories: Vec<Incoming> = rest
                .status_v3_messages
                .iter()
                .flat_map(|info| history_messages(&Jid::status_broadcast(), info))
                .collect();
            if !stories.is_empty() {
                chats.push(HistoryChat {
                    chat: Jid::status_broadcast(),
                    chat_alt: None,
                    name: None,
                    unread: None,
                    marked_unread: false,
                    archived: false,
                    pinned_at_ms: None,
                    muted_until_ms: None,
                    messages: stories,
                });
            }
        }
        if !names.is_empty() || !aliases.is_empty() {
            self.tell(Inbound::History {
                chats: Vec::new(),
                names,
                aliases,
                asked: false,
            });
        }
        send(&mut chats, true);
    }
}

fn identity(client: &Client) -> Identity {
    Identity {
        pn: client.pn().map(Jid::into_non_ad),
        lid: client.lid().map(Jid::into_non_ad),
        push_name: client.push_name(),
    }
}

fn story_audience(action: &wa::sync_action_value::StatusPrivacyAction) -> Option<StoryPrivacy> {
    use wa::sync_action_value::status_privacy_action::StatusDistributionMode as Mode;
    let listed: Vec<ContactId> = action
        .user_jid
        .iter()
        .filter_map(|address| address.parse::<Jid>().ok())
        .map(|jid| contact_id(&jid))
        .collect();
    let mode = action.mode.as_ref().and_then(|mode| mode.as_known())?;
    Some(match mode {
        Mode::DENY_LIST => StoryPrivacy {
            audience: StoryAudience::ContactsExcept,
            except: listed,
            only: Vec::new(),
        },
        Mode::ALLOW_LIST => StoryPrivacy {
            audience: StoryAudience::OnlyShareWith,
            except: Vec::new(),
            only: listed,
        },
        _ => StoryPrivacy::default(),
    })
}

fn history_chat(conversation: &wa::Conversation) -> Option<HistoryChat> {
    let chat: Jid = conversation.id.parse().ok()?;
    let chat = chat.into_non_ad();
    let alt = if chat.server.is_lid_family() {
        parse(&conversation.pn_jid)
    } else {
        parse(&conversation.lid_jid)
    };
    let messages = conversation
        .messages
        .iter()
        .filter_map(|message| message.message.as_option())
        .flat_map(|info| history_messages(&chat, info))
        .collect();
    Some(HistoryChat {
        chat,
        chat_alt: alt.map(Jid::into_non_ad),
        name: conversation
            .name
            .clone()
            .or_else(|| conversation.display_name.clone())
            .filter(|name| !name.trim().is_empty()),
        unread: conversation.unread_count,
        marked_unread: conversation.marked_as_unread.unwrap_or(false),
        archived: conversation.archived.unwrap_or(false),
        pinned_at_ms: conversation
            .pinned
            .filter(|pinned| *pinned > 0)
            .map(|pinned| to_millis(i64::from(pinned))),
        muted_until_ms: conversation
            .mute_end_time
            .filter(|end| *end > 0)
            .map(|end| to_millis(end as i64)),
        messages,
    })
}

/// A message of the phone's history, and the reactions it carries, as
/// messages.
fn history_messages(chat: &Jid, info: &wa::WebMessageInfo) -> Vec<Incoming> {
    use wa::web_message_info::Status;
    let Some(key) = info.key.as_option() else {
        return Vec::new();
    };
    let (Some(id), Some(message)) = (key.id.clone(), info.message.as_option()) else {
        return Vec::new();
    };
    let from_me = key.from_me.unwrap_or(false);
    let author = parse(&key.participant)
        .or_else(|| parse(&info.participant))
        .map(Jid::into_non_ad)
        .unwrap_or_else(|| chat.clone());
    let timestamp_ms = info
        .message_timestamp
        .map(|at| to_millis(at as i64))
        .unwrap_or(0);
    let status = match info.status {
        Some(Status::READ | Status::PLAYED) => Some(crate::convert::READ),
        Some(Status::DELIVERY_ACK) => Some(crate::convert::DELIVERED),
        Some(Status::SERVER_ACK) => Some(crate::convert::SENT),
        _ => None,
    };
    let mut messages = vec![Incoming {
        chat: chat.clone(),
        chat_alt: None,
        sender: author,
        sender_alt: None,
        from_me,
        id: id.clone(),
        push_name: info.push_name.clone().filter(|name| !name.is_empty()),
        timestamp_ms,
        message: Arc::new(message.clone()),
        status,
        starred: info.starred.unwrap_or(false),
    }];
    for reaction in &info.reactions {
        let Some(reaction_key) = reaction.key.as_option() else {
            continue;
        };
        let Some(reaction_id) = reaction_key.id.clone() else {
            continue;
        };
        let reacted = wa::Message {
            reaction_message: whatsapp_rust::buffa::MessageField::some(
                wa::message::ReactionMessage {
                    key: info.key.clone(),
                    text: reaction.text.clone(),
                    sender_timestamp_ms: reaction.sender_timestamp_ms,
                    ..Default::default()
                },
            ),
            ..Default::default()
        };
        messages.push(Incoming {
            chat: chat.clone(),
            chat_alt: None,
            sender: parse(&reaction_key.participant)
                .map(Jid::into_non_ad)
                .unwrap_or_else(|| chat.clone()),
            sender_alt: None,
            from_me: reaction_key.from_me.unwrap_or(false),
            id: reaction_id,
            push_name: None,
            timestamp_ms: reaction.sender_timestamp_ms.unwrap_or(timestamp_ms),
            message: Arc::new(reacted),
            status: None,
            starred: false,
        });
    }
    messages
}

// ----- groups ---------------------------------------------------------------

fn to_group(metadata: &GroupMetadata) -> Group {
    let seconds = |at: Option<u64>| {
        at.filter(|at| *at > 0)
            .map(|at| Timestamp::from_millis(to_millis(at as i64)))
    };
    Group {
        id: chat_id(&metadata.id),
        // The provider knows which account asked.
        account_id: AccountId::new(""),
        subject: metadata.subject.clone().unwrap_or_default(),
        description: metadata.description.clone().filter(|text| !text.is_empty()),
        owner: metadata
            .creator_pn
            .as_ref()
            .or(metadata.creator.as_ref())
            .map(contact_id),
        created_at: seconds(metadata.creation_time),
        community: metadata.is_parent_group,
        community_id: metadata.parent_group_jid.as_ref().map(chat_id),
        announcements: metadata.is_default_sub_group,
        announce: metadata.is_announcement,
        locked: metadata.is_locked,
        join_approval: Some(metadata.membership_approval),
        members_can_add: metadata
            .member_add_mode
            .as_ref()
            .map(|mode| matches!(mode, MemberAddMode::AllMemberAdd)),
        participants: metadata
            .participants
            .iter()
            .map(|participant| GroupParticipant {
                contact: contact_id(
                    participant
                        .phone_number
                        .as_ref()
                        .unwrap_or(&participant.jid),
                ),
                name: None,
                role: match participant.participant_type {
                    ParticipantType::SuperAdmin => GroupRole::Owner,
                    ParticipantType::Admin => GroupRole::Admin,
                    _ => GroupRole::Member,
                },
            })
            .collect(),
        subgroups: Vec::new(),
    }
}

/// WhatsApp answers a change to participants one by one, each with a
/// status code where it refused.
fn outcomes(answers: Vec<ParticipantChangeResponse>) -> Vec<ParticipantOutcome> {
    answers
        .into_iter()
        .map(|answer| {
            let code = answer
                .error
                .or(answer.status.filter(|status| status != "200"));
            let invited = answer.add_request.is_some();
            ParticipantOutcome {
                contact: contact_id(answer.phone_number.as_ref().unwrap_or(&answer.jid)),
                error: code.filter(|_| !invited).map(|code| {
                    match code.as_str() {
                        "401" => refusal::NOT_ADMIN,
                        "403" => refusal::PRIVACY,
                        "404" => refusal::NOT_FOUND,
                        "406" => refusal::NOT_MEMBER,
                        "408" => refusal::PRIVACY,
                        "409" => refusal::ALREADY_MEMBER,
                        _ => return format!("whatsapp_{code}"),
                    }
                    .to_owned()
                }),
                invite_code: answer.add_request.map(|request| request.code),
            }
        })
        .collect()
}

// ----- the session ----------------------------------------------------------

struct LiveSession {
    client: Arc<Client>,
    handle: tokio::sync::Mutex<Option<BotHandle>>,
    hearing: Hearing,
}

fn message_id(id: &str) -> SessionResult<MessageId> {
    MessageId::new(id).map_err(invalid)
}

/// A message as the library addresses one.
fn reference(target: &Target) -> SessionResult<MessageRef<'_>> {
    MessageRef::new(
        &target.chat,
        message_id(&target.id)?,
        target.sender.as_ref(),
        target.from_me,
    )
    .map_err(invalid)
}

fn media_type(class: &str) -> MediaType {
    match class {
        "image" => MediaType::Image,
        "video" => MediaType::Video,
        "audio" => MediaType::Audio,
        "sticker" => MediaType::Sticker,
        _ => MediaType::Document,
    }
}

impl LiveSession {
    /// The range WhatsApp wants named when a chat is archived or marked:
    /// the chat's newest message.
    fn range(
        &self,
        last: Option<&Target>,
    ) -> Option<wa::sync_action_value::SyncActionMessageRange> {
        let last = last?;
        let reference = reference(last).ok()?;
        let seconds = last.timestamp_ms / 1000;
        Some(whatsapp_rust::message_range(
            seconds,
            None,
            [(reference, seconds)],
        ))
    }

    /// Has the other side asked for a picture to start from whenever
    /// theirs cannot be decoded: one that was lost on the way breaks all
    /// that follow it.
    fn ask_when_stuck(link: &Option<Arc<VideoLink>>, handle: &Arc<CallHandle>) {
        if let Some(link) = link {
            let handle = handle.clone();
            let asks = handle.clone();
            link.when_stuck(move || {
                asks.request_peer_keyframe(whatsapp_rust::voip::KeyframeUrgency::Immediate);
            });
            // What the call's media counted, while the other side's video
            // is awaited: whether their packets arrive and whether they
            // were asked for a picture, for when the video is late.
            let (counted, screen) = (handle.clone(), link.pictures.remote.clone());
            tokio::spawn(async move {
                // Long enough to ring and be answered; a line only when
                // something moved.
                let mut last = (0, 0);
                for _ in 0..180 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let stats = counted.media_stats();
                    let shown = screen.newest().1.is_some();
                    let now = (stats.rtp_received, stats.peer_keyframe_requests);
                    if now == last && !shown {
                        continue;
                    }
                    last = now;
                    tracing::info!(
                        shown,
                        rtp = stats.rtp_received,
                        asked = stats.peer_keyframe_requests,
                        unprotect_failed = stats.srtp_unprotect_failed,
                        unexpected = stats.rtp_payload_type_unexpected,
                        pipe_dropped = stats.inbound_pipe_dropped,
                        sink_dropped = stats.video_sink_dropped,
                        "a video call's media"
                    );
                    if shown {
                        break;
                    }
                }
            });
        }
    }

    /// Ends a call that is on when the other side hangs up or the media
    /// fails, and says so.
    fn watch_call(&self, id: String, peer: Jid, handle: Arc<CallHandle>) {
        let hearing = self.hearing.clone();
        tokio::spawn(async move {
            handle.wait_ended().await;
            if hearing.calls.active().remove(&id).is_some() {
                hearing.call(
                    &id,
                    &peer,
                    CallSignal::Ended {
                        missed: false,
                        reason: None,
                    },
                    None,
                );
            }
        });
    }
}

#[async_trait]
impl Session for LiveSession {
    fn identity(&self) -> Identity {
        identity(&self.client)
    }

    async fn send(&self, to: &Jid, message: wa::Message, id: &str) -> SessionResult<()> {
        let options = SendOptions::default().with_message_id(message_id(id)?);
        let request = SendRequest::new(to, message).with_options(options);
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .send(request)
                .await
                .map(|_| ())
                .map_err(send_error)
        })
        .await
    }

    async fn mark_read(
        &self,
        chat: &Jid,
        sender: Option<&Jid>,
        ids: &[String],
    ) -> SessionResult<()> {
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .mark_as_read(chat, sender, &ids)
                .await
                .map_err(anyhow_failed)
        })
        .await
    }

    async fn download(&self, pointer: &MediaPointer) -> SessionResult<Vec<u8>> {
        let params = DownloadParams::encrypted(
            pointer.direct_path.clone(),
            &pointer.media_key,
            &pointer.file_sha256,
            &pointer.file_enc_sha256,
            pointer.file_length,
            media_type(&pointer.class),
        )
        .map_err(|error| {
            SessionError::refused(
                refusal::FORWARD_FILE_GONE,
                format!("This file cannot be fetched ({error})."),
            )
        })?;
        bounded(MEDIA_TIMEOUT, async {
            self.client.download(&params).await.map_err(|error| {
                let words = error.to_string();
                // WhatsApp keeps a file for a few weeks; after that its
                // address answers 404 or 410.
                if words.contains("404") || words.contains("410") {
                    SessionError::refused(
                        refusal::FORWARD_FILE_GONE,
                        "WhatsApp no longer has this file. Ask for it to be sent again.",
                    )
                } else {
                    failed(error)
                }
            })
        })
        .await
    }

    async fn upload(&self, bytes: Vec<u8>, class: &str) -> SessionResult<MediaPointer> {
        let class = class.to_owned();
        bounded(MEDIA_TIMEOUT, async {
            let uploaded = self
                .client
                .upload(bytes, media_type(&class), UploadOptions::new())
                .await
                .map_err(anyhow_failed)?;
            Ok(MediaPointer {
                direct_path: uploaded.direct_path,
                media_key: uploaded.media_key.to_vec(),
                file_sha256: uploaded.file_sha256.to_vec(),
                file_enc_sha256: uploaded.file_enc_sha256.to_vec(),
                file_length: uploaded.file_length,
                class,
                mime_type: None,
                url: Some(uploaded.url),
                key_timestamp: Some(uploaded.media_key_timestamp),
            })
        })
        .await
    }

    async fn request_history(&self, oldest: &Target, count: u32) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .fetch_message_history(
                    &oldest.chat,
                    &oldest.id,
                    oldest.from_me,
                    oldest.timestamp_ms,
                    count as i32,
                )
                .await
                .map(|_| ())
                .map_err(anyhow_failed)
        })
        .await
    }

    async fn pair_code(&self, phone: &str) -> SessionResult<String> {
        let options = PairCodeOptions {
            phone_number: phone.to_owned(),
            ..Default::default()
        };
        bounded(REQUEST_TIMEOUT, async {
            // A code is asked for over the socket, which a session that
            // just started is still opening.
            self.client
                .wait_for_socket_ready(REQUEST_TIMEOUT / 2)
                .await
                .map_err(failed)?;
            self.client.pair_with_code(options).await.map_err(failed)
        })
        .await
    }

    async fn cancel_pair_code(&self) -> SessionResult<()> {
        self.client.cancel_pair_code().await;
        Ok(())
    }

    async fn reconnect(&self) {
        self.client.reconnect().await;
    }

    async fn logout(&self) -> SessionResult<()> {
        let _ = tokio::time::timeout(REQUEST_TIMEOUT, self.client.logout()).await;
        Ok(())
    }

    async fn shutdown(&self) {
        if let Some(handle) = self.handle.lock().await.take() {
            let _ = tokio::time::timeout(REQUEST_TIMEOUT, handle.shutdown()).await;
        }
    }

    // ----- messages -------------------------------------------------------

    async fn react(&self, target: &Target, emoji: &str) -> SessionResult<String> {
        let reference = reference(target)?;
        bounded(REQUEST_TIMEOUT, async {
            let sent = self
                .client
                .send_reaction(&reference, emoji)
                .await
                .map_err(send_error)?;
            Ok(sent.message_id.as_str().to_owned())
        })
        .await
    }

    async fn edit(&self, target: &Target, replacement: wa::Message) -> SessionResult<()> {
        let request = EditRequest::new(reference(target)?, replacement);
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .edit_message(request)
                .await
                .map(|_| ())
                .map_err(send_error)
        })
        .await
    }

    async fn revoke(&self, target: &Target) -> SessionResult<()> {
        let reference = reference(target)?;
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .revoke_message(&reference)
                .await
                .map(|_| ())
                .map_err(send_error)
        })
        .await
    }

    async fn delete_for_me(&self, target: &Target) -> SessionResult<()> {
        let reference = reference(target)?;
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .chat_actions()
                .delete_message_for_me(&reference, false, Some(target.timestamp_ms / 1000))
                .await
                .map_err(failed)
        })
        .await
    }

    async fn star(&self, target: &Target, starred: bool) -> SessionResult<()> {
        let reference = reference(target)?;
        bounded(REQUEST_TIMEOUT, async {
            let actions = self.client.chat_actions();
            if starred {
                actions.star_message(&reference).await.map_err(failed)
            } else {
                actions.unstar_message(&reference).await.map_err(failed)
            }
        })
        .await
    }

    async fn pin(
        &self,
        target: &Target,
        pin: Option<client_provider::PinFor>,
    ) -> SessionResult<()> {
        use client_provider::PinFor;
        use whatsapp_rust::PinDuration;
        let reference = reference(target)?;
        bounded(REQUEST_TIMEOUT, async {
            match pin {
                Some(pin) => {
                    let duration = match pin {
                        PinFor::Day => PinDuration::Hours24,
                        PinFor::Week => PinDuration::Days7,
                        PinFor::Month => PinDuration::Days30,
                    };
                    self.client.pin_message(&reference, duration).await
                }
                None => self.client.unpin_message(&reference).await,
            }
            .map(|_| ())
            .map_err(send_error)
        })
        .await
    }

    async fn vote(
        &self,
        target: &Target,
        poll: &wa::Message,
        options: &[String],
    ) -> SessionResult<()> {
        let secret = crate::convert::poll_secret(poll).ok_or_else(|| {
            SessionError::refused("invalid", "This poll came without what a vote needs.")
        })?;
        let secret = MessageSecret::try_from(secret.as_slice()).map_err(invalid)?;
        // A poll is encrypted for whoever made it.
        let own = self.client.pn().map(Jid::into_non_ad);
        let creator = match (&target.sender, own) {
            (Some(sender), _) => sender.clone(),
            (None, Some(own)) => own,
            (None, None) => return Err(SessionError::NotLinked),
        };
        let poll = PollRef::new(reference(target)?, &creator, &secret).map_err(invalid)?;
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .polls()
                .vote(&poll, options)
                .await
                .map(|_| ())
                .map_err(failed)
        })
        .await
    }

    // ----- chats ----------------------------------------------------------

    async fn change_chat(
        &self,
        chat: &Jid,
        change: ChatChange,
        last: Option<&Target>,
    ) -> SessionResult<()> {
        let range = self.range(last);
        bounded(REQUEST_TIMEOUT, async {
            let actions = self.client.chat_actions();
            match change {
                ChatChange::Pinned(true) => actions.pin_chat(chat).await,
                ChatChange::Pinned(false) => actions.unpin_chat(chat).await,
                ChatChange::Muted(true) => actions.mute_chat(chat).await,
                ChatChange::Muted(false) => actions.unmute_chat(chat).await,
                ChatChange::MutedFor(seconds) => {
                    let until = Timestamp::now()
                        .as_millis()
                        .saturating_add(seconds as i64 * 1000);
                    actions.mute_chat_until(chat, until).await
                }
                ChatChange::Archived(true) => actions.archive_chat(chat, range).await,
                ChatChange::Archived(false) => actions.unarchive_chat(chat, range).await,
                ChatChange::MarkedUnread => actions.mark_chat_as_read(chat, false, range).await,
            }
            .map_err(failed)
        })
        .await
    }

    async fn set_chat_read(
        &self,
        chat: &Jid,
        read: bool,
        last: Option<&Target>,
    ) -> SessionResult<()> {
        let range = self.range(last);
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .chat_actions()
                .mark_chat_as_read(chat, read, range)
                .await
                .map_err(failed)
        })
        .await
    }

    async fn watch(&self, who: &Jid) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .presence()
                .subscribe(who.clone())
                .await
                .map_err(failed)
        })
        .await
    }

    // ----- people ---------------------------------------------------------

    async fn on_whatsapp(&self, digits: &[String]) -> SessionResult<Vec<OnWhatsApp>> {
        let asked: Vec<Jid> = digits
            .iter()
            .map(|digits| Jid::pn(digits.as_str()))
            .collect();
        bounded(REQUEST_TIMEOUT, async {
            let found = self
                .client
                .contacts()
                .is_on_whatsapp(&asked)
                .await
                .map_err(failed)?;
            Ok(digits
                .iter()
                .map(|digits| OnWhatsApp {
                    digits: digits.clone(),
                    jid: found
                        .iter()
                        .find(|answer| {
                            answer.is_registered
                                && (answer.jid.user_base() == digits
                                    || answer
                                        .pn_jid
                                        .as_ref()
                                        .is_some_and(|pn| pn.user_base() == digits))
                        })
                        .map(|answer| {
                            answer
                                .pn_jid
                                .clone()
                                .unwrap_or_else(|| answer.jid.clone())
                                .into_non_ad()
                        }),
                })
                .collect())
        })
        .await
    }

    async fn lookup(&self, who: &Jid) -> SessionResult<Lookup> {
        bounded(REQUEST_TIMEOUT, async {
            let found = self
                .client
                .contacts()
                .get_user_info(std::slice::from_ref(who))
                .await
                .map_err(failed)?;
            let info = found.into_values().next();
            Ok(info
                .map(|info| Lookup {
                    about: info.status.filter(|text| !text.is_empty()),
                    picture_id: info.picture_id,
                    alt: if who.server.is_lid_family() {
                        Some(info.jid).filter(|jid| jid.server.is_pn_family())
                    } else {
                        info.lid
                    }
                    .map(Jid::into_non_ad),
                    business_name: None,
                })
                .unwrap_or_default())
        })
        .await
    }

    async fn business(&self, who: &Jid) -> SessionResult<Option<BusinessProfile>> {
        bounded(REQUEST_TIMEOUT, async {
            let found = self
                .client
                .get_business_profile(who)
                .await
                .map_err(|error| iq_error(&error))?;
            Ok(found.map(|profile| {
                let clock =
                    |minutes: Option<u32>| minutes.map(|m| format!("{:02}:{:02}", m / 60, m % 60));
                BusinessProfile {
                    description: Some(profile.description).filter(|text| !text.is_empty()),
                    address: profile.address.filter(|text| !text.is_empty()),
                    email: profile.email.filter(|text| !text.is_empty()),
                    websites: profile.website,
                    categories: profile
                        .categories
                        .into_iter()
                        .map(|category| category.name)
                        .collect(),
                    hours: profile
                        .business_hours
                        .business_config
                        .unwrap_or_default()
                        .into_iter()
                        .map(|day| BusinessHours {
                            day: format!("{:?}", day.day_of_week).to_lowercase(),
                            open: clock(day.open_time),
                            close: clock(day.close_time),
                            mode: format!("{:?}", day.mode).to_lowercase(),
                        })
                        .collect(),
                    time_zone: profile.business_hours.timezone,
                }
            }))
        })
        .await
    }

    async fn picture(&self, whose: &Jid, known: Option<&str>) -> SessionResult<AvatarAnswer> {
        let target = if matches!(whose.server, whatsapp_rust::Server::Group) {
            ProfilePictureTarget::Group(whose)
        } else {
            ProfilePictureTarget::Contact(whose)
        };
        let request =
            ProfilePictureRequest::new(target, ProfilePictureType::Preview).existing_id(known);
        let found = bounded(REQUEST_TIMEOUT, async {
            match self.client.pictures().lookup(request).await {
                Ok(found) => Ok(Some(found)),
                // No picture the account may see is an answer, not a failure.
                Err(error) => match failed(error) {
                    SessionError::Refused { .. } => Ok(None),
                    other => Err(other),
                },
            }
        })
        .await?;
        let picture = match found {
            Some(ProfilePictureLookup::Found(picture)) => picture,
            Some(ProfilePictureLookup::Unchanged) => return Ok(AvatarAnswer::Unchanged),
            _ => return Ok(AvatarAnswer::None),
        };
        // The picture itself is a plain file on WhatsApp's hosts.
        let fetch = async {
            let response = reqwest::get(&picture.url)
                .await
                .map_err(|error| SessionError::NotNow(error.to_string()))?;
            if !response.status().is_success() {
                return Ok(None);
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|error| SessionError::NotNow(error.to_string()))?;
            Ok(Some(bytes.to_vec()))
        };
        Ok(match bounded(REQUEST_TIMEOUT, fetch).await? {
            Some(bytes) => AvatarAnswer::New(Avatar {
                id: picture.id,
                bytes,
                mime_type: Some("image/jpeg".into()),
            }),
            None => AvatarAnswer::None,
        })
    }

    async fn blocked(&self) -> SessionResult<Vec<Jid>> {
        bounded(REQUEST_TIMEOUT, async {
            let entries = self
                .client
                .blocking()
                .get_blocklist()
                .await
                .map_err(failed)?;
            Ok(entries
                .into_iter()
                .map(|entry| entry.jid.into_non_ad())
                .collect())
        })
        .await
    }

    async fn set_blocked(&self, who: &Jid, blocked: bool) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            let blocking = self.client.blocking();
            if blocked {
                blocking.block(who).await.map_err(failed)
            } else {
                blocking.unblock(who).await.map_err(failed)
            }
        })
        .await
    }

    async fn own_about(&self) -> SessionResult<Option<String>> {
        let Some(own) = self.client.pn().map(Jid::into_non_ad) else {
            return Ok(None);
        };
        Ok(self.lookup(&own).await?.about)
    }

    async fn set_name(&self, name: &str) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .profile()
                .set_push_name(name)
                .await
                .map(|_| ())
                .map_err(failed)
        })
        .await
    }

    async fn set_about(&self, about: &str) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .profile()
                .set_status_text(about)
                .await
                .map_err(failed)
        })
        .await
    }

    async fn set_picture(
        &self,
        group: Option<&Jid>,
        jpeg: Option<&[u8]>,
    ) -> SessionResult<Option<String>> {
        bounded(MEDIA_TIMEOUT, async {
            let answer = match (group, jpeg) {
                (None, Some(jpeg)) => self
                    .client
                    .profile()
                    .set_profile_picture(jpeg.to_vec())
                    .await
                    .map_err(failed)?,
                (None, None) => self
                    .client
                    .profile()
                    .remove_profile_picture()
                    .await
                    .map_err(failed)?,
                (Some(group), Some(jpeg)) => self
                    .client
                    .groups()
                    .set_profile_picture(group.clone(), jpeg.to_vec())
                    .await
                    .map_err(failed)?,
                (Some(group), None) => self
                    .client
                    .groups()
                    .remove_profile_picture(group.clone())
                    .await
                    .map_err(failed)?,
            };
            Ok(jpeg.map(|_| answer.id).filter(|id| !id.is_empty()))
        })
        .await
    }

    // ----- groups ---------------------------------------------------------

    async fn groups(&self) -> SessionResult<Vec<Group>> {
        bounded(MEDIA_TIMEOUT, async {
            let all = self
                .client
                .community()
                .fetch_participating_metadata()
                .await
                .map_err(failed)?;
            Ok(all.values().map(to_group).collect())
        })
        .await
    }

    async fn group(&self, group: &Jid) -> SessionResult<Group> {
        bounded(REQUEST_TIMEOUT, async {
            let metadata = self
                .client
                .groups()
                .fetch_metadata(group)
                .await
                .map_err(failed)?;
            let mut found = to_group(&metadata);
            if metadata.is_parent_group {
                // The groups a community links are another question; a
                // community without them is still a community.
                if let Ok(linked) = self.client.community().get_subgroups(group).await {
                    found.subgroups = linked
                        .into_iter()
                        .map(|linked| Subgroup {
                            id: chat_id(&linked.id),
                            subject: linked.subject,
                            announcements: linked.is_default_sub_group,
                        })
                        .collect();
                }
            }
            Ok(found)
        })
        .await
    }

    async fn create_group(
        &self,
        new: &NewGroup,
        participants: &[Jid],
        parent: Option<&Jid>,
    ) -> SessionResult<Group> {
        bounded(REQUEST_TIMEOUT, async {
            let metadata = if new.community {
                let subject = GroupSubject::new(new.subject.clone()).map_err(invalid)?;
                // WhatsApp answers 400 bad-request to whatsapp-rust's own
                // community create: it carries a plain group's settings
                // (link and add modes, disappearing messages, join approval
                // off) and a `<parent/>` without
                // `default_membership_approval_mode`. What is sent here is
                // what WhatsApp Web sends, as Baileys and whatsmeow do.
                let mut options = GroupCreateOptions::new(subject.into_string());
                options.member_link_mode = None;
                options.member_add_mode = None;
                options.membership_approval_mode = None;
                options.ephemeral_expiration = None;
                options.is_parent = true;
                options.closed = true;
                options.create_general_chat = true;
                self.client
                    .groups()
                    .create_group(options)
                    .await
                    .map_err(failed)?
                    .metadata
            } else if let Some(parent) = parent {
                self.client
                    .community()
                    .create_subgroup(new.subject.clone(), participants, parent.clone())
                    .await
                    .map_err(failed)?
                    .metadata
            } else {
                let members = participants
                    .iter()
                    .cloned()
                    .map(GroupParticipantOptions::new)
                    .collect();
                let options =
                    GroupCreateOptions::new(new.subject.clone()).with_participants(members);
                self.client
                    .groups()
                    .create_group(options)
                    .await
                    .map_err(failed)?
                    .metadata
            };
            Ok(to_group(&metadata))
        })
        .await
    }

    async fn change_group(&self, group: &Jid, change: &GroupChange) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            let groups = self.client.groups();
            let group = group.clone();
            match change {
                GroupChange::Subject(subject) => {
                    let subject = GroupSubject::new(subject.clone()).map_err(invalid)?;
                    groups.set_subject(group, subject).await
                }
                GroupChange::Description(text) => {
                    let description = if text.trim().is_empty() {
                        None
                    } else {
                        Some(GroupDescription::new(text.clone()).map_err(invalid)?)
                    };
                    groups
                        .set_description(group, description, PreviousDescription::Resolve)
                        .await
                }
                GroupChange::Announce(on) => groups.set_announce(group, *on).await,
                GroupChange::Locked(on) => groups.set_locked(group, *on).await,
                GroupChange::JoinApproval(on) => {
                    let mode = if *on {
                        MembershipApprovalMode::On
                    } else {
                        MembershipApprovalMode::Off
                    };
                    groups.set_membership_approval(group, mode).await
                }
                GroupChange::MembersCanAdd(all) => {
                    let mode = if *all {
                        MemberAddMode::AllMemberAdd
                    } else {
                        MemberAddMode::AdminAdd
                    };
                    groups.set_member_add_mode(group, mode).await
                }
            }
            .map_err(failed)
        })
        .await
    }

    async fn change_participants(
        &self,
        group: &Jid,
        change: ParticipantChange,
        who: &[Jid],
    ) -> SessionResult<Vec<ParticipantOutcome>> {
        bounded(REQUEST_TIMEOUT, async {
            let groups = self.client.groups();
            let group = group.clone();
            let answers = match change {
                ParticipantChange::Add => groups.add_participants(group, who).await,
                ParticipantChange::Remove => groups.remove_participants(group, who).await,
                ParticipantChange::Promote => groups.promote_participants(group, who).await,
                ParticipantChange::Demote => groups.demote_participants(group, who).await,
            }
            .map_err(failed)?;
            Ok(outcomes(answers))
        })
        .await
    }

    async fn invite_link(&self, group: &Jid, reset: bool) -> SessionResult<String> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .groups()
                .get_invite_link(group.clone(), reset)
                .await
                .map_err(failed)
        })
        .await
    }

    async fn join_requests(&self, group: &Jid) -> SessionResult<Vec<JoinRequest>> {
        bounded(REQUEST_TIMEOUT, async {
            let requests = self
                .client
                .groups()
                .get_membership_requests(group.clone())
                .await
                .map_err(failed)?;
            Ok(requests
                .into_iter()
                .map(|request| JoinRequest {
                    contact: contact_id(&request.jid),
                    requested_at: request
                        .request_time
                        .map(|at| Timestamp::from_millis(to_millis(at as i64))),
                })
                .collect())
        })
        .await
    }

    async fn answer_join_requests(
        &self,
        group: &Jid,
        approve: bool,
        who: &[Jid],
    ) -> SessionResult<Vec<ParticipantOutcome>> {
        bounded(REQUEST_TIMEOUT, async {
            let groups = self.client.groups();
            let answers = if approve {
                groups.approve_membership_requests(group.clone(), who).await
            } else {
                groups.reject_membership_requests(group.clone(), who).await
            }
            .map_err(failed)?;
            Ok(outcomes(answers))
        })
        .await
    }

    async fn leave_group(&self, group: &Jid) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .groups()
                .leave(group.clone())
                .await
                .map_err(failed)
        })
        .await
    }

    async fn link_subgroup(&self, community: &Jid, group: &Jid) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            let answer = self
                .client
                .community()
                .link_subgroups(community.clone(), std::slice::from_ref(group))
                .await
                .map_err(failed)?;
            match answer.failed_groups.first() {
                // Linking a group that is already in the community is
                // having linked it.
                Some(failure) if failure.code != 409 => Err(SessionError::refused(
                    if failure.code == 401 || failure.code == 403 {
                        refusal::NOT_ADMIN
                    } else {
                        "failed"
                    },
                    format!("WhatsApp did not link the group ({}).", failure.code),
                )),
                _ => Ok(()),
            }
        })
        .await
    }

    async fn unlink_subgroup(&self, community: &Jid, group: &Jid) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .community()
                .unlink_subgroups(community.clone(), std::slice::from_ref(group), false)
                .await
                .map(|_| ())
                .map_err(failed)
        })
        .await
    }

    async fn community_participants(&self, community: &Jid) -> SessionResult<Vec<Jid>> {
        bounded(REQUEST_TIMEOUT, async {
            let people = self
                .client
                .community()
                .get_linked_groups_participants(community.clone())
                .await
                .map_err(failed)?;
            Ok(people
                .into_iter()
                .map(|person| person.phone_number.unwrap_or(person.jid).into_non_ad())
                .collect())
        })
        .await
    }

    // ----- stories --------------------------------------------------------

    async fn post_status(&self, post: StatusPost) -> SessionResult<()> {
        let options = StatusSendOptions::default().with_message_id(message_id(&post.id)?);
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .status()
                .send_raw(post.message, &post.recipients, options)
                .await
                .map(|_| ())
                .map_err(send_error)
        })
        .await
    }

    async fn revoke_status(&self, id: &str, recipients: &[Jid]) -> SessionResult<()> {
        let id = message_id(id)?;
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .status()
                .revoke(id, recipients, StatusSendOptions::default())
                .await
                .map(|_| ())
                .map_err(send_error)
        })
        .await
    }

    async fn mute_stories(&self, author: &Jid, muted: bool) -> SessionResult<()> {
        bounded(REQUEST_TIMEOUT, async {
            self.client
                .chat_actions()
                .set_user_status_mute(author, muted)
                .await
                .map_err(failed)
        })
        .await
    }

    // ----- calls ----------------------------------------------------------

    async fn start_call(&self, peer: &Jid, video: bool) -> SessionResult<String> {
        let asked = std::time::Instant::now();
        let audio =
            call_audio::open().map_err(|reason| SessionError::refused("no_audio", reason))?;
        tracing::info!(after = ?asked.elapsed(), video, "placing a call: the sound is open");
        let video = match video {
            // The camera starts while the call rings there.
            true => Some(open_video(false).await?),
            false => None,
        };
        let link = video.as_ref().map(|(link, _)| link.clone());
        let handle = bounded(REQUEST_TIMEOUT, async {
            let voip = self.client.voip();
            let call = voip
                .call(peer)
                .audio(audio.microphone.clone(), audio.speaker.clone());
            match video {
                Some((link, units)) => call.video(units, link.screen.clone()).start().await,
                None => call.start().await,
            }
            .map_err(failed)
        })
        .await?;
        tracing::info!(after = ?asked.elapsed(), "placing a call: offered, it rings there");
        let handle = Arc::new(handle);
        Self::ask_when_stuck(&link, &handle);
        let id = handle.call_id().to_owned();
        self.hearing.calls.active().insert(
            id.clone(),
            ActiveCall {
                handle: handle.clone(),
                audio,
                video: link,
            },
        );
        self.watch_call(id.clone(), peer.clone(), handle);
        Ok(id)
    }

    async fn accept_call(&self, id: &str) -> SessionResult<()> {
        let RingingCall {
            offer,
            preview,
            camera,
        } = self
            .hearing
            .calls
            .ringing()
            .remove(id)
            .ok_or_else(|| SessionError::refused(refusal::NOT_FOUND, "This call is over."))?;
        let incoming = offer;
        let asked = std::time::Instant::now();
        let with_video = matches!(&incoming.action, CallAction::Offer { is_video: true, .. });
        // The camera that showed while it rang goes on, unless it gave up:
        // then it is opened again, and waited for.
        // So does one of a camera that is not the one chosen any more.
        let chosen = call_audio::chosen().camera;
        let preview = preview.filter(|(_, units)| !units.is_closed() && camera == chosen);
        // What is needed to answer is opened first: a call that cannot be
        // answered here goes on ringing, for the phone to take.
        let opened = match call_audio::open() {
            Ok(audio) if with_video => match preview {
                Some(video) => Ok((audio, Some(video))),
                None => open_video(true).await.map(|video| (audio, Some(video))),
            },
            Ok(audio) => Ok((audio, None)),
            Err(reason) => Err(SessionError::refused("no_audio", reason)),
        };
        let (audio, video) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                self.hearing.calls.ringing().insert(
                    id.to_owned(),
                    RingingCall {
                        offer: incoming,
                        preview: None,
                        camera: None,
                    },
                );
                return Err(error);
            }
        };
        tracing::info!(
            after = ?asked.elapsed(),
            video = with_video,
            "answering a call: the sound and the camera are open"
        );
        let link = video.as_ref().map(|(link, _)| link.clone());
        let handle = bounded(REQUEST_TIMEOUT, async {
            let voip = self.client.voip();
            let call = voip
                .accept(&incoming)
                .audio(audio.microphone.clone(), audio.speaker.clone());
            match video {
                Some((link, units)) => call.video(units, link.screen.clone()).start().await,
                None => call.start().await,
            }
            .map_err(failed)
        })
        .await?;
        tracing::info!(after = ?asked.elapsed(), "answering a call: accepted");
        let handle = Arc::new(handle);
        Self::ask_when_stuck(&link, &handle);
        let peer = incoming.from.to_non_ad();
        self.hearing.calls.active().insert(
            id.to_owned(),
            ActiveCall {
                handle: handle.clone(),
                audio,
                video: link,
            },
        );
        self.hearing.call(id, &peer, CallSignal::Connected, None);
        self.watch_call(id.to_owned(), peer, handle);
        Ok(())
    }

    async fn end_call(&self, id: &str) -> SessionResult<()> {
        let active = self.hearing.calls.active().remove(id);
        if let Some(active) = active {
            let peer = active.handle.peer_jid();
            let _ = tokio::time::timeout(REQUEST_TIMEOUT, active.handle.terminate()).await;
            drop(active.audio);
            self.hearing.call(
                id,
                &peer,
                CallSignal::Ended {
                    missed: false,
                    reason: None,
                },
                None,
            );
            return Ok(());
        }
        let ringing = self.hearing.calls.ringing().remove(id);
        if let Some(RingingCall {
            offer: incoming, ..
        }) = ringing
        {
            let declined = bounded(REQUEST_TIMEOUT, async {
                self.client.voip().reject(&incoming).await.map_err(failed)
            })
            .await;
            self.hearing.call(
                id,
                &incoming.from,
                CallSignal::Ended {
                    missed: false,
                    reason: Some("Declined.".into()),
                },
                None,
            );
            return declined;
        }
        Ok(())
    }

    async fn mute_call(&self, id: &str, muted: bool) -> SessionResult<()> {
        let handle = {
            let active = self.hearing.calls.active();
            let Some(call) = active.get(id) else {
                return Ok(());
            };
            call.audio.set_muted(muted);
            call.handle.clone()
        };
        bounded(REQUEST_TIMEOUT, async {
            handle.set_muted(muted).await.map_err(failed)
        })
        .await
    }

    async fn invite_to_call(&self, id: &str, guest: &Jid) -> SessionResult<()> {
        let handle = {
            let active = self.hearing.calls.active();
            active.get(id).map(|call| call.handle.clone())
        };
        let Some(handle) = handle else {
            return Err(SessionError::refused(
                refusal::NOT_FOUND,
                "Somebody can be added once the call is on.",
            ));
        };
        bounded(REQUEST_TIMEOUT, async {
            handle.invite_participant(guest).await.map_err(failed)
        })
        .await
    }

    async fn set_call_camera(&self, id: &str, on: bool) -> SessionResult<()> {
        let held = {
            let active = self.hearing.calls.active();
            active
                .get(id)
                .and_then(|call| Some((call.handle.clone(), call.video.clone()?)))
        };
        let Some((handle, link)) = held else {
            return Ok(());
        };
        if !on {
            // The camera goes first: whatever the other side is told, it
            // films nothing more.
            link.stop_camera();
            return bounded(REQUEST_TIMEOUT, async {
                handle.stop_video().await.map_err(failed)
            })
            .await;
        }
        let filming = link.clone();
        let units = tokio::task::spawn_blocking(move || filming.start_camera(true))
            .await
            .map_err(|error| SessionError::refused("no_video", error.to_string()))?
            .map_err(|reason| SessionError::refused("no_video", reason))?;
        bounded(REQUEST_TIMEOUT, async {
            handle
                .resume_video(units, link.screen.clone())
                .await
                .map_err(failed)
        })
        .await
    }

    fn call_devices_changed(&self) {
        self.hearing.calls.follow_camera();
    }

    fn call_video(&self, id: &str) -> Option<client_provider::CallVideo> {
        if let Some(call) = self.hearing.calls.active().get(id) {
            return Some(call.video.as_ref()?.pictures.clone());
        }
        let ringing = self.hearing.calls.ringing();
        let (link, _) = ringing.get(id)?.preview.as_ref()?;
        Some(link.pictures.clone())
    }
}
