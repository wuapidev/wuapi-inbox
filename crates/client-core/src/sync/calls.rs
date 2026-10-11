//! Calls: what rings or is on, and what the user does with it.
//!
//! A call is not kept: like somebody typing, it is true for a while and
//! then it is not. The provider says where each call stands
//! ([`ProviderEvent::Call`](client_provider::ProviderEvent::Call)) and
//! carries its sound; the engine remembers the calls that are not over,
//! says so ([`StoreChange::Calls`]), and passes on what the user does.
//! When a call nobody answered ends, the provider writes its line in the
//! chat itself, as a message.

use super::SyncEngine;
use crate::StoreChange;
use client_provider::{
    AccountId, CallCamera, CallDevices, CallEvent, CallState, CallVideo, ChatId, Timestamp,
};
use std::collections::HashMap;
use std::sync::Mutex;

/// A call that rings or is on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveCall {
    /// Where it stands, as the provider last said.
    pub event: CallEvent,
    /// When both sides started hearing each other.
    pub connected_at: Option<Timestamp>,
    /// The account's microphone is silenced.
    pub muted: bool,
    /// The account's camera is turned off, in a call with video.
    pub camera_off: bool,
}

impl LiveCall {
    /// It rings here and waits for an answer.
    pub fn waits_for_answer(&self) -> bool {
        matches!(self.event.state, CallState::Ringing { incoming: true, .. })
    }
}

/// The calls that are not over, and what last went wrong with one.
#[derive(Default)]
pub(super) struct Calls {
    live: Mutex<HashMap<String, LiveCall>>,
    problem: Mutex<Option<String>>,
}

impl SyncEngine {
    /// The calls that ring or are on, the oldest first.
    pub fn calls(&self) -> Vec<LiveCall> {
        let live = self.inner.calls.live.lock().expect("calls lock");
        let mut calls: Vec<LiveCall> = live.values().cloned().collect();
        calls.sort_by_key(|call| (call.event.at, call.event.call_id.clone()));
        calls
    }

    /// Why the last thing asked of a call did not happen, in words for the
    /// user. Cleared by the next thing that is asked.
    pub fn call_problem(&self) -> Option<String> {
        self.inner.calls.problem.lock().expect("calls lock").clone()
    }

    /// Takes the provider's word on a call.
    pub(super) fn apply_call(&self, event: CallEvent) {
        let account = event.account_id.clone();
        {
            let mut live = self.inner.calls.live.lock().expect("calls lock");
            if event.state.is_over() {
                live.remove(&event.call_id);
            } else {
                let held = live.remove(&event.call_id);
                let connected_at = held
                    .as_ref()
                    .and_then(|call| call.connected_at)
                    .or_else(|| matches!(event.state, CallState::Connected).then_some(event.at));
                // A call that is on does not go back to ringing because a
                // late signal says so.
                let event = match (&held, &event.state) {
                    (Some(held), CallState::Ringing { .. }) if connected_at.is_some() => {
                        held.event.clone()
                    }
                    _ => event,
                };
                let (muted, camera_off) = held
                    .map(|call| (call.muted, call.camera_off))
                    .unwrap_or_default();
                live.insert(
                    event.call_id.clone(),
                    LiveCall {
                        event,
                        connected_at,
                        muted,
                        camera_off,
                    },
                );
            }
        }
        self.inner.store.notify(StoreChange::Calls {
            account_id: account,
        });
    }

    fn call(&self, id: &str) -> Option<LiveCall> {
        self.inner
            .calls
            .live
            .lock()
            .expect("calls lock")
            .get(id)
            .cloned()
    }

    fn call_failed(&self, account: &AccountId, problem: Option<String>) {
        *self.inner.calls.problem.lock().expect("calls lock") = problem;
        self.inner.store.notify(StoreChange::Calls {
            account_id: account.clone(),
        });
    }

    /// Calls a chat's contact. How it goes on is in [`calls`](Self::calls);
    /// why it did not start, in [`call_problem`](Self::call_problem).
    pub fn start_call(&self, account: &AccountId, chat: &ChatId, video: bool) {
        if !self.capabilities_for(account).calls || self.is_stopped() {
            return;
        }
        self.call_failed(account, None);
        let this = self.clone();
        let (account, chat) = (account.clone(), chat.clone());
        self.inner.runtime.spawn(async move {
            if let Err(error) = this.inner.provider.start_call(&account, &chat, video).await {
                this.call_failed(
                    &account,
                    Some(super::failure_sentence(&super::SyncError::Provider(error))),
                );
            }
        });
    }

    /// Answers a call that is ringing.
    pub fn accept_call(&self, id: &str) {
        let Some(call) = self.call(id) else {
            return;
        };
        let account = call.event.account_id.clone();
        self.call_failed(&account, None);
        // The answer is on its way: stop the ringing at once.
        self.apply_call(CallEvent {
            state: CallState::Connecting,
            at: Timestamp::now(),
            ..call.event
        });
        let this = self.clone();
        let id = id.to_owned();
        self.inner.runtime.spawn(async move {
            if let Err(error) = this.inner.provider.accept_call(&account, &id).await {
                // It could not be answered here: it is over for this
                // window, and the reason is said.
                this.inner
                    .calls
                    .live
                    .lock()
                    .expect("calls lock")
                    .remove(&id);
                this.call_failed(
                    &account,
                    Some(super::failure_sentence(&super::SyncError::Provider(error))),
                );
            }
        });
    }

    /// Declines a call that is ringing, or hangs up one that is on. It
    /// leaves the screen at once, whatever the provider answers.
    pub fn end_call(&self, id: &str) {
        let Some(call) = self.inner.calls.live.lock().expect("calls lock").remove(id) else {
            return;
        };
        let account = call.event.account_id.clone();
        self.call_failed(&account, None);
        let this = self.clone();
        let id = id.to_owned();
        self.inner.runtime.spawn(async move {
            if let Err(error) = this.inner.provider.end_call(&account, &id).await {
                tracing::debug!("a call could not be ended on the provider: {error}");
            }
        });
    }

    /// Silences the account's microphone in a call, or lets it be heard
    /// again.
    pub fn mute_call(&self, id: &str, muted: bool) {
        let account = {
            let mut live = self.inner.calls.live.lock().expect("calls lock");
            let Some(call) = live.get_mut(id) else {
                return;
            };
            call.muted = muted;
            call.event.account_id.clone()
        };
        self.inner.store.notify(StoreChange::Calls {
            account_id: account.clone(),
        });
        let this = self.clone();
        let id = id.to_owned();
        self.inner.runtime.spawn(async move {
            if let Err(error) = this.inner.provider.mute_call(&account, &id, muted).await {
                tracing::debug!("a call could not be muted on the provider: {error}");
            }
        });
    }

    /// Turns the account's camera off in a call with video, or on again.
    pub fn set_call_camera(&self, id: &str, on: bool) {
        let account = {
            let mut live = self.inner.calls.live.lock().expect("calls lock");
            let Some(call) = live.get_mut(id) else {
                return;
            };
            call.camera_off = !on;
            call.event.account_id.clone()
        };
        self.inner.store.notify(StoreChange::Calls {
            account_id: account.clone(),
        });
        let this = self.clone();
        let id = id.to_owned();
        self.inner.runtime.spawn(async move {
            if let Err(error) = this.inner.provider.set_call_camera(&account, &id, on).await {
                tracing::debug!("a call's camera could not be switched on the provider: {error}");
            }
        });
    }

    /// The pictures of a call with video, to draw.
    pub fn call_video(&self, id: &str) -> Option<CallVideo> {
        let call = self.call(id)?;
        self.inner.provider.call_video(&call.event.account_id, id)
    }

    /// The microphone, speakers and camera calls use from now on.
    pub fn set_call_devices(&self, devices: CallDevices) {
        self.inner.provider.set_call_devices(devices);
    }

    /// The cameras a call with video can use.
    pub fn call_cameras(&self) -> Vec<CallCamera> {
        self.inner.provider.call_cameras()
    }
}
