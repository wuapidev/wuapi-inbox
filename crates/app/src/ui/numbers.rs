//! Numbers: linking a new one, and renaming, reconnecting and logging out
//! the ones there are.
//!
//! Linking is a screen that waits, so it is built for a connection that
//! drops: the request that creates the number carries one id for as many
//! attempts as it takes (a retry never creates a second number), the wait
//! for the phone is a poll that shrugs off a failed round, and the only
//! things that end it are the provider saying so, the user, or a quarter
//! of an hour without a link. Nothing here unlinks or deletes a number
//! that was ever linked without being told to.
//!
//! A number links by scanning a QR code or by typing a code on the phone,
//! and the choice is not only made on the form: while the QR code waits,
//! "Link with phone number instead" asks for the number and then for a
//! code, for the same number on the session it already has. Nothing is
//! created or deleted to change the way. The way back to the QR code is
//! free until the code is asked for; after that it is the provider's to
//! offer (`Capabilities::link_back_to_scan`).

use super::link_qr::{self, QrLook};
use super::shell::{Overlay, Shell};
use super::widgets::{label, mono, switch, text_button, MarkPlay};
use crate::icons::{icon, IconName};
use crate::linking::{self, GIVE_UP, POLL};
use crate::motion::{self, Tween};
use crate::settings;
use crate::theme::px;
use crate::theme::{fonts, metrics, Palette};
use client_core::SyncError;
use client_provider::{
    Account, AccountChange, AccountId, ConnectionState, HistoryImport, LinkPlace, LinkStatus,
    LinkStep, NewAccount, ProviderError, Timestamp,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, ClipboardItem, Context, Div, Entity, FontWeight, Image, ImageFormat, SharedString,
    Stateful, Subscription, Task, Window,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many places the picker lists at once.
const PLACES_SHOWN: usize = 4;
/// The characters of a pairing code, for the boxes that wait for one.
const CODE_GROUPS: [usize; 2] = [4, 4];
/// What a wait shows before the provider has said anything.
const STARTING: LinkStep = LinkStep::Starting;

/// Where the linking screen is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) enum LinkStage {
    /// Asking how to link.
    Form,
    /// The number is being created.
    Creating,
    /// Waiting for the phone.
    Waiting(LinkStatus),
    /// Linked.
    Done(Account),
    /// Stopped, with the reason. The number may exist.
    Stopped {
        account: Option<AccountId>,
        was_linked: bool,
        reason: SharedString,
    },
}

/// The linking screen's state.
pub(in crate::ui) struct LinkFlow {
    pub(in crate::ui) name: Entity<InputState>,
    pub(in crate::ui) phone: Entity<InputState>,
    pub(in crate::ui) place_search: Entity<InputState>,
    /// The places to choose from; `None` while they load.
    pub(in crate::ui) places: Option<Vec<LinkPlace>>,
    pub(in crate::ui) place: Option<LinkPlace>,
    /// The user chose a place: the default no longer follows the number.
    place_chosen: bool,
    /// Linking with a code to type, not a QR code to scan: what the form
    /// chose, and afterwards what the number is doing.
    pub(in crate::ui) by_code: bool,
    /// The number to link with a code is being asked for, over the wait.
    pub(in crate::ui) ask_phone: bool,
    /// A code, or the way back to the QR code, was asked for and the
    /// answer is on its way.
    pub(in crate::ui) code_busy: bool,
    /// The code was copied a moment ago.
    pub(in crate::ui) copied: bool,
    pub(in crate::ui) history: bool,
    pub(in crate::ui) stage: LinkStage,
    pub(in crate::ui) error: Option<SharedString>,
    /// Something passing, said while the wait goes on.
    pub(in crate::ui) note: Option<SharedString>,
    /// "Stop and delete?" is showing.
    pub(in crate::ui) confirm_cancel: bool,
    /// The QR code as drawn, with the bytes it was made from.
    qr: Option<(Vec<u8>, Arc<Image>)>,
    /// The provider's QR code could not be read as a picture.
    pub(in crate::ui) qr_unreadable: bool,
    /// The QR code coming in over its placeholder.
    qr_in: Option<Tween>,
    /// Since when the placeholder has been waiting for a code.
    waiting_since: Instant,
    /// One id for every attempt at creating this number.
    request_id: String,
    _following: Option<Task<()>>,
    _work: Option<Task<()>>,
    _copied: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl LinkFlow {
    /// Takes the QR code of a step, or that the step has none. A code
    /// that arrives where the placeholder was fades in over it; one that
    /// replaces another is just the next one.
    fn take_qr(&mut self, png: Option<&[u8]>, now: Instant) {
        let had = self.qr.is_some();
        match png {
            Some(png) if self.qr.as_ref().is_some_and(|(bytes, _)| bytes == png) => {}
            Some(png) => {
                self.qr = qr_image(png).map(|image| (png.to_vec(), image));
                self.qr_unreadable = self.qr.is_none();
                if self.qr.is_some() && !had {
                    self.qr_in = Some(Tween::begin(now, motion::link::CROSSFADE));
                }
            }
            None => {
                self.qr = None;
                self.qr_unreadable = false;
            }
        }
        if had && self.qr.is_none() {
            // The placeholder is back: its wait starts again.
            self.qr_in = None;
            self.waiting_since = now;
        }
    }
}

/// What is being asked about a number that exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum NumberAsk {
    Rename,
    LogOut,
    /// Delete a number that was never linked.
    Remove,
}

/// A question about a number, in its own card.
pub(in crate::ui) struct NumberAction {
    pub(in crate::ui) account: AccountId,
    pub(in crate::ui) ask: NumberAsk,
    pub(in crate::ui) name: Entity<InputState>,
    pub(in crate::ui) error: Option<SharedString>,
    pub(in crate::ui) busy: bool,
    /// Where the card was opened from, to go back to: Settings, or
    /// nothing (the rail's menu).
    pub(in crate::ui) back: Overlay,
    _subscription: Subscription,
}

/// A failure as one sentence for the user. A passing one never names the
/// machinery: it will be tried again.
fn said(error: &SyncError) -> SharedString {
    match error {
        SyncError::Provider(ProviderError::RateLimited { retry_after }) => match retry_after {
            Some(wait) => format!(
                "Too many requests. Try again in {} seconds.",
                wait.as_secs().max(1)
            )
            .into(),
            None => "Too many requests. Wait a moment and try again.".into(),
        },
        SyncError::Provider(provider) if provider.is_transient() => {
            "Could not reach the provider. Try again.".into()
        }
        SyncError::Provider(ProviderError::Rejected { message, .. }) => message.clone().into(),
        other => other.to_string().into(),
    }
}

/// The QR code as an image this process made: decoded behind limits and
/// encoded again, never the provider's bytes as they came.
fn qr_image(png: &[u8]) -> Option<Arc<Image>> {
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(2048);
    limits.max_image_height = Some(2048);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    let mut own = Vec::new();
    decoded
        .to_luma8()
        .write_to(&mut std::io::Cursor::new(&mut own), image::ImageFormat::Png)
        .ok()?;
    Some(Arc::new(Image::from_bytes(ImageFormat::Png, own)))
}

impl Shell {
    // ----- linking ------------------------------------------------------

    /// Opens the linking screen on its form.
    pub(in crate::ui) fn begin_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let flow = self.new_link_flow(LinkStage::Form, window, cx);
        self.linking = Some(flow);
        self.load_places(cx);
        if let Some(flow) = &self.linking {
            flow.name.update(cx, |field, cx| field.focus(window, cx));
        }
    }

    fn new_link_flow(
        &mut self,
        stage: LinkStage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LinkFlow {
        let name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Sales, Support, My number"));
        let phone = cx.new(|cx| InputState::new(window, cx).placeholder("+58 424 555 0199"));
        let place_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search a city or country"));
        let subscriptions = vec![
            cx.subscribe_in(&phone, window, |this, _, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Change => {
                        this.follow_number_with_place(cx);
                        cx.notify();
                    }
                    // The number typed over the wait: Enter asks for the code.
                    InputEvent::PressEnter { .. } => this.request_link_code(window, cx),
                    _ => {}
                }
            }),
            cx.subscribe_in(&place_search, window, |_, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }),
        ];
        LinkFlow {
            name,
            phone,
            place_search,
            places: None,
            place: None,
            place_chosen: false,
            by_code: false,
            ask_phone: false,
            code_busy: false,
            copied: false,
            // On by default: WhatsApp hands its history over once, when the
            // number links, and there is no asking for it afterwards.
            history: true,
            stage,
            error: None,
            note: None,
            confirm_cancel: false,
            qr: None,
            qr_unreadable: false,
            qr_in: None,
            waiting_since: cx.background_executor().now(),
            request_id: client_core::new_client_id().to_string(),
            _following: None,
            _work: None,
            _copied: None,
            _subscriptions: subscriptions,
        }
    }

    /// Asks the provider where a number can connect from.
    fn load_places(&mut self, cx: &mut Context<Self>) {
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.link_places().await });
        let task = cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(flow) = this.linking.as_mut() else {
                    return;
                };
                match outcome {
                    Ok(Ok(places)) => {
                        flow.places = Some(places);
                        this.follow_number_with_place(cx);
                    }
                    Ok(Err(error)) => {
                        flow.places = Some(Vec::new());
                        flow.error = Some(
                            format!("The list of places could not be loaded. {}", said(&error))
                                .into(),
                        );
                    }
                    Err(error) => flow.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._work = Some(task);
        }
    }

    /// Until the user chooses a place, the default follows the number
    /// being typed, then the numbers already linked.
    fn follow_number_with_place(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.linking.as_ref() else {
            return;
        };
        if flow.place_chosen {
            return;
        }
        let typed = flow.phone.read(cx).value().to_string();
        let known: Vec<String> = self
            .accounts
            .iter()
            .filter_map(|account| account.phone.clone())
            .collect();
        let hints = std::iter::once(typed.as_str())
            .filter(|_| flow.by_code)
            .chain(known.iter().map(String::as_str));
        let place = flow
            .places
            .as_deref()
            .and_then(|places| linking::default_place(places, hints))
            .cloned();
        if let Some(flow) = self.linking.as_mut() {
            flow.place = place;
        }
    }

    /// "Link number": creates the number and starts waiting for the phone.
    pub(in crate::ui) fn submit_link(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        if flow.stage != LinkStage::Form {
            return;
        }
        let name = flow.name.read(cx).value().trim().to_owned();
        // Asked for only when linking with a code, and checked before
        // anything is created.
        let phone = match linking::pairing_phone(&flow.phone.read(cx).value()) {
            Ok(phone) => phone,
            Err(why) if flow.by_code => {
                flow.error = Some(why.into());
                return cx.notify();
            }
            Err(_) => String::new(),
        };
        let needs_place = flow
            .places
            .as_ref()
            .is_some_and(|places| !places.is_empty());
        if flow.places.is_none() || (needs_place && flow.place.is_none()) {
            flow.error = Some("Choose where the number connects from.".into());
            return cx.notify();
        }
        let new = NewAccount {
            name: (!name.is_empty()).then_some(name),
            place: flow.place.clone(),
            pairing_phone: flow.by_code.then_some(phone),
            history: if flow.history {
                HistoryImport::Recent
            } else {
                HistoryImport::Off
            },
            // The same id for as long as this form is open: sending it
            // again after a dropped connection finds the same number.
            request_id: flow.request_id.clone(),
        };
        flow.stage = LinkStage::Creating;
        flow.error = None;
        flow.waiting_since = cx.background_executor().now();
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.create_account(&new).await });
        let task = cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(flow) = this.linking.as_mut() else {
                    return;
                };
                match outcome {
                    Ok(Ok(status)) => {
                        this.link_moved(status, Duration::ZERO, cx);
                    }
                    Ok(Err(error)) => {
                        // Back to the form with what was typed. Nothing
                        // was lost: the request can be sent again.
                        flow.stage = LinkStage::Form;
                        flow.error = Some(said(&error));
                    }
                    Err(error) => {
                        flow.stage = LinkStage::Form;
                        flow.error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._work = Some(task);
        }
        cx.notify();
    }

    /// Takes a linking answer: shows it, and keeps asking unless it is
    /// final. Returns whether there is more to wait for.
    fn link_moved(&mut self, status: LinkStatus, waited: Duration, cx: &mut Context<Self>) -> bool {
        let Some(flow) = self.linking.as_mut() else {
            return false;
        };
        flow.note = None;
        let account = status.account.id.clone();
        let following = flow._following.is_some();
        let now = cx.background_executor().now();
        match &status.step {
            LinkStep::Scan { png } => {
                flow.take_qr(Some(png), now);
                flow.by_code = false;
            }
            LinkStep::TypeCode { .. } => {
                flow.take_qr(None, now);
                flow.by_code = true;
            }
            _ => flow.take_qr(None, now),
        }
        let more = match &status.step {
            LinkStep::Linked => {
                flow.stage = LinkStage::Done(status.account.clone());
                flow.confirm_cancel = false;
                flow.ask_phone = false;
                false
            }
            LinkStep::Stopped { reason } => {
                flow.ask_phone = false;
                flow.stage = LinkStage::Stopped {
                    account: Some(account.clone()),
                    was_linked: status.was_linked,
                    reason: reason.clone().into(),
                };
                false
            }
            _ if waited >= GIVE_UP => {
                flow.ask_phone = false;
                flow.stage = LinkStage::Stopped {
                    account: Some(account.clone()),
                    was_linked: status.was_linked,
                    reason: "The number was not linked in time.".into(),
                };
                false
            }
            _ => {
                flow.stage = LinkStage::Waiting(status);
                true
            }
        };
        if more && !following {
            self.follow_link(account, cx);
        }
        cx.notify();
        more
    }

    /// Asks where linking stands every [`POLL`], until it is final.
    fn follow_link(&mut self, account: AccountId, cx: &mut Context<Self>) {
        let engine = self.engine.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut waited = Duration::ZERO;
            loop {
                cx.background_executor().timer(POLL).await;
                waited += POLL;
                let (asked, about) = (engine.clone(), account.clone());
                let work = engine
                    .runtime()
                    .spawn(async move { asked.link_status(&about).await });
                let outcome = work.await;
                let more = this.update(cx, |this, cx| match outcome {
                    Ok(Ok(status)) => this.link_moved(status, waited, cx),
                    Ok(Err(error)) => this.link_hiccup(&error, waited, cx),
                    Err(_) => false,
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
            this.update(cx, |this, _| {
                if let Some(flow) = this.linking.as_mut() {
                    flow._following = None;
                }
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._following = Some(task);
        }
    }

    /// A round of asking failed. A passing failure is not the user's
    /// business: the wait goes on. Anything else ends it.
    fn link_hiccup(&mut self, error: &SyncError, waited: Duration, cx: &mut Context<Self>) -> bool {
        let Some(flow) = self.linking.as_mut() else {
            return false;
        };
        let (account, was_linked) = match &flow.stage {
            LinkStage::Waiting(status) => (Some(status.account.id.clone()), status.was_linked),
            _ => (None, false),
        };
        let more = if error.is_transient() && waited < GIVE_UP {
            flow.note = Some("The connection dropped. Still waiting for the phone.".into());
            true
        } else {
            flow.stage = LinkStage::Stopped {
                account,
                was_linked,
                reason: if error.is_transient() {
                    "The number was not linked in time.".into()
                } else {
                    said(error)
                },
            };
            false
        };
        cx.notify();
        more
    }

    /// "Try again" on a stopped link: restarts the session, which brings
    /// a new code.
    pub(in crate::ui) fn retry_link(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        let LinkStage::Stopped {
            account: Some(account),
            ..
        } = flow.stage.clone()
        else {
            // Nothing was created: the form again.
            flow.stage = LinkStage::Form;
            return cx.notify();
        };
        flow.error = None;
        flow.stage = LinkStage::Creating;
        flow.waiting_since = cx.background_executor().now();
        self.reconnect_into_link(account, cx);
        cx.notify();
    }

    // ----- QR code or code to type ----------------------------------------

    /// What the number to link starts with, when the screen can tell: the
    /// whole number of one that was linked before, or else the country
    /// code of the place it connects from or of a number already linked.
    fn link_phone_start(&self) -> Option<String> {
        let flow = self.linking.as_ref()?;
        if let LinkStage::Waiting(status) = &flow.stage {
            if let Some(own) = &status.account.phone {
                return Some(client_core::phone::format(own));
            }
        }
        linking::phone_prefix(
            flow.place.as_ref(),
            self.accounts
                .iter()
                .filter_map(|account| account.phone.as_deref()),
        )
    }

    /// "Link with phone number instead": asks for the number, over the
    /// wait. Nothing is asked of the provider yet, and the QR code goes on
    /// working underneath.
    pub(in crate::ui) fn ask_link_phone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.engine.capabilities().link_by_code {
            return;
        }
        let start = self.link_phone_start();
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        if !matches!(flow.stage, LinkStage::Waiting(_)) {
            return;
        }
        flow.ask_phone = true;
        flow.error = None;
        flow.confirm_cancel = false;
        let phone = flow.phone.clone();
        let empty = phone.read(cx).value().trim().is_empty();
        phone.update(cx, |field, cx| {
            if let Some(start) = start.filter(|_| empty) {
                field.set_value(start, window, cx);
            }
            field.focus(window, cx);
        });
        cx.notify();
    }

    /// "Get code": asks the provider for a code to type on the phone, for
    /// the number that is waiting. The same number and the same session:
    /// asking again, after a dropped connection or a code that ran out,
    /// never makes a second one.
    pub(in crate::ui) fn request_link_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        let LinkStage::Waiting(status) = &flow.stage else {
            return;
        };
        if flow.code_busy {
            return;
        }
        let account = status.account.id.clone();
        let typed = flow.phone.read(cx).value().to_string();
        let phone = match linking::pairing_phone(&typed) {
            Ok(phone) => phone,
            Err(why) => {
                // Said where the number is typed, with the field ready.
                flow.error = Some(why.into());
                flow.ask_phone = true;
                let field = flow.phone.clone();
                field.update(cx, |field, cx| field.focus(window, cx));
                return cx.notify();
            }
        };
        flow.code_busy = true;
        flow.error = None;
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.pairing_code(&account, &phone).await });
        let task = cx.spawn_in(window, async move |this, cx| {
            let outcome = work.await;
            this.update_in(cx, |this, window, cx| {
                let Some(flow) = this.linking.as_mut() else {
                    return;
                };
                flow.code_busy = false;
                match outcome {
                    Ok(Ok(status)) => {
                        flow.ask_phone = false;
                        flow.by_code = true;
                        // The field is gone: the keyboard is the card's.
                        this.overlay_focus.focus(window, cx);
                        this.link_moved(status, Duration::ZERO, cx);
                    }
                    // Still where the number is typed, or where the code
                    // was: the same button asks again.
                    Ok(Err(error)) => flow.error = Some(said(&error)),
                    Err(error) => flow.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._work = Some(task);
        }
        cx.notify();
    }

    /// "Use QR code instead". While the number is being typed nothing was
    /// asked of the provider: the QR code is still there. Once a code was
    /// asked for, the provider has to take the number back to its QR code.
    pub(in crate::ui) fn use_qr_instead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let back = self.engine.capabilities().link_back_to_scan;
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        if flow.ask_phone {
            flow.ask_phone = false;
            flow.error = None;
            self.overlay_focus.focus(window, cx);
            return cx.notify();
        }
        let LinkStage::Waiting(status) = &flow.stage else {
            return;
        };
        if !back || flow.code_busy {
            return;
        }
        let account = status.account.id.clone();
        flow.code_busy = true;
        flow.error = None;
        flow.waiting_since = cx.background_executor().now();
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.scan_instead(&account).await });
        let task = cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(flow) = this.linking.as_mut() else {
                    return;
                };
                flow.code_busy = false;
                match outcome {
                    Ok(Ok(status)) => {
                        flow.by_code = false;
                        this.link_moved(status, Duration::ZERO, cx);
                    }
                    Ok(Err(error)) => flow.error = Some(said(&error)),
                    Err(error) => flow.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._work = Some(task);
        }
        cx.notify();
    }

    /// "Copy code": the code as it is typed, on the clipboard.
    pub(in crate::ui) fn copy_link_code(&mut self, cx: &mut Context<Self>) {
        let Some(flow) = self.linking.as_mut() else {
            return;
        };
        let LinkStage::Waiting(LinkStatus {
            step: LinkStep::TypeCode { code, .. },
            ..
        }) = &flow.stage
        else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(linking::code_text(code)));
        flow.copied = true;
        flow._copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(motion::link::COPIED).await;
            this.update(cx, |this, cx| {
                if let Some(flow) = this.linking.as_mut() {
                    flow.copied = false;
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Restarts `account`'s session and shows the result on the linking
    /// screen, which must be open.
    fn reconnect_into_link(&mut self, account: AccountId, cx: &mut Context<Self>) {
        let engine = self.engine.clone();
        let asked = account.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.reconnect_account(&asked).await });
        let task = cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(flow) = this.linking.as_mut() else {
                    return;
                };
                match outcome {
                    Ok(Ok(status)) => {
                        this.link_moved(status, Duration::ZERO, cx);
                    }
                    Ok(Err(error)) => {
                        flow.stage = LinkStage::Stopped {
                            account: Some(account.clone()),
                            was_linked: true,
                            reason: said(&error),
                        };
                    }
                    Err(error) => {
                        flow.stage = LinkStage::Stopped {
                            account: Some(account.clone()),
                            was_linked: true,
                            reason: error.to_string().into(),
                        };
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            flow._work = Some(task);
        }
    }

    /// The number the linking screen has created, when it was never
    /// linked: what cancelling would leave behind.
    fn half_made(&self) -> Option<AccountId> {
        match &self.linking.as_ref()?.stage {
            LinkStage::Waiting(status) if !status.was_linked => Some(status.account.id.clone()),
            LinkStage::Stopped {
                account: Some(account),
                was_linked: false,
                ..
            } => Some(account.clone()),
            _ => None,
        }
    }

    /// Cancel, Escape or a click outside. A number that was created and
    /// never linked is not left behind silently, nor deleted silently:
    /// the screen asks.
    pub(in crate::ui) fn cancel_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let half_made = self.half_made().is_some();
        let Some(flow) = self.linking.as_mut() else {
            return self.close_overlay(window, cx);
        };
        if flow.ask_phone {
            // Escape leaves the number being typed, not the linking.
            return self.use_qr_instead(window, cx);
        }
        if flow.stage == LinkStage::Creating {
            // The request is on its way; its answer decides.
            return;
        }
        if half_made && !flow.confirm_cancel {
            flow.confirm_cancel = true;
            return cx.notify();
        }
        if half_made {
            // Already asking: Escape again goes back to waiting.
            flow.confirm_cancel = false;
            return cx.notify();
        }
        self.linking = None;
        self.close_overlay(window, cx);
    }

    /// "Stop and delete": removes the number that was never linked.
    pub(in crate::ui) fn delete_half_made(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.half_made() else {
            return;
        };
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.delete_account(&account).await });
        let task = cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(())) => {
                        this.linking = None;
                        this.overlay = Overlay::None;
                    }
                    Ok(Err(error)) => {
                        if let Some(flow) = this.linking.as_mut() {
                            flow.error = Some(
                                format!("The number could not be deleted. {}", said(&error)).into(),
                            );
                        }
                    }
                    Err(error) => {
                        if let Some(flow) = this.linking.as_mut() {
                            flow.error = Some(error.to_string().into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        });
        if let Some(flow) = self.linking.as_mut() {
            // The wait is over either way.
            flow._following = None;
            flow._work = Some(task);
        }
    }

    /// "Keep it": leaves the linking screen and the number as it is, to
    /// be linked later from Settings.
    pub(in crate::ui) fn leave_link(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.linking = None;
        self.close_overlay(window, cx);
    }

    /// "Open": shows the number that was just linked.
    pub(in crate::ui) fn open_linked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let linked = match self.linking.as_ref().map(|flow| &flow.stage) {
            Some(LinkStage::Done(account)) => Some(account.id.clone()),
            _ => None,
        };
        self.linking = None;
        self.close_overlay(window, cx);
        if let Some(account) = linked {
            self.reload_accounts(cx);
            self.select_account(account, window, cx);
        }
    }

    // ----- the numbers there are ----------------------------------------

    /// Restarts a number's session. One that is no longer linked comes
    /// back with a code, shown on the linking screen.
    pub(in crate::ui) fn reconnect_number(
        &mut self,
        account: AccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let flow = self.new_link_flow(LinkStage::Creating, window, cx);
        self.linking = Some(flow);
        self.overlay = Overlay::AddNumber;
        self.overlay_focus.focus(window, cx);
        self.reconnect_into_link(account, cx);
        cx.notify();
    }

    /// Opens the card that asks about a number.
    pub(in crate::ui) fn ask_number(
        &mut self,
        account: AccountId,
        ask: NumberAsk,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .accounts
            .iter()
            .find(|known| known.id == account)
            .map(|known| known.display_name.clone())
            .unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("A name for this number"));
        name.update(cx, |field, cx| field.set_value(current, window, cx));
        let subscription = cx.subscribe_in(&name, window, |this, _, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.confirm_number(cx);
            }
        });
        self.number_action = Some(NumberAction {
            account,
            ask,
            name: name.clone(),
            error: None,
            busy: false,
            back: if self.overlay == Overlay::Settings {
                Overlay::Settings
            } else {
                Overlay::None
            },
            _subscription: subscription,
        });
        self.rail_menu = None;
        self.overlay = Overlay::NumberAction;
        match ask {
            NumberAsk::Rename => name.update(cx, |field, cx| field.focus(window, cx)),
            NumberAsk::LogOut | NumberAsk::Remove => self.overlay_focus.focus(window, cx),
        }
        cx.notify();
    }

    /// Does what the card asked about, and goes back to Settings.
    pub(in crate::ui) fn confirm_number(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.number_action.as_mut() else {
            return;
        };
        if action.busy {
            return;
        }
        let (account, ask) = (action.account.clone(), action.ask);
        let typed = action.name.read(cx).value().trim().to_owned();
        let (renamed, new_name) = (account.clone(), typed.clone());
        if ask == NumberAsk::Rename && typed.is_empty() {
            action.error = Some("Type a name.".into());
            return cx.notify();
        }
        action.busy = true;
        action.error = None;
        let engine = self.engine.clone();
        let work = self.engine.runtime().spawn(async move {
            match ask {
                NumberAsk::Rename => engine
                    .update_account(&account, AccountChange::Rename(typed))
                    .await
                    .map(|_| ()),
                NumberAsk::LogOut => engine.unlink_account(&account).await.map(|_| ()),
                NumberAsk::Remove => engine.delete_account(&account).await,
            }
        });
        self._number_work = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(())) => {
                        this.leave_number_card();
                        if ask == NumberAsk::Rename {
                            // The provider has the name now: no local one
                            // stands in front of it.
                            let key = this.rail_key(&renamed);
                            this.change_rail(
                                |rail| rail.set_look(&key, |look| look.label = None),
                                cx,
                            );
                        }
                        this.reload_accounts(cx);
                    }
                    // The provider will not take the name: it is kept
                    // here instead, as the user's own label.
                    Ok(Err(error)) if ask == NumberAsk::Rename && !error.is_transient() => {
                        tracing::info!(%error, "the provider refused the name; kept locally");
                        let key = this.rail_key(&renamed);
                        this.change_rail(
                            |rail| rail.set_look(&key, |look| look.label = Some(new_name.clone())),
                            cx,
                        );
                        this.leave_number_card();
                    }
                    Ok(Err(error)) => {
                        if let Some(action) = this.number_action.as_mut() {
                            action.busy = false;
                            action.error = Some(said(&error));
                        }
                    }
                    Err(error) => {
                        if let Some(action) = this.number_action.as_mut() {
                            action.busy = false;
                            action.error = Some(error.to_string().into());
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// "Import history on next link": a setting of the number, nothing
    /// more. It never unlinks or links anything.
    pub(in crate::ui) fn set_history_import(
        &mut self,
        account: AccountId,
        history: HistoryImport,
        cx: &mut Context<Self>,
    ) {
        let engine = self.engine.clone();
        let asked = account.clone();
        let work = self.engine.runtime().spawn(async move {
            engine
                .update_account(&asked, AccountChange::History(history))
                .await
        });
        self._number_work = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                this.number_note = match outcome {
                    Ok(Ok(_)) => {
                        this.reload_accounts(cx);
                        None
                    }
                    Ok(Err(error)) => Some((account, said(&error))),
                    Err(error) => Some((account, error.to_string().into())),
                };
                cx.notify();
            })
            .ok();
        }));
    }

    // ----- drawing ------------------------------------------------------

    /// The linking screen.
    pub(in crate::ui) fn render_add_number(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let flow = self.linking.as_ref()?;
        let title = match &flow.stage {
            LinkStage::Form => "Link a number",
            LinkStage::Waiting(_) if flow.ask_phone => linking::PHONE_TITLE,
            LinkStage::Creating => linking::step_text(&STARTING).0,
            LinkStage::Waiting(status) => linking::step_text(&status.step).0,
            LinkStage::Done(_) => "Linked",
            LinkStage::Stopped { .. } => "Linking stopped",
        };
        let body = match &flow.stage {
            LinkStage::Form => self.render_link_form(flow, palette, cx),
            // The same screen as the wait it becomes: nothing jumps when
            // the provider answers.
            LinkStage::Creating => self
                .render_link_wait(flow, None, palette, cx)
                .debug_selector(|| "link-creating".into()),
            LinkStage::Waiting(status) => self.render_link_wait(flow, Some(status), palette, cx),
            LinkStage::Done(account) => self.render_link_done(account, palette, cx),
            LinkStage::Stopped { reason, .. } => {
                self.render_link_stopped(flow, reason.clone(), palette, cx)
            }
        };
        // The card is as tall as what it holds, up to a height that fits
        // the form whole, and never taller than the window: the column
        // around it has the window's height and the card shrinks into it.
        Some(
            div()
                .h_full()
                .py_4()
                .flex()
                .flex_col()
                .justify_center()
                .child(
                    self.card("add-number", palette)
                        .w(px(440.))
                        .max_h(px(780.))
                        .min_h_0()
                        .flex()
                        .flex_col()
                        .child(super::menus::panel_header(
                            title,
                            palette,
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.cancel_link(window, cx);
                            }),
                        ))
                        .child(body),
                ),
        )
    }

    fn render_link_form(&self, flow: &LinkFlow, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let caps = self.engine.capabilities();
        let view = cx.entity().downgrade();
        let pick_method = move |by_code: bool, _: &mut Window, cx: &mut gpui_kit::App| {
            cx.stop_propagation();
            view.update(cx, |this, cx| {
                if let Some(flow) = this.linking.as_mut() {
                    flow.by_code = by_code;
                    flow.error = None;
                }
                this.follow_number_with_place(cx);
                cx.notify();
            })
            .ok();
        };

        let mut form = div().flex().flex_col().gap_3().child(field_block(
            "link-name",
            "Name (optional)",
            &flow.name,
            false,
            palette,
        ));

        if caps.link_by_code {
            let (by_scan, by_code) = (pick_method.clone(), pick_method);
            form = form.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(label("How to link", palette))
                    .child(
                        div()
                            .debug_selector(|| "link-method".into())
                            .flex()
                            .gap_2()
                            .child(
                                method_card(
                                    "link-method-qr",
                                    IconName::Scan,
                                    "Scan a QR code",
                                    "The phone's camera reads a code shown here.",
                                    !flow.by_code,
                                    palette,
                                )
                                .on_click(move |_, window, cx| by_scan(false, window, cx)),
                            )
                            .child(
                                method_card(
                                    "link-method-code",
                                    IconName::Phone,
                                    "Use the phone number",
                                    "You type an 8-character code on the phone.",
                                    flow.by_code,
                                    palette,
                                )
                                .on_click(move |_, window, cx| by_code(true, window, cx)),
                            ),
                    ),
            );
        }
        if flow.by_code {
            form = form.child(field_block(
                "link-phone",
                "Number to link",
                &flow.phone,
                true,
                palette,
            ));
        }

        // Where it connects from.
        match &flow.places {
            None => {
                form = form.child(
                    div()
                        .debug_selector(|| "link-places-loading".into())
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child("Loading the places a number can connect from…"),
                );
            }
            Some(places) if places.is_empty() => {}
            Some(places) => {
                let typed = flow.place_search.read(cx).value().to_string();
                let matches = linking::matching_places(places, &typed);
                let mut list = div().flex().flex_col();
                for (index, place) in matches.iter().take(PLACES_SHOWN).enumerate() {
                    let chosen = flow.place.as_ref() == Some(*place);
                    let pick = (*place).clone();
                    let hover = palette.hover;
                    list = list.child(
                        div()
                            .id(("link-place", index))
                            .debug_selector(move || format!("link-place-{index}"))
                            .h(px(30.))
                            .px_2()
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .cursor_pointer()
                            .text_size(metrics::TEXT_SMALL())
                            .when(chosen, |this| this.bg(palette.muted))
                            .hover(move |style| style.bg(hover))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(flow) = this.linking.as_mut() {
                                    flow.place = Some(pick.clone());
                                    flow.place_chosen = true;
                                    flow.error = None;
                                }
                                cx.notify();
                            }))
                            .child(SharedString::from(linking::place_name(place)))
                            .when(chosen, |this| {
                                this.child(icon(IconName::Check, px(14.), palette.icon))
                            }),
                    );
                }
                let more = matches.len().saturating_sub(PLACES_SHOWN);
                let chosen: SharedString = match &flow.place {
                    Some(place) => linking::place_name(place).into(),
                    None => "Not chosen".into(),
                };
                form = form.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .child(label("Connects from", palette))
                                .child(
                                    div()
                                        .debug_selector(|| "link-place-chosen".into())
                                        .text_size(metrics::TEXT_SMALL())
                                        .text_color(palette.text)
                                        .child(chosen),
                                ),
                        )
                        .child(
                            div()
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(18.))
                                .text_color(palette.text_muted)
                                .child(
                                    "The number reaches WhatsApp from here. Its own country \
                                     is the best choice.",
                                ),
                        )
                        .child(input_box(
                            "link-place-search",
                            &flow.place_search,
                            false,
                            palette,
                        ))
                        .child(list)
                        .when(matches.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(palette.text_muted)
                                    .child("No place matches."),
                            )
                        })
                        .when(more > 0, |this| {
                            this.child(
                                mono(format!("{more} MORE: TYPE TO NARROW"))
                                    .text_size(px(9.5))
                                    .text_color(palette.text_faint),
                            )
                        }),
                );
            }
        }

        if caps.history_import {
            form = form.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(metrics::TEXT_BODY())
                                    .child("Import recent chats from the phone"),
                            )
                            .child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .line_height(px(18.))
                                    .text_color(palette.text_muted)
                                    .child(
                                        "WhatsApp hands its history over once, when the number \
                                         links. It cannot be asked for later.",
                                    ),
                            ),
                    )
                    .child(
                        switch("link-history", flow.history, palette).on_click(cx.listener(
                            |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(flow) = this.linking.as_mut() {
                                    flow.history = !flow.history;
                                }
                                cx.notify();
                            },
                        )),
                    ),
            );
        }

        // The fields scroll when the card is not tall enough for them (a
        // small window, a large interface size); what went wrong and the
        // button that links are always in sight under them.
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("link-form-fields")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(form),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(
                        flow.error
                            .clone()
                            .map(|error| error_line("link-error", error, palette)),
                    )
                    .child(div().flex().justify_end().gap_2().child(
                        text_button("link-submit", "Link number", None, true, palette).on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.submit_link(cx);
                            }),
                        ),
                    )),
            )
    }

    /// What the QR code's frame shows at this moment: the placeholder
    /// with its light, the code, or one fading into the other.
    pub(in crate::ui) fn qr_look(&self, flow: &LinkFlow, cx: &gpui_kit::App) -> QrLook {
        let still = settings::reduce_motion(cx);
        let frozen = motion::frozen_at(cx);
        let now = cx.background_executor().now();
        let shown = match (&flow.qr, flow.qr_in) {
            (None, _) => 0.,
            (Some(_), Some(coming)) if frozen.is_none() => coming.at(now, still),
            (Some(_), _) => 1.,
        };
        let waited = frozen.unwrap_or_else(|| now.saturating_duration_since(flow.waiting_since));
        let light = if shown < 1. {
            motion::link::sweep(waited, still)
        } else {
            None
        };
        QrLook {
            image: flow.qr.as_ref().map(|(_, image)| image.clone()),
            shown,
            light,
            moving: frozen.is_none() && !still && shown < 1. && (light.is_some() || shown > 0.),
            mark: MarkPlay {
                paused: false,
                reduced: still,
                intro: false,
                frozen,
            },
        }
    }

    /// The wait for the phone. `status` is `None` while the number is
    /// being created: the same screen, with nothing on it to act on yet.
    fn render_link_wait(
        &self,
        flow: &LinkFlow,
        status: Option<&LinkStatus>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let step = status.map_or(&STARTING, |status| &status.step);
        let steps = [
            ("Session", !matches!(step, LinkStep::Starting)),
            ("Phone", matches!(step, LinkStep::Finishing)),
            ("Linked", false),
        ];
        let mut progress = div().flex().items_center().gap_3();
        for (name, done) in steps {
            progress = progress.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(icon(
                        if done {
                            IconName::CircleCheck
                        } else {
                            IconName::Clock3
                        },
                        px(13.),
                        if done {
                            palette.accent
                        } else {
                            palette.text_faint
                        },
                    ))
                    .child(mono(name.to_uppercase()).text_color(palette.text_muted)),
            );
        }
        let body = div()
            .p_4()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(progress);
        let note = |body: Div| {
            body.children(flow.note.clone().map(|note| {
                div()
                    .debug_selector(|| "link-note".into())
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(note)
            }))
        };
        let body = if flow.ask_phone && status.is_some() {
            note(self.render_link_phone(body, flow, caps.link_back_to_scan, palette, cx))
        } else if matches!(step, LinkStep::TypeCode { .. })
            || (flow.by_code && !matches!(step, LinkStep::Scan { .. }))
        {
            note(self.render_link_code(body, flow, step, caps.link_back_to_scan, palette, cx))
        } else {
            self.render_link_scan(
                body,
                flow,
                step,
                caps.link_by_code,
                status.is_some(),
                palette,
                cx,
            )
        };
        body.children(
            flow.error
                .clone()
                .map(|error| error_line("link-error", error, palette)),
        )
        .child(self.render_link_exit(flow, palette, cx))
    }

    /// The QR code in its frame, what to do with it, and the way to a
    /// code to type instead.
    #[allow(clippy::too_many_arguments)]
    fn render_link_scan(
        &self,
        body: Div,
        flow: &LinkFlow,
        step: &LinkStep,
        offers_code: bool,
        started: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (_, what_to_do) = linking::step_text(step);
        // One place under the frame says what is going on: a connection
        // that dropped, a code that could not be drawn, or what to do. It
        // is two lines tall whatever it says, so the card keeps its height
        // and the frame its place.
        let said: Div = if let Some(note) = flow.note.clone() {
            div().debug_selector(|| "link-note".into()).child(note)
        } else if flow.qr_unreadable {
            div()
                .debug_selector(|| "link-qr-unreadable".into())
                .child("The code could not be drawn. A new one is on its way.")
        } else {
            div().child(what_to_do)
        };
        body.child(link_qr::frame(self.qr_look(flow, cx), palette))
            .child(
                div()
                    .debug_selector(|| "link-hint".into())
                    .w_full()
                    .min_h(px(38.))
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .text_color(palette.text_muted)
                    .child(said),
            )
            // Offered while there is a QR code to give up, not once the
            // phone has accepted.
            .when(
                offers_code && matches!(step, LinkStep::Starting | LinkStep::Scan { .. }),
                |this| {
                    let button = text_button(
                        "link-use-code",
                        "Link with phone number instead",
                        Some(IconName::Phone),
                        false,
                        palette,
                    )
                    .w_full();
                    this.child(if started {
                        button.on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.ask_link_phone(window, cx);
                        }))
                    } else {
                        // The number is still being created: there is
                        // nothing to ask a code for yet.
                        button.opacity(0.5).cursor_default()
                    })
                },
            )
    }

    /// The number to link with a code, asked for over the wait.
    fn render_link_phone(
        &self,
        body: Div,
        flow: &LinkFlow,
        way_back: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        body.child(
            div()
                .debug_selector(|| "link-phone-step".into())
                .w_full()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(19.))
                        .text_color(palette.text_muted)
                        .child(
                            "WhatsApp gives a code for this number. You type it on the phone \
                             and nothing has to be scanned.",
                        ),
                )
                .child(field_block(
                    "link-code-phone",
                    "Number to link",
                    &flow.phone,
                    true,
                    palette,
                ))
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_faint)
                        .child(if way_back || flow.by_code {
                            "With its country code, like +58 412 123 4567."
                        } else {
                            "With its country code, like +58 412 123 4567. Once the code is \
                             asked for, this number links with the code."
                        }),
                )
                .child(
                    div()
                        .pt_1()
                        .flex()
                        .justify_between()
                        .gap_2()
                        .child(
                            text_button(
                                "link-use-qr",
                                if flow.by_code {
                                    "Back to the code"
                                } else {
                                    "Use QR code instead"
                                },
                                Some(IconName::ArrowLeft),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.use_qr_instead(window, cx);
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "link-get-code",
                                if flow.code_busy {
                                    "Getting the code…"
                                } else {
                                    "Get code"
                                },
                                None,
                                true,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.request_link_code(window, cx);
                                },
                            )),
                        ),
                ),
        )
    }

    /// The code to type on the phone: large, in its two groups, with how
    /// long it lasts, a way to copy it and what to do on the phone.
    fn render_link_code(
        &self,
        body: Div,
        flow: &LinkFlow,
        step: &LinkStep,
        way_back: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (code, expires_at) = match step {
            LinkStep::TypeCode { code, expires_at } => (Some(code.as_str()), *expires_at),
            _ => (None, None),
        };
        // `None`: past its time.
        let life = linking::code_life(expires_at, Timestamp::now());
        let live = code.is_some() && life.is_some();
        let number = linking::pairing_phone(&flow.phone.read(cx).value())
            .ok()
            .map(|phone| client_core::phone::format(&phone));
        let said: SharedString = match (code, &life) {
            // One line each: the row keeps its height.
            (None, _) if matches!(step, LinkStep::Finishing) => "The phone accepted.".into(),
            (None, _) => "The code is on its way.".into(),
            (Some(_), None) => "This code ran out.".into(),
            (Some(_), Some(None)) => "Type it on the phone.".into(),
            (Some(_), Some(Some(left))) => format!("Runs out in {left}").into(),
        };
        let copy = text_button(
            "link-code-copy",
            if flow.copied { "Copied" } else { "Copy code" },
            Some(if flow.copied {
                IconName::Check
            } else {
                IconName::Copy
            }),
            false,
            palette,
        );
        let renew = text_button(
            "link-code-renew",
            if flow.code_busy {
                "Getting the code…"
            } else {
                "Get a new code"
            },
            Some(IconName::RotateCw),
            code.is_some() && !live,
            palette,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            cx.stop_propagation();
            this.request_link_code(window, cx);
        }));
        let mut steps = div()
            .debug_selector(|| "link-code-steps".into())
            .w_full()
            .flex()
            .flex_col()
            .gap_1();
        for (index, text) in linking::CODE_STEPS.iter().enumerate() {
            steps = steps.child(
                div()
                    .flex()
                    .gap_2()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .child(
                        mono((index + 1).to_string())
                            .flex_none()
                            .line_height(px(19.))
                            .text_color(palette.text_faint),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(palette.text_muted)
                            .child(*text),
                    ),
            );
        }
        body.child(code_boxes(code, live, palette))
            .children(number.map(|number| {
                mono(format!("FOR {number}"))
                    .debug_selector(|| "link-code-for".into())
                    .text_color(palette.text_muted)
            }))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "link-code-life".into())
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(if code.is_some() && !live {
                                palette.warning
                            } else {
                                palette.text_muted
                            })
                            .child(said),
                    )
                    .child(if live {
                        copy.on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.copy_link_code(cx);
                        }))
                    } else if code.is_some() {
                        renew
                    } else {
                        // Nothing to copy yet: the button holds its place.
                        copy.opacity(0.5).cursor_default()
                    }),
            )
            .child(steps)
            .when(way_back, |this| {
                this.child(
                    text_button(
                        "link-use-qr",
                        "Use QR code instead",
                        Some(IconName::Scan),
                        false,
                        palette,
                    )
                    .w_full()
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.use_qr_instead(window, cx);
                    })),
                )
            })
    }

    /// Cancel, or the question cancelling raises.
    fn render_link_exit(&self, flow: &LinkFlow, palette: &Palette, cx: &mut Context<Self>) -> Div {
        if !flow.confirm_cancel {
            return div().w_full().flex().justify_end().child(
                text_button("link-cancel", "Cancel", None, false, palette).on_click(cx.listener(
                    |this, _, window, cx| {
                        cx.stop_propagation();
                        this.cancel_link(window, cx);
                    },
                )),
            );
        }
        div()
            .debug_selector(|| "link-confirm-cancel".into())
            .w_full()
            .p_3()
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .child(
                        "Stop linking? This number was created just now and never linked, so \
                         deleting it loses nothing. Keeping it leaves it in Settings to link \
                         later.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("link-keep-waiting", "Go back", None, false, palette).on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(flow) = this.linking.as_mut() {
                                    flow.confirm_cancel = false;
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        text_button("link-keep", "Keep it", None, false, palette).on_click(
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.leave_link(window, cx);
                            }),
                        ),
                    )
                    .child(
                        text_button(
                            "link-delete",
                            "Stop and delete",
                            Some(IconName::Trash),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.delete_half_made(cx);
                        })),
                    ),
            )
    }

    fn render_link_done(
        &self,
        account: &Account,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let who: SharedString = match &account.phone {
            Some(phone) => format!("{} ({phone}) is linked.", account.display_name).into(),
            None => format!("{} is linked.", account.display_name).into(),
        };
        div()
            .debug_selector(|| "link-done".into())
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(IconName::CircleCheck, px(18.), palette.accent))
                    .child(div().text_size(metrics::TEXT_BODY()).child(who)),
            )
            .child(
                div()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .text_color(palette.text_muted)
                    .child(
                        "Its chats appear as they arrive. Imported history can take a few \
                         minutes.",
                    ),
            )
            .child(div().flex().justify_end().child(
                text_button("link-open", "Open", None, true, palette).on_click(cx.listener(
                    |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_linked(window, cx);
                    },
                )),
            ))
    }

    fn render_link_stopped(
        &self,
        flow: &LinkFlow,
        reason: SharedString,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .debug_selector(|| "link-stopped".into())
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(icon(IconName::TriangleAlert, px(16.), palette.danger))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_BODY())
                            .line_height(px(21.))
                            .child(reason),
                    ),
            )
            .children(
                flow.error
                    .clone()
                    .map(|error| error_line("link-error", error, palette)),
            )
            .when(!flow.confirm_cancel, |this| {
                this.child(
                    div().flex().justify_end().child(
                        text_button(
                            "link-retry",
                            "Try again",
                            Some(IconName::RotateCw),
                            true,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.retry_link(cx);
                        })),
                    ),
                )
            })
            .child(self.render_link_exit(flow, palette, cx))
    }

    /// The card that asks about a number: its new name, or whether to
    /// log it out.
    pub(in crate::ui) fn render_number_action(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let action = self.number_action.as_ref()?;
        let account = self
            .accounts
            .iter()
            .find(|known| known.id == action.account)?;
        let (title, confirm, glyph) = match action.ask {
            NumberAsk::Rename => ("Rename number", "Save", None),
            NumberAsk::LogOut => ("Log this number out?", "Log out", Some(IconName::LogOut)),
            NumberAsk::Remove => ("Remove this number?", "Remove", Some(IconName::Trash)),
        };
        let body: Div = match action.ask {
            NumberAsk::Rename => input_box("number-name", &action.name, false, palette),
            NumberAsk::Remove => div()
                .text_size(metrics::TEXT_BODY())
                .line_height(px(21.))
                .text_color(palette.text_muted)
                .child(SharedString::from(format!(
                    "{} was created and never linked to a phone, so it holds no chats. \
                     Removing it deletes it for good.",
                    account.display_name
                ))),
            NumberAsk::LogOut => div()
                .text_size(metrics::TEXT_BODY())
                .line_height(px(21.))
                .text_color(palette.text_muted)
                .child(SharedString::from(format!(
                    "{} will be unlinked from its phone and stop sending and receiving. The \
                     number and its messages stay; to use it again it has to be linked again \
                     from the phone.",
                    account.display_name
                ))),
        };
        Some(
            self.card("number-action", palette)
                .w(px(420.))
                .flex()
                .flex_col()
                .child(super::menus::panel_header(
                    title,
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.back_to_settings(window, cx);
                    }),
                ))
                .child(
                    div().p_4().flex().flex_col().gap_3().child(body).children(
                        action
                            .error
                            .clone()
                            .map(|error| error_line("number-error", error, palette)),
                    ),
                )
                .child(
                    div()
                        .p_4()
                        .border_t_1()
                        .border_color(palette.border)
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button("number-cancel", "Cancel", None, false, palette).on_click(
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.back_to_settings(window, cx);
                                }),
                            ),
                        )
                        .child(
                            text_button(
                                "number-confirm",
                                if action.busy { "Working…" } else { confirm },
                                glyph,
                                action.ask == NumberAsk::Rename,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirm_number(cx);
                            })),
                        ),
                ),
        )
    }

    /// Closes a number's card, back to where it was opened from.
    fn leave_number_card(&mut self) {
        self.overlay = self
            .number_action
            .take()
            .map_or(Overlay::None, |action| action.back);
    }

    /// Leaves a number's card for where it came from.
    pub(in crate::ui) fn back_to_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_number_card();
        if self.overlay == Overlay::None {
            return self.close_overlay(window, cx);
        }
        self.overlay_focus.focus(window, cx);
        cx.notify();
    }

    /// What Settings shows under a number: what the server does for it,
    /// and what can be done to it.
    pub(in crate::ui) fn render_number_extras(
        &self,
        index: usize,
        number: &Account,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let mut extras = div().pl(px(20.)).pb_2().flex().flex_col().gap_1();
        let line = |text: SharedString| {
            div()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(palette.text_muted)
                .child(text)
        };

        if let Some(history) = number
            .settings
            .history_import
            .filter(|_| caps.history_import)
        {
            let text = match history {
                HistoryImport::Recent => {
                    "History: the phone's recent chats are imported when this number links."
                }
                HistoryImport::Off => {
                    "History: not imported. Chats from before this number was linked are not \
                     here, because WhatsApp only hands them over at the moment of linking."
                }
            };
            extras = extras
                .child(line(text.into()).debug_selector(move || format!("number-history-{index}")));
            if history == HistoryImport::Recent && number.connection.is_connected() {
                extras = extras.child(line(
                    "It applies to the next link: to import now, log this number out and link \
                     it again from the phone."
                        .into(),
                ));
            }
        }
        if let Some(setting) = &number.settings.server_media {
            extras = extras.child(
                line(super::panels::server_media_line(setting).into())
                    .debug_selector(|| "number-server-media".into()),
            );
        }
        if let Some((_, note)) = self
            .number_note
            .as_ref()
            .filter(|(account, _)| *account == number.id)
        {
            extras = extras.child(error_line("number-note", note.clone(), palette));
        }

        let mut actions = div().pt_1().flex().flex_wrap().gap_2();
        let mut any = false;
        if caps.manage_accounts && number.never_linked() {
            // Not a session to reconnect or log out: a number waiting
            // for a phone. Link it, or remove it.
            any = true;
            let (link, remove) = (number.id.clone(), number.id.clone());
            extras = extras.child(
                line("This number was created and never linked to a phone.".into())
                    .debug_selector(move || format!("number-unlinked-{index}")),
            );
            actions = actions
                .child(
                    small_button(
                        ("number-link", index),
                        format!("number-link-{index}"),
                        "Link",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.reconnect_number(link.clone(), window, cx);
                    })),
                )
                .child(
                    small_button(
                        ("number-remove", index),
                        format!("number-remove-{index}"),
                        "Remove",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.ask_number(remove.clone(), NumberAsk::Remove, window, cx);
                    })),
                );
        } else if caps.manage_accounts {
            any = true;
            let (rename, reconnect, log_out) =
                (number.id.clone(), number.id.clone(), number.id.clone());
            actions = actions
                .child(
                    small_button(
                        ("number-rename", index),
                        format!("number-rename-{index}"),
                        "Rename",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.ask_number(rename.clone(), NumberAsk::Rename, window, cx);
                    })),
                )
                .child(
                    small_button(
                        ("number-reconnect", index),
                        format!("number-reconnect-{index}"),
                        if number.connection == ConnectionState::LoggedOut {
                            "Link again"
                        } else {
                            "Reconnect"
                        },
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.reconnect_number(reconnect.clone(), window, cx);
                    })),
                );
            if number.connection != ConnectionState::LoggedOut {
                actions = actions.child(
                    small_button(
                        ("number-logout", index),
                        format!("number-logout-{index}"),
                        "Log out",
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.ask_number(log_out.clone(), NumberAsk::LogOut, window, cx);
                    })),
                );
            }
        }
        if caps.profile_edit && number.connection != ConnectionState::LoggedOut {
            any = true;
            let profile = number.id.clone();
            actions = actions.child(
                small_button(
                    ("number-profile", index),
                    format!("number-profile-{index}"),
                    "WhatsApp profile",
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_own_profile(profile.clone(), window, cx);
                })),
            );
        }
        if caps.history_import
            && caps.manage_accounts
            && number.settings.history_import == Some(HistoryImport::Off)
        {
            any = true;
            let account = number.id.clone();
            actions = actions.child(
                small_button(
                    ("number-import", index),
                    format!("number-import-{index}"),
                    "Import history on next link",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.set_history_import(account.clone(), HistoryImport::Recent, cx);
                })),
            );
        }
        if any {
            extras = extras.child(actions);
        }
        extras
    }
}

/// One of the two ways to link, on the form: what it is and what it asks
/// of the phone, with the chosen one marked.
fn method_card(
    id: &'static str,
    glyph: IconName,
    title: &'static str,
    text: &'static str,
    chosen: bool,
    palette: &Palette,
) -> Stateful<Div> {
    let (hover, ring) = (palette.hover, palette.text);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_1()
        .min_w_0()
        .p_2()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(if chosen {
            palette.accent
        } else {
            palette.border
        })
        .flex()
        .flex_col()
        .gap_1()
        .cursor_pointer()
        .tab_index(0)
        .map(|this| {
            if chosen {
                this.bg(palette.muted)
            } else {
                this.hover(move |style| style.bg(hover))
            }
        })
        .focus_visible(move |style| style.border_color(ring))
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(icon(glyph, px(15.), palette.icon))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(metrics::TEXT_BODY())
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .when(chosen, |this| {
                    this.child(
                        div()
                            .debug_selector(move || format!("{id}-chosen"))
                            .child(icon(IconName::CircleCheck, px(15.), palette.accent)),
                    )
                }),
        )
        .child(
            div()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(17.))
                .text_color(palette.text_muted)
                .child(text),
        )
}

/// A pairing code, one character to a box, in its groups. Without a code
/// the same boxes wait empty, so nothing moves when it arrives; one that
/// ran out is dimmed.
fn code_boxes(code: Option<&str>, live: bool, palette: &Palette) -> Div {
    let groups: Vec<Vec<Option<char>>> = match code {
        Some(code) => linking::code_groups(code)
            .iter()
            .map(|group| group.chars().map(Some).collect())
            .collect(),
        None => CODE_GROUPS.iter().map(|size| vec![None; *size]).collect(),
    };
    let selector = if code.is_some() {
        "link-code"
    } else {
        "link-code-skeleton"
    };
    let mut row = div()
        .debug_selector(move || selector.into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.));
    for (index, group) in groups.into_iter().enumerate() {
        if index > 0 {
            row = row.child(
                div()
                    .flex_none()
                    .w(px(10.))
                    .h(px(2.))
                    .bg(palette.text_faint),
            );
        }
        for character in group {
            row = row.child(
                div()
                    .flex_none()
                    .w(px(34.))
                    .h(px(46.))
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(if character.is_some() {
                        palette.background
                    } else {
                        palette.muted
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(fonts::MONO)
                    .text_size(px(24.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if live {
                        palette.text
                    } else {
                        palette.text_faint
                    })
                    .children(character.map(|c| SharedString::from(c.to_string()))),
            );
        }
    }
    row
}

/// A quiet button for a row of settings: several rows have one each.
pub(super) fn small_button(
    id: impl Into<gpui_kit::ElementId>,
    selector: String,
    text: &'static str,
    palette: &Palette,
) -> Stateful<Div> {
    let (hover, ring) = (palette.hover, palette.text);
    div()
        .id(id)
        .debug_selector(move || selector.clone())
        .h(px(26.))
        .px_2()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(metrics::TEXT_SMALL())
        .text_color(palette.text)
        .tab_index(0)
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
        .child(text)
}

/// A text field in its box.
pub(super) fn input_box(
    selector: &'static str,
    input: &Entity<InputState>,
    mono_face: bool,
    palette: &Palette,
) -> Div {
    div()
        .debug_selector(move || selector.into())
        .h(px(36.))
        .px_2()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(palette.border)
        .bg(palette.background)
        .flex()
        .items_center()
        .when(mono_face, |this| this.font_family(fonts::MONO))
        .child(super::widgets::field(input))
}

/// A labelled text field.
pub(super) fn field_block(
    selector: &'static str,
    name: &str,
    input: &Entity<InputState>,
    mono_face: bool,
    palette: &Palette,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(label(name, palette))
        .child(input_box(selector, input, mono_face, palette))
}

/// Something that went wrong, in the card it went wrong in.
pub(super) fn error_line(selector: &'static str, text: SharedString, palette: &Palette) -> Div {
    div()
        .debug_selector(move || selector.into())
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(18.))
        .text_color(palette.danger)
        .child(text)
}
