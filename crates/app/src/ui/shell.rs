//! The root view: owns the UI state and keeps it in step with the store.

use super::bubble::{MessageRow, Row};
use super::media::{FileState, MediaShelf};
use super::senders::{Quoted, Senders};
use super::widgets::{grid_mark, grid_mark_in};
use crate::audio::{self, AudioOutput, Clip, Playback, Player, Shape, Speed, SystemOutput};
use crate::format::{day_label, same_day};
use crate::login::{Identity, IdentityLoader};
use crate::motion::{self, Entrance};
use crate::settings::{self, ThemeChoice};
use crate::theme::px;
use crate::theme::{fonts, metrics, palette};
use client_core::{ChatSummary, SearchHit, Store, StoreChange, StoredMessage, SyncEngine};
use client_provider::{
    Account, AccountId, ChatChange, ChatId, ChatKind, Contact, Media, MediaKind, MessageContent,
    MessageId, Timestamp,
};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, Context, Entity, EventEmitter, FocusHandle, FollowMode, FontFeatures, KeyDownEvent,
    ListAlignment, ListOffset, ListState, SharedString, Subscription, Task,
    UniformListScrollHandle, Window,
};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// How many contacts "New chat" lists at once.
const NEW_CHAT_ROWS: usize = 7;
/// How old the address book may be when "New chat" opens before a fresh
/// copy is fetched behind it.
const CONTACTS_FRESH: Duration = Duration::from_secs(5 * 60);

/// How long a problem stays on screen.
const PROBLEM_SHOWN: Duration = Duration::from_secs(8);

/// How often a playing clip's position is repainted.
const AUDIO_TICK: Duration = Duration::from_millis(100);

/// Audio in the open chat: the player, the clips decoded so far, and what
/// the user is waiting to hear.
pub(super) struct AudioDeck {
    pub(super) player: Player,
    /// Decoded clips, by URL. Let go when the chat is left.
    pub(super) clips: HashMap<String, Arc<Clip>>,
    /// The output device chosen in the settings; `None` is the system's.
    pub(super) output_device: Option<String>,
    /// The clip being fetched or decoded because play was pressed.
    pub(super) waiting: Option<String>,
    /// Clips that could not be fetched or decoded, with the reason.
    /// Pressing play again retries.
    pub(super) failed: HashMap<String, SharedString>,
    /// The clip that could not be played for want of an output, and why.
    pub(super) no_output: Option<(String, SharedString)>,
    _decoding: Option<Task<()>>,
    _watching: Option<Task<()>>,
}

impl AudioDeck {
    fn new(output: Box<dyn AudioOutput>, speed: crate::audio::Speed) -> Self {
        let mut player = Player::new(output);
        player.set_speed(speed);
        Self {
            player,
            clips: HashMap::new(),
            output_device: None,
            waiting: None,
            failed: HashMap::new(),
            no_output: None,
            _decoding: None,
            _watching: None,
        }
    }

    fn fail(&mut self, url: &str, reason: String) {
        self.failed.insert(url.to_owned(), reason.into());
        if self.waiting.as_deref() == Some(url) {
            self.waiting = None;
        }
    }

    /// The chat's messages were read again. A message whose file goes by
    /// another URL now (the provider stores a file it used to fetch on
    /// demand) keeps its decoded clip, and its place in the player.
    fn follow(&mut self, old: &[Row], new: &[Row]) {
        if self.clips.is_empty() {
            return;
        }
        let audio = |row: &Row| match row {
            Row::Message(row) => match &row.stored.message.content {
                MessageContent::Media(media) => media
                    .source
                    .as_ref()
                    .map(|source| (row_key(&row.stored.message), source.to_string())),
                _ => None,
            },
            Row::Day(_) => None,
        };
        let decoded: HashMap<String, String> = old
            .iter()
            .filter_map(audio)
            .filter(|(_, url)| self.clips.contains_key(url))
            .collect();
        for (key, url) in new.iter().filter_map(audio) {
            let Some(was) = decoded.get(&key).filter(|was| **was != url) else {
                continue;
            };
            if let Some(clip) = self.clips.get(was).cloned() {
                self.clips.insert(url.clone(), clip);
            }
            self.player.rename(was, &url);
        }
    }

    /// The chat was closed (or another opened): silence, and nothing kept.
    fn leave_chat(&mut self) {
        self.player.stop();
        self.clips.clear();
        self.waiting = None;
        self.failed.clear();
        self.no_output = None;
        self._decoding = None;
        self._watching = None;
    }

    /// What the bubbles need to know to draw themselves.
    pub(super) fn view(&self) -> AudioView {
        AudioView {
            current: self.player.current().map(|url| {
                let url = url.to_owned();
                let playback = self.player.playback(&url);
                (url, playback)
            }),
            speed: self.player.speed(),
            waiting: self.waiting.clone(),
            failed: self.failed.clone(),
            no_output: self.no_output.clone(),
        }
    }
}

/// The state of audio as one frame draws it.
pub struct AudioView {
    /// The clip in the player, and what it is doing.
    pub current: Option<(String, Playback)>,
    pub speed: Speed,
    pub waiting: Option<String>,
    pub failed: HashMap<String, SharedString>,
    pub no_output: Option<(String, SharedString)>,
}

/// Messages loaded when a chat is opened, and added per step when the user
/// scrolls back.
const MESSAGE_WINDOW: usize = 120;
/// Message search results shown under the matching chats.
const MAX_HITS: usize = 40;
/// How many of an account's matches are read to find those of one chat.
const THREAD_SEARCH_SCAN: usize = 400;

/// Where the session's credentials come from, which decides what the
/// settings menu offers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionKind {
    /// The demo provider: nothing to sign out of.
    #[default]
    Demo,
    /// Signed in to wuapi from the application; the key is in the OS
    /// keychain and "Sign out" removes it.
    Keychain,
    /// Signed in from the application, but the key is not kept: the next
    /// start asks again. "Sign out" ends the session.
    Unsaved,
    /// The key came from the environment; the application cannot forget it.
    Environment,
}

/// How the window starts.
#[derive(Clone, Default)]
pub struct ShellOptions {
    /// Open the chat at this position of the list once it has loaded.
    pub open_chat: Option<usize>,
    /// What kind of session this is.
    pub session: SessionKind,
    /// Who is signed in, when the provider can say.
    pub identity: Option<IdentityLoader>,
    /// What to say about where the chats are kept, when they are not
    /// being saved to disk.
    pub storage_note: Option<String>,
    /// How files are picked: the system's dialog, unless a test brings
    /// its own.
    pub pick_files: Option<PickFiles>,
    /// How pictures and files leave the application (the clipboard, the
    /// save dialogs, the Downloads folder): the system's, unless a test
    /// brings its own.
    pub out: Option<super::media_out::MediaOut>,
}

/// What a file dialog is opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    /// One picture.
    Image,
    /// Any files, several at once.
    Files,
}

/// Asks the user for files without blocking the interface: the answer
/// arrives later, `None` when the dialog was dismissed.
pub type PickFiles =
    Rc<dyn Fn(PickKind, &mut gpui_kit::App) -> Task<Option<Vec<std::path::PathBuf>>>>;

/// The system's own file dialog (the desktop portal on Linux, the native
/// panels on macOS and Windows), through the toolkit.
pub fn system_file_dialog() -> PickFiles {
    Rc::new(|kind, cx| {
        let answer = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: true,
            directories: false,
            multiple: kind == PickKind::Files,
            prompt: None,
        });
        cx.spawn(async move |_| answer.await.ok()?.ok()?)
    })
}

/// What is showing on top of the three panes. One thing at a time; Escape
/// or a click outside closes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Overlay {
    None,
    /// The chat list's menu.
    ListMenu,
    /// The open conversation's menu.
    ChatMenu,
    /// The menu of a row of the chat list (right-click, or its arrow).
    RowMenu,
    /// "New chat": a number to write to.
    NewChat,
    /// The settings panel.
    Settings,
    /// Who the open conversation is with.
    ContactInfo,
    /// "Sign out?", with what it deletes.
    ConfirmSignOut,
    /// What is about to be sent: files, a caption.
    AttachSheet,
    /// The menu of a number or a group in the rail.
    RailMenu,
    /// The card that edits a number's look, its local name, or a group.
    RailEdit,
    /// The menu of a message (right-click, its arrow, or M).
    MessageMenu,
    /// The reactions to pick from, for the message in focus.
    React,
    /// Who reacted to a message, and with what.
    Reactors,
    /// "Delete for everyone, or for me?"
    DeleteMessage,
    /// When a message was sent and how far it got.
    MessageInfo,
    /// The chats to forward to.
    Forward,
    /// The command palette.
    Palette,
    /// Every shortcut, by section.
    Shortcuts,
    /// Linking a number: the form, then the wait for the phone.
    AddNumber,
    /// A question about a number: its new name, or whether to log it out.
    NumberAction,
    /// An image, as large as the window allows.
    Viewer,
    /// "New poll": a question and its options, for the open chat.
    NewPoll,
    /// The form that creates a group.
    NewGroup,
    /// A number's own WhatsApp profile: picture, name, About.
    OwnProfile,
    /// Every emoji, over the composer.
    EmojiPicker,
    /// A sheet of Status: a new status, who sees it, delete, forward.
    Status,
}

/// The sections of the settings panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SettingsSection {
    #[default]
    Account,
    Sync,
    Appearance,
    Notifications,
    Audio,
    Stickers,
    Keyboard,
    /// Who sees the account's status, and its notifications.
    Status,
    About,
}

/// What a menu entry does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MenuAction {
    NewChat,
    NewGroup,
    NewCommunity,
    MarkAllRead,
    /// Marks the menu's chat as read.
    MarkRead,
    ShowArchived,
    OpenSettings,
    ContactInfo,
    SearchThread,
    Change(ChatChange),
    CloseChat,
    NewPoll,
    /// A command of the registry, for the message in focus.
    Run(crate::keys::Command),
}

/// One entry of a menu. Without an `action` it is shown disabled, with
/// `hint` saying why: no entry does nothing silently.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct MenuItem {
    /// Name for tests (`menu-pin`).
    pub(super) id: &'static str,
    pub(super) label: &'static str,
    pub(super) icon: crate::icons::IconName,
    pub(super) action: Option<MenuAction>,
    pub(super) hint: Option<&'static str>,
    /// The command whose keys are printed beside the entry.
    pub(super) keys: Option<crate::keys::Command>,
}

/// Why an entry is disabled.
pub(super) const NOT_YET: &str = "Not available yet";
const NOT_HERE: &str = "Not available with this provider";

/// Who is signed in, as far as the settings panel knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum IdentityState {
    /// Not asked yet, or the provider has no identity (demo data).
    Unknown,
    Loading,
    Known(Identity),
    /// The provider could not be asked just now. Asked again next time.
    Unavailable,
}

/// The search inside the open conversation.
pub(super) struct ThreadSearch {
    pub(super) input: Entity<InputState>,
    pub(super) query: String,
    pub(super) hits: Vec<SearchHit>,
    /// The match the conversation is on, among `hits` (newest first).
    pub(super) current: usize,
    _subscription: Subscription,
}

/// What the shell asks of whoever owns it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    /// The user chose "Sign out".
    SignOut,
}

impl EventEmitter<ShellEvent> for Shell {}

/// The filter chips above the chat list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ChatFilter {
    All,
    Unread,
    Groups,
    /// The chats that are in a community, under a heading for each. Only
    /// offered while there is such a chat.
    Communities,
    /// The archive: the chats the other filters leave out.
    Archived,
}

/// The heading of a community in the chat list.
pub(super) struct CommunityHeading {
    /// The community's group id.
    pub(super) id: ChatId,
    /// What the heading reads: the community's name, once it is known.
    pub(super) name: SharedString,
}

/// The rows of [`ChatFilter::Communities`]: for each community, in the
/// order its first chat comes in, its heading, then its announcement
/// group, then its other chats in the order they came in. Chats that are
/// in no community are left out.
fn community_rows(chats: Vec<ChatSummary>) -> Vec<ListRow> {
    let mut communities: Vec<(CommunityHeading, Vec<ChatSummary>)> = Vec::new();
    for chat in chats {
        let Some(community) = chat.community.clone() else {
            continue;
        };
        let at = match communities
            .iter()
            .position(|(heading, _)| heading.id == community.id)
        {
            Some(at) => at,
            None => {
                let name = if community.name.is_empty() {
                    "Community".into()
                } else {
                    community.name.clone().into()
                };
                let id = community.id.clone();
                communities.push((CommunityHeading { id, name }, Vec::new()));
                communities.len() - 1
            }
        };
        communities[at].1.push(chat);
    }
    let mut rows = Vec::new();
    for (heading, mut chats) in communities {
        rows.push(ListRow::Community(heading));
        // Stable: the rest keep the chat list's order.
        chats.sort_by_key(|chat| !chat.community.as_ref().is_some_and(|c| c.announcements));
        rows.extend(chats.into_iter().map(ListRow::Chat));
    }
    rows
}

/// One row of the chat list.
// A chat is nearly every row; boxing it would be an allocation for each.
#[allow(clippy::large_enum_variant)]
pub(super) enum ListRow {
    Chat(ChatSummary),
    /// A community's heading, above its chats, under
    /// [`ChatFilter::Communities`].
    Community(CommunityHeading),
    /// A section title, shown while searching.
    Section(&'static str),
    /// A message matching the search.
    Hit(Box<SearchHit>),
}

/// The chat shown in the conversation pane.
pub(super) struct OpenChat {
    pub(super) chat: ChatSummary,
    pub(super) rows: Rc<Vec<Row>>,
    pub(super) list: ListState,
    /// The line under the name in the header: who is in the group, or
    /// the contact's number. Read when the chat opens and when the store
    /// says people changed, never per frame.
    pub(super) subtitle: Option<SharedString>,
    /// What was unread when the chat was opened, for the divider above
    /// the first of those messages. It stays until the chat is left.
    pub(super) unread: Option<UnreadAtOpen>,
    /// How many of the newest stored messages are loaded into `rows`.
    limit: usize,
    /// A page of older history has been requested from the provider.
    awaiting_older: bool,
}

/// What was unread in a chat when it was opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UnreadAtOpen {
    /// How many messages the store counted as unread.
    count: u32,
    /// The newest message there was then: what came later is not part of
    /// the count.
    newest: String,
    /// The first unread message the divider was last scrolled to.
    shown_at: Option<String>,
}

/// The root view.
pub struct Shell {
    pub(super) engine: SyncEngine,
    pub(super) store: Arc<Store>,

    pub(super) accounts: Vec<Account>,
    pub(super) account: Option<AccountId>,
    /// Unread messages per account, for the rail badges.
    pub(super) unread: HashMap<AccountId, u32>,

    pub(super) filter: ChatFilter,
    pub(super) query: String,
    pub(super) list_rows: Vec<ListRow>,
    /// Some chat of the number on screen is in a community: the
    /// [`ChatFilter::Communities`] chip is offered.
    pub(super) has_communities: bool,
    pub(super) chat_scroll: UniformListScrollHandle,
    pub(super) search: Entity<InputState>,

    pub(super) open: Option<OpenChat>,
    pub(super) composer: Entity<TextareaState>,
    /// The people the composer's text mentions, and the list that picks
    /// them.
    pub(super) mentioning: super::mentions::Mentioning,
    /// The keyboard in the conversation: which message has it.
    pub(super) keys: super::thread_keys::ThreadKeys,
    /// What is being done to messages: a reply or an edit under way, a
    /// selection, the sheets of the actions.
    pub(super) acting: super::message_actions::Acting,
    /// The command palette.
    pub(super) palette: super::palette::PaletteState,
    /// How pictures and files leave: the clipboard, the dialogs.
    pub(super) out: super::media_out::MediaOut,
    /// What is leaving, and what is said about it.
    pub(super) leaving: super::media_out::Leaving,
    /// The voice note being recorded in the open conversation.
    pub(super) voice: super::voice_record::VoiceDeck,
    /// The chat list has the keyboard (see `panes.rs`).
    pub(super) pane_list: bool,
    /// The chat the list's outline is on.
    pub(super) list_cursor: Option<ChatId>,
    /// What gives each number and group of the rail the keyboard.
    pub(super) rail_handles: std::cell::RefCell<HashMap<String, gpui_kit::FocusHandle>>,
    /// The picture viewer: its scale, where the picture is, its pixels.
    pub(super) viewer: super::viewer::Viewer,
    /// What had the keyboard when the palette or the shortcuts sheet
    /// opened: it gets it back when they close.
    return_focus: Option<FocusHandle>,
    /// Where the message menu opens: at the pointer, or (`None`) beside
    /// the message in focus.
    pub(super) message_menu_at: Option<gpui_kit::Point<gpui_kit::Pixels>>,
    /// The window's size as of the last frame, for what must stay inside.
    pub(super) viewport: gpui_kit::Size<gpui_kit::Pixels>,

    /// The pictures and media the views show.
    pub(super) media: Rc<MediaShelf>,
    /// Who people are, and the colour each is told apart by.
    pub(super) senders: Rc<Senders>,
    /// What the tiles of contact cards remember between frames.
    pub(super) tiles: super::tiles::TileState,
    /// The poll being written, while "New poll" is open.
    pub(super) poll_form: Option<super::poll_form::PollForm>,
    /// The image the viewer is showing.
    pub(super) viewing: Option<Media>,
    /// A file that is being downloaded to be opened when it arrives.
    opening: Option<Media>,
    /// Voice notes and other audio: the player and what it is waiting for.
    pub(super) audio: AudioDeck,
    pub(super) session: SessionKind,
    pub(super) storage_note: Option<SharedString>,
    /// The window's own focus: where the keyboard is when no field or
    /// overlay has it, so the shortcuts always have somewhere to land.
    pub(super) focus: FocusHandle,
    pub(super) overlay: Overlay,
    /// The chat a row menu is about, and where the menu opens.
    pub(super) menu_target: Option<ChatSummary>,
    pub(super) menu_at: gpui_kit::Point<gpui_kit::Pixels>,
    /// Something that did not work, said for a few seconds.
    pub(super) problem: Option<SharedString>,
    _problem: Option<Task<()>>,
    /// The highlighted entry of the open menu.
    pub(super) menu_cursor: usize,
    /// Holds the keyboard while an overlay is open: arrows, Enter, Escape.
    pub(super) overlay_focus: FocusHandle,
    /// The number field of "New chat".
    pub(super) new_chat: Entity<InputState>,
    pub(super) new_chat_error: Option<SharedString>,
    /// The highlighted row of "New chat".
    pub(super) new_chat_cursor: usize,
    /// A typed number is being checked.
    pub(super) new_chat_checking: bool,
    _starting_chat: Option<Task<()>>,
    pub(super) thread_search: Option<ThreadSearch>,
    /// The width the user dragged the chat list to, in design pixels;
    /// `None` while it follows the window.
    pub(super) list_width: Option<f32>,
    /// The chat list's width as drawn in this frame.
    pub(super) list_px: gpui_kit::Pixels,
    /// The edge of the chat list is being dragged.
    resizing_list: bool,
    /// The interface size the last frame was drawn at.
    drawn_scale: f32,
    /// The files about to be sent, while the sheet is open.
    pub(super) attach: Option<super::attach::AttachDraft>,
    /// The sheet's strip of files: scrolls to the one on show.
    pub(super) attach_strip: gpui_kit::ScrollHandle,
    /// The strip has the keyboard: the arrows go from file to file.
    pub(super) attach_strip_focus: FocusHandle,
    /// The caption of the file on show takes the keyboard at the next
    /// frame: the sheet just opened, a file was chosen, or the emoji
    /// picker it opened closed.
    pub(super) attach_wants_focus: bool,
    /// The rail's arrangement, and where it is kept.
    pub(super) rail: crate::rail::RailLayout,
    pub(super) rail_path: Option<std::path::PathBuf>,
    /// A press on the rail on its way to being a click or a drag.
    pub(super) rail_drag: crate::rail::DragState,
    /// What is in motion on the rail.
    pub(super) rail_fx: super::rail::RailFx,
    /// The rail's rows as last drawn, for telling what a drag is over.
    pub(super) rail_rows: Rc<std::cell::RefCell<Vec<crate::rail::Row>>>,
    pub(super) rail_scroll: gpui_kit::ScrollHandle,
    /// The rail's context menu, while it is open.
    pub(super) rail_menu: Option<super::rail_menu::RailMenu>,
    /// The card that edits a number's look or a group, while it is open.
    pub(super) rail_edit: Option<super::rail_menu::RailEdit>,
    pub(super) _rail_work: Option<Task<()>>,
    /// How files are picked.
    pub(super) pick_files: PickFiles,
    /// The linking screen, while it is open.
    pub(super) linking: Option<super::numbers::LinkFlow>,
    /// The question being asked about a number.
    pub(super) number_action: Option<super::numbers::NumberAction>,
    /// Why a change to a number did not go through.
    pub(super) number_note: Option<(AccountId, SharedString)>,
    pub(super) _number_work: Option<Task<()>>,
    pub(super) settings_section: SettingsSection,
    /// The profile and group panels (see `social.rs`).
    pub(super) social: super::social::SocialUi,
    pub(super) identity: IdentityState,
    identity_loader: Option<IdentityLoader>,
    _identity: Option<Task<()>>,
    /// The emoji picker (see `emoji_picker.rs`).
    pub(super) emoji: super::emoji_picker::EmojiUi,
    /// The stickers and GIFs tabs of the same picker (see `picker.rs`).
    pub(super) picker: super::picker::PickerUi,
    /// Settings > Stickers and GIFs (see `library_ui.rs`).
    pub(super) library: super::library_ui::LibraryUi,
    /// What the About section holds for the updater.
    pub(super) updates: super::updates::UpdateUi,
    /// Status: the list, the viewer, posting (see `status.rs`).
    pub(super) status: super::status::StatusUi,
    /// The start screen's entrance, played once per window.
    pub(super) entrance: Entrance,
    /// The mark still has its entrance to play: true until the start
    /// screen has been left or covered once.
    pub(super) mark_intro: bool,
    _entrance: Option<Task<()>>,

    pending_open: Option<usize>,
    _subscriptions: Vec<Subscription>,
    _changes: Task<()>,
}

impl Shell {
    /// Builds the view and starts following the store's changes.
    pub fn new(
        engine: SyncEngine,
        options: ShellOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let store = engine.store().clone();
        let picker_store = store.clone();
        engine.set_read_receipts(settings::get(cx).read_receipts);
        // Whether files can be sent is asked now, so the answer is there
        // before anyone picks one.
        let _ = engine.uploads_available();

        // The field says where the rest is.
        let search_hint = match crate::keys::keys_label(crate::keys::Command::Palette) {
            Some(keys) => format!("Search  ·  {keys} for everything"),
            None => "Search".to_owned(),
        };
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(search_hint));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Type a message")
        });

        let new_chat =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search a name, or type a number"));

        let acting = super::message_actions::Acting::new(window, cx);
        let palette = super::palette::PaletteState::new(window, cx);
        let emoji = super::emoji_picker::EmojiUi::new(&composer, window, cx);
        let own_window = window.window_handle();
        let interceptor = cx.weak_entity();
        let subscriptions = vec![
            // Keys that are the conversation's while the composer has the
            // keyboard are taken before the text field's own bindings see
            // them (see `composer_key`).
            cx.intercept_keystrokes({
                let view = interceptor;
                move |event, window, cx| {
                    if window.window_handle() != own_window {
                        return;
                    }
                    let taken = view
                        .update(cx, |this, cx| {
                            this.attach_key(&event.keystroke, window, cx)
                                || this.palette_tab(&event.keystroke, window, cx)
                                || this.list_tab(&event.keystroke, window, cx)
                                || this.composer_key(&event.keystroke, window, cx)
                                || this.story_key(&event.keystroke, window, cx)
                        })
                        .unwrap_or(false);
                    if taken {
                        cx.stop_propagation();
                    }
                }
            }),
            // The shortcuts sheet narrows as it is typed in.
            cx.subscribe_in(
                &palette.shortcuts_input,
                window,
                |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &palette.input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => this.palette_changed(cx),
                    // Cmd or Ctrl with Enter runs and keeps it open.
                    InputEvent::PressEnter { secondary, .. } => {
                        this.run_palette_item(this.palette.cursor, *secondary, window, cx)
                    }
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &acting.forward_input,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    // Enter picks the row the keyboard is on; with Cmd or
                    // Ctrl it sends.
                    InputEvent::PressEnter {
                        secondary: true, ..
                    } => this.send_forward(window, cx),
                    InputEvent::PressEnter { .. } => {
                        let rows = this.forward_rows(cx);
                        if let Some((chat, _, _)) = rows.get(this.acting.forward_cursor) {
                            this.toggle_forward_to(chat.clone(), cx);
                        }
                    }
                    InputEvent::Change => this.refresh_forward_rows(cx),
                    _ => {}
                },
            ),
            cx.subscribe_in(
                &new_chat,
                window,
                |this, _, event: &InputEvent, window, cx| match event {
                    InputEvent::PressEnter { .. } => this.submit_new_chat(window, cx),
                    InputEvent::Change => {
                        this.new_chat_error = None;
                        this.new_chat_cursor = 0;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
            // When the focused element goes away (a menu closes, a field
            // is removed) the keyboard comes back to the window, so the
            // shortcuts always have somewhere to land.
            cx.on_focus_lost(window, |this, window, cx| {
                // Under an overlay the keyboard stays with the overlay
                // (a field of it went away): Escape still means it.
                if this.overlay == Overlay::None {
                    this.focus.focus(window, cx)
                } else {
                    this.overlay_focus.focus(window, cx)
                }
            }),
            // Losing or regaining the window decides whether the start
            // screen may move.
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.mark_intro = false;
                }
                cx.notify();
            }),
            cx.subscribe_in(&search, window, |this, state, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_owned();
                    this.reload_chats(cx);
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &composer,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    match event {
                        // Enter sends; Shift+Enter is a line break and never
                        // gets here.
                        InputEvent::PressEnter { shift: false, .. } => {
                            // With the list of people open, Enter picks.
                            if this.pick_mention_under_cursor(window, cx) {
                                return;
                            }
                            // An edit under way is saved, not sent.
                            if matches!(
                                this.acting.compose,
                                super::message_actions::Compose::Edit(_)
                            ) {
                                this.save_message_edit(window, cx)
                            } else {
                                this.send_composed(window, cx)
                            }
                        }
                        InputEvent::Change => {
                            this.mention_text_changed();
                            cx.notify()
                        }
                        _ => {}
                    }
                },
            ),
        ];

        // Re-query whatever the store says changed. A burst of changes is
        // drained and handled in one go, so the view repaints once.
        let mut listener = store.subscribe();
        let changes = cx.spawn(async move |this, cx| {
            while let Some(first) = listener.next().await {
                let mut batch = vec![first];
                while let Some(more) = listener.try_next() {
                    batch.push(more);
                }
                let alive = this.update(cx, |this, cx| this.apply_changes(batch, cx));
                if alive.is_err() {
                    break;
                }
            }
        });

        let mut this = Self {
            media: Rc::new(MediaShelf::new(engine.clone())),
            senders: Rc::new(Senders::new(store.clone())),
            mentioning: Default::default(),
            keys: Default::default(),
            acting,
            palette,
            out: options
                .out
                .clone()
                .unwrap_or_else(super::media_out::MediaOut::system),
            leaving: Default::default(),
            viewer: Default::default(),
            voice: super::voice_record::VoiceDeck::new(
                crate::record::Recorder::new(super::voice_record::system_input()),
                None,
            ),
            pane_list: false,
            list_cursor: None,
            rail_handles: Default::default(),
            return_focus: None,
            message_menu_at: None,
            viewport: window.viewport_size(),
            tiles: Default::default(),
            poll_form: None,
            viewing: None,
            opening: None,
            // At the speed voice notes were last played.
            audio: AudioDeck::new(
                Box::<SystemOutput>::default(),
                settings::get(cx).voice_speed,
            ),
            engine,
            store,
            accounts: Vec::new(),
            account: None,
            unread: HashMap::new(),
            filter: ChatFilter::All,
            has_communities: false,
            query: String::new(),
            list_rows: Vec::new(),
            chat_scroll: UniformListScrollHandle::new(),
            search,
            open: None,
            composer,
            session: options.session,
            storage_note: options.storage_note.map(SharedString::from),
            focus: cx.focus_handle(),
            overlay: Overlay::None,
            menu_target: None,
            menu_at: gpui_kit::Point::default(),
            problem: None,
            _problem: None,
            menu_cursor: 0,
            overlay_focus: cx.focus_handle(),
            new_chat,
            new_chat_error: None,
            new_chat_cursor: 0,
            new_chat_checking: false,
            _starting_chat: None,
            thread_search: None,
            attach: None,
            attach_strip: gpui_kit::ScrollHandle::new(),
            attach_strip_focus: cx.focus_handle().tab_stop(true),
            attach_wants_focus: false,
            rail: settings::sibling(crate::rail::FILE_NAME, cx)
                .map(|path| crate::rail::RailLayout::load(&path))
                .unwrap_or_default(),
            rail_path: settings::sibling(crate::rail::FILE_NAME, cx),
            rail_drag: Default::default(),
            rail_fx: Default::default(),
            rail_rows: Default::default(),
            rail_scroll: gpui_kit::ScrollHandle::new(),
            rail_menu: None,
            rail_edit: None,
            _rail_work: None,
            pick_files: options
                .pick_files
                .clone()
                .unwrap_or_else(system_file_dialog),
            list_width: settings::get(cx).list_width.map(f32::from),
            list_px: metrics::LIST_WIDTH(),
            resizing_list: false,
            drawn_scale: crate::theme::scale(),
            linking: None,
            number_action: None,
            number_note: None,
            _number_work: None,
            settings_section: SettingsSection::default(),
            social: super::social::SocialUi::new(),
            identity: IdentityState::Unknown,
            identity_loader: options.identity,
            _identity: None,
            emoji,
            picker: super::picker::PickerUi::new(picker_store, super::library_ui::session_secret()),
            library: super::library_ui::LibraryUi::new(window, cx),
            updates: super::updates::UpdateUi::new(window, cx),
            status: super::status::StatusUi::new(window, cx),
            entrance: Entrance::default(),
            mark_intro: true,
            _entrance: None,
            pending_open: options.open_chat,
            _subscriptions: subscriptions,
            _changes: changes,
        };
        // Whatever is already in the store shows up in the first frame.
        this.reload_accounts(cx);
        // The stories the store holds too: the rings, the strip above the
        // chats and the dot on Status are there before anything is asked
        // of anybody, and asking is what this does next.
        this.reload_status(cx);
        this.load_audio_devices(cx);
        this.focus.focus(window, cx);
        if let Some(at) = motion::frozen_at(cx) {
            this.entrance = Entrance::frozen(at);
        } else if this.open.is_none() && !settings::reduce_motion(cx) {
            this.play_entrance(cx);
        }
        this
    }

    // ----- reacting to the store ----------------------------------------

    fn apply_changes(&mut self, changes: Vec<StoreChange>, cx: &mut Context<Self>) {
        let (mut accounts, mut chats, mut messages) = (false, false, false);
        let mut stories = false;
        for change in changes {
            match change {
                StoreChange::Everything => {
                    accounts = true;
                    chats = true;
                    messages = true;
                    stories = true;
                }
                StoreChange::Stories { .. } => stories = true,
                StoreChange::Accounts => accounts = true,
                StoreChange::Chats { .. } => chats = true,
                StoreChange::Messages { chat_id, .. } => {
                    messages |= self
                        .open
                        .as_ref()
                        .is_some_and(|open| open.chat.id == chat_id);
                }
                // Presence is read from the engine while rendering.
                StoreChange::Presence { .. } => {}
                // Pictures and media are looked up again by whoever draws
                // them; the repaint below is all it takes.
                StoreChange::Avatar {
                    account_id,
                    subject,
                } => self.media.forget_avatar(&account_id, &subject),
                StoreChange::Problem { message } => self.show_problem(message, cx),
                // A name became known, or changed: what is already on
                // screen (senders, mentions, quotes, the list's preview
                // lines) is read again and says it.
                StoreChange::Contacts { .. } | StoreChange::Social { .. } => {
                    // Who is who may have changed with it.
                    self.senders.forget();
                    chats = true;
                    messages = true;
                }
                StoreChange::Media { key } => {
                    self.media.forget_media(&key);
                    self.file_arrived(&key, cx);
                    self.status_media_arrived(&key, cx);
                }
                StoreChange::Library => self.picker_library_changed(cx),
            }
        }
        if accounts {
            self.reload_accounts(cx);
        } else if chats {
            self.reload_chats(cx);
        }
        if messages {
            self.reload_messages(cx);
        }
        if stories || accounts {
            self.reload_status(cx);
        }
        cx.notify();
    }

    pub(super) fn reload_accounts(&mut self, cx: &mut Context<Self>) {
        self.accounts = self.store.accounts().unwrap_or_else(|error| {
            tracing::error!(%error, "could not read accounts");
            Vec::new()
        });
        let still_there = self
            .account
            .as_ref()
            .is_some_and(|id| self.accounts.iter().any(|account| &account.id == id));
        if !still_there {
            self.account = self.accounts.first().map(|account| account.id.clone());
            self.open = None;
        }
        // The rail follows the numbers that exist.
        let provider = self.engine.provider_id();
        let keys: Vec<String> = self
            .accounts
            .iter()
            .map(|account| crate::rail::key(provider, account.id.as_str()))
            .collect();
        if self.rail.reconcile(provider, &keys) {
            self.save_rail();
        }
        self.reload_chats(cx);
    }

    pub(super) fn reload_chats(&mut self, cx: &mut Context<Self>) {
        self.unread.clear();
        self.has_communities = self
            .account
            .as_ref()
            .and_then(|account| self.store.chats(account, None).ok())
            .is_some_and(|chats| chats.iter().any(|chat| chat.community.is_some()));
        // The last chat left its community while its view was on.
        if self.filter == ChatFilter::Communities && !self.has_communities {
            self.filter = ChatFilter::All;
        }
        let mut rows = Vec::new();
        for account in &self.accounts {
            let is_current = Some(&account.id) == self.account.as_ref();
            let filter = Some(self.query.as_str()).filter(|query| is_current && !query.is_empty());
            let listed = if is_current && self.filter == ChatFilter::Archived {
                self.store.archived_chats(&account.id).map(|mut chats| {
                    if let Some(query) = filter {
                        let query = query.to_lowercase();
                        chats.retain(|chat| chat.title.to_lowercase().contains(&query));
                    }
                    chats
                })
            } else {
                self.store.chats(&account.id, filter)
            };
            let chats = match listed {
                Ok(chats) => chats,
                Err(error) => {
                    tracing::error!(%error, "could not read chats");
                    continue;
                }
            };
            // The rail's count for the number: every unread message of
            // its chats outside the archive, whatever the list is
            // filtered to, and for the number on screen too.
            let narrowed = is_current && (filter.is_some() || self.filter == ChatFilter::Archived);
            let total: u32 = if narrowed {
                self.store
                    .chats(&account.id, None)
                    .map(|all| all.iter().map(|c| c.unread_count).sum())
                    .unwrap_or(0)
            } else {
                chats.iter().map(|c| c.unread_count).sum()
            };
            if total > 0 {
                self.unread.insert(account.id.clone(), total);
            }
            if !is_current {
                continue;
            }
            if self.filter == ChatFilter::Communities {
                rows.extend(community_rows(chats));
                continue;
            }
            for chat in chats {
                let keep = match self.filter {
                    ChatFilter::All | ChatFilter::Archived => true,
                    ChatFilter::Unread => chat.unread_count > 0,
                    ChatFilter::Groups => chat.kind == ChatKind::Group,
                    ChatFilter::Communities => false,
                };
                if keep {
                    rows.push(ListRow::Chat(chat));
                }
            }
        }

        if let (Some(account), false) = (&self.account, self.query.is_empty()) {
            let hits = self
                .store
                .search_messages(account, &self.query, MAX_HITS)
                .unwrap_or_default();
            if !hits.is_empty() {
                rows.push(ListRow::Section("Messages"));
                rows.extend(hits.into_iter().map(|hit| ListRow::Hit(Box::new(hit))));
            }
        }
        self.list_rows = rows;

        // Keep the open chat's header fresh, and its unread count at zero:
        // the user is looking at it.
        if let Some(open) = &mut self.open {
            if let Ok(Some(chat)) = self.store.chat(&open.chat.account_id, &open.chat.id) {
                if chat.unread_count > 0 {
                    self.engine.mark_read(&chat.account_id, &chat.id);
                }
                open.chat = chat;
            }
        }

        if let Some(index) = self.pending_open {
            let target = self
                .list_rows
                .iter()
                .filter_map(|row| match row {
                    ListRow::Chat(chat) => Some(chat.id.clone()),
                    _ => None,
                })
                .nth(index);
            if let Some(chat_id) = target {
                self.pending_open = None;
                self.open_chat(chat_id, None, cx);
            }
        }
    }

    fn reload_messages(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open else { return };
        if open.awaiting_older {
            // The page that was asked for has arrived: widen the window so
            // it becomes visible.
            open.awaiting_older = false;
            open.limit += MESSAGE_WINDOW;
        }
        let messages = self
            .store
            .messages(&open.chat.account_id, &open.chat.id, open.limit)
            .unwrap_or_else(|error| {
                tracing::error!(%error, "could not read messages");
                Vec::new()
            });
        let first_unread = open
            .unread
            .as_ref()
            .and_then(|unread| first_unread(&messages, unread));
        let rows = build_rows(messages, &open.chat, &self.senders, first_unread.as_ref());
        splice_rows(&open.list, &open.rows, &rows);
        self.audio.follow(&open.rows, &rows);
        open.rows = Rc::new(rows);
        open.subtitle = chat_subtitle(&self.store, &self.senders, &open.chat);
        // The panel of who reacted lists what the store has now.
        self.refresh_reactors();
        let Some(open) = &mut self.open else { return };
        // A chat opened with unread messages starts at the first of them,
        // not at the end. History that arrives a moment later can move
        // that message further up: the list follows it there.
        if let (Some(unread), Some((key, _))) = (&mut open.unread, first_unread) {
            if unread.shown_at.as_deref() != Some(key.as_str()) {
                if let Some(index) = open
                    .rows
                    .iter()
                    .position(|row| matches!(row, Row::Message(row) if row.unread_above.is_some()))
                {
                    open.list.scroll_to(ListOffset {
                        item_ix: index,
                        offset_in_item: px(0.),
                    });
                }
                unread.shown_at = Some(key);
            }
        }
        cx.notify();
    }

    // ----- user actions -------------------------------------------------

    pub(super) fn select_account(
        &mut self,
        account: AccountId,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account.as_ref() == Some(&account) {
            return;
        }
        self.account = Some(account);
        self.audio.leave_chat();
        self.leave_recording();
        self.media.leave_chat();
        self.open = None;
        self.thread_search = None;
        self.keys.clear();
        self.acting.leave_chat();
        self.reload_chats(cx);
        self.status_account_changed(cx);
        cx.notify();
    }

    pub(super) fn set_filter(&mut self, filter: ChatFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        self.reload_chats(cx);
        cx.notify();
    }

    /// Back to the start screen: no chat open.
    pub(super) fn close_chat(&mut self, cx: &mut Context<Self>) {
        self.audio.leave_chat();
        self.leave_recording();
        self.media.leave_chat();
        self.open = None;
        self.thread_search = None;
        self.keys.clear();
        self.acting.leave_chat();
        cx.notify();
    }

    // ----- interface size and the chat list's width --------------------

    /// One step larger or smaller, or back to the design's size.
    pub(super) fn step_scale(&mut self, step: Option<bool>, cx: &mut Context<Self>) {
        settings::update(cx, |settings| {
            settings.interface_scale = match step {
                Some(up) => crate::theme::next_step(settings.interface_scale, up),
                None => 100,
            };
        });
        cx.notify();
    }

    /// How wide the chat list is for a window this wide: where the user
    /// dragged it, or about 30 % of the window, and never so wide that
    /// the conversation is squeezed out.
    fn list_width_for(&self, window_width: gpui_kit::Pixels) -> gpui_kit::Pixels {
        let scale = crate::theme::scale();
        let wanted = match self.list_width {
            Some(design) => {
                gpui_kit::px(design * scale).clamp(metrics::LIST_MIN(), metrics::LIST_MAX())
            }
            None => (window_width * 0.3).clamp(metrics::LIST_DEFAULT_MIN(), metrics::LIST_WIDTH()),
        };
        let room = window_width - metrics::RAIL_WIDTH() - metrics::THREAD_MIN();
        wanted.min(room.max(metrics::LIST_MIN()))
    }

    /// Whether the chat list's edge is being dragged.
    pub(super) fn resizing_list(&self) -> bool {
        self.resizing_list
    }

    /// The edge of the chat list was pressed: a double click puts it
    /// back where it follows the window, a single one starts a drag.
    pub(super) fn grab_list_edge(&mut self, clicks: usize, cx: &mut Context<Self>) {
        if clicks >= 2 {
            self.resizing_list = false;
            self.list_width = None;
            settings::update(cx, |settings| settings.list_width = None);
        } else {
            self.resizing_list = true;
        }
        cx.notify();
    }

    /// The pointer moved while the edge is held: the list follows it.
    fn drag_list_edge(&mut self, x: gpui_kit::Pixels, cx: &mut Context<Self>) {
        if !self.resizing_list {
            return;
        }
        let wanted = (x - metrics::RAIL_WIDTH()).clamp(metrics::LIST_MIN(), metrics::LIST_MAX());
        // Kept in design pixels: the same width at every interface size.
        self.list_width = Some(wanted.as_f32() / crate::theme::scale());
        cx.notify();
    }

    /// The edge was let go: the width is remembered.
    fn drop_list_edge(&mut self, cx: &mut Context<Self>) {
        if !self.resizing_list {
            return;
        }
        self.resizing_list = false;
        let width = self.list_width.map(|width| width.round() as u16);
        settings::update(cx, |settings| settings.list_width = width);
        cx.notify();
    }

    /// The rail's sun or moon: the other theme, kept for the next start.
    pub(super) fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let next = if palette(cx).is_dark() {
            ThemeChoice::Light
        } else {
            ThemeChoice::Dark
        };
        settings::update(cx, |settings| settings.theme = next);
        cx.notify();
    }

    /// Messages still waiting to be sent: what signing out would lose.
    pub(super) fn unsent(&self) -> usize {
        self.store
            .outbox_pending()
            .map(|pending| pending.len())
            .unwrap_or(0)
    }

    /// Changes how much history is fetched in the background: saved, and
    /// in effect at once.
    pub(super) fn set_history(
        &mut self,
        change: impl FnOnce(&mut settings::Settings),
        cx: &mut Context<Self>,
    ) {
        settings::update(cx, change);
        self.engine.set_history(settings::get(cx).history_mode());
        cx.notify();
    }

    /// Asks the owner to sign out, which deletes the key and the chats on
    /// this computer. Reached only through the confirmation.
    pub(super) fn sign_out(&mut self, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        cx.emit(ShellEvent::SignOut);
        cx.notify();
    }

    // ----- the start screen's entrance ----------------------------------

    /// Plays the entrance, repainting each frame while it runs and not
    /// once more after.
    fn play_entrance(&mut self, cx: &mut Context<Self>) {
        self.entrance.begin(cx.background_executor().now());
        self._entrance = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                clock.timer(motion::FRAME).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    // Nothing moves behind an open conversation.
                    if this.open.is_some() {
                        this.entrance.skip();
                    }
                    this.entrance.running(clock.now())
                });
                if !matches!(running, Ok(true)) {
                    return;
                }
            }
        }));
    }

    // ----- overlays: menus, new chat, settings, contact info ------------

    pub(super) fn open_overlay(
        &mut self,
        overlay: Overlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(overlay, Overlay::Palette | Overlay::Shortcuts)
            && !matches!(self.overlay, Overlay::Palette | Overlay::Shortcuts)
        {
            self.return_focus = window.focused(cx);
        }
        self.overlay = overlay;
        self.new_chat_error = None;
        self.mark_intro = false;
        match overlay {
            Overlay::None => {}
            Overlay::NewChat => {
                self.new_chat_cursor = 0;
                self.new_chat_checking = false;
                self.new_chat.update(cx, |field, cx| {
                    field.set_value("", window, cx);
                    field.focus(window, cx);
                });
                // The address book is read from the store; a fresher copy
                // is fetched behind it.
                if let Some(account) = &self.account {
                    self.engine.want_contacts(account, CONTACTS_FRESH);
                }
            }
            Overlay::ListMenu | Overlay::ChatMenu | Overlay::RowMenu | Overlay::MessageMenu => {
                // Start on the first entry that does something.
                self.menu_cursor = self
                    .menu_items(cx)
                    .iter()
                    .position(|item| item.action.is_some())
                    .unwrap_or(0);
                self.overlay_focus.focus(window, cx);
            }
            Overlay::Settings => {
                self.settings_section = SettingsSection::default();
                self.load_identity(cx);
                self.overlay_focus.focus(window, cx);
            }
            Overlay::AddNumber => self.begin_link(window, cx),
            Overlay::NewPoll => self.begin_poll(window, cx),
            Overlay::NewGroup => self.begin_new_group(window, cx),
            Overlay::RailMenu | Overlay::RailEdit | Overlay::AttachSheet => {
                self.overlay_focus.focus(window, cx)
            }
            Overlay::OwnProfile
            | Overlay::ContactInfo
            | Overlay::ConfirmSignOut
            | Overlay::Viewer
            | Overlay::React
            | Overlay::Reactors
            | Overlay::DeleteMessage
            | Overlay::MessageInfo
            | Overlay::Forward
            | Overlay::Palette
            | Overlay::EmojiPicker
            | Overlay::NumberAction => self.overlay_focus.focus(window, cx),
            // The post sheet is typed in: its fields take the keyboard.
            Overlay::Status => {
                if self.status.sheet != Some(super::status_post::Sheet::Post) {
                    self.overlay_focus.focus(window, cx);
                }
            }
            // The sheet is searched by typing: its field has the keyboard.
            Overlay::Shortcuts => self.palette.shortcuts_input.update(cx, |field, cx| {
                field.set_value("", window, cx);
                field.focus(window, cx);
            }),
        }
        cx.notify();
    }

    /// Opens `overlay`, or closes it if it is the one showing.
    pub(super) fn toggle_overlay(
        &mut self,
        overlay: Overlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay == overlay {
            self.close_overlay(window, cx);
        } else {
            self.open_overlay(overlay, window, cx);
        }
    }

    /// Escape, or a click outside the card. Most overlays just close; the
    /// ones in the middle of something say what leaving means first.
    pub(super) fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.overlay {
            Overlay::AddNumber => self.cancel_link(window, cx),
            Overlay::NumberAction => self.back_to_settings(window, cx),
            Overlay::ContactInfo => self.info_back(window, cx),
            Overlay::NewGroup => self.leave_new_group(window, cx),
            Overlay::OwnProfile => self.leave_own_profile(window, cx),
            Overlay::AttachSheet => self.dismiss_attach_sheet(window, cx),
            Overlay::Status => self.dismiss_status_sheet(window, cx),
            _ => self.close_overlay(window, cx),
        }
    }

    pub(super) fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay == Overlay::None {
            return;
        }
        if self.overlay == Overlay::Viewer {
            // The picture's pixels are let go with the viewer.
            self.viewer.reset();
        }
        if self.overlay == Overlay::Status {
            self.status.sheet = None;
        }
        // The picker that was opened from the caption gives the sheet back.
        if self.emoji_back_to_caption(window, cx) {
            return;
        }
        // And the one a field of Status opened gives back what was under
        // it: the post sheet with what was written, or the story.
        if self.emoji_back_to_status(window, cx) {
            return;
        }
        // The microphone test ends with the settings.
        self.end_level_test();
        // The picker lets go of its pictures and what it was fetching.
        if self.overlay == Overlay::EmojiPicker {
            self.picker.close();
        }
        self.overlay = Overlay::None;
        self.number_action = None;
        self.social.close();
        self.rail_menu = None;
        self.rail_edit = None;
        // The palette and the shortcuts sheet give the keyboard back to
        // whatever had it.
        if let Some(before) = self.return_focus.take() {
            before.focus(window, cx);
            return cx.notify();
        }
        // The keyboard goes back to where typing makes sense: to the
        // chat list or the message that had it, else to the composer.
        if self.pane_list
            || self.status_active()
            || (self.keys.focused.is_some() && self.open.is_some())
        {
            self.focus.focus(window, cx);
        } else if self.open.is_some() {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Keys while an overlay holds the keyboard: Escape closes it; in a
    /// menu the arrows move and Enter chooses.
    pub(super) fn overlay_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        // In the palette, Escape goes back one question before it closes.
        if key == "escape" && self.overlay == Overlay::Palette && self.pop_step(window, cx) {
            return cx.stop_propagation();
        }
        if key == "escape" {
            self.dismiss_overlay(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.overlay == Overlay::NewChat {
            // The field has the keyboard; the arrows walk the rows.
            let (contacts, number) = self.new_chat_rows(cx);
            let rows = contacts.len() + usize::from(number.is_some());
            match key {
                "down" if rows > 0 => self.new_chat_cursor = (self.new_chat_cursor + 1) % rows,
                "up" if rows > 0 => self.new_chat_cursor = (self.new_chat_cursor + rows - 1) % rows,
                _ => return,
            }
            cx.stop_propagation();
            return cx.notify();
        }
        if self.overlay == Overlay::AttachSheet {
            // (In the caption, Enter is the field's: it sends from there,
            // or picks from a list that is open.)
            if key == "enter" && !self.caption_has_keyboard(window, cx) {
                // A question about leaving is answered by going on.
                let asking = self
                    .attach
                    .as_mut()
                    .is_some_and(|draft| std::mem::take(&mut draft.confirm_discard));
                if asking {
                    self.attach_wants_focus = true;
                    cx.notify();
                } else {
                    self.send_attachments(window, cx);
                }
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::RailMenu {
            self.rail_menu_key(key, window, cx);
            return cx.stop_propagation();
        }
        if self.overlay == Overlay::Viewer {
            // The viewer's own keys, and those that act on a message's
            // file, which act on the picture here.
            let viewer = crate::keys::Context {
                chat: true,
                message: false,
                typing: false,
                viewer: true,
                list: false,
                rail: false,
                recording: false,
                attaching: false,
                status: false,
                story: false,
            };
            use crate::keys::Command as C;
            let ran = match crate::keys::resolve(&event.keystroke, viewer) {
                Some(C::ViewerNext) => self.step_viewer(true, cx),
                Some(C::ViewerPrevious) => self.step_viewer(false, cx),
                Some(C::ZoomIn) => {
                    self.zoom_viewer(Some(true), window, cx);
                    true
                }
                Some(C::ZoomOut) => {
                    self.zoom_viewer(Some(false), window, cx);
                    true
                }
                Some(C::ZoomFit) => {
                    self.zoom_viewer(None, window, cx);
                    true
                }
                Some(C::ZoomActual) => {
                    self.zoom_viewer_to(super::viewer::Zoom::At(1.), None, window, cx);
                    true
                }
                Some(C::Copy | C::CopyImage) => self.copy_image(cx),
                Some(C::SaveAs) => self.save_as(None, cx),
                Some(C::SaveToDownloads) => self.save_to_downloads(None, cx),
                Some(C::Open) => {
                    self.open_viewed(cx);
                    true
                }
                _ => false,
            };
            if ran {
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::Palette {
            if self.palette_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::React {
            if self.react_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::Forward {
            if self.forward_key(event, window, cx) {
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::DeleteMessage {
            let choices = self.delete_choices();
            let count = choices.len();
            match key {
                "down" | "tab" => {
                    self.acting.delete_cursor = (self.acting.delete_cursor + 1) % count
                }
                "up" => self.acting.delete_cursor = (self.acting.delete_cursor + count - 1) % count,
                "enter" | "space" => {
                    let choice = choices[self.acting.delete_cursor.min(count - 1)];
                    self.delete_chosen(choice, window, cx);
                }
                _ => return,
            }
            cx.stop_propagation();
            return cx.notify();
        }
        if self.overlay == Overlay::RailEdit {
            if key == "enter" {
                self.finish_rail_edit(window, cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::NumberAction {
            if key == "enter" {
                self.confirm_number(cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::ConfirmSignOut {
            // Enter confirms only when nothing would be lost; with
            // messages still waiting, it takes the button.
            if key == "enter" && self.unsent() == 0 {
                self.sign_out(cx);
                cx.stop_propagation();
            }
            return;
        }
        if self.overlay == Overlay::Settings {
            // The arrows walk the sections, as in any list on the left.
            use SettingsSection as S;
            let sections = [
                S::Account,
                S::Sync,
                S::Appearance,
                S::Notifications,
                S::Keyboard,
                S::About,
            ];
            let place = sections
                .iter()
                .position(|section| *section == self.settings_section)
                .unwrap_or(0);
            self.settings_section = match key {
                "down" => sections[(place + 1) % sections.len()],
                "up" => sections[(place + sections.len() - 1) % sections.len()],
                _ => return,
            };
            cx.stop_propagation();
            return cx.notify();
        }
        if !matches!(
            self.overlay,
            Overlay::ListMenu | Overlay::ChatMenu | Overlay::RowMenu | Overlay::MessageMenu
        ) {
            return;
        }
        let items = self.menu_items(cx);
        // In a message's menu, an entry's own key runs it.
        if self.overlay == Overlay::MessageMenu {
            let message = crate::keys::Context {
                chat: true,
                message: true,
                typing: false,
                viewer: false,
                list: false,
                rail: false,
                recording: false,
                attaching: false,
                status: false,
                story: false,
            };
            let pressed = crate::keys::resolve(&event.keystroke, message);
            let entry = items.iter().find(|item| {
                item.action.is_some()
                    && pressed.is_some()
                    && item.action == pressed.map(MenuAction::Run)
            });
            if let Some(action) = entry.and_then(|item| item.action) {
                self.run_menu_action(action, window, cx);
                return cx.stop_propagation();
            }
        }
        let enabled: Vec<usize> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.action.is_some())
            .map(|(index, _)| index)
            .collect();
        let Some(place) = enabled.iter().position(|index| *index == self.menu_cursor) else {
            return;
        };
        match key {
            "down" => self.menu_cursor = enabled[(place + 1) % enabled.len()],
            "up" => self.menu_cursor = enabled[(place + enabled.len() - 1) % enabled.len()],
            "home" => self.menu_cursor = enabled[0],
            "end" => self.menu_cursor = enabled[enabled.len() - 1],
            "enter" | "space" => {
                if let Some(action) = items[self.menu_cursor].action {
                    self.run_menu_action(action, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// The chat a menu's entries act on: the row's for a row menu, the
    /// open conversation's otherwise.
    fn menu_chat(&self) -> Option<ChatSummary> {
        match self.overlay {
            Overlay::RowMenu => self.menu_target.clone(),
            _ => self.open.as_ref().map(|open| open.chat.clone()),
        }
    }

    /// The menu of a row of the chat list, at the pointer.
    pub(super) fn open_row_menu(
        &mut self,
        chat: ChatSummary,
        at: gpui_kit::Point<gpui_kit::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu_target = Some(chat);
        self.menu_at = at;
        self.open_overlay(Overlay::RowMenu, window, cx);
    }

    /// Says that something did not work, for a few seconds.
    pub(super) fn show_problem(&mut self, message: String, cx: &mut Context<Self>) {
        self.problem = Some(message.into());
        self._problem = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PROBLEM_SHOWN).await;
            this.update(cx, |this, cx| {
                this.problem = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The shortcuts of the window, wherever the keyboard is:
    ///
    /// * `Ctrl+,` settings (`Cmd` on macOS, here and below);
    /// * `Ctrl+N` new chat;
    /// * `Ctrl+F` search in the open conversation, or the chat list's
    ///   search when none is open;
    /// * `Ctrl+.` the menu of what is in front: the conversation's, or the
    ///   chat list's;
    /// * `Escape` closes, innermost first: the conversation's search, then
    ///   the conversation.
    fn shortcut(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let key = stroke.key.as_str();
        // Escape in the middle of a drag on the rail calls the drag off.
        if key == "escape" && self.rail_cancel(cx) {
            return cx.stop_propagation();
        }
        // In the middle of linking a number, or of a question about one:
        // nothing else opens over it and leaves it half done.
        if matches!(
            self.overlay,
            Overlay::AddNumber
                | Overlay::NumberAction
                | Overlay::NewGroup
                | Overlay::OwnProfile
                | Overlay::AttachSheet
        ) {
            return;
        }
        // Every other key is looked up in the registry (`crate::keys`),
        // for what is true of the window right now.
        let Some(command) = crate::keys::resolve(stroke, self.key_context(window, cx)) else {
            // On the chat list, what is typed is what is looked for.
            if self.list_has_keyboard(window) && self.type_into_search(stroke, window, cx) {
                cx.stop_propagation();
            }
            return;
        };
        // With something open over the panes, Escape and the keys of a
        // conversation are that overlay's; what applies anywhere still
        // does.
        let anywhere = crate::keys::binding(command)
            .is_some_and(|binding| binding.when == crate::keys::When::Anywhere);
        if self.overlay != Overlay::None && !anywhere {
            return;
        }
        if self.run_command(command, window, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    /// The entries of the open menu, for what is on screen right now.
    pub(super) fn menu_items(&self, _cx: &Context<Self>) -> Vec<MenuItem> {
        self.menu_items_now()
    }

    pub(super) fn menu_items_now(&self) -> Vec<MenuItem> {
        use crate::icons::IconName as I;
        let caps = self.engine.capabilities();
        // The keys printed beside an entry are its command's. The entries
        // of a chat row's menu act on that row, not on the open chat, so
        // they are given none.
        let on_a_row = self.overlay == Overlay::RowMenu;
        let item = |id, label, icon, action: Option<MenuAction>, hint: &'static str| MenuItem {
            id,
            label,
            icon,
            hint: action.is_none().then_some(hint),
            keys: super::palette_steps::menu_command(id).filter(|_| !on_a_row),
            action,
        };
        match self.overlay {
            Overlay::ListMenu => {
                let unread = self
                    .list_rows
                    .iter()
                    .any(|row| matches!(row, ListRow::Chat(chat) if chat.unread_count > 0));
                vec![
                    item(
                        "menu-new-chat",
                        "New chat",
                        I::SquarePen,
                        caps.start_chat.then_some(MenuAction::NewChat),
                        NOT_HERE,
                    ),
                    item(
                        "menu-new-group",
                        "New group",
                        I::Users,
                        (caps.group_create && self.account.is_some())
                            .then_some(MenuAction::NewGroup),
                        NOT_HERE,
                    ),
                    item(
                        "menu-new-community",
                        "New community",
                        I::Users,
                        (caps.group_create && caps.community_manage && self.account.is_some())
                            .then_some(MenuAction::NewCommunity),
                        NOT_HERE,
                    ),
                    item(
                        "menu-read-all",
                        "Mark all as read",
                        I::CheckCheck,
                        unread.then_some(MenuAction::MarkAllRead),
                        "Nothing unread",
                    ),
                    item(
                        "menu-archived",
                        "Archived chats",
                        I::Archive,
                        Some(MenuAction::ShowArchived),
                        "",
                    ),
                    item(
                        "menu-settings",
                        "Settings",
                        I::Settings,
                        Some(MenuAction::OpenSettings),
                        "",
                    ),
                    item(
                        "menu-shortcuts",
                        "Keyboard shortcuts",
                        I::Keyboard,
                        Some(MenuAction::Run(crate::keys::Command::Shortcuts)),
                        "",
                    ),
                ]
            }
            Overlay::RowMenu => {
                let Some(chat) = &self.menu_target else {
                    return Vec::new();
                };
                let change =
                    |change: ChatChange| caps.chat_state.then_some(MenuAction::Change(change));
                vec![
                    if chat.unread_count > 0 {
                        item(
                            "menu-read",
                            "Mark as read",
                            I::CheckCheck,
                            Some(MenuAction::MarkRead),
                            "",
                        )
                    } else {
                        item(
                            "menu-unread",
                            "Mark as unread",
                            I::MessageCircle,
                            change(ChatChange::MarkedUnread),
                            NOT_HERE,
                        )
                    },
                    item(
                        "menu-pin",
                        if chat.pinned { "Unpin" } else { "Pin to top" },
                        I::Pin,
                        change(ChatChange::Pinned(!chat.pinned)),
                        NOT_HERE,
                    ),
                    item(
                        "menu-mute",
                        if chat.muted { "Unmute" } else { "Mute" },
                        I::BellOff,
                        change(ChatChange::Muted(!chat.muted)),
                        NOT_HERE,
                    ),
                    item(
                        "menu-archive",
                        if chat.archived {
                            "Unarchive"
                        } else {
                            "Archive"
                        },
                        I::Archive,
                        change(ChatChange::Archived(!chat.archived)),
                        NOT_HERE,
                    ),
                    item("menu-delete", "Delete chat", I::Trash, None, NOT_YET),
                ]
            }
            Overlay::ChatMenu => {
                let Some(open) = &self.open else {
                    return Vec::new();
                };
                let chat = &open.chat;
                let change =
                    |change: ChatChange| caps.chat_state.then_some(MenuAction::Change(change));
                vec![
                    item(
                        "menu-info",
                        match chat.kind {
                            ChatKind::Group => "Group info",
                            ChatKind::Direct => "Contact info",
                        },
                        I::Info,
                        Some(MenuAction::ContactInfo),
                        "",
                    ),
                    item(
                        "menu-search",
                        "Search in conversation",
                        I::Search,
                        Some(MenuAction::SearchThread),
                        "",
                    ),
                    item(
                        "menu-unread",
                        "Mark as unread",
                        I::MessageCircle,
                        change(ChatChange::MarkedUnread),
                        NOT_HERE,
                    ),
                    item(
                        "menu-pin",
                        if chat.pinned { "Unpin" } else { "Pin" },
                        I::Pin,
                        change(ChatChange::Pinned(!chat.pinned)),
                        NOT_HERE,
                    ),
                    item(
                        "menu-mute",
                        if chat.muted { "Unmute" } else { "Mute" },
                        I::BellOff,
                        change(ChatChange::Muted(!chat.muted)),
                        NOT_HERE,
                    ),
                    item(
                        "menu-archive",
                        if chat.archived {
                            "Unarchive"
                        } else {
                            "Archive"
                        },
                        I::Archive,
                        change(ChatChange::Archived(!chat.archived)),
                        NOT_HERE,
                    ),
                    item(
                        "menu-poll",
                        "New poll",
                        I::ChartNoAxesColumn,
                        caps.polls.then_some(MenuAction::NewPoll),
                        NOT_HERE,
                    ),
                    item(
                        "menu-close",
                        "Close chat",
                        I::X,
                        Some(MenuAction::CloseChat),
                        "",
                    ),
                    item("menu-delete", "Delete chat", I::Trash, None, NOT_YET),
                    item(
                        "menu-shortcuts",
                        "Keyboard shortcuts",
                        I::Keyboard,
                        Some(MenuAction::Run(crate::keys::Command::Shortcuts)),
                        "",
                    ),
                ]
            }
            Overlay::MessageMenu => self.message_menu_items(),
            _ => Vec::new(),
        }
    }

    pub(super) fn run_menu_action(
        &mut self,
        action: MenuAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            MenuAction::NewChat => return self.open_overlay(Overlay::NewChat, window, cx),
            MenuAction::NewGroup => return self.open_overlay(Overlay::NewGroup, window, cx),
            MenuAction::NewCommunity => {
                return self.open_new_group(super::social::GroupKind::Community, window, cx)
            }
            MenuAction::OpenSettings => return self.open_overlay(Overlay::Settings, window, cx),
            MenuAction::ContactInfo => {
                return match self.menu_chat() {
                    Some(chat) => self.open_chat_info(&chat, window, cx),
                    None => self.close_overlay(window, cx),
                }
            }
            MenuAction::NewPoll => return self.open_overlay(Overlay::NewPoll, window, cx),
            MenuAction::MarkAllRead => {
                for row in &self.list_rows {
                    if let ListRow::Chat(chat) = row {
                        if chat.unread_count > 0 {
                            self.engine.mark_read(&chat.account_id, &chat.id);
                        }
                    }
                }
            }
            MenuAction::ShowArchived => self.set_filter(ChatFilter::Archived, cx),
            MenuAction::SearchThread => {
                self.overlay = Overlay::None;
                return self.open_thread_search(window, cx);
            }
            MenuAction::MarkRead => {
                if let Some(chat) = self.menu_chat() {
                    self.engine.mark_read(&chat.account_id, &chat.id);
                }
            }
            MenuAction::Change(change) => {
                if let Some(chat) = self.menu_chat() {
                    self.engine.update_chat(&chat.account_id, &chat.id, change);
                    // A chat that was just marked unread or put away is not
                    // one to keep looking at: looking would read it again.
                    let is_open = self
                        .open
                        .as_ref()
                        .is_some_and(|open| open.chat.id == chat.id);
                    if is_open
                        && matches!(
                            change,
                            ChatChange::MarkedUnread | ChatChange::Archived(true)
                        )
                    {
                        self.audio.leave_chat();
                        self.leave_recording();
                        self.media.leave_chat();
                        self.open = None;
                        self.thread_search = None;
                        self.keys.clear();
                        self.acting.leave_chat();
                    }
                }
            }
            MenuAction::CloseChat => self.close_chat(cx),
            MenuAction::Run(command) => {
                // The menu goes first: what the command opens (a sheet,
                // the composer) is then on top of nothing.
                self.close_overlay(window, cx);
                self.run_command(command, window, cx);
                return cx.notify();
            }
        }
        self.close_overlay(window, cx);
        cx.notify();
    }

    // ----- new chat -----------------------------------------------------

    /// What "New chat" lists for what is typed: the contacts that match,
    /// from the store, and the typed number when it reads as one.
    pub(super) fn new_chat_rows(&self, cx: &gpui_kit::App) -> (Vec<Contact>, Option<String>) {
        let typed = self.new_chat.read(cx).value().trim().to_owned();
        let contacts = match &self.account {
            Some(account) => self
                .engine
                .store()
                .contacts(account, Some(&typed), NEW_CHAT_ROWS)
                .unwrap_or_default(),
            None => Vec::new(),
        };
        let digits = typed.chars().filter(char::is_ascii_digit).count();
        let number = (digits >= 7
            && typed
                .chars()
                .all(|c| c.is_ascii_digit() || "+ -().".contains(c)))
        .then_some(typed);
        (contacts, number)
    }

    /// Enter in "New chat": the highlighted row.
    pub(super) fn submit_new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (contacts, number) = self.new_chat_rows(cx);
        let rows = contacts.len() + usize::from(number.is_some());
        if rows == 0 {
            self.new_chat_error = Some(
                if self.new_chat.read(cx).value().trim().is_empty() {
                    "Choose a contact, or type a number with its country code."
                } else {
                    "No contact matches. To write to a number, type it with its country code."
                }
                .into(),
            );
            return cx.notify();
        }
        let chosen = self.new_chat_cursor.min(rows - 1);
        match contacts.get(chosen) {
            Some(contact) => self.open_contact(contact.clone(), window, cx),
            None => self.open_number(number.unwrap_or_default(), cx),
        }
    }

    /// Opens the conversation with a contact of the address book. Nothing
    /// is asked of anyone: the contact is in the store.
    pub(super) fn open_contact(
        &mut self,
        contact: Contact,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.engine.chat_with_contact(&contact) {
            Ok(chat) => {
                self.close_overlay(window, cx);
                self.filter = ChatFilter::All;
                self.reload_chats(cx);
                self.open_chat(chat, None, cx);
            }
            Err(error) => self.new_chat_error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    /// Opens the conversation with a typed number, once the provider has
    /// said the number has WhatsApp. That runs off this thread.
    pub(super) fn open_number(&mut self, typed: String, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            self.new_chat_error = Some("There is no number to write from yet.".into());
            return cx.notify();
        };
        if self.new_chat_checking {
            return;
        }
        self.new_chat_checking = true;
        self.new_chat_error = None;
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.start_chat_checked(&account, &typed).await });
        self._starting_chat = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                this.new_chat_checking = false;
                match outcome {
                    Ok(Ok(client_core::NewChat::Open(chat))) => {
                        this.overlay = Overlay::None;
                        this.filter = ChatFilter::All;
                        this.reload_chats(cx);
                        this.open_chat(chat, None, cx);
                    }
                    Ok(Ok(client_core::NewChat::NotOnWhatsApp(phone))) => {
                        this.new_chat_error = Some(format!("{phone} is not on WhatsApp.").into());
                    }
                    Ok(Err(error)) if error.is_transient() => {
                        this.new_chat_error =
                            Some("Could not reach the provider. Try again.".into());
                    }
                    Ok(Err(error)) => this.new_chat_error = Some(error.to_string().into()),
                    Err(error) => this.new_chat_error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ----- search in the conversation -----------------------------------

    pub(super) fn open_thread_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_none() {
            return;
        }
        if self.thread_search.is_none() {
            let input =
                cx.new(|cx| InputState::new(window, cx).placeholder("Search in this conversation"));
            let subscription =
                cx.subscribe_in(&input, window, |this, state, event: &InputEvent, _, cx| {
                    match event {
                        InputEvent::Change => {
                            let query = state.read(cx).value().trim().to_owned();
                            this.search_thread(query, cx);
                        }
                        // Enter goes to the next match (the one before, in
                        // time); with Shift, back towards the newest.
                        InputEvent::PressEnter { shift, .. } => {
                            this.step_match(!*shift, cx);
                        }
                        _ => {}
                    }
                });
            self.thread_search = Some(ThreadSearch {
                input,
                query: String::new(),
                hits: Vec::new(),
                current: 0,
                _subscription: subscription,
            });
        }
        if let Some(search) = &self.thread_search {
            search.input.update(cx, |field, cx| field.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn close_thread_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.thread_search = None;
        self.acting.flash = None;
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
    }

    pub(super) fn toggle_thread_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread_search.is_some() {
            self.close_thread_search(window, cx);
        } else {
            self.open_thread_search(window, cx);
        }
    }

    /// The stored messages of the open chat that match `query`, newest
    /// first. Local, like every search.
    fn search_thread(&mut self, query: String, cx: &mut Context<Self>) {
        let (Some(open), Some(search)) = (&self.open, &mut self.thread_search) else {
            return;
        };
        search.hits = if query.is_empty() {
            Vec::new()
        } else {
            // The index is per account; keep what belongs to this chat.
            self.store
                .search_messages(&open.chat.account_id, &query, THREAD_SEARCH_SCAN)
                .unwrap_or_default()
                .into_iter()
                .filter(|hit| hit.message.chat_id == open.chat.id)
                .take(MAX_HITS)
                .collect()
        };
        search.query = query;
        search.current = 0;
        // The conversation goes to the newest match as it is typed.
        self.show_match(cx);
        cx.notify();
    }

    /// Goes to the match the search is on and keeps it lit while the
    /// search is open.
    fn show_match(&mut self, cx: &mut Context<Self>) {
        let target = self.thread_search.as_ref().and_then(|search| {
            search
                .hits
                .get(search.current)
                .map(|hit| hit.message.id.clone())
        });
        match target {
            Some(message) => {
                self.jump_to_message(message.clone(), cx);
                self.acting.flash = self.key_of(&message);
            }
            None => self.acting.flash = None,
        }
        cx.notify();
    }

    /// The next match of the search in the conversation (older), or the
    /// previous one; round and round.
    pub(super) fn step_match(&mut self, next: bool, cx: &mut Context<Self>) -> bool {
        let Some(search) = &mut self.thread_search else {
            return false;
        };
        let count = search.hits.len();
        if count == 0 {
            return false;
        }
        search.current = if next {
            (search.current + 1) % count
        } else {
            (search.current + count - 1) % count
        };
        self.show_match(cx);
        true
    }

    /// Scrolls the conversation to a message, loading as much stored
    /// history as it takes to show it.
    pub(super) fn jump_to_message(&mut self, message: MessageId, cx: &mut Context<Self>) {
        let position = |open: &OpenChat| {
            open.rows.iter().position(
                |row| matches!(row, Row::Message(row) if row.stored.message.id == message),
            )
        };
        let Some(open) = &mut self.open else { return };
        if position(open).is_none() {
            let stored = self
                .store
                .message_count(&open.chat.account_id, &open.chat.id)
                .unwrap_or(0);
            open.limit = open.limit.max(stored);
            self.reload_messages(cx);
        }
        if let Some(open) = &self.open {
            if let Some(index) = position(open) {
                open.list.pause_following_tail();
                open.list.scroll_to_reveal_item(index);
            }
        }
        cx.notify();
    }

    // ----- media --------------------------------------------------------

    /// Shows an image as large as the window allows. The thumbnail is
    /// there at once; the original replaces it when it has been fetched.
    pub(super) fn view_image(&mut self, media: Media, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account), Some(source)) = (self.account.clone(), media.source.clone()) else {
            return;
        };
        self.engine.want_file(&account, source.as_str());
        self.viewing = Some(media);
        self.open_overlay(Overlay::Viewer, window, cx);
    }

    /// Opens a file with the operating system's application for it,
    /// downloading it first if need be. Only ever on the user's click.
    pub(super) fn open_file(&mut self, media: Media, cx: &mut Context<Self>) {
        let (Some(account), Some(source)) = (self.account.clone(), media.source.clone()) else {
            return;
        };
        match self.media.file_state(source.as_str()) {
            FileState::Ready => self.hand_over(&media, cx),
            _ => {
                self.engine.want_file(&account, source.as_str());
                self.opening = Some(media);
            }
        }
        cx.notify();
    }

    // ----- audio --------------------------------------------------------

    /// Records from `input` instead of the system's microphone: for tests.
    #[cfg(test)]
    pub(super) fn set_audio_input(&mut self, input: Box<dyn crate::record::AudioInput>) {
        self.voice = super::voice_record::VoiceDeck::new(crate::record::Recorder::new(input), None);
    }

    /// Plays through `output` instead of the system's: for tests.
    #[cfg(test)]
    pub(super) fn set_audio_output(&mut self, output: Box<dyn AudioOutput>) {
        self.audio = AudioDeck::new(output, self.audio.player.speed());
    }

    /// The play button of an audio bubble: pause what is playing, resume
    /// what is paused, and otherwise fetch (if need be), decode and play.
    pub(super) fn toggle_audio(&mut self, media: Media, cx: &mut Context<Self>) {
        let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
            return;
        };
        match self.audio.player.playback(&url) {
            Playback::Playing(_) => self.audio.player.pause(),
            Playback::Paused(_) => self.play_clip(&url, cx),
            Playback::Idle => self.start_audio(&url, cx),
        }
        cx.notify();
    }

    fn start_audio(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        self.audio.failed.remove(url);
        if self.audio.clips.contains_key(url) {
            return self.play_clip(url, cx);
        }
        self.audio.waiting = Some(url.to_owned());
        match self.media.file_state(url) {
            FileState::Ready => self.decode_audio(url, cx),
            // It failed before: pressing play again is the retry.
            FileState::Unavailable(_) => self.media.retry_file(&account, url),
            _ => self.engine.want_file(&account, url),
        }
    }

    /// Decodes a downloaded file off this thread, then plays it if it is
    /// still the one being waited for.
    fn decode_audio(&mut self, url: &str, cx: &mut Context<Self>) {
        let Some((bytes, mime)) = self.media.file(url) else {
            return;
        };
        // Decoding is CPU work: on the blocking pool, awaited by a task of
        // the runtime, which is what wakes this view when it is done.
        let work = self.engine.runtime().spawn(async move {
            tokio::task::spawn_blocking(move || audio::decode(bytes, mime.as_deref()))
                .await
                .unwrap_or_else(|error| Err(error.to_string()))
        });
        let url = url.to_owned();
        self.audio._decoding = Some(cx.spawn(async move |this, cx| {
            let decoded = work.await;
            this.update(cx, |this, cx| {
                match decoded {
                    Ok(Ok(clip)) => {
                        this.media.keep_shape(&url, Shape::of(&clip));
                        this.audio.clips.insert(url.clone(), Arc::new(clip));
                        if this.audio.waiting.as_deref() == Some(url.as_str()) {
                            this.play_clip(&url, cx);
                        }
                    }
                    Ok(Err(reason)) => this.audio.fail(&url, reason),
                    Err(error) => this.audio.fail(&url, error.to_string()),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn play_clip(&mut self, url: &str, cx: &mut Context<Self>) {
        self.audio.waiting = None;
        let Some(clip) = self.audio.clips.get(url).cloned() else {
            return;
        };
        match self.audio.player.play(url, clip) {
            Ok(()) => {
                self.audio.no_output = None;
                self.watch_audio(cx);
            }
            // No device to play on, say: said in the bubble.
            Err(reason) => self.audio.no_output = Some((url.to_owned(), reason.into())),
        }
    }

    /// While something plays, repaints its position a few times a second
    /// and, when it ends, plays the voice note that follows it.
    fn watch_audio(&mut self, cx: &mut Context<Self>) {
        self.audio._watching = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                clock.timer(AUDIO_TICK).await;
                let playing = this.update(cx, |this, cx| {
                    if let Some(finished) = this.audio.player.poll() {
                        this.play_next_voice_note(&finished, cx);
                    }
                    cx.notify();
                    this.audio.player.is_playing()
                });
                if !matches!(playing, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// After a voice note, the one right after it in the chat, if the
    /// next message is one.
    fn play_next_voice_note(&mut self, finished: &str, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let voice_note = |row: &Row| match row {
            // Never one that opens once: that file is not fetched here.
            Row::Message(row) if row.stored.message.extras.view_once => None,
            Row::Message(row) => match &row.stored.message.content {
                MessageContent::Media(media) if media.kind == MediaKind::Voice => {
                    media.source.as_ref().map(|source| source.to_string())
                }
                _ => None,
            },
            Row::Day(_) => None,
        };
        let Some(index) = open
            .rows
            .iter()
            .position(|row| voice_note(row).as_deref() == Some(finished))
        else {
            return;
        };
        if let Some(next) = open.rows.get(index + 1).and_then(voice_note) {
            self.start_audio(&next, cx);
        }
    }

    /// A click or drag on the waveform.
    pub(super) fn seek_audio(&mut self, url: &str, fraction: f32, cx: &mut Context<Self>) {
        if let Err(reason) = self.audio.player.seek(url, fraction) {
            self.audio.no_output = Some((url.to_owned(), reason.into()));
        }
        cx.notify();
    }

    /// The speed toggle.
    pub(super) fn cycle_audio_speed(&mut self, cx: &mut Context<Self>) {
        let speed = self.audio.player.cycle_speed();
        // The next voice note, and the next session, start at it.
        settings::update(cx, |settings| settings.voice_speed = speed);
        cx.notify();
    }

    /// Something was cached, or could not be: if it is the file the user
    /// is waiting to open or to hear, go on with it.
    fn file_arrived(&mut self, key: &str, cx: &mut Context<Self>) {
        let waiting_audio = self
            .audio
            .waiting
            .clone()
            .filter(|url| client_core::file_key(url) == key);
        if let Some(url) = waiting_audio {
            match self.media.file_state(&url) {
                FileState::Ready => self.decode_audio(&url, cx),
                FileState::Unavailable(reason) => self.audio.fail(&url, reason.to_string()),
                _ => {}
            }
        }
        self.out_arrived(key, cx);
        // The original of the picture in the viewer: decoded, it replaces
        // the thumbnail.
        if self.overlay == Overlay::Viewer {
            self.load_viewed(cx);
        }
        let waiting = self
            .opening
            .as_ref()
            .and_then(|media| media.source.as_ref())
            .is_some_and(|source| client_core::file_key(source.as_str()) == key);
        if !waiting {
            return;
        }
        if let Some(media) = self.opening.take() {
            if let Some(source) = &media.source {
                if self.media.file_state(source.as_str()) == FileState::Ready {
                    self.hand_over(&media, cx);
                }
            }
        }
    }

    fn hand_over(&mut self, media: &Media, cx: &mut Context<Self>) {
        let Some(source) = &media.source else { return };
        match self.media.export(source.as_str(), media) {
            Ok(path) => cx.open_with_system(&path),
            Err(error) => tracing::warn!(%error, "the file could not be opened"),
        }
    }

    // ----- settings -----------------------------------------------------

    /// Asks who is signed in, once per look at the settings (again if the
    /// last answer did not arrive). Off this thread; the panel shows what
    /// it has meanwhile.
    fn load_identity(&mut self, cx: &mut Context<Self>) {
        let Some(loader) = &self.identity_loader else {
            return;
        };
        if matches!(
            self.identity,
            IdentityState::Loading | IdentityState::Known(_)
        ) {
            return;
        }
        self.identity = IdentityState::Loading;
        let lookup = loader();
        self._identity = Some(cx.spawn(async move |this, cx| {
            let answer = lookup.await;
            this.update(cx, |this, cx| {
                this.identity = match answer {
                    Ok(identity) => IdentityState::Known(identity),
                    Err(error) => {
                        tracing::debug!(%error, "the identity is not available");
                        IdentityState::Unavailable
                    }
                };
                cx.notify();
            })
            .ok();
        }));
    }

    pub(super) fn open_chat(
        &mut self,
        chat_id: ChatId,
        window: Option<&mut Window>,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.account.clone() else {
            return;
        };
        // A chat is wanted: Status gives the panes back.
        self.status_yields_to_chat(cx);
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.chat.id == chat_id)
        {
            return;
        }
        let Ok(Some(chat)) = self.store.chat(&account, &chat_id) else {
            return;
        };

        let list = ListState::new(0, ListAlignment::Bottom, px(1200.));
        list.set_follow_mode(FollowMode::Tail);
        // Reaching the top asks for older messages. The handler runs while
        // the list is being laid out, so the work is deferred.
        let view = cx.weak_entity();
        list.set_scroll_handler(move |event, _, cx| {
            if event.visible_range.start <= 2 {
                let view = view.clone();
                cx.defer(move |cx| {
                    view.update(cx, |this, cx| this.load_older(cx)).ok();
                });
            }
        });

        self.thread_search = None;
        self.keys.clear();
        self.acting.leave_chat();
        self.palette.note_chat(&chat_id);
        self.mark_intro = false;
        self.mentioning.clear();
        self.audio.leave_chat();
        self.leave_recording();
        self.media.leave_chat();
        // What is unread now is what the divider is about: asked before
        // the chat is marked as read, below.
        let unread = (chat.unread_count > 0)
            .then(|| {
                let newest = self.store.messages(&account, &chat_id, 1).ok()?.pop()?;
                Some(UnreadAtOpen {
                    count: chat.unread_count,
                    newest: row_key(&newest.message),
                    shown_at: None,
                })
            })
            .flatten();
        let limit = MESSAGE_WINDOW.max((chat.unread_count as usize + 20).min(MAX_UNREAD_WINDOW));
        self.open = Some(OpenChat {
            chat,
            rows: Rc::new(Vec::new()),
            list,
            subtitle: None,
            unread,
            limit,
            awaiting_older: false,
        });
        // Local first: show what is stored now, let the engine catch up.
        self.reload_messages(cx);
        self.engine.open_chat(&account, &chat_id);
        self.engine.mark_read(&account, &chat_id);
        if let Some(window) = window {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        }
        cx.notify();
    }

    /// Shows one more window of older messages: from disk when they are
    /// there, otherwise by asking the engine to fetch a page.
    pub(super) fn load_older(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &mut self.open else { return };
        if open.awaiting_older {
            return;
        }
        let (account, chat) = (open.chat.account_id.clone(), open.chat.id.clone());
        let stored = self.store.message_count(&account, &chat).unwrap_or(0);
        if stored > open.limit {
            // More is already on disk: just show it.
            open.limit += MESSAGE_WINDOW;
            self.reload_messages(cx);
            return;
        }
        let complete = self
            .store
            .history_state(&account, &chat)
            .map(|state| state.complete)
            .unwrap_or(true);
        if !complete {
            open.awaiting_older = true;
            self.engine.request_older(&account, &chat);
        }
    }

    pub(super) fn send_composed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        // The people picked from the list go by their ids in what is sent.
        let (text, mentions) = crate::mentioning::outgoing(&text, &self.mentioning.picked);
        // What it answers, when a reply is under way.
        let reply_to = match &self.acting.compose {
            super::message_actions::Compose::Reply(message) => Some(message.id.clone()),
            _ => None,
        };
        // Into the outbox: the bubble appears now, the network comes later.
        match self.engine.send_text_mentioning(
            &open.chat.account_id,
            &open.chat.id,
            text,
            reply_to,
            mentions,
        ) {
            Ok(_) => {
                self.acting.compose = super::message_actions::Compose::New;
                self.mentioning.clear();
                self.composer
                    .update(cx, |composer, cx| composer.set_value("", window, cx));
                open.list.scroll_to_end();
            }
            Err(error) => tracing::error!(%error, "could not queue the message"),
        }
        cx.notify();
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let chosen = settings::get(cx);
        self.media.set_policy(chosen.media);
        // Frames of animations that were let go leave the window too.
        self.media.release(window);
        self.picker.release(window);
        self.viewer.release(window);
        self.engine.set_read_receipts(chosen.read_receipts);
        self.engine.set_story_receipts(chosen.story_receipts);
        // A new interface size: what was measured at the old one is
        // measured again.
        let scale = crate::theme::scale();
        if scale != self.drawn_scale {
            self.drawn_scale = scale;
            self.media.forget_boxes();
            if let Some(open) = &self.open {
                open.list.remeasure();
            }
        }
        // And from the chat list.
        if self.pane_list && self.overlay == Overlay::None && !self.focus.is_focused(window) {
            self.pane_list = false;
        }
        // Typing somewhere takes the keyboard from the messages.
        if self.keys.focused.is_some()
            && self.overlay == Overlay::None
            && !self.focus.is_focused(window)
        {
            self.keys.clear();
        }
        self.viewport = window.viewport_size();
        self.list_px = self.list_width_for(self.viewport.width);
        // The sheet just opened: the caption is where typing goes.
        self.ensure_captions(window, cx);
        if std::mem::take(&mut self.attach_wants_focus) && self.overlay == Overlay::AttachSheet {
            if let Some(caption) = self.captioned_field() {
                caption.update(cx, |field, cx| field.focus(window, cx));
            }
        }
        let features = FontFeatures(Arc::new(
            fonts::SANS_FEATURES
                .iter()
                .map(|(tag, value)| ((*tag).to_owned(), *value))
                .collect(),
        ));
        div()
            .id("shell")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.shortcut(event, window, cx)
            }))
            .on_mouse_move(
                cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                    this.drag_list_edge(event.position.x, cx);
                    this.rail_pointer(event.position, cx);
                }),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.drop_list_edge(cx);
                    this.rail_release(cx);
                }),
            )
            .relative()
            .size_full()
            .flex()
            .bg(palette.background)
            .text_color(palette.text)
            .font_family(fonts::SANS)
            .font_features(features)
            .child(self.render_rail(&palette, cx))
            .child(self.render_list_pane(&palette, cx))
            .child(if self.status_active() {
                self.render_status_main(&palette, window, cx)
            } else {
                self.render_conversation(&palette, window, cx)
                    .into_any_element()
            })
            // The grid's crosshairs, where the pane rules meet the header
            // rule that runs across the window.
            .child(grid_mark(
                metrics::RAIL_WIDTH(),
                metrics::HEADER_HEIGHT(),
                &palette,
            ))
            .child(grid_mark_in(
                metrics::RAIL_WIDTH() + self.list_px,
                metrics::HEADER_HEIGHT(),
                self.entrance.reveal(0, cx.background_executor().now()),
                &palette,
            ))
            .children(self.render_overlay(&palette, window, cx))
            // What is being dragged along the rail, above everything.
            .children(self.render_rail_floating(&palette, cx))
    }
}

/// The most messages a chat is opened with so that its first unread one
/// is among them.
const MAX_UNREAD_WINDOW: usize = 1000;

/// A message keeps its row when it trades its local id for the provider's,
/// so rows are keyed by client id when there is one.
fn row_key(message: &client_provider::Message) -> String {
    match &message.client_id {
        Some(client_id) => format!("c:{client_id}"),
        None => format!("m:{}", message.id),
    }
}

/// Whether a message counts as unread when it has not been seen: somebody
/// else's, and not a notice of the chat itself.
fn counts_as_unread(message: &client_provider::Message) -> bool {
    message.direction == client_provider::Direction::Incoming
        && !matches!(
            message.content,
            MessageContent::System(_) | MessageContent::Reaction { .. }
        )
}

/// The first of the messages that were unread when the chat was opened:
/// its row key and how many there are from it on. `messages` are oldest
/// first. What arrived after the chat was opened is not counted, and
/// nothing before the account's own last message can be unread.
fn first_unread(messages: &[StoredMessage], unread: &UnreadAtOpen) -> Option<(String, u32)> {
    let newest = messages
        .iter()
        .rposition(|stored| row_key(&stored.message) == unread.newest)?;
    let mut first = None;
    let mut found = 0;
    for stored in messages[..=newest].iter().rev() {
        let message = &stored.message;
        if message.direction == client_provider::Direction::Outgoing {
            break;
        }
        if !counts_as_unread(message) {
            continue;
        }
        found += 1;
        first = Some(row_key(message));
        if found == unread.count {
            break;
        }
    }
    first.map(|key| (key, found))
}

/// The line under a chat's name in the header: for a group, who is in it
/// (or how many, when nobody can be named); for a person, their number
/// when the chat is not already called by it. Never anything about being
/// online: that is the engine's to say, when it knows.
fn chat_subtitle(store: &Store, senders: &Senders, chat: &ChatSummary) -> Option<SharedString> {
    /// How many participants are named before the line is cut anyway.
    const NAMED: usize = 24;
    match chat.kind {
        ChatKind::Group => {
            let count = store
                .group(&chat.account_id, &chat.id)
                .ok()
                .flatten()
                .map_or(0, |group| group.participant_count);
            let people = store
                .group_participants(&chat.account_id, &chat.id, None, NAMED)
                .unwrap_or_default();
            let mut names: Vec<&str> = people
                .iter()
                .filter(|person| !person.me && person.name != person.contact.as_str())
                .map(|person| person.name.as_str())
                .collect();
            if people.iter().any(|person| person.me) {
                names.push("You");
            }
            match (names.len(), count) {
                (0, 0) => Some("Group".into()),
                (0, 1) => Some("1 participant".into()),
                (0, count) => Some(format!("{count} participants").into()),
                // Everybody at hand is named: the line is who they are.
                (named, count) if named >= count.min(NAMED) => Some(names.join(", ").into()),
                (_, count) => Some(format!("{count} participants").into()),
            }
        }
        ChatKind::Direct => {
            let contact = client_provider::ContactId::new(chat.id.as_str().to_owned());
            senders
                .of(&chat.account_id, None, &contact)
                .phone
                .filter(|phone| phone.as_ref() != chat.title)
        }
    }
}

/// Turns messages (oldest first) into list rows, adding day separators,
/// the unread divider, and marking where runs of messages by one person
/// begin and end.
fn build_rows(
    messages: Vec<StoredMessage>,
    chat: &ChatSummary,
    senders: &Senders,
    first_unread: Option<&(String, u32)>,
) -> Vec<Row> {
    use client_provider::Direction;
    let now = Timestamp::now();
    let in_group = chat.kind == ChatKind::Group;
    // Who wrote each message at hand, for the quotes that point at them.
    let authors: HashMap<MessageId, Quoted> = messages
        .iter()
        .map(|stored| {
            let message = &stored.message;
            let quoted = match message.direction {
                Direction::Outgoing => Quoted::Me,
                Direction::Incoming => Quoted::Person(
                    senders
                        .of(&message.account_id, Some(&message.chat_id), &message.sender)
                        .tone,
                ),
            };
            (message.id.clone(), quoted)
        })
        .collect();
    let mut rows = Vec::with_capacity(messages.len() + 8);
    // The day and the person of the message before: a run is one person's
    // messages, under whichever of their ids each came.
    let mut previous: Option<(Timestamp, Rc<str>)> = None;
    for stored in messages {
        let message = &stored.message;
        // Defensive: the store folds reactions, they are never rows.
        if matches!(message.content, MessageContent::Reaction { .. }) {
            continue;
        }
        let new_day = previous
            .as_ref()
            .is_none_or(|(at, _)| !same_day(*at, message.timestamp));
        if new_day {
            rows.push(Row::Day(SharedString::from(day_label(
                message.timestamp,
                now,
            ))));
        }
        let key = row_key(message);
        let unread_above = first_unread
            .filter(|(first, _)| *first == key)
            .map(|(_, count)| *count);
        let sender = senders.of(&message.account_id, Some(&message.chat_id), &message.sender);
        // A notice of the chat is nobody's: it ends the run before it and
        // the one after it starts anew.
        let person: Rc<str> = match (&message.content, message.direction) {
            (MessageContent::System(_), _) => "".into(),
            (_, Direction::Outgoing) => "me".into(),
            (_, Direction::Incoming) => sender.key.clone(),
        };
        let first_of_run = new_day
            || unread_above.is_some()
            || person.is_empty()
            || previous
                .as_ref()
                .is_none_or(|(_, before)| *before != person);
        previous = Some((message.timestamp, person));
        let sender = (message.direction == Direction::Incoming && in_group).then_some(sender);
        let quoted = message
            .reply_to
            .as_ref()
            .and_then(|reply| authors.get(&reply.message_id).copied())
            .unwrap_or_default();

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        first_of_run.hash(&mut hasher);
        message.deleted.hash(&mut hasher);
        message.edited.hash(&mut hasher);
        sender.hash(&mut hasher);
        quoted.hash(&mut hasher);
        unread_above.hash(&mut hasher);
        message.sender_name.hash(&mut hasher);
        format!(
            "{:?}{:?}{:?}{:?}",
            message.content, message.reply_to, stored.reactions, message.extras
        )
        .hash(&mut hasher);
        matches!(
            message.status,
            client_provider::DeliveryStatus::Failed { .. }
        )
        .hash(&mut hasher);
        let signature = hasher.finish();

        rows.push(Row::Message(Box::new(MessageRow {
            stored,
            first_of_run,
            last_of_run: true,
            sender,
            quoted,
            unread_above,
            key,
            signature,
        })));
    }
    // A run ends where the next row is a separator or starts a new run.
    let mut next_starts_run = true;
    for row in rows.iter_mut().rev() {
        match row {
            Row::Day(_) => next_starts_run = true,
            Row::Message(message) => {
                message.last_of_run = next_starts_run;
                next_starts_run = message.first_of_run;
            }
        }
    }
    // Where a run ends decides the bubble's corner and whether the avatar
    // stands beside it: it is part of what the row looks like.
    for row in &mut rows {
        if let Row::Message(message) = row {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            message.signature.hash(&mut hasher);
            message.last_of_run.hash(&mut hasher);
            message.signature = hasher.finish();
        }
    }
    rows
}

fn row_identity(row: &Row) -> (&str, u64) {
    match row {
        Row::Day(label) => (label.as_ref(), 0),
        Row::Message(message) => (message.key.as_str(), message.signature),
    }
}

/// Tells the list which rows changed, so it keeps its scroll position and
/// only measures what is new: everything between the unchanged head and the
/// unchanged tail is replaced.
fn splice_rows(list: &ListState, old: &[Row], new: &[Row]) {
    let same = |a: &Row, b: &Row| row_identity(a) == row_identity(b);
    let head = old.iter().zip(new).take_while(|(a, b)| same(a, b)).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| same(a, b))
        .count();
    if head == old.len() && head == new.len() {
        return;
    }
    list.splice(head..old.len() - tail, new.len() - head - tail);
}
