//! What the palette's commands are made of: whether one can be run right
//! now (and why not), the value it shows, the steps it asks for, and what
//! running it does.
//!
//! Every command is an entry of the registry (`crate::keys`): the palette
//! lists the registry, so a command cannot be in a menu or on a key and
//! be missing here. A command that needs to be told something (which
//! chat, how long, which value) asks in place: the palette stays open on
//! a list of its own, and Escape goes back one step.

use super::connection::connection_words;
use super::rail_menu::RailAction;
use super::shell::{ChatFilter, Overlay, SettingsSection, Shell};
use crate::emoji::data::Lang;
use crate::emoji::locale::LanguageChoice;
use crate::emoji::prefs;
use crate::keys::{self, Command, When};
use crate::settings::{
    self, HistoryChoice, MediaChoice, MotionChoice, ThemeChoice, WallpaperChoice,
};
use crate::theme::{BubbleStyle, SCALE_STEPS};
use client_core::ChatSummary;
use client_provider::{AccountId, ChatChange, ChatKind, LiveUpdates, MessageId, PollingReason};
use gpui_kit::{Context, SharedString, Window};

/// Why a command of a conversation is not offered without one.
const NO_CHAT: &str = "Open a chat first";
const NOT_HERE: &str = "Not available with this provider";
const NO_NUMBER: &str = "There is no number yet";

/// The commands of a message the palette offers while one is in focus.
pub(super) const OF_A_MESSAGE: [Command; 18] = [
    Command::Reply,
    Command::Edit,
    Command::Copy,
    Command::CopyImage,
    Command::CopyLink,
    Command::SaveAs,
    Command::SaveToDownloads,
    Command::Forward,
    Command::Star,
    Command::React,
    Command::Reactions,
    Command::Delete,
    Command::Info,
    Command::SelectMessages,
    Command::Open,
    Command::Download,
    Command::SaveSticker,
    Command::SaveGif,
];

/// Whether a command can be run right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Avail {
    Enabled,
    /// Not now, and why: shown dimmed when it is what was looked for.
    Disabled(&'static str),
    /// Not a thing here at all.
    Hidden,
}

/// What a choice of a step sets.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Value {
    Theme(ThemeChoice),
    Scale(u16),
    Bubbles(BubbleStyle),
    Wallpaper(WallpaperChoice),
    Motion(MotionChoice),
    Language(LanguageChoice),
    Media(MediaChoice),
    History(HistoryChoice),
    /// Seconds, or until it is unmuted.
    Mute(Option<u64>),
    Number(AccountId),
    Group(u32),
    /// The first message of a day.
    Day(MessageId),
}

/// One row of a step.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Choice {
    pub(super) label: SharedString,
    pub(super) detail: SharedString,
    /// It is what is set now.
    pub(super) current: bool,
    pub(super) value: Value,
}

fn choice(label: impl Into<SharedString>, current: bool, value: Value) -> Choice {
    Choice {
        label: label.into(),
        detail: if current { "now".into() } else { "".into() },
        current,
        value,
    }
}

/// What a step asks for.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum StepKind {
    /// One of these.
    Choices(Vec<Choice>),
    /// A chat.
    Chat,
    /// Somebody, or a number, to write to.
    Contact,
    /// Nothing: lines to read.
    Lines(Vec<(SharedString, SharedString)>),
}

/// A question the palette asks in place.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Step {
    pub(super) command: Command,
    /// What is being asked, before the field.
    pub(super) title: SharedString,
    pub(super) kind: StepKind,
    /// The chat an earlier step chose, for the commands about "a chat".
    pub(super) chat: Option<ChatSummary>,
}

/// The lengths a chat can be muted for.
const MUTES: [(&str, Option<u64>); 4] = [
    ("8 hours", Some(8 * 3600)),
    ("1 week", Some(7 * 24 * 3600)),
    ("1 year", Some(365 * 24 * 3600)),
    ("Until I unmute it", None),
];

/// How live updates arrive, in the words of the Connection panel.
pub(super) fn live_words(live: LiveUpdates) -> &'static str {
    match live {
        LiveUpdates::Stream => "Stream",
        LiveUpdates::Reconnecting => "Stream, reconnecting",
        LiveUpdates::Polling(PollingReason::Chosen) => "Polling",
        LiveUpdates::Polling(PollingReason::Unavailable) => {
            "Polling (the stream is not available yet)"
        }
        LiveUpdates::Polling(PollingReason::ConnectionLimit) => {
            "Polling (too many stream connections are open)"
        }
        LiveUpdates::Polling(PollingReason::Refused) => "Polling (the stream refused this key)",
        LiveUpdates::Polling(PollingReason::Failing) => "Polling (the stream keeps failing)",
    }
}

/// The "Live updates" line, after the one that names the provider. A
/// provider with no choice of transport says nothing and gets no line.
pub(super) fn push_live_line(
    lines: &mut Vec<(SharedString, SharedString)>,
    live: Option<LiveUpdates>,
) {
    if let Some(live) = live {
        lines.push(("Live updates".into(), live_words(live).into()));
    }
}

fn on_off(on: bool) -> SharedString {
    if on { "On" } else { "Off" }.into()
}

fn theme_name(theme: ThemeChoice) -> &'static str {
    match theme {
        ThemeChoice::Light => "Light",
        ThemeChoice::Dark => "Dark",
        ThemeChoice::System => "System",
    }
}

fn motion_name(motion: MotionChoice) -> &'static str {
    match motion {
        MotionChoice::System => "System",
        MotionChoice::On => "On",
        MotionChoice::Off => "Off",
    }
}

fn bubbles_name(style: BubbleStyle) -> &'static str {
    match style {
        BubbleStyle::Brand => "Brand",
        BubbleStyle::Lime => "Lime",
        BubbleStyle::Neutral => "Neutral",
    }
}

fn wallpaper_name(wallpaper: WallpaperChoice) -> &'static str {
    match wallpaper {
        WallpaperChoice::Pattern => "Pattern",
        WallpaperChoice::Plain => "Plain",
    }
}

fn media_name(media: MediaChoice) -> &'static str {
    match media {
        MediaChoice::Images => "Images, stickers and GIFs",
        MediaChoice::Never => "Nothing",
        MediaChoice::Everything => "Everything",
    }
}

fn history_name(history: HistoryChoice) -> &'static str {
    match history {
        HistoryChoice::OnOpen => "When a chat is opened",
        HistoryChoice::Recent => "The recent chats",
        HistoryChoice::Everything => "Everything",
    }
}

fn language_name(language: LanguageChoice) -> String {
    match language {
        LanguageChoice::System => "System".to_owned(),
        LanguageChoice::Lang(lang) => lang.label().to_owned(),
    }
}

impl Shell {
    /// The number on screen.
    fn current_account(&self) -> Option<&client_provider::Account> {
        let current = self.account.as_ref()?;
        self.accounts.iter().find(|account| &account.id == current)
    }

    /// What the provider can do for the number on screen right now: its
    /// capabilities, less what is missing for that number for now.
    pub(super) fn caps_now(&self) -> client_provider::Capabilities {
        match &self.account {
            Some(account) => self.engine.capabilities_for(account),
            None => self.engine.capabilities(),
        }
    }

    /// Whether `command` can be run from the palette right now.
    pub(super) fn palette_avail(&self, command: Command, cx: &gpui_kit::App) -> Avail {
        use Avail::{Disabled, Enabled, Hidden};
        use Command as C;
        let Some(binding) = keys::binding(command) else {
            return Hidden;
        };
        let caps = self.engine.capabilities();
        match binding.when {
            When::Viewer | When::List | When::Rail | When::Recording | When::Attach => {
                return Hidden
            }
            // Of the message that had the keyboard when the palette opened.
            When::Message => {
                if !OF_A_MESSAGE.contains(&command) {
                    return Hidden;
                }
                let Some(message) = &self.palette.subject else {
                    return Hidden;
                };
                return match self.command_state(command, message) {
                    super::message_menu::CommandState::Enabled => Enabled,
                    super::message_menu::CommandState::Disabled(why) => Disabled(why),
                    super::message_menu::CommandState::Hidden => Hidden,
                };
            }
            When::Chat if self.open.is_none() => return Disabled(NO_CHAT),
            // The chat is behind Status: nothing is done to it unseen.
            When::Chat if self.status_active() => return Disabled("Show the chats first"),
            // Status: its list and its viewer, where they are on screen.
            When::Status if !self.status_active() => return Disabled("Open Status first"),
            When::Story if !self.story_is_open() => return Disabled("Open a status first"),
            _ => {}
        }
        let state = |on: bool| if on { Enabled } else { Disabled(NOT_HERE) };
        let numbered = |on: bool| match self.current_account() {
            Some(_) if on => Enabled,
            Some(_) => Disabled(NOT_HERE),
            None => Disabled(NO_NUMBER),
        };
        match command {
            // The palette itself, and keys that mean nothing as a row.
            C::Palette
            | C::PaletteCommands
            | C::FindNext
            | C::FindPrevious
            | C::LeaveConversation
            | C::Menu
            | C::PaneLeft
            | C::PaneRight => Hidden,
            C::Account(place) if usize::from(place) > self.accounts.len() => Hidden,
            C::NewChat => state(caps.start_chat),
            C::NewGroup => numbered(caps.group_create),
            C::NewPoll => state(caps.polls),
            C::AddNumber => state(caps.link_accounts),
            C::CheckForUpdates | C::RestartToUpdate
                if matches!(
                    crate::update::snapshot(cx).phase,
                    updater::Phase::Disabled(_)
                ) =>
            {
                Disabled("Updates are off in this build")
            }
            C::RestartToUpdate if crate::update::ready_version(cx).is_none() => {
                Disabled("No update is waiting")
            }
            C::SignOut => {
                let signed_in = matches!(
                    self.session,
                    super::shell::SessionKind::Keychain | super::shell::SessionKind::Unsaved
                );
                if signed_in {
                    Enabled
                } else {
                    Hidden
                }
            }
            C::Attach | C::Paste if !self.can_attach() => Disabled(super::attach::NOT_AVAILABLE),
            C::StickerPicker | C::GifPicker if !self.can_attach() => {
                Disabled(super::attach::NOT_AVAILABLE)
            }
            C::FavoriteSticker if !self.sticker_in_hand() => {
                Disabled("Open the stickers, or put a sticker message in focus")
            }
            C::RecordVoice if !self.can_record() => Disabled(super::voice_record::NOT_AVAILABLE),
            C::MuteChat
            | C::ArchiveChat
            | C::PinChat
            | C::MarkUnread
            | C::MuteFor
            | C::PinAChat
            | C::ArchiveAChat
            | C::MuteAChat
            | C::MarkAChat => state(caps.chat_state),
            // In the menus too, as an entry that says the same.
            C::DeleteChat => Disabled(super::shell::NOT_YET),
            C::InsertMention => match &self.open {
                Some(open) if open.chat.kind == ChatKind::Group => Enabled,
                _ => Disabled("Only in a group"),
            },
            C::JumpToFirstUnread => match self.first_unread_row() {
                Some(_) => Enabled,
                None => Disabled("Nothing unread here"),
            },
            C::SwitchNumber if self.accounts.len() < 2 => Disabled("There is one number"),
            C::FocusConversation if self.open.is_none() => Disabled(NO_CHAT),
            C::RenameNumber
            | C::NumberLook
            | C::MoveNumberUp
            | C::MoveNumberDown
            | C::MuteNumber => numbered(true),
            C::RemoveNumber => match self.current_account() {
                Some(account) if account.never_linked() => state(caps.manage_accounts),
                Some(_) => Disabled("Only a number that never was linked"),
                None => Disabled(NO_NUMBER),
            },
            C::LeaveRailGroup => match self.current_account() {
                Some(account) if self.rail.group_of(&self.rail_key(&account.id)).is_some() => {
                    Enabled
                }
                Some(_) => Disabled("This number is in no group"),
                None => Disabled(NO_NUMBER),
            },
            C::GroupNumberWith if self.accounts.len() < 2 => Disabled("Needs another number"),
            C::ToggleRailGroup | C::EditRailGroup | C::Ungroup if self.rail.groups().is_empty() => {
                Disabled("There are no groups in the rail")
            }
            C::EditProfile => numbered(caps.profile_edit),
            C::ReconnectNumber | C::LogOutNumber => numbered(caps.manage_accounts),
            C::LinkNumber => match self.current_account() {
                Some(account) if account.never_linked() => state(caps.link_accounts),
                Some(_) => Disabled("This number is linked"),
                None => Disabled(NO_NUMBER),
            },
            C::MoveNumberToGroup => match self.current_account() {
                Some(_) if self.rail.groups().is_empty() => {
                    Disabled("There are no groups in the rail")
                }
                Some(_) => Enabled,
                None => Disabled(NO_NUMBER),
            },
            C::ToggleReadReceipts if !caps.read_receipts => Disabled(NOT_HERE),
            C::ShowStatus => state(caps.story_list || caps.story_post || caps.story_contacts),
            C::ToggleStatusStrip => {
                state(caps.story_list || caps.story_post || caps.story_contacts)
            }
            C::NewStatus => state(caps.story_post),
            C::SettingsStatus => state(caps.story_privacy),
            C::ToggleStoryReceipts => state(caps.story_view),
            C::ShowChats if !self.status_active() => Disabled("The chats are showing"),
            _ => {
                let _ = cx;
                Enabled
            }
        }
    }

    /// What a command shows beside its name: the value it would change.
    pub(super) fn command_detail(&self, command: Command, cx: &gpui_kit::App) -> SharedString {
        use Command as C;
        let chosen = settings::get(cx);
        let chat = self.open.as_ref().map(|open| &open.chat);
        match command {
            C::SetTheme => theme_name(chosen.theme).into(),
            C::SetInterfaceSize => format!("{}%", chosen.interface_scale).into(),
            C::SetBubbles => bubbles_name(chosen.bubbles).into(),
            C::SetWallpaper => wallpaper_name(chosen.wallpaper).into(),
            C::SetMotion => motion_name(chosen.motion).into(),
            C::SetEmojiLanguage => language_name(prefs::language(cx)).into(),
            C::SetMediaDownload => media_name(chosen.media).into(),
            C::SetHistory => history_name(chosen.history).into(),
            C::ToggleReadReceipts => on_off(chosen.read_receipts),
            C::ToggleStoryReceipts => on_off(chosen.story_receipts),
            C::ToggleDesktopNotifications => on_off(chosen.desktop_notifications),
            C::ToggleMessagePreviews => on_off(chosen.message_previews),
            C::PinChat => chat.map_or("".into(), |chat| {
                if chat.pinned { "Pinned" } else { "Not pinned" }.into()
            }),
            C::MuteChat | C::MuteFor => chat.map_or("".into(), |chat| {
                if chat.muted { "Muted" } else { "Not muted" }.into()
            }),
            C::ArchiveChat => chat.map_or("".into(), |chat| {
                if chat.archived { "Archived" } else { "" }.into()
            }),
            _ => "".into(),
        }
    }

    /// The row of the open conversation the unread divider stands above.
    fn first_unread_row(&self) -> Option<MessageId> {
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            super::bubble::Row::Message(row) if row.unread_above.is_some() => {
                Some(row.stored.message.id.clone())
            }
            _ => None,
        })
    }

    /// The step `command` asks for before it can be run, if it asks.
    pub(super) fn step_for(
        &self,
        command: Command,
        chat: Option<ChatSummary>,
        cx: &gpui_kit::App,
    ) -> Option<Step> {
        use Command as C;
        let chosen = settings::get(cx);
        let choices = |title: &str, choices: Vec<Choice>| {
            Some(Step {
                command,
                title: title.to_owned().into(),
                kind: StepKind::Choices(choices),
                chat: chat.clone(),
            })
        };
        match command {
            C::SetTheme => choices(
                "Theme",
                [ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System]
                    .into_iter()
                    .map(|theme| {
                        choice(
                            theme_name(theme),
                            chosen.theme == theme,
                            Value::Theme(theme),
                        )
                    })
                    .collect(),
            ),
            C::SetInterfaceSize => choices(
                "Interface size",
                SCALE_STEPS
                    .into_iter()
                    .map(|step| {
                        choice(
                            format!("{step}%"),
                            chosen.interface_scale == step,
                            Value::Scale(step),
                        )
                    })
                    .collect(),
            ),
            C::SetBubbles => choices(
                "Message bubbles",
                [BubbleStyle::Brand, BubbleStyle::Lime, BubbleStyle::Neutral]
                    .into_iter()
                    .map(|style| {
                        choice(
                            bubbles_name(style),
                            chosen.bubbles == style,
                            Value::Bubbles(style),
                        )
                    })
                    .collect(),
            ),
            C::SetWallpaper => choices(
                "Chat wallpaper",
                [WallpaperChoice::Pattern, WallpaperChoice::Plain]
                    .into_iter()
                    .map(|wallpaper| {
                        choice(
                            wallpaper_name(wallpaper),
                            chosen.wallpaper == wallpaper,
                            Value::Wallpaper(wallpaper),
                        )
                    })
                    .collect(),
            ),
            C::SetMotion => choices(
                "Motion",
                [MotionChoice::System, MotionChoice::On, MotionChoice::Off]
                    .into_iter()
                    .map(|motion| {
                        choice(
                            motion_name(motion),
                            chosen.motion == motion,
                            Value::Motion(motion),
                        )
                    })
                    .collect(),
            ),
            C::SetEmojiLanguage => {
                let now = prefs::language(cx);
                choices(
                    "Emoji search language",
                    std::iter::once(LanguageChoice::System)
                        .chain(Lang::ALL.into_iter().map(LanguageChoice::Lang))
                        .map(|language| {
                            choice(
                                language_name(language),
                                now == language,
                                Value::Language(language),
                            )
                        })
                        .collect(),
                )
            }
            C::SetMediaDownload => choices(
                "Download automatically",
                [
                    MediaChoice::Images,
                    MediaChoice::Never,
                    MediaChoice::Everything,
                ]
                .into_iter()
                .map(|media| {
                    choice(
                        media_name(media),
                        chosen.media == media,
                        Value::Media(media),
                    )
                })
                .collect(),
            ),
            C::SetHistory => choices(
                "History to fetch",
                [
                    HistoryChoice::OnOpen,
                    HistoryChoice::Recent,
                    HistoryChoice::Everything,
                ]
                .into_iter()
                .map(|history| {
                    choice(
                        history_name(history),
                        chosen.history == history,
                        Value::History(history),
                    )
                })
                .collect(),
            ),
            C::MuteFor => {
                let subject = chat
                    .clone()
                    .or_else(|| self.open.as_ref().map(|open| open.chat.clone()));
                Some(Step {
                    command,
                    title: match &subject {
                        Some(chat) => format!("Mute {} for", chat.title).into(),
                        None => "Mute for".into(),
                    },
                    kind: StepKind::Choices(
                        MUTES
                            .into_iter()
                            .map(|(label, seconds)| choice(label, false, Value::Mute(seconds)))
                            .collect(),
                    ),
                    chat: subject,
                })
            }
            C::PinAChat | C::ArchiveAChat | C::MuteAChat | C::MarkAChat => Some(Step {
                command,
                title: match command {
                    C::PinAChat => "Pin or unpin",
                    C::ArchiveAChat => "Archive or unarchive",
                    C::MuteAChat => "Mute",
                    _ => "Mark as read or unread",
                }
                .into(),
                kind: StepKind::Chat,
                chat: None,
            }),
            C::NewChat => Some(Step {
                command,
                title: "New chat with".into(),
                kind: StepKind::Contact,
                chat: None,
            }),
            C::SwitchNumber => choices(
                "Number",
                self.accounts
                    .iter()
                    .map(|account| {
                        choice(
                            self.account_name(account),
                            self.account.as_ref() == Some(&account.id),
                            Value::Number(account.id.clone()),
                        )
                    })
                    .collect(),
            ),
            C::GroupNumberWith => choices(
                "Group with",
                self.accounts
                    .iter()
                    .filter(|account| self.account.as_ref() != Some(&account.id))
                    .map(|account| {
                        choice(
                            self.account_name(account),
                            false,
                            Value::Number(account.id.clone()),
                        )
                    })
                    .collect(),
            ),
            C::ToggleRailGroup | C::EditRailGroup | C::Ungroup => choices(
                match command {
                    C::ToggleRailGroup => "Expand or collapse",
                    C::EditRailGroup => "Rename",
                    _ => "Ungroup",
                },
                self.rail
                    .groups()
                    .into_iter()
                    .map(|group| choice(group.name.clone(), false, Value::Group(group.id)))
                    .collect(),
            ),
            C::MoveNumberToGroup => choices(
                "Move to",
                self.rail
                    .groups()
                    .into_iter()
                    .map(|group| choice(group.name.clone(), false, Value::Group(group.id)))
                    .collect(),
            ),
            C::JumpToDate => {
                // The days there are in what is loaded of the conversation,
                // newest first: each with its first message.
                let rows = &self.open.as_ref()?.rows;
                let mut days = Vec::new();
                for (index, row) in rows.iter().enumerate() {
                    if let super::bubble::Row::Day(label) = row {
                        let first = rows[index + 1..].iter().find_map(|row| match row {
                            super::bubble::Row::Message(row) => Some(row.stored.message.id.clone()),
                            super::bubble::Row::Day(_) => None,
                        });
                        if let Some(first) = first {
                            days.push(choice(label.clone(), false, Value::Day(first)));
                        }
                    }
                }
                days.reverse();
                choices("Go to", days)
            }
            C::Diagnostics => Some(Step {
                command,
                title: "Connection".into(),
                kind: StepKind::Lines(self.diagnostics()),
                chat: None,
            }),
            _ => None,
        }
    }

    /// Where the numbers, the outbox and the uploads stand, in lines.
    fn diagnostics(&self) -> Vec<(SharedString, SharedString)> {
        let mut lines: Vec<(SharedString, SharedString)> = self
            .accounts
            .iter()
            .map(|account| {
                let state: SharedString = connection_words(account).summary.into();
                (self.account_name(account).into(), state)
            })
            .collect();
        if lines.is_empty() {
            lines.push(("Numbers".into(), "None yet".into()));
        }
        let waiting = self
            .engine
            .store()
            .outbox_pending()
            .map(|pending| pending.len())
            .unwrap_or(0);
        lines.push((
            "Waiting to be sent".into(),
            match waiting {
                0 => "Nothing".into(),
                1 => "1 message".into(),
                n => format!("{n} messages").into(),
            },
        ));
        lines.push((
            "Sending files".into(),
            match self.engine.uploads_available() {
                Some(true) => "Available".into(),
                Some(false) => "Not available".into(),
                None => "Not checked yet".into(),
            },
        ));
        lines.push((
            "Provider".into(),
            self.engine.provider_id().to_owned().into(),
        ));
        push_live_line(&mut lines, self.engine.live_updates());
        lines
    }

    /// Sets what a choice says. `false` when there was nothing to do.
    pub(super) fn apply_choice(
        &mut self,
        step: &Step,
        value: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match value {
            Value::Theme(theme) => settings::update(cx, |settings| settings.theme = theme),
            Value::Scale(step) => settings::update(cx, |settings| settings.interface_scale = step),
            Value::Bubbles(style) => settings::update(cx, |settings| settings.bubbles = style),
            Value::Wallpaper(wallpaper) => {
                settings::update(cx, |settings| settings.wallpaper = wallpaper)
            }
            Value::Motion(motion) => settings::update(cx, |settings| settings.motion = motion),
            Value::Media(media) => settings::update(cx, |settings| settings.media = media),
            Value::History(history) => settings::update(cx, |settings| settings.history = history),
            Value::Language(language) => {
                prefs::update(cx, |chosen| chosen.language = language);
                cx.refresh_windows();
            }
            Value::Mute(seconds) => {
                let Some(chat) = &step.chat else {
                    return false;
                };
                let change = match seconds {
                    Some(seconds) => ChatChange::MutedFor(seconds),
                    None => ChatChange::Muted(true),
                };
                self.engine.update_chat(&chat.account_id, &chat.id, change);
            }
            Value::Number(other) if step.command == Command::GroupNumberWith => {
                let Some(account) = self.account.clone() else {
                    return false;
                };
                let with = self.rail_key(&other);
                self.rail_action_on(account, RailAction::GroupWith(with), window, cx);
            }
            Value::Number(account) => self.select_account(account, window, cx),
            Value::Group(group) => match step.command {
                Command::ToggleRailGroup => {
                    self.rail_group_action(group, RailAction::GroupToggle, window, cx)
                }
                Command::EditRailGroup => {
                    self.rail_group_action(group, RailAction::GroupEdit, window, cx)
                }
                Command::Ungroup => self.rail_group_action(group, RailAction::Ungroup, window, cx),
                _ => {
                    let Some(account) = self.account.clone() else {
                        return false;
                    };
                    self.rail_action_on(account, RailAction::MoveTo(group), window, cx);
                }
            },
            Value::Day(message) => self.jump_and_flash(message, cx),
        }
        cx.notify();
        true
    }

    /// Does to `chat` what a command about "a chat" does. `Some`: the
    /// step it asks for next.
    pub(super) fn apply_to_chat(
        &mut self,
        command: Command,
        chat: ChatSummary,
        cx: &mut Context<Self>,
    ) -> Option<Step> {
        use Command as C;
        let change = match command {
            C::MuteAChat if chat.muted => ChatChange::Muted(false),
            // How long comes next.
            C::MuteAChat => return self.step_for(C::MuteFor, Some(chat), cx),
            C::PinAChat => ChatChange::Pinned(!chat.pinned),
            C::ArchiveAChat => ChatChange::Archived(!chat.archived),
            C::MarkAChat if chat.unread_count > 0 => {
                self.engine.mark_read(&chat.account_id, &chat.id);
                return None;
            }
            C::MarkAChat => ChatChange::MarkedUnread,
            _ => return None,
        };
        self.engine.update_chat(&chat.account_id, &chat.id, change);
        None
    }

    /// Runs a command that only the palette reaches and that asks for
    /// nothing. `None`: not one of them.
    pub(super) fn run_palette_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use Command as C;
        let account = self.account.clone();
        Some(match command {
            C::PaletteCommands => {
                if self.overlay == Overlay::Palette {
                    self.close_overlay(window, cx);
                } else {
                    self.open_palette_with(">", window, cx);
                }
                true
            }
            C::ShowAllChats => {
                self.set_filter(ChatFilter::All, cx);
                true
            }
            C::ShowUnread => {
                self.set_filter(ChatFilter::Unread, cx);
                true
            }
            C::ShowGroups => {
                self.set_filter(ChatFilter::Groups, cx);
                true
            }
            C::FocusConversation => self.focus_conversation(window, cx),
            C::FocusRail => self.focus_rail(window, cx),
            C::MarkRead => match &self.open {
                Some(open) => {
                    self.engine.mark_read(&open.chat.account_id, &open.chat.id);
                    true
                }
                None => false,
            },
            C::JumpToFirstUnread => match self.first_unread_row() {
                Some(message) => {
                    self.jump_and_flash(message, cx);
                    true
                }
                None => false,
            },
            C::JumpToNewest => match &self.open {
                Some(open) => {
                    open.list.scroll_to_end();
                    true
                }
                None => false,
            },
            C::Paste => {
                let Some(item) = cx.read_from_clipboard() else {
                    return Some(false);
                };
                if !self.paste_into_chat(&item, cx) {
                    // Text: at the end of what is typed.
                    let Some(text) = item.text() else {
                        return Some(false);
                    };
                    self.composer.update(cx, |composer, cx| {
                        let typed = format!("{}{text}", composer.value());
                        composer.set_value(typed, window, cx);
                        composer.focus(window, cx);
                    });
                }
                true
            }
            C::InsertMention => {
                if !self
                    .open
                    .as_ref()
                    .is_some_and(|open| open.chat.kind == ChatKind::Group)
                {
                    return Some(false);
                }
                self.composer.update(cx, |composer, cx| {
                    let typed = composer.value().to_string();
                    let gap = if typed.is_empty() || typed.ends_with(' ') {
                        ""
                    } else {
                        " "
                    };
                    composer.set_value(format!("{typed}{gap}@"), window, cx);
                    composer.focus(window, cx);
                });
                self.mention_text_changed();
                true
            }
            C::LinkNumber | C::ReconnectNumber => match account {
                Some(account) => {
                    self.reconnect_number(account, window, cx);
                    true
                }
                None => false,
            },
            C::RenameNumber
            | C::NumberLook
            | C::LeaveRailGroup
            | C::MoveNumberUp
            | C::MoveNumberDown
            | C::MuteNumber
            | C::RemoveNumber => match account {
                Some(account) => {
                    let action = match command {
                        C::RenameNumber => RailAction::Rename,
                        C::NumberLook => RailAction::Look,
                        C::LeaveRailGroup => RailAction::LeaveGroup,
                        C::MoveNumberUp => RailAction::MoveUp,
                        C::MoveNumberDown => RailAction::MoveDown,
                        C::MuteNumber => RailAction::ToggleMute,
                        _ => RailAction::Remove,
                    };
                    self.rail_action_on(account, action, window, cx);
                    true
                }
                None => false,
            },
            C::EditProfile => match account {
                Some(account) => {
                    self.rail_action_on(account, RailAction::EditProfile, window, cx);
                    true
                }
                None => false,
            },
            C::LogOutNumber => match account {
                Some(account) => {
                    self.rail_action_on(account, RailAction::LogOut, window, cx);
                    true
                }
                None => false,
            },
            C::ToggleReadReceipts => {
                settings::update(cx, |settings| {
                    settings.read_receipts = !settings.read_receipts
                });
                true
            }
            C::ToggleDesktopNotifications => {
                settings::update(cx, |settings| {
                    settings.desktop_notifications = !settings.desktop_notifications
                });
                true
            }
            C::ToggleMessagePreviews => {
                settings::update(cx, |settings| {
                    settings.message_previews = !settings.message_previews
                });
                true
            }
            C::SettingsAccount | C::SettingsAbout => {
                self.open_overlay(Overlay::Settings, window, cx);
                self.settings_section = if command == C::SettingsAbout {
                    SettingsSection::About
                } else {
                    SettingsSection::Account
                };
                true
            }
            C::CheckForUpdates => {
                self.check_for_updates(window, cx);
                true
            }
            C::RestartToUpdate => {
                if crate::update::ready_version(cx).is_none() {
                    return Some(false);
                }
                self.restart_to_update(false, window, cx);
                true
            }
            C::Quit => {
                cx.quit();
                true
            }
            // A command that asks: the palette opens on its question.
            // ("New chat" has a panel of its own on its key.)
            _ if command != C::NewChat && self.step_for(command, None, cx).is_some() => {
                self.open_palette_on(command, window, cx);
                true
            }
            _ => return None,
        })
    }
}

/// The command an entry of a menu of the chat list or of a conversation
/// stands for, by the entry's name.
pub(super) fn menu_command(id: &str) -> Option<Command> {
    use Command as C;
    Some(match id {
        "menu-new-chat" => C::NewChat,
        "menu-new-group" => C::NewGroup,
        "menu-read-all" => C::MarkAllRead,
        "menu-archived" => C::ShowArchived,
        "menu-settings" => C::Settings,
        "menu-read" => C::MarkRead,
        "menu-unread" => C::MarkUnread,
        "menu-pin" => C::PinChat,
        "menu-mute" => C::MuteChat,
        "menu-archive" => C::ArchiveChat,
        "menu-info" => C::ChatInfo,
        "menu-search" => C::Find,
        "menu-poll" => C::NewPoll,
        "menu-close" => C::CloseChat,
        "menu-delete" => C::DeleteChat,
        "menu-shortcuts" => C::Shortcuts,
        _ => return None,
    })
}

/// The command an entry of the rail's menu stands for, by its name:
/// what the tests hold the rail's menu to.
#[cfg(test)]
pub(super) fn rail_command(id: &str) -> Option<Command> {
    use Command as C;
    Some(match id {
        "rail-rename" => C::RenameNumber,
        "rail-look" => C::NumberLook,
        "rail-profile" => C::EditProfile,
        "rail-new-group" => C::GroupNumberWith,
        "rail-leave-group" => C::LeaveRailGroup,
        "rail-up" => C::MoveNumberUp,
        "rail-down" => C::MoveNumberDown,
        "rail-read" => C::MarkAllRead,
        "rail-mute" => C::MuteNumber,
        "rail-reconnect" => C::ReconnectNumber,
        "rail-link" => C::LinkNumber,
        "rail-remove" => C::RemoveNumber,
        "rail-logout" => C::LogOutNumber,
        "rail-group-toggle" => C::ToggleRailGroup,
        "rail-group-edit" => C::EditRailGroup,
        "rail-ungroup" => C::Ungroup,
        _ if id.starts_with("rail-move-") => C::MoveNumberToGroup,
        _ if id.starts_with("rail-group-with-") => C::GroupNumberWith,
        _ => return None,
    })
}
