//! Calls: the neutral model.
//!
//! A call is optional for a provider ([`Capabilities::calls`]). The
//! provider carries the sound itself, between the machine's microphone and
//! speakers and the other side: the client only says what to do with a
//! call ([`Provider::start_call`], [`accept_call`], [`end_call`],
//! [`mute_call`]) and is told where it stands ([`ProviderEvent::Call`]).
//!
//! A call with video ([`Capabilities::video_calls`]) is carried the same
//! way: the provider runs the camera and hands the window the pictures to
//! draw ([`Provider::call_video`]), its own and the other side's. Which
//! microphone, speakers and camera a call uses is the user's choice, told
//! to the provider with [`Provider::set_call_devices`].
//!
//! [`Capabilities::calls`]: crate::Capabilities::calls
//! [`Capabilities::video_calls`]: crate::Capabilities::video_calls
//! [`Provider::call_video`]: crate::Provider::call_video
//! [`Provider::set_call_devices`]: crate::Provider::set_call_devices
//! [`Provider::start_call`]: crate::Provider::start_call
//! [`accept_call`]: crate::Provider::accept_call
//! [`end_call`]: crate::Provider::end_call
//! [`mute_call`]: crate::Provider::mute_call
//! [`ProviderEvent::Call`]: crate::ProviderEvent::Call

use crate::ids::{AccountId, ChatId, ContactId};
use crate::model::Timestamp;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

/// Where a call stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CallState {
    /// It rings: here when `incoming`, on the other side otherwise.
    Ringing {
        /// Somebody is calling the account.
        incoming: bool,
        /// It is a group call.
        group: bool,
    },
    /// It was answered and the sound is being set up.
    Connecting,
    /// Both sides hear each other.
    Connected,
    /// It is over.
    Ended {
        /// It rang here and nobody answered.
        missed: bool,
        /// Why, in the provider's words, when it says.
        reason: Option<String>,
    },
}

impl CallState {
    /// True once the call is over.
    pub fn is_over(&self) -> bool {
        matches!(self, Self::Ended { .. })
    }
}

/// A call changed state. Like every event it is a hint that may arrive
/// twice; a call is known by its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallEvent {
    /// The account that is called or calls.
    pub account_id: AccountId,
    /// The provider's id of the call.
    pub call_id: String,
    /// The chat of who it is with.
    pub chat_id: ChatId,
    /// Who it is with.
    pub peer: ContactId,
    /// With video.
    pub video: bool,
    /// Where it stands.
    pub state: CallState,
    /// When that happened.
    pub at: Timestamp,
}

/// The devices the user chose for calls, by the names the system gives
/// them. `None` is the system's default, which is also used when there is
/// no device of that name any more.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallDevices {
    /// What the other side hears.
    pub microphone: Option<String>,
    /// Where the other side is heard.
    pub speaker: Option<String>,
    /// What the other side sees: a [`CallCamera::id`].
    pub camera: Option<String>,
}

/// A camera a call with video can use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallCamera {
    /// What the provider knows it by.
    pub id: String,
    /// What it is called, for people.
    pub name: String,
}

/// One picture of a call's video: `width * height * 4` bytes, blue,
/// green, red and alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallPicture {
    /// Across, in pixels.
    pub width: u32,
    /// Down, in pixels.
    pub height: u32,
    /// The pixels, row after row.
    pub bgra: Vec<u8>,
}

/// The newest picture of one side of a call. The provider puts each one
/// here as it arrives and the window looks when it draws: a picture
/// nobody looked at is replaced, never queued.
#[derive(Clone, Debug, Default)]
pub struct CallScreen(Arc<Mutex<(u64, Option<Arc<CallPicture>>)>>);

impl CallScreen {
    /// Shows `picture`, or nothing (the camera was turned off).
    pub fn show(&self, picture: Option<Arc<CallPicture>>) {
        let mut held = self.0.lock().expect("call screen lock");
        held.0 += 1;
        held.1 = picture;
    }

    /// The picture to draw, and a count that changes whenever it does.
    pub fn newest(&self) -> (u64, Option<Arc<CallPicture>>) {
        self.0.lock().expect("call screen lock").clone()
    }
}

/// The pictures of a call with video.
#[derive(Clone, Debug, Default)]
pub struct CallVideo {
    /// The other side.
    pub remote: CallScreen,
    /// The account's own camera, as the other side sees it.
    pub local: CallScreen,
}
