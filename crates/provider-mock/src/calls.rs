//! Calls in the demo world: no sound, the signalling only, so that the
//! window's side of a call can be seen and tested.
//!
//! A call placed from the window rings on the other side and is answered
//! a moment later. [`MockProvider::ring`] is the other direction: somebody
//! calls the account.

use crate::MockProvider;
use client_provider::{
    AccountId, CallCamera, CallDevices, CallEvent, CallPicture, CallState, CallVideo, ChatId,
    ContactId, DeliveryStatus, Direction, Message, MessageContent, MessageId, ProviderEvent,
    ProviderResult, SystemEvent, SystemKind, Timestamp,
};
use std::sync::Arc;

/// A call of the demo world that is not over.
#[derive(Clone, Debug)]
pub(crate) struct MockCall {
    account: AccountId,
    chat: ChatId,
    video: bool,
    incoming: bool,
    answered: bool,
    /// The pictures of a call with video: a drawn pattern for each side.
    screens: Option<CallVideo>,
}

/// A still picture that stands for a camera: bands of `tint`, so that the
/// two sides of a call can be told apart.
fn pattern(width: u32, height: u32, tint: [u8; 3]) -> Arc<CallPicture> {
    let mut bgra = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let band = ((x / 16 + y / 16) % 2) as u8;
            let shade = |channel: u8| channel / (1 + band);
            bgra.extend([shade(tint[0]), shade(tint[1]), shade(tint[2]), 255]);
        }
    }
    Arc::new(CallPicture {
        width,
        height,
        bgra,
    })
}

/// The cameras of the demo world.
pub(crate) fn cameras() -> Vec<CallCamera> {
    ["Built-in camera", "USB camera"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| CallCamera {
            id: format!("mock-camera-{index}"),
            name: name.to_owned(),
        })
        .collect()
}

impl MockProvider {
    fn call_event(&self, id: &str, call: &MockCall, state: CallState) -> ProviderEvent {
        ProviderEvent::Call(CallEvent {
            account_id: call.account.clone(),
            call_id: id.to_owned(),
            chat_id: call.chat.clone(),
            peer: ContactId::new(call.chat.as_str()),
            video: call.video,
            state,
            at: Timestamp::now(),
        })
    }

    fn open_call(&self, account: &AccountId, chat: &ChatId, video: bool, incoming: bool) -> String {
        let call = MockCall {
            account: account.clone(),
            chat: chat.clone(),
            video,
            incoming,
            answered: false,
            screens: video.then(|| {
                let screens = CallVideo::default();
                screens.remote.show(Some(pattern(360, 480, [160, 120, 60])));
                screens.local.show(Some(pattern(160, 120, [70, 150, 90])));
                screens
            }),
        };
        let id = {
            let mut state = self.state();
            state.next_call += 1;
            let id = format!("mock-call-{}", state.next_call);
            state.calls.insert(id.clone(), call.clone());
            id
        };
        self.emit(self.call_event(
            &id,
            &call,
            CallState::Ringing {
                incoming,
                group: false,
            },
        ));
        id
    }

    /// Somebody calls the account: the call rings until it is answered,
    /// declined, or [`stop_ringing`](Self::stop_ringing) gives up.
    /// Answers the call's id.
    pub fn ring(&self, account: &AccountId, chat: &ChatId, video: bool) -> String {
        self.open_call(account, chat, video, true)
    }

    /// The caller gives up on a call nobody answered: it ends as a missed
    /// call, with its line in the chat.
    pub fn stop_ringing(&self, call: &str) {
        let Some(held) = self.state().calls.remove(call) else {
            return;
        };
        let missed = held.incoming && !held.answered;
        if missed {
            let notice = Message {
                id: MessageId::new(format!("{call}-missed")),
                client_id: None,
                account_id: held.account.clone(),
                chat_id: held.chat.clone(),
                sender: ContactId::new(held.chat.as_str()),
                sender_name: None,
                direction: Direction::Incoming,
                timestamp: Timestamp::now(),
                content: MessageContent::System(SystemEvent {
                    kind: if held.video {
                        SystemKind::MissedVideoCall
                    } else {
                        SystemKind::MissedVoiceCall
                    },
                    actor: None,
                    targets: Vec::new(),
                    detail: None,
                }),
                reply_to: None,
                status: DeliveryStatus::Delivered,
                edited: false,
                deleted: false,
                extras: Default::default(),
            };
            self.push_event(ProviderEvent::MessageUpserted(notice));
        }
        self.emit(self.call_event(
            call,
            &held,
            CallState::Ended {
                missed,
                reason: None,
            },
        ));
    }

    pub(crate) async fn call_start(
        &self,
        account: &AccountId,
        chat: &ChatId,
        video: bool,
    ) -> ProviderResult<String> {
        let id = self.open_call(account, chat, video, false);
        // The other side picks up after a moment.
        let (this, call) = (self.clone(), id.clone());
        tokio::spawn(async move {
            tokio::time::sleep(this.scaled(1800)).await;
            let _ = this.call_accept(&call).await;
        });
        Ok(id)
    }

    pub(crate) async fn call_accept(&self, call: &str) -> ProviderResult<()> {
        let held = {
            let mut state = self.state();
            let Some(held) = state.calls.get_mut(call) else {
                return Ok(());
            };
            if held.answered {
                return Ok(());
            }
            held.answered = true;
            held.clone()
        };
        self.emit(self.call_event(call, &held, CallState::Connected));
        Ok(())
    }

    pub(crate) async fn call_end(&self, call: &str) -> ProviderResult<()> {
        let Some(held) = self.state().calls.remove(call) else {
            return Ok(());
        };
        self.emit(self.call_event(
            call,
            &held,
            CallState::Ended {
                missed: false,
                reason: None,
            },
        ));
        Ok(())
    }

    pub(crate) fn call_camera(&self, call: &str, on: bool) {
        if let Some(screens) = self
            .state()
            .calls
            .get(call)
            .and_then(|held| held.screens.clone())
        {
            screens
                .local
                .show(on.then(|| pattern(160, 120, [70, 150, 90])));
        }
    }

    pub(crate) fn call_screens(&self, call: &str) -> Option<CallVideo> {
        self.state().calls.get(call)?.screens.clone()
    }

    /// The devices the window last chose for calls.
    pub fn call_devices(&self) -> CallDevices {
        self.state().call_devices.clone()
    }

    /// Somebody is asked into a call: in a call with video they show up
    /// beside the others, with a pattern of their own.
    pub(crate) fn call_invite(&self, call: &str, contact: &ContactId) {
        let (screens, chat) = {
            let mut state = self.state();
            state.call_invites.push((call.to_owned(), contact.clone()));
            let held = state.calls.get(call);
            (
                held.and_then(|held| held.screens.clone()),
                held.map(|held| held.chat.clone()),
            )
        };
        if let (Some(screens), Some(chat)) = (screens, chat) {
            // A call with several: the one it started with is a guest
            // like the others.
            if screens.guests.all().is_empty() {
                screens
                    .guests
                    .screen_of(&ContactId::new(chat.as_str()))
                    .show(Some(pattern(360, 480, [160, 120, 60])));
            }
            let shade = 60 + (screens.guests.all().len() as u8) * 45;
            screens.guests.screen_of(contact).show(Some(pattern(
                480,
                360,
                [shade, 90, 200 - shade / 2],
            )));
        }
    }

    /// Who was asked into which call, in order.
    pub fn call_invites(&self) -> Vec<(String, ContactId)> {
        self.state().call_invites.clone()
    }
}
