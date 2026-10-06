//! Profiles and groups: what the panels remember and what their buttons
//! do. The panels themselves are drawn in `profiles.rs` (a contact, the
//! account's own profile) and `groups.rs` (a group, a new group).
//!
//! Nothing here calls a provider. Everything shown is read from the store;
//! a change either shows at once and is offered to the provider by the
//! engine in the background (a refusal puts it back and is said in the
//! window's problem line), or, when it cannot be shown before it happened
//! (creating a group, adding people, the invite link, leaving), is awaited
//! by the panel, which says in place what went wrong.

use super::shell::{ChatFilter, Overlay, Shell};
use super::widgets::{avatar_or, label, mono, switch, AvatarKind};
use crate::icons::{icon, IconName};
use crate::pictures::{PicturePicker, SystemPicker};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{ChatSummary, GroupPlace, StoreChange, SyncError};
use client_provider::{
    AccountId, ChatChange, ChatId, ChatKind, Contact, ContactId, GroupChange, JoinRequest,
    ParticipantChange, ProfileChange,
};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, ClipboardItem, Context, Div, ElementId, Entity, FontWeight, Image, ImageFormat,
    SharedString, Stateful, Subscription, Task, Window,
};
use std::future::Future;
use std::rc::Rc;
use std::sync::Arc;

/// How many people a list shows before it asks to narrow the search.
pub(super) const PEOPLE_SHOWN: usize = 30;
/// How many contacts a picker lists at once.
pub(super) const PICKER_ROWS: usize = 6;

/// What the info panel is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// A person: their profile.
    Contact {
        account: AccountId,
        contact: ContactId,
    },
    /// A group: its details and participants.
    Group { account: AccountId, group: ChatId },
}

/// The pages of the group panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum GroupTab {
    #[default]
    Overview,
    People,
    Manage,
}

/// What the "New group" form makes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum GroupKind {
    #[default]
    Group,
    /// A community: no participants to choose, and no chat.
    Community,
    /// A group inside this community, which is called `name`.
    InCommunity { community: ChatId, name: String },
}

/// The people of a community, asked for when its Members page is shown.
pub(super) enum CommunityPeople {
    Loading,
    Failed(SharedString),
    Loaded(Vec<ContactId>),
}

/// A question the panel asks before doing something that is not undone
/// with a click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Confirm {
    /// Take this group out of the community in the panel.
    Unlink(ChatId),
    /// Block (`true`) or unblock the contact.
    Block(bool),
    Leave,
    ResetLink,
    Remove(ContactId),
    RemovePicture,
}

/// A text being edited in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EditWhat {
    Subject,
    Description,
    OwnName,
    OwnAbout,
}

impl EditWhat {
    /// The longest text the field takes, as WhatsApp and the API have it.
    fn limit(self) -> usize {
        match self {
            Self::Subject => 100,
            Self::Description => 2048,
            Self::OwnName => 25,
            Self::OwnAbout => 139,
        }
    }
}

pub(super) struct Edit {
    pub(super) what: EditWhat,
    pub(super) input: Entity<InputState>,
    pub(super) error: Option<SharedString>,
    _subscription: Subscription,
}

/// Choosing people from the address book: for a new group, or to add to
/// one.
pub(super) struct People {
    pub(super) search: Entity<InputState>,
    pub(super) chosen: Vec<Contact>,
    _subscription: Subscription,
}

/// The "New group" form.
pub(super) struct NewGroupForm {
    pub(super) account: AccountId,
    pub(super) kind: GroupKind,
    pub(super) subject: Entity<InputState>,
    pub(super) people: People,
    /// The picture chosen for it: the JPEG to upload, and how it is drawn.
    pub(super) picture: Option<(Vec<u8>, Arc<Image>)>,
    /// One id for every attempt at creating this group.
    request_id: String,
    pub(super) busy: bool,
    pub(super) error: Option<SharedString>,
}

/// The number whose own profile is being edited.
pub(super) struct OwnProfilePanel {
    pub(super) account: AccountId,
    /// It was opened from Settings: closing goes back there.
    back_to_settings: bool,
}

/// What a picture is being chosen for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PictureFor {
    Own(AccountId),
    Group(AccountId, ChatId),
    NewGroup,
}

/// Everything the profile and group panels remember.
pub(super) struct SocialUi {
    /// What the info panel shows. A stack: a participant opened from a
    /// group goes back to the group.
    pub(super) stack: Vec<Target>,
    pub(super) tab: GroupTab,
    /// "Add participants" is showing in the group panel.
    pub(super) adding: Option<People>,
    /// "Add a group" is showing in a community's panel: the groups that
    /// can be linked to it.
    pub(super) linking: bool,
    /// The people of the community in the panel, once asked for.
    pub(super) community_people: Option<CommunityPeople>,
    /// What the next "New group" form makes.
    pub(super) next_group_kind: GroupKind,
    pub(super) confirm: Option<Confirm>,
    pub(super) edit: Option<Edit>,
    /// The participant whose actions are showing.
    pub(super) expanded: Option<ContactId>,
    pub(super) member_search: Option<(Entity<InputState>, Subscription)>,
    /// What went wrong with the last thing the panel waited for.
    pub(super) error: Option<SharedString>,
    /// Something worth saying that is not a failure.
    pub(super) note: Option<SharedString>,
    /// The panel is waiting for the provider.
    pub(super) busy: bool,
    pub(super) new_group: Option<NewGroupForm>,
    pub(super) own: Option<OwnProfilePanel>,
    pub(super) pictures: Rc<dyn PicturePicker>,
    /// The id of the request the panel is waiting on: the same while it
    /// is retried, a new one once it is answered.
    request_id: String,
    _work: Option<Task<()>>,
    _people: Option<Task<()>>,
    _creating: Option<Task<()>>,
    _picking: Option<Task<()>>,
}

fn new_request_id() -> String {
    client_core::new_client_id().to_string()
}

impl SocialUi {
    pub(super) fn new() -> Self {
        Self {
            stack: Vec::new(),
            tab: GroupTab::default(),
            adding: None,
            linking: false,
            community_people: None,
            next_group_kind: GroupKind::default(),
            confirm: None,
            edit: None,
            expanded: None,
            member_search: None,
            error: None,
            note: None,
            busy: false,
            new_group: None,
            own: None,
            pictures: Rc::new(SystemPicker),
            request_id: new_request_id(),
            _work: None,
            _people: None,
            _creating: None,
            _picking: None,
        }
    }

    /// What the info panel is showing.
    pub(super) fn target(&self) -> Option<&Target> {
        self.stack.last()
    }

    /// Clears what belongs to one page of the panel.
    fn clear_page(&mut self) {
        self.tab = GroupTab::default();
        self.adding = None;
        self.linking = false;
        self.community_people = None;
        self.confirm = None;
        self.edit = None;
        self.expanded = None;
        self.error = None;
        self.note = None;
        self.busy = false;
        self._work = None;
        self._people = None;
    }

    /// The panels were closed: nothing is kept.
    pub(super) fn close(&mut self) {
        self.clear_page();
        self.stack.clear();
        self.member_search = None;
        self.new_group = None;
        self.own = None;
        self._creating = None;
        self._picking = None;
    }
}

/// A search field that repaints the panel as it is typed in.
fn search_field(
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> (Entity<InputState>, Subscription) {
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
    let subscription = cx.subscribe_in(&input, window, |_, _, event: &InputEvent, _, cx| {
        if matches!(event, InputEvent::Change) {
            cx.notify();
        }
    });
    (input, subscription)
}

fn people_picker(window: &mut Window, cx: &mut Context<Shell>) -> People {
    let (search, _subscription) = search_field("Search your contacts", window, cx);
    People {
        search,
        chosen: Vec::new(),
        _subscription,
    }
}

impl Shell {
    // ----- the info panel: opening, going back --------------------------

    /// Opens the profile of a chat's contact, or the details of its group.
    pub(super) fn open_chat_info(
        &mut self,
        chat: &ChatSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let account = chat.account_id.clone();
        let target = match chat.kind {
            ChatKind::Group => Target::Group {
                account,
                group: chat.id.clone(),
            },
            ChatKind::Direct => Target::Contact {
                account,
                contact: ContactId::new(chat.id.as_str()),
            },
        };
        self.open_info(target, window, cx);
    }

    /// Opens the info panel on `target`.
    pub(super) fn open_info(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.social.close();
        self.overlay = Overlay::ContactInfo;
        self.mark_intro = false;
        self.show_target(target, window, cx);
    }

    /// Goes one level deeper: a participant from a group, a group from a
    /// contact's "groups in common".
    pub(super) fn push_info(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.social.clear_page();
        self.show_target(target, window, cx);
    }

    fn show_target(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.social.member_search = match &target {
            Target::Group { .. } => Some(search_field("Search participants", window, cx)),
            Target::Contact { .. } => None,
        };
        self.social.stack.push(target);
        self.overlay_focus.focus(window, cx);
        cx.notify();
    }

    /// Escape, a click outside, or the back arrow: the innermost thing
    /// that is open closes first.
    pub(super) fn info_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let social = &mut self.social;
        if social.busy {
            // The provider is being asked: its answer decides.
            return;
        }
        if social.confirm.take().is_some()
            || social.edit.take().is_some()
            || social.adding.take().is_some()
            || std::mem::take(&mut social.linking)
        {
            social.error = None;
            self.overlay_focus.focus(window, cx);
            return cx.notify();
        }
        if social.stack.len() > 1 {
            social.stack.pop();
            social.clear_page();
            let again = social.stack.pop();
            if let Some(target) = again {
                self.show_target(target, window, cx);
            }
            return;
        }
        self.close_overlay(window, cx);
    }

    /// Whether the back arrow has somewhere to go that is not "closed".
    pub(super) fn info_can_go_back(&self) -> bool {
        self.social.stack.len() > 1 || self.social.adding.is_some() || self.social.linking
    }

    // ----- small actions -------------------------------------------------

    /// Copies `text`, and says so in the panel.
    pub(super) fn copy_from_panel(
        &mut self,
        text: String,
        said: &'static str,
        cx: &mut Context<Self>,
    ) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.social.note = Some(said.into());
        self.social.error = None;
        cx.notify();
    }

    /// Mutes or unmutes a chat from its panel.
    pub(super) fn mute_from_panel(&mut self, account: &AccountId, chat: &ChatId, muted: bool) {
        self.engine
            .update_chat(account, chat, ChatChange::Muted(muted));
    }

    /// Opens the conversation `chat` and its search.
    pub(super) fn search_from_panel(
        &mut self,
        chat: ChatId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_overlay(window, cx);
        self.open_chat(chat, Some(window), cx);
        self.open_thread_search(window, cx);
    }

    /// Opens the conversation with a contact, creating the chat here if
    /// there is none yet.
    pub(super) fn message_from_panel(
        &mut self,
        account: &AccountId,
        contact: &ContactId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known = self
            .engine
            .store()
            .contact(account, contact)
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                let mut bare = Contact::new(account.clone(), contact.clone());
                bare.phone = contact
                    .as_str()
                    .starts_with('+')
                    .then(|| contact.to_string());
                bare
            });
        match self.engine.chat_with_contact(&known) {
            Ok(chat) => {
                self.close_overlay(window, cx);
                self.show_chat(chat, Some(window), cx);
            }
            Err(error) => self.social.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// Shows a conversation that was just found or made for a contact:
    /// in the list whatever its filter was, and open. The profile's
    /// "Message" and a contact card's both end here.
    pub(super) fn show_chat(
        &mut self,
        chat: ChatId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        self.filter = ChatFilter::All;
        self.reload_chats(cx);
        self.open_chat(chat, window, cx);
    }

    /// Opens the profile of somebody a conversation names: who sent a
    /// group's message, who a message mentions.
    pub(super) fn open_contact_profile(
        &mut self,
        account: AccountId,
        contact: ContactId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_info(Target::Contact { account, contact }, window, cx);
    }

    /// Asks before something that is not undone with a click.
    pub(super) fn ask(&mut self, confirm: Confirm, cx: &mut Context<Self>) {
        self.social.confirm = Some(confirm);
        self.social.error = None;
        self.social.note = None;
        cx.notify();
    }

    /// "Yes" to the question the panel asked.
    pub(super) fn confirmed(&mut self, cx: &mut Context<Self>) {
        let (Some(confirm), Some(target)) =
            (self.social.confirm.clone(), self.social.target().cloned())
        else {
            return;
        };
        match (confirm, target) {
            (Confirm::Block(blocked), Target::Contact { account, contact }) => {
                self.engine.set_blocked(&account, &contact, blocked);
                self.social.confirm = None;
            }
            (Confirm::Remove(contact), Target::Group { account, group }) => {
                self.engine.change_participant(
                    &account,
                    &group,
                    &contact,
                    ParticipantChange::Remove,
                );
                self.social.confirm = None;
                self.social.expanded = None;
            }
            (Confirm::RemovePicture, Target::Group { account, group }) => {
                self.engine.set_group_picture(&account, &group, None);
                self.social.confirm = None;
            }
            (Confirm::ResetLink, Target::Group { account, group }) => {
                self.invite_link(account, group, true, cx);
            }
            (Confirm::Leave, Target::Group { account, group }) => {
                self.leave_group(account, group, cx)
            }
            (Confirm::Unlink(linked), Target::Group { account, group }) => {
                self.unlink_group(account, group, linked, cx)
            }
            _ => self.social.confirm = None,
        }
        cx.notify();
    }

    /// Runs something the panel waits for on the engine's runtime, and
    /// hands its outcome (a sentence when it failed) back to the view.
    fn await_engine<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl Future<Output = Result<T, SyncError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, SharedString>, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        let handle = self.engine.runtime().spawn(work);
        cx.spawn(async move |this, cx| {
            let outcome = match handle.await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(client_core::failure_sentence(&error).into()),
                Err(error) => Err(error.to_string().into()),
            };
            this.update(cx, |this, cx| {
                done(this, outcome, cx);
                cx.notify();
            })
            .ok();
        })
    }

    /// [`await_engine`](Self::await_engine) for an outcome that needs the
    /// window: to open a panel.
    fn await_engine_in<T: Send + 'static>(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        work: impl Future<Output = Result<T, SyncError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, SharedString>, &mut Window, &mut Context<Self>) + 'static,
    ) -> Task<()> {
        let handle = self.engine.runtime().spawn(work);
        cx.spawn_in(window, async move |this, cx| {
            let outcome = match handle.await {
                Ok(Ok(value)) => Ok(value),
                Ok(Err(error)) => Err(client_core::failure_sentence(&error).into()),
                Err(error) => Err(error.to_string().into()),
            };
            this.update_in(cx, |this, window, cx| {
                done(this, outcome, window, cx);
                cx.notify();
            })
            .ok();
        })
    }

    // ----- groups --------------------------------------------------------

    /// Changes one of a group's settings.
    pub(super) fn change_group(&mut self, change: GroupChange, cx: &mut Context<Self>) {
        if let Some(Target::Group { account, group }) = self.social.target().cloned() {
            self.engine.update_group(&account, &group, change);
        }
        cx.notify();
    }

    /// Promotes, demotes or (after the question) removes a participant.
    pub(super) fn change_participant(
        &mut self,
        contact: ContactId,
        change: ParticipantChange,
        cx: &mut Context<Self>,
    ) {
        if let Some(Target::Group { account, group }) = self.social.target().cloned() {
            self.engine
                .change_participant(&account, &group, &contact, change);
        }
        self.social.expanded = None;
        cx.notify();
    }

    /// Approves or rejects a request to join the group in the panel.
    pub(super) fn answer_request(
        &mut self,
        request: JoinRequest,
        approve: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(Target::Group { account, group }) = self.social.target().cloned() {
            self.engine
                .answer_join_request(&account, &group, &request, approve);
        }
        cx.notify();
    }

    /// Reads (or resets) the group's invite link. It arrives in the store.
    pub(super) fn invite_link(
        &mut self,
        account: AccountId,
        group: ChatId,
        reset: bool,
        cx: &mut Context<Self>,
    ) {
        if self.social.busy {
            return;
        }
        self.social.busy = true;
        self.social.error = None;
        self.social.note = None;
        let (engine, request_id) = (self.engine.clone(), self.social.request_id.clone());
        let work = async move {
            engine
                .invite_link(&account, &group, reset, &request_id)
                .await
        };
        let task = self.await_engine(cx, work, move |this, outcome, _| {
            let social = &mut this.social;
            social.busy = false;
            match outcome {
                Ok(_) => {
                    social.confirm = None;
                    social.request_id = new_request_id();
                    if reset {
                        social.note = Some("The old link no longer works.".into());
                    }
                }
                Err(sentence) => social.error = Some(sentence),
            }
        });
        self.social._work = Some(task);
        cx.notify();
    }

    fn leave_group(&mut self, account: AccountId, group: ChatId, cx: &mut Context<Self>) {
        if self.social.busy {
            return;
        }
        self.social.busy = true;
        self.social.error = None;
        let (engine, request_id) = (self.engine.clone(), self.social.request_id.clone());
        let work = async move { engine.leave_group(&account, &group, &request_id).await };
        let task = self.await_engine(cx, work, |this, outcome, _| {
            let social = &mut this.social;
            social.busy = false;
            match outcome {
                Ok(()) => {
                    social.confirm = None;
                    social.request_id = new_request_id();
                    social.tab = GroupTab::Overview;
                }
                // The question stays: the button is the way to try again.
                Err(sentence) => social.error = Some(sentence),
            }
        });
        self.social._work = Some(task);
        cx.notify();
    }

    /// Opens "Add participants" in the group panel.
    pub(super) fn begin_adding(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let people = people_picker(window, cx);
        people
            .search
            .update(cx, |field, cx| field.focus(window, cx));
        self.social.adding = Some(people);
        self.social.error = None;
        self.social.note = None;
        if let Some(Target::Group { account, .. }) = self.social.target() {
            self.engine
                .want_contacts(account, std::time::Duration::from_secs(5 * 60));
        }
        cx.notify();
    }

    /// The contacts a picker lists for what is typed in it, without those
    /// in `without` (already in the group).
    pub(super) fn picker_rows(
        &self,
        account: &AccountId,
        people: &People,
        without: &[ContactId],
        cx: &gpui_kit::App,
    ) -> Vec<Contact> {
        let typed = people.search.read(cx).value().trim().to_owned();
        self.engine
            .store()
            .contacts(account, Some(&typed), PICKER_ROWS + without.len().min(64))
            .unwrap_or_default()
            .into_iter()
            .filter(|contact| !without.contains(&contact.id))
            .take(PICKER_ROWS)
            .collect()
    }

    /// Ticks or unticks a contact in the picker that is open.
    pub(super) fn toggle_pick(&mut self, contact: Contact, cx: &mut Context<Self>) {
        let people = match (&mut self.social.adding, &mut self.social.new_group) {
            (Some(people), _) => people,
            (None, Some(form)) => {
                form.error = None;
                &mut form.people
            }
            (None, None) => return,
        };
        match people
            .chosen
            .iter()
            .position(|known| known.id == contact.id)
        {
            Some(at) => {
                people.chosen.remove(at);
            }
            None => people.chosen.push(contact),
        }
        cx.notify();
    }

    /// "Add": the chosen contacts go into the group. Each has its own
    /// answer; who could not be added is said by name.
    pub(super) fn submit_adding(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Group { account, group }) = self.social.target().cloned() else {
            return;
        };
        let Some(people) = &self.social.adding else {
            return;
        };
        if self.social.busy {
            return;
        }
        if people.chosen.is_empty() {
            self.social.error = Some("Choose who to add.".into());
            return cx.notify();
        }
        let chosen = people.chosen.clone();
        let ids: Vec<ContactId> = chosen.iter().map(|contact| contact.id.clone()).collect();
        self.social.busy = true;
        self.social.error = None;
        let (engine, request_id) = (self.engine.clone(), self.social.request_id.clone());
        let work = async move {
            engine
                .add_participants(&account, &group, &ids, &request_id)
                .await
        };
        let task = self.await_engine(cx, work, move |this, outcome, _| {
            let social = &mut this.social;
            social.busy = false;
            match outcome {
                Ok(outcomes) => {
                    social.request_id = new_request_id();
                    social.adding = None;
                    social.tab = GroupTab::People;
                    let refused: Vec<String> = outcomes
                        .iter()
                        .filter_map(|outcome| {
                            let name = chosen
                                .iter()
                                .find(|contact| contact.id == outcome.contact)
                                .map(Contact::display_name)
                                .unwrap_or_else(|| outcome.contact.to_string());
                            client_core::outcome_sentence(&name, ParticipantChange::Add, outcome)
                        })
                        .collect();
                    if !refused.is_empty() {
                        social.error = Some(refused.join(" ").into());
                    }
                }
                Err(sentence) => social.error = Some(sentence),
            }
        });
        self.social._work = Some(task);
        cx.notify();
    }

    // ----- a new group ---------------------------------------------------

    /// Opens the form that makes `kind` for the number on screen.
    pub(super) fn open_new_group(
        &mut self,
        kind: GroupKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.social.next_group_kind = kind;
        self.open_overlay(Overlay::NewGroup, window, cx);
    }

    /// Opens the "New group" form for the number on screen.
    pub(super) fn begin_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let kind = std::mem::take(&mut self.social.next_group_kind);
        self.social.close();
        let Some(account) = self.account.clone() else {
            self.overlay = Overlay::None;
            return;
        };
        let subject = cx.new(|cx| {
            InputState::new(window, cx).placeholder(match kind {
                GroupKind::Community => "Community name",
                _ => "Group name",
            })
        });
        subject.update(cx, |field, cx| field.focus(window, cx));
        self.engine
            .want_contacts(&account, std::time::Duration::from_secs(5 * 60));
        self.social.new_group = Some(NewGroupForm {
            account,
            kind,
            subject,
            people: people_picker(window, cx),
            picture: None,
            request_id: new_request_id(),
            busy: false,
            error: None,
        });
    }

    /// Escape or Cancel on "New group". While the group is being created
    /// the answer decides.
    pub(super) fn leave_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.social.new_group.as_ref().is_some_and(|form| form.busy) {
            return;
        }
        self.close_overlay(window, cx);
    }

    /// "Create": the group is made by the provider, then shown.
    pub(super) fn submit_new_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.social.new_group.as_mut() else {
            return;
        };
        if form.busy {
            return;
        }
        let subject = form.subject.read(cx).value().trim().to_owned();
        if subject.is_empty() {
            // The keyboard goes where the missing thing is typed.
            form.subject
                .clone()
                .update(cx, |field, cx| field.focus(window, cx));
        }
        let Some(form) = self.social.new_group.as_mut() else {
            return;
        };
        form.error = if subject.is_empty() {
            Some("Give the group a name.".into())
        } else if subject.chars().count() > EditWhat::Subject.limit() {
            Some("A group's name has at most 100 characters.".into())
        } else if form.people.chosen.is_empty() && form.kind != GroupKind::Community {
            Some("Choose at least one participant.".into())
        } else {
            None
        };
        if form.error.is_some() {
            return cx.notify();
        }
        form.busy = true;
        let chosen = form.people.chosen.clone();
        let ids: Vec<ContactId> = chosen.iter().map(|contact| contact.id.clone()).collect();
        let picture = form.picture.as_ref().map(|(jpeg, _)| jpeg.clone());
        let (account, request_id) = (form.account.clone(), form.request_id.clone());
        let kind = form.kind.clone();
        let place = match &kind {
            GroupKind::Group => GroupPlace::Plain,
            GroupKind::Community => GroupPlace::Community,
            GroupKind::InCommunity { community, .. } => GroupPlace::InCommunity(community.clone()),
        };
        let engine = self.engine.clone();
        let of_account = account.clone();
        let work = async move {
            engine
                .create_group_in(&account, &subject, ids, picture, &request_id, place)
                .await
        };
        let task =
            self.await_engine_in(
                window,
                cx,
                work,
                move |this, outcome, window, cx| match outcome {
                    // A community has no chat, and a group made inside one is
                    // seen where it is: in the community's panel.
                    Ok(created) if kind != GroupKind::Group => {
                        let community = match kind {
                            GroupKind::InCommunity { community, .. } => community,
                            _ => created.chat,
                        };
                        this.reload_chats(cx);
                        this.open_info(
                            Target::Group {
                                account: of_account,
                                group: community,
                            },
                            window,
                            cx,
                        );
                    }
                    Ok(created) => {
                        this.social.close();
                        this.overlay = Overlay::None;
                        this.filter = ChatFilter::All;
                        this.reload_chats(cx);
                        this.open_chat(created.chat.clone(), None, cx);
                        if !created.missing.is_empty() {
                            let names: Vec<String> = created
                                .missing
                                .iter()
                                .map(|missing| {
                                    chosen
                                        .iter()
                                        .find(|contact| &contact.id == missing)
                                        .map(Contact::display_name)
                                        .unwrap_or_else(|| missing.to_string())
                                })
                                .collect();
                            // Said like every other thing that did not happen.
                            this.engine.store().notify(StoreChange::Problem {
                                message: format!(
                            "The group was created without {}: their privacy settings do not \
                             allow adding them. Send them the invite link instead.",
                            names.join(", ")
                        ),
                            });
                        }
                    }
                    Err(sentence) => {
                        // Back to the form with what was typed: the same request
                        // can be sent again and makes one group.
                        if let Some(form) = this.social.new_group.as_mut() {
                            form.busy = false;
                            form.error = Some(sentence);
                        }
                    }
                },
            );
        self.social._creating = Some(task);
        cx.notify();
    }

    // ----- a community's groups and people -----------------------------

    /// Opens the list of groups that can be added to the community in
    /// the panel.
    pub(super) fn begin_linking(&mut self, cx: &mut Context<Self>) {
        if let Some(Target::Group { account, .. }) = self.social.target() {
            // Which groups the number administers comes from the listing.
            self.engine.want_groups(account);
        }
        self.social.linking = true;
        self.social.error = None;
        self.social.note = None;
        cx.notify();
    }

    /// Adds `group` to the community in the panel.
    pub(super) fn link_group(&mut self, group: ChatId, cx: &mut Context<Self>) {
        let Some(Target::Group {
            account,
            group: community,
        }) = self.social.target().cloned()
        else {
            return;
        };
        if self.social.busy {
            return;
        }
        self.social.busy = true;
        self.social.error = None;
        let (engine, request_id) = (self.engine.clone(), self.social.request_id.clone());
        let work = async move {
            engine
                .link_subgroup(&account, &community, &group, &request_id)
                .await
        };
        let task = self.await_engine(cx, work, |this, outcome, _| {
            let social = &mut this.social;
            social.busy = false;
            match outcome {
                Ok(()) => {
                    social.linking = false;
                    social.request_id = new_request_id();
                    social.note = Some("The group is in the community now.".into());
                }
                // The picker stays: another group can be chosen.
                Err(sentence) => social.error = Some(sentence),
            }
        });
        self.social._work = Some(task);
        cx.notify();
    }

    /// Takes `linked` out of the community `community`, after the question.
    fn unlink_group(
        &mut self,
        account: AccountId,
        community: ChatId,
        linked: ChatId,
        cx: &mut Context<Self>,
    ) {
        if self.social.busy {
            return;
        }
        self.social.busy = true;
        self.social.error = None;
        let engine = self.engine.clone();
        let work = async move { engine.unlink_subgroup(&account, &community, &linked).await };
        let task = self.await_engine(cx, work, |this, outcome, _| {
            let social = &mut this.social;
            social.busy = false;
            match outcome {
                Ok(()) => {
                    social.confirm = None;
                    social.note = Some("The group is out of the community.".into());
                }
                // The question stays: its button is the way to try again.
                Err(sentence) => social.error = Some(sentence),
            }
        });
        self.social._work = Some(task);
        cx.notify();
    }

    /// Asks for the people of the community in the panel, unless they
    /// are being asked for or were. Looking again is "Try again".
    pub(super) fn load_community_people(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Group { account, group }) = self.social.target().cloned() else {
            return;
        };
        if matches!(
            self.social.community_people,
            Some(CommunityPeople::Loading | CommunityPeople::Loaded(_))
        ) {
            return;
        }
        self.social.community_people = Some(CommunityPeople::Loading);
        let engine = self.engine.clone();
        let asked = group.clone();
        let work = async move { engine.community_participants(&account, &asked).await };
        let task = self.await_engine(cx, work, move |this, outcome, _| {
            // The panel moved on to another page meanwhile.
            if this.social.community_people.is_none()
                || this.social.target().map(|target| match target {
                    Target::Group { group, .. } => group.clone(),
                    Target::Contact { .. } => ChatId::new(""),
                }) != Some(group)
            {
                return;
            }
            this.social.community_people = Some(match outcome {
                Ok(people) => CommunityPeople::Loaded(people),
                Err(sentence) => CommunityPeople::Failed(sentence),
            });
        });
        self.social._people = Some(task);
        cx.notify();
    }

    // ----- pictures ------------------------------------------------------

    /// Asks for an image file and uses it as a picture. The dialog is the
    /// system's and does not hold this thread; reading and converting the
    /// file happens on the engine's blocking pool.
    pub(super) fn choose_picture(&mut self, purpose: PictureFor, cx: &mut Context<Self>) {
        let picked = self.social.pictures.pick(cx);
        let runtime = self.engine.runtime().clone();
        self.social._picking = Some(cx.spawn(async move |this, cx| {
            let Some(path) = picked.await else {
                return;
            };
            // Reading and converting is blocking work: on the blocking
            // pool, awaited by a task of the runtime, which is what wakes
            // this view when it is done.
            let work = runtime.spawn(async move {
                tokio::task::spawn_blocking(move || crate::pictures::load(&path))
                    .await
                    .unwrap_or_else(|error| Err(error.to_string()))
            });
            let loaded = match work.await {
                Ok(loaded) => loaded,
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| {
                this.picture_chosen(purpose, loaded);
                cx.notify();
            })
            .ok();
        }));
    }

    fn picture_chosen(&mut self, purpose: PictureFor, loaded: Result<Vec<u8>, String>) {
        match (purpose, loaded) {
            (PictureFor::NewGroup, loaded) => {
                let Some(form) = self.social.new_group.as_mut() else {
                    return;
                };
                match loaded {
                    Ok(jpeg) => {
                        let drawn = Arc::new(Image::from_bytes(ImageFormat::Jpeg, jpeg.clone()));
                        form.picture = Some((jpeg, drawn));
                        form.error = None;
                    }
                    Err(sentence) => form.error = Some(sentence.into()),
                }
            }
            (_, Err(sentence)) => self.social.error = Some(sentence.into()),
            (PictureFor::Own(account), Ok(jpeg)) => {
                self.social.error = None;
                self.engine.set_profile_picture(&account, Some(jpeg));
            }
            (PictureFor::Group(account, group), Ok(jpeg)) => {
                self.social.error = None;
                self.engine.set_group_picture(&account, &group, Some(jpeg));
            }
        }
    }

    /// Chooses pictures through `picker` instead of the system's dialog:
    /// for tests, which must never open one.
    #[cfg(test)]
    pub(super) fn set_picture_picker(&mut self, picker: Rc<dyn PicturePicker>) {
        self.social.pictures = picker;
    }

    // ----- editing a text in place ---------------------------------------

    /// Opens the field for `what`, holding what it is now.
    pub(super) fn begin_edit(
        &mut self,
        what: EditWhat,
        current: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let placeholder = match what {
            EditWhat::Subject => "Group name",
            EditWhat::Description => "What the group is for",
            EditWhat::OwnName => "Your name on WhatsApp",
            EditWhat::OwnAbout => "About",
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |field, cx| {
            field.set_value(current, window, cx);
            field.focus(window, cx);
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_edit(window, cx);
                }
            });
        self.social.edit = Some(Edit {
            what,
            input,
            error: None,
            _subscription: subscription,
        });
        self.social.confirm = None;
        self.social.error = None;
        self.social.note = None;
        cx.notify();
    }

    /// "Save": the text is shown at once and offered to the provider.
    pub(super) fn save_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = self.social.edit.as_mut() else {
            return;
        };
        let what = edit.what;
        let text = edit.input.read(cx).value().trim().to_owned();
        edit.error = if text.chars().count() > what.limit() {
            Some(format!("At most {} characters.", what.limit()).into())
        } else if text.is_empty() && what != EditWhat::Description {
            Some("Type something first.".into())
        } else {
            None
        };
        if edit.error.is_some() {
            // Back in the field: the text is there to be corrected.
            edit.input
                .clone()
                .update(cx, |field, cx| field.focus(window, cx));
            return cx.notify();
        }
        self.overlay_focus.focus(window, cx);
        let own = self.social.own.as_ref().map(|own| own.account.clone());
        match (what, self.social.target().cloned(), own) {
            (EditWhat::Subject, Some(Target::Group { account, group }), _) => {
                self.engine
                    .update_group(&account, &group, GroupChange::Subject(text));
            }
            (EditWhat::Description, Some(Target::Group { account, group }), _) => {
                self.engine
                    .update_group(&account, &group, GroupChange::Description(text));
            }
            (EditWhat::OwnName, _, Some(account)) => {
                self.engine
                    .update_profile(&account, ProfileChange::Name(text));
            }
            (EditWhat::OwnAbout, _, Some(account)) => {
                self.engine
                    .update_profile(&account, ProfileChange::About(text));
            }
            _ => {}
        }
        self.social.edit = None;
        cx.notify();
    }

    // ----- the account's own profile -------------------------------------

    /// Opens the editor of a number's own WhatsApp profile: its picture,
    /// the name others see and its About text.
    ///
    /// THE ENTRY POINT for anything that wants to offer "Edit WhatsApp
    /// profile" for a number (Settings does; a menu on the rail can).
    pub fn open_own_profile(
        &mut self,
        account: AccountId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.engine.capabilities().profile_edit {
            return;
        }
        let back_to_settings = self.overlay == Overlay::Settings;
        self.social.close();
        self.social.own = Some(OwnProfilePanel {
            account,
            back_to_settings,
        });
        self.overlay = Overlay::OwnProfile;
        self.mark_intro = false;
        self.overlay_focus.focus(window, cx);
        cx.notify();
    }

    /// Escape, a click outside or the close button on the own profile.
    pub(super) fn leave_own_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.social.confirm.take().is_some() || self.social.edit.take().is_some() {
            self.overlay_focus.focus(window, cx);
            return cx.notify();
        }
        let back = self
            .social
            .own
            .as_ref()
            .is_some_and(|own| own.back_to_settings);
        if back {
            self.social.close();
            self.overlay = Overlay::Settings;
            self.overlay_focus.focus(window, cx);
            cx.notify();
        } else {
            self.close_overlay(window, cx);
        }
    }

    /// "Remove picture", after the question.
    pub(super) fn remove_own_picture(&mut self, cx: &mut Context<Self>) {
        if let Some(own) = &self.social.own {
            self.engine.set_profile_picture(&own.account, None);
        }
        self.social.confirm = None;
        cx.notify();
    }
}

// ----- pieces the panels share ----------------------------------------------

/// A row of the panel that does something: an icon and what it does.
pub(super) fn action_row(
    id: &'static str,
    glyph: IconName,
    text: impl Into<SharedString>,
    danger: bool,
    palette: &Palette,
) -> Stateful<Div> {
    let (hover, ring) = (palette.hover, palette.accent);
    let ink = if danger { palette.danger } else { palette.text };
    div()
        .id(id)
        .debug_selector(move || id.into())
        .h(px(36.))
        .px_2()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(gpui_kit::transparent_black())
        .flex()
        .items_center()
        .gap_3()
        .cursor_pointer()
        .text_size(metrics::TEXT_BODY())
        .text_color(ink)
        .tab_index(0)
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
        .child(icon(
            glyph,
            px(16.),
            if danger { palette.danger } else { palette.icon },
        ))
        .child(div().flex_1().min_w_0().truncate().child(text.into()))
}

/// A part of the panel under a mono title.
pub(super) fn block(title: &str, palette: &Palette) -> Div {
    div()
        .px_4()
        .py_3()
        .border_b_1()
        .border_color(palette.border)
        .flex()
        .flex_col()
        .gap_1()
        .child(label(title, palette))
}

/// Running text: wraps, at the body size.
pub(super) fn paragraph(text: impl Into<SharedString>, palette: &Palette) -> Div {
    div()
        .text_size(metrics::TEXT_BODY())
        .line_height(px(21.))
        .text_color(palette.text)
        .child(text.into())
}

/// A quiet line: an explanation, a state.
pub(super) fn hint(text: impl Into<SharedString>, palette: &Palette) -> Div {
    div()
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(18.))
        .text_color(palette.text_muted)
        .child(text.into())
}

/// A mono tag in brackets: `[ ADMIN ]`.
pub(super) fn badge(text: &str, palette: &Palette) -> Div {
    mono(format!("[ {} ]", text.to_uppercase())).text_color(palette.text_muted)
}

/// The question the panel asks before doing something for good: the
/// words, and room for its two buttons.
pub(super) fn question(
    selector: &'static str,
    text: impl Into<SharedString>,
    palette: &Palette,
) -> Div {
    div()
        .debug_selector(move || selector.into())
        .mx_4()
        .my_2()
        .p_3()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(palette.border)
        .bg(palette.background)
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(19.))
                .text_color(palette.text)
                .child(text.into()),
        )
}

/// A setting with a switch: what it is, what it means, and the switch.
pub(super) fn toggle_row(
    id: &'static str,
    title: &'static str,
    detail: &'static str,
    on: bool,
    palette: &Palette,
) -> (Div, Stateful<Div>) {
    let words = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(div().text_size(metrics::TEXT_BODY()).child(title))
        .child(hint(detail, palette));
    (words, switch(id, on, palette))
}

/// The top of a profile: a large picture, a name, and lines under it.
pub(super) fn hero(picture: Div, name: SharedString, lines: Vec<Div>, palette: &Palette) -> Div {
    div()
        .px_4()
        .pt_4()
        .pb_3()
        .border_b_1()
        .border_color(palette.border)
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .child(picture)
        .child(
            div()
                .debug_selector(|| "profile-name".into())
                .max_w_full()
                .truncate()
                .text_size(metrics::TEXT_TITLE())
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(palette.text)
                .child(name),
        )
        .children(lines)
}

impl Shell {
    /// A person (or a group) in a list: picture, name, a second line.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn person_row(
        &self,
        id: impl Into<ElementId>,
        selector: String,
        account: &AccountId,
        subject: &ChatId,
        name: &str,
        detail: Option<String>,
        kind: AvatarKind,
        palette: &Palette,
    ) -> Stateful<Div> {
        let (hover, ring) = (palette.hover, palette.accent);
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .min_h(px(44.))
            .px_2()
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .tab_index(0)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(ring))
            .child(match kind {
                // A person has their colour wherever they are listed.
                AvatarKind::Person => super::senders::person_avatar(
                    self.media.avatar(account, subject),
                    name,
                    px(32.),
                    self.senders
                        .of(account, None, &ContactId::new(subject.as_str().to_owned()))
                        .tone,
                    palette,
                ),
                AvatarKind::Group => avatar_or(
                    self.media.avatar(account, subject),
                    name,
                    kind,
                    px(32.),
                    palette,
                ),
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .text_color(palette.text)
                            .child(SharedString::from(name.to_owned())),
                    )
                    .children(detail.map(|detail| mono(detail).text_color(palette.text_muted))),
            )
    }
}
