//! Every keyboard shortcut of the application, in one table.
//!
//! [`BINDINGS`] is the registry: what a command is called, the keys it has,
//! the section it is listed under and when it applies. The key handling
//! ([`resolve`]), the menus (which print a command's keys next to its
//! entry), the command palette and the shortcuts sheet all read this table
//! and nothing else, so they cannot drift apart.
//!
//! Keys are written once for every platform: [`Chord::secondary`] is Cmd on
//! macOS and Ctrl elsewhere, and [`keys_label`] prints a chord the way the
//! platform does (`⌘K`, `Ctrl+K`).
//!
//! The bindings are fixed. They are data, though, not code: a table read
//! from the user's settings could replace or extend this one without
//! anything else changing.

use gpui_kit::Keystroke;

/// Something the application can be told to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    // ----- anywhere
    /// The command palette: go to a chat, a person, a message.
    Palette,
    /// The palette on its commands (`>`).
    PaletteCommands,
    /// The palette, searching messages.
    SearchMessages,
    /// The list of shortcuts.
    Shortcuts,
    /// "New chat".
    NewChat,
    /// "New group".
    NewGroup,
    /// Settings.
    Settings,
    /// Settings, on its Keyboard section.
    SettingsKeyboard,
    /// Settings, on its Appearance section.
    SettingsAppearance,
    /// Settings, on its Sync section.
    SettingsSync,
    /// Settings, on its Notifications section.
    SettingsNotifications,
    /// Settings, on its Audio section.
    SettingsAudio,
    /// Settings, on its Stickers and GIFs section.
    SettingsStickers,
    /// The menu of the open conversation, or of the chat list.
    Menu,
    /// Search: in the open conversation, else in the chat list.
    Find,
    /// The next chat down the list.
    NextChat,
    /// The chat above.
    PreviousChat,
    /// The next chat with unread messages.
    NextUnread,
    /// The number at this place in the rail, from 1.
    Account(u8),
    /// The next number in the rail.
    NextAccount,
    /// The number before.
    PreviousAccount,
    /// Light to dark and back.
    ToggleTheme,
    /// The interface one step larger.
    Larger,
    /// One step smaller.
    Smaller,
    /// Back to the design's size.
    ActualSize,
    /// Marks every chat as read.
    MarkAllRead,
    /// The archived chats.
    ShowArchived,
    /// Links another number.
    AddNumber,
    /// Signs out.
    SignOut,
    /// Every chat: no filter.
    ShowAllChats,
    /// The unread chats.
    ShowUnread,
    /// The groups.
    ShowGroups,
    /// Chooses the number on screen.
    SwitchNumber,
    /// Moves the keyboard to the conversation.
    FocusConversation,
    /// Moves the keyboard to the rail.
    FocusRail,
    /// Pins or unpins a chat that is chosen next.
    PinAChat,
    /// Archives or unarchives a chat that is chosen next.
    ArchiveAChat,
    /// Mutes a chat that is chosen next, for a time chosen after.
    MuteAChat,
    /// Marks a chat that is chosen next as read, or as unread.
    MarkAChat,
    /// Links the number on screen, which never was.
    LinkNumber,
    /// Renames the number on screen.
    RenameNumber,
    /// Connects the number on screen again.
    ReconnectNumber,
    /// The WhatsApp profile of the number on screen.
    EditProfile,
    /// Moves the number on screen into a group of the rail.
    MoveNumberToGroup,
    /// Logs the number on screen out of WhatsApp.
    LogOutNumber,
    /// The icon and the colour of the number on screen, in the rail.
    NumberLook,
    /// Makes a group of the rail of the number on screen and another.
    GroupNumberWith,
    /// Takes the number on screen out of its group.
    LeaveRailGroup,
    /// Moves the number on screen up the rail.
    MoveNumberUp,
    /// Moves it down.
    MoveNumberDown,
    /// Mutes or unmutes the notifications of the number on screen.
    MuteNumber,
    /// Removes the number on screen, which never was linked.
    RemoveNumber,
    /// Opens or closes a group of the rail.
    ToggleRailGroup,
    /// Renames a group of the rail, or changes its colour.
    EditRailGroup,
    /// Takes a group of the rail apart.
    Ungroup,
    /// Light, dark, or the desktop's.
    SetTheme,
    /// The size of the interface.
    SetInterfaceSize,
    /// How the account's own bubbles are drawn.
    SetBubbles,
    /// What is behind the messages.
    SetWallpaper,
    /// Whether the interface moves.
    SetMotion,
    /// The language emoji are searched in.
    SetEmojiLanguage,
    /// Which media is downloaded without being asked.
    SetMediaDownload,
    /// How much history is fetched in the background.
    SetHistory,
    /// Read receipts, on or off.
    ToggleReadReceipts,
    /// Desktop notifications, on or off.
    ToggleDesktopNotifications,
    /// The message's text in notifications, on or off.
    ToggleMessagePreviews,
    /// Settings, on its Account section.
    SettingsAccount,
    /// Settings, on its About section.
    SettingsAbout,
    /// Asks whether there is a new version, now.
    CheckForUpdates,
    /// Restarts the application so that the downloaded version is installed.
    RestartToUpdate,
    /// Where the connection, the outbox and the uploads stand.
    Diagnostics,
    /// Quits the application.
    Quit,
    /// Moves the keyboard to the chat list.
    FocusList,
    /// Moves the keyboard a pane to the left: conversation, list, rail.
    PaneLeft,
    /// A pane to the right.
    PaneRight,

    // ----- status (stories)
    /// The Status list, in place of the chats.
    ShowStatus,
    /// The chats, in place of the Status list.
    ShowChats,
    /// A new status: a text, a picture or a video.
    NewStatus,
    /// Who sees the status, in the settings.
    SettingsStatus,
    /// Story view receipts, on or off.
    ToggleStoryReceipts,
    /// The strip of status updates above the chats, open or folded.
    ToggleStatusStrip,

    // ----- with the keyboard on the status list
    /// The row above.
    StatusUp,
    /// The row below.
    StatusDown,
    /// The first row.
    StatusFirst,
    /// The last row.
    StatusLast,
    /// Opens the status of the row.
    StatusOpen,
    /// Mutes or unmutes the author of the row.
    StatusMute,
    /// Shows or hides the muted updates.
    StatusToggleMuted,
    /// The detail of the account's own status: who saw it.
    StatusMine,
    /// Asks the provider for the updates again, now.
    StatusRefresh,

    // ----- while a status is being viewed
    /// The next story of the author.
    StoryNext,
    /// The story before.
    StoryPrevious,
    /// The next author's stories.
    StoryNextAuthor,
    /// The previous author's stories.
    StoryPreviousAuthor,
    /// Holds the story where it is, or lets it go on.
    StoryPause,
    /// Closes the viewer.
    StoryClose,
    /// Mutes the author's stories.
    StoryMute,
    /// Who saw it, and when it goes.
    StoryInfo,
    /// Writes a reply to the story.
    StoryReply,
    /// Reacts to the story.
    StoryReact,
    /// Plays the video in the system's player.
    StoryOpenVideo,
    /// Takes the story down (the account's own).
    StoryDelete,

    // ----- with the keyboard on the chat list
    /// The chat above.
    ListUp,
    /// The chat below.
    ListDown,
    /// The first chat.
    ListFirst,
    /// The last.
    ListLast,
    /// A screen up.
    ListPageUp,
    /// A screen down.
    ListPageDown,
    /// Opens the chat, and gives its composer the keyboard.
    ListOpen,
    /// The search field.
    ListSearch,
    /// The next filter: All, Unread, Groups, Archived.
    ListNextFilter,
    /// The filter before.
    ListPreviousFilter,
    /// Pins or unpins the chat, without opening it.
    ListPin,
    /// Archives or unarchives it.
    ListArchive,
    /// Mutes or unmutes it.
    ListMute,
    /// Marks it as read, or as unread.
    ListToggleRead,
    /// Its menu.
    ListMenu,
    /// Empties the search.
    ListClear,

    // ----- with the keyboard on the rail
    /// The number or group above.
    RailUp,
    /// The one below.
    RailDown,
    /// To the chat list.
    RailToList,

    // ----- with a chat open
    /// The next match of the search in the conversation.
    FindNext,
    /// The match before.
    FindPrevious,
    /// Picks files to send.
    Attach,
    /// "New poll".
    NewPoll,
    /// Who the conversation is with.
    ChatInfo,
    /// Mutes or unmutes the chat.
    MuteChat,
    /// Archives or unarchives it.
    ArchiveChat,
    /// Pins or unpins it.
    PinChat,
    /// Marks it as unread.
    MarkUnread,
    /// Closes the conversation.
    CloseChat,
    /// Closes the search in the conversation, else moves the keyboard to
    /// the chat list. The conversation stays open.
    LeaveConversation,
    /// Moves the keyboard to the newest message.
    FocusMessages,
    /// Starts recording a voice note, or stops the one under way.
    RecordVoice,
    /// Marks the chat as read.
    MarkRead,
    /// Deletes the chat. No provider can yet: listed, and said so.
    DeleteChat,
    /// Mutes the chat for a time chosen next.
    MuteFor,
    /// Goes to a day of the conversation.
    JumpToDate,
    /// Goes to the first unread message.
    JumpToFirstUnread,
    /// Goes to the newest message.
    JumpToNewest,
    /// Pastes what the clipboard holds into the conversation.
    Paste,
    /// Starts a mention in the composer.
    InsertMention,

    // ----- while a voice note is being recorded, or was
    /// Sends it.
    RecordSend,
    /// Throws it away.
    RecordDiscard,
    /// Pauses the recording or goes on; plays what was recorded.
    RecordPause,
    /// The emoji picker, over the composer.
    EmojiPicker,
    /// The same picker, opened on its Stickers tab.
    StickerPicker,
    /// The same picker, opened on its GIFs tab.
    GifPicker,
    /// Stars or unstars the sticker in hand: the one the picker is on, or
    /// the one in the message in focus.
    FavoriteSticker,
    /// Makes a sticker of a picture on this computer.
    ImportSticker,
    /// Adds GIFs from this computer: `.gif` files and short MP4s.
    ImportGif,

    // ----- while files are being attached
    /// The file before, in the sheet that shows what is about to be sent.
    AttachPrevious,
    /// The file after.
    AttachNext,
    /// Moves the file one place earlier in what is sent.
    AttachMoveEarlier,
    /// Moves it one place later.
    AttachMoveLater,
    /// Takes the file out of the sheet.
    AttachRemove,

    // ----- with a message in focus
    /// The message above.
    MessageUp,
    /// The message below.
    MessageDown,
    /// The oldest message at hand.
    MessageFirst,
    /// The newest.
    MessageLast,
    /// A screen up.
    PageUp,
    /// A screen down.
    PageDown,
    /// Takes the message above into the selection.
    ExtendUp,
    /// Takes the message below into the selection.
    ExtendDown,
    /// Back to the composer.
    BackToComposer,
    /// Replies to the message.
    Reply,
    /// Edits it.
    Edit,
    /// Copies its text, or what is selected.
    Copy,
    /// Copies its picture.
    CopyImage,
    /// Copies the link or the number it carries.
    CopyLink,
    /// Selects all of its text.
    SelectText,
    /// Forwards it.
    Forward,
    /// Stars or unstars it.
    Star,
    /// Deletes it.
    Delete,
    /// Reacts to it.
    React,
    /// Who reacted to it, and with what.
    Reactions,
    /// When it was sent, and how far it got.
    Info,
    /// Opens its file or its link.
    Open,
    /// Downloads its file.
    Download,
    /// Saves its file where the user says.
    SaveAs,
    /// Saves its file to the Downloads folder.
    SaveToDownloads,
    /// Keeps its sticker in the library, starred.
    SaveSticker,
    /// Keeps its GIF in the library.
    SaveGif,
    /// Its menu.
    MessageMenu,
    /// Starts selecting messages, with this one.
    SelectMessages,

    // ----- in the picture viewer
    /// The next picture of the chat.
    ViewerNext,
    /// The picture before.
    ViewerPrevious,
    /// Closer.
    ZoomIn,
    /// Further.
    ZoomOut,
    /// The whole picture.
    ZoomFit,
    /// The picture at its own size.
    ZoomActual,
}

/// When a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum When {
    /// Always, whatever has the keyboard.
    Anywhere,
    /// While a conversation is open.
    Chat,
    /// While a message has the keyboard: never while typing.
    Message,
    /// While the picture viewer is open.
    Viewer,
    /// While the chat list has the keyboard: never while typing.
    List,
    /// While a number or a group of the rail has the keyboard.
    Rail,
    /// While a voice note is being recorded, or waits to be sent.
    Recording,
    /// While the sheet of files about to be sent is open.
    Attach,
    /// While the status list has the keyboard.
    Status,
    /// While a status is being viewed.
    Story,
}

/// The sections of the shortcuts sheet, in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    /// Finding things.
    Search,
    /// Getting around.
    Navigation,
    /// From one pane to the next.
    Panes,
    /// With the keyboard on the chat list.
    ChatList,
    /// Chats.
    Chats,
    /// Writing: a new chat, a file, an emoji, a voice note.
    Compose,
    /// The numbers of the rail.
    Numbers,
    /// What the settings hold, changed from the palette.
    Settings,
    /// Moving through a conversation.
    Messages,
    /// What is done to the message in focus.
    Message,
    /// The picture viewer.
    Viewer,
    /// Status: the list, and the viewer.
    Status,
    /// The window.
    View,
    /// The application.
    Application,
}

impl Section {
    /// Every section, in the order they are listed.
    pub const ALL: [Section; 14] = [
        Section::Search,
        Section::Navigation,
        Section::Panes,
        Section::ChatList,
        Section::Chats,
        Section::Compose,
        Section::Messages,
        Section::Message,
        Section::Viewer,
        Section::Status,
        Section::View,
        Section::Numbers,
        Section::Settings,
        Section::Application,
    ];

    /// Its heading.
    pub fn title(self) -> &'static str {
        match self {
            Section::Search => "Search",
            Section::Navigation => "Navigation",
            Section::Panes => "Panes",
            Section::ChatList => "Chat list",
            Section::Chats => "Chats",
            Section::Compose => "Compose",
            Section::Numbers => "Numbers",
            Section::Settings => "Settings",
            Section::Messages => "Messages",
            Section::Message => "The message in focus",
            Section::Viewer => "The picture viewer",
            Section::Status => "Status",
            Section::View => "View",
            Section::Application => "Application",
        }
    }
}

/// One way of pressing a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    /// The key, as GPUI names it: a character (`k`, `+`, `/`) or a name
    /// (`enter`, `up`, `backspace`).
    pub key: &'static str,
    /// Cmd on macOS, Ctrl elsewhere.
    pub secondary: bool,
    /// Shift. For a character that needs Shift to be typed at all (`+`,
    /// `?`, `:`), it is not asked for: the character is what counts.
    pub shift: bool,
    /// Alt (Option).
    pub alt: bool,
    /// Ctrl itself, on every platform (Ctrl+Tab).
    pub control: bool,
}

/// A key on its own.
const fn key(key: &'static str) -> Chord {
    Chord {
        key,
        secondary: false,
        shift: false,
        alt: false,
        control: false,
    }
}

/// Cmd or Ctrl with a key.
const fn secondary(key: &'static str) -> Chord {
    Chord {
        secondary: true,
        ..self::key(key)
    }
}

/// Cmd or Ctrl, Shift and a key.
const fn secondary_shift(key: &'static str) -> Chord {
    Chord {
        shift: true,
        ..secondary(key)
    }
}

/// Shift with a key.
const fn shift(key: &'static str) -> Chord {
    Chord {
        shift: true,
        ..self::key(key)
    }
}

/// Alt with a key.
const fn alt(key: &'static str) -> Chord {
    Chord {
        alt: true,
        ..self::key(key)
    }
}

/// Ctrl itself with a key.
const fn control(key: &'static str) -> Chord {
    Chord {
        control: true,
        ..self::key(key)
    }
}

/// Ctrl itself, Shift and a key.
const fn control_shift(key: &'static str) -> Chord {
    Chord {
        shift: true,
        ..control(key)
    }
}

/// Characters that are typed with Shift on most layouts: a chord for one
/// of them matches the character, whatever was held to type it.
fn typed_with_shift(key: &str) -> bool {
    matches!(key, "+" | "?" | ":" | "<" | ">" | "_" | "~" | "!")
}

impl Chord {
    /// Whether `stroke` is this chord.
    pub fn matches(&self, stroke: &Keystroke) -> bool {
        let held = &stroke.modifiers;
        // Ctrl is the secondary key off macOS: a chord asks for one or
        // the other, never both.
        let (wants_secondary, wants_control) = (self.secondary, self.control);
        let secondary_ok = if cfg!(target_os = "macos") {
            held.platform == wants_secondary && held.control == wants_control
        } else {
            held.control == (wants_secondary || wants_control) && !held.platform
        };
        if !secondary_ok || held.alt != self.alt || held.function {
            return false;
        }
        // The character that was typed is what a character chord is about
        // (so `+` is `+` on every layout); named keys go by their name.
        let named = self.key.chars().count() > 1;
        if named {
            return stroke.key == self.key && held.shift == self.shift;
        }
        let typed = stroke
            .key_char
            .as_deref()
            .filter(|typed| !typed.is_empty() && !self.secondary && !self.control && !self.alt);
        match typed {
            Some(typed) if typed_with_shift(self.key) => typed == self.key,
            Some(typed) => typed.to_lowercase() == self.key && held.shift == self.shift,
            // With Ctrl or Cmd held nothing is typed: the key's own name.
            None if typed_with_shift(self.key) => {
                stroke.key == self.key || (held.shift && shifted(&stroke.key) == Some(self.key))
            }
            None => stroke.key.to_lowercase() == self.key && held.shift == self.shift,
        }
    }

    /// The chord as the platform writes it: `⇧⌘K` on macOS, `Ctrl+Shift+K`
    /// elsewhere.
    pub fn label(&self) -> String {
        self.label_for(cfg!(target_os = "macos"))
    }

    /// [`Self::label`] for a platform that is, or is not, macOS.
    pub fn label_for(&self, mac: bool) -> String {
        let name = key_name(self.key, mac);
        if mac {
            let mut out = String::new();
            for (held, glyph) in [
                (self.control, "⌃"),
                (self.alt, "⌥"),
                (self.shift, "⇧"),
                (self.secondary, "⌘"),
            ] {
                if held {
                    out.push_str(glyph);
                }
            }
            out.push_str(&name);
            out
        } else {
            let mut parts = Vec::new();
            if self.secondary || self.control {
                parts.push("Ctrl".to_owned());
            }
            if self.alt {
                parts.push("Alt".to_owned());
            }
            if self.shift {
                parts.push("Shift".to_owned());
            }
            parts.push(name);
            parts.join("+")
        }
    }
}

/// What Shift makes of a key on a US layout, for the characters a chord
/// may name: the fallback when the platform does not say what was typed.
fn shifted(key: &str) -> Option<&'static str> {
    Some(match key {
        "=" => "+",
        "/" => "?",
        ";" => ":",
        "," => "<",
        "." => ">",
        "-" => "_",
        "`" => "~",
        "1" => "!",
        _ => return None,
    })
}

/// A key's name as this platform prints it (`Esc`, `↩`).
pub fn key_label(key: &str) -> String {
    key_name(key, cfg!(target_os = "macos"))
}

/// A key's name as it is printed on the key.
fn key_name(key: &str, mac: bool) -> String {
    match key {
        "enter" => if mac { "↩" } else { "Enter" }.to_owned(),
        "escape" => "Esc".to_owned(),
        "backspace" => if mac { "⌫" } else { "Backspace" }.to_owned(),
        "delete" => if mac { "⌦" } else { "Delete" }.to_owned(),
        "tab" => if mac { "⇥" } else { "Tab" }.to_owned(),
        "up" => "↑".to_owned(),
        "down" => "↓".to_owned(),
        "left" => "←".to_owned(),
        "right" => "→".to_owned(),
        "home" => "Home".to_owned(),
        "end" => "End".to_owned(),
        "pageup" => "PgUp".to_owned(),
        "pagedown" => "PgDn".to_owned(),
        "space" => "Space".to_owned(),
        other => other.to_uppercase(),
    }
}

/// A command and the keys it has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// What it does.
    pub command: Command,
    /// What it is called, in menus, the palette and the sheet.
    pub label: &'static str,
    /// Where the sheet lists it.
    pub section: Section,
    /// When its keys apply.
    pub when: When,
    /// Its keys: the first is the one shown. Empty for a command that is
    /// reached from the palette and the menus only.
    pub chords: &'static [Chord],
}

const fn bind(
    command: Command,
    label: &'static str,
    section: Section,
    when: When,
    chords: &'static [Chord],
) -> Binding {
    Binding {
        command,
        label,
        section,
        when,
        chords,
    }
}

use Command as C;
use Section as S;
use When as W;

/// The registry.
pub const BINDINGS: &[Binding] = &[
    // Search
    bind(
        C::Palette,
        "Go to a chat, a person or a message",
        S::Search,
        W::Anywhere,
        &[secondary("p"), secondary("k")],
    ),
    bind(
        C::PaletteCommands,
        "Command palette",
        S::Search,
        W::Anywhere,
        &[secondary_shift("p")],
    ),
    bind(
        C::SearchMessages,
        "Search all messages",
        S::Search,
        W::Anywhere,
        &[secondary_shift("f")],
    ),
    bind(
        C::Find,
        "Search in the conversation",
        S::Search,
        W::Anywhere,
        &[secondary("f")],
    ),
    bind(
        C::FindNext,
        "Next match",
        S::Search,
        W::Chat,
        &[secondary("g")],
    ),
    bind(
        C::FindPrevious,
        "Previous match",
        S::Search,
        W::Chat,
        &[secondary_shift("g")],
    ),
    // Navigation
    bind(
        C::NextChat,
        "Next chat",
        S::Navigation,
        W::Anywhere,
        &[control("tab")],
    ),
    bind(
        C::PreviousChat,
        "Previous chat",
        S::Navigation,
        W::Anywhere,
        &[control_shift("tab")],
    ),
    bind(
        C::NextUnread,
        "Next unread chat",
        S::Navigation,
        W::Anywhere,
        &[secondary_shift("u")],
    ),
    bind(
        C::Account(1),
        "Number 1",
        S::Navigation,
        W::Anywhere,
        &[secondary("1")],
    ),
    bind(
        C::Account(2),
        "Number 2",
        S::Navigation,
        W::Anywhere,
        &[secondary("2")],
    ),
    bind(
        C::Account(3),
        "Number 3",
        S::Navigation,
        W::Anywhere,
        &[secondary("3")],
    ),
    bind(
        C::Account(4),
        "Number 4",
        S::Navigation,
        W::Anywhere,
        &[secondary("4")],
    ),
    bind(
        C::Account(5),
        "Number 5",
        S::Navigation,
        W::Anywhere,
        &[secondary("5")],
    ),
    bind(
        C::Account(6),
        "Number 6",
        S::Navigation,
        W::Anywhere,
        &[secondary("6")],
    ),
    bind(
        C::Account(7),
        "Number 7",
        S::Navigation,
        W::Anywhere,
        &[secondary("7")],
    ),
    bind(
        C::Account(8),
        "Number 8",
        S::Navigation,
        W::Anywhere,
        &[secondary("8")],
    ),
    bind(
        C::Account(9),
        "Number 9",
        S::Navigation,
        W::Anywhere,
        &[secondary("9")],
    ),
    bind(
        C::NextAccount,
        "Next number",
        S::Navigation,
        W::Anywhere,
        &[secondary_shift("]")],
    ),
    bind(
        C::PreviousAccount,
        "Previous number",
        S::Navigation,
        W::Anywhere,
        &[secondary_shift("[")],
    ),
    // Chats
    bind(
        C::NewChat,
        "New chat",
        S::Compose,
        W::Anywhere,
        &[secondary("n")],
    ),
    bind(
        C::NewGroup,
        "New group",
        S::Compose,
        W::Anywhere,
        &[secondary_shift("n")],
    ),
    bind(C::Menu, "Menu", S::Chats, W::Anywhere, &[secondary(".")]),
    bind(
        C::MarkAllRead,
        "Mark all as read",
        S::Chats,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ShowArchived,
        "Archived chats",
        S::Chats,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ChatInfo,
        "Contact or group info",
        S::Chats,
        W::Chat,
        &[secondary_shift("i")],
    ),
    bind(
        C::MuteChat,
        "Mute or unmute the chat",
        S::Chats,
        W::Chat,
        &[secondary_shift("m")],
    ),
    bind(
        C::ArchiveChat,
        "Archive or unarchive the chat",
        S::Chats,
        W::Chat,
        &[secondary_shift("a")],
    ),
    bind(
        C::PinChat,
        "Pin or unpin the chat",
        S::Chats,
        W::Chat,
        // "T" for the top of the list. (Ctrl+Shift+P is the command
        // palette, as in every editor.)
        &[secondary_shift("t")],
    ),
    bind(
        C::MarkUnread,
        "Mark the chat as unread",
        S::Chats,
        W::Chat,
        &[],
    ),
    bind(
        C::Attach,
        "Attach files",
        S::Compose,
        W::Chat,
        &[secondary("o")],
    ),
    bind(C::NewPoll, "New poll", S::Compose, W::Chat, &[]),
    bind(
        C::EmojiPicker,
        "Emoji",
        S::Compose,
        W::Chat,
        &[secondary("e")],
    ),
    // The sheet of files about to be sent. The arrows are the files' only
    // when no text is being typed (the strip has the keyboard): a caption
    // keeps its caret, so there the files go by Ctrl or Cmd with Page
    // Up and Page Down, which a text field does nothing with.
    bind(
        C::AttachPrevious,
        "Previous file, while attaching",
        S::Compose,
        W::Attach,
        &[secondary("pageup"), key("left")],
    ),
    bind(
        C::AttachNext,
        "Next file, while attaching",
        S::Compose,
        W::Attach,
        &[secondary("pagedown"), key("right")],
    ),
    bind(
        C::AttachMoveEarlier,
        "Move the file earlier, while attaching",
        S::Compose,
        W::Attach,
        &[secondary_shift("pageup")],
    ),
    bind(
        C::AttachMoveLater,
        "Move the file later, while attaching",
        S::Compose,
        W::Attach,
        &[secondary_shift("pagedown")],
    ),
    bind(
        C::AttachRemove,
        "Remove the file, while attaching",
        S::Compose,
        W::Attach,
        &[key("delete"), key("backspace")],
    ),
    bind(
        C::StickerPicker,
        "Stickers",
        S::Compose,
        W::Chat,
        &[secondary_shift("e")],
    ),
    bind(
        C::GifPicker,
        "GIFs",
        S::Compose,
        W::Chat,
        &[secondary_shift("j")],
    ),
    bind(
        C::FavoriteSticker,
        "Star or unstar a sticker",
        S::Compose,
        W::Chat,
        &[secondary("d")],
    ),
    bind(
        C::ImportSticker,
        "Make a sticker from a picture…",
        S::Compose,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ImportGif,
        "Add GIFs from files…",
        S::Compose,
        W::Anywhere,
        &[],
    ),
    bind(
        C::RecordVoice,
        "Record a voice note, or stop recording",
        S::Compose,
        W::Chat,
        &[secondary_shift("r")],
    ),
    bind(
        C::RecordSend,
        "Send the voice note",
        S::Chats,
        W::Recording,
        &[key("enter")],
    ),
    bind(
        C::RecordPause,
        "Pause the recording, or listen to it",
        S::Chats,
        W::Recording,
        &[key("space")],
    ),
    bind(
        C::RecordDiscard,
        "Throw the voice note away",
        S::Chats,
        W::Recording,
        &[key("escape")],
    ),
    bind(
        C::LeaveConversation,
        "Close the search, or go to the chat list",
        S::Chats,
        W::Chat,
        &[key("escape")],
    ),
    bind(
        C::CloseChat,
        "Close the chat",
        S::Chats,
        W::Chat,
        &[secondary("w")],
    ),
    // Panes
    bind(
        C::FocusList,
        "Focus the chat list",
        S::Panes,
        W::Anywhere,
        &[secondary("l")],
    ),
    bind(
        C::PaneLeft,
        "The pane to the left",
        S::Panes,
        W::Anywhere,
        &[alt("left")],
    ),
    bind(
        C::PaneRight,
        "The pane to the right",
        S::Panes,
        W::Anywhere,
        &[alt("right")],
    ),
    bind(
        C::RailUp,
        "The number above",
        S::Panes,
        W::Rail,
        &[key("up")],
    ),
    bind(
        C::RailDown,
        "The number below",
        S::Panes,
        W::Rail,
        &[key("down")],
    ),
    bind(
        C::RailToList,
        "From the rail to the chat list",
        S::Panes,
        W::Rail,
        &[key("right")],
    ),
    // The chat list
    bind(
        C::ListUp,
        "The chat above",
        S::ChatList,
        W::List,
        &[key("up"), key("k")],
    ),
    bind(
        C::ListDown,
        "The chat below",
        S::ChatList,
        W::List,
        &[key("down"), key("j")],
    ),
    bind(
        C::ListFirst,
        "The first chat",
        S::ChatList,
        W::List,
        &[key("home")],
    ),
    bind(
        C::ListLast,
        "The last chat",
        S::ChatList,
        W::List,
        &[key("end")],
    ),
    bind(
        C::ListPageUp,
        "A screen up",
        S::ChatList,
        W::List,
        &[key("pageup")],
    ),
    bind(
        C::ListPageDown,
        "A screen down",
        S::ChatList,
        W::List,
        &[key("pagedown")],
    ),
    bind(
        C::ListOpen,
        "Open the chat",
        S::ChatList,
        W::List,
        &[key("enter"), key("right")],
    ),
    bind(
        C::ListSearch,
        "Search the chats",
        S::ChatList,
        W::List,
        &[key("/")],
    ),
    bind(
        C::ListNextFilter,
        "The next filter",
        S::ChatList,
        W::List,
        &[key("tab")],
    ),
    bind(
        C::ListPreviousFilter,
        "The filter before",
        S::ChatList,
        W::List,
        &[shift("tab")],
    ),
    bind(
        C::ListPin,
        "Pin or unpin the chat",
        S::ChatList,
        W::List,
        &[key("p")],
    ),
    bind(
        C::ListArchive,
        "Archive or unarchive it",
        S::ChatList,
        W::List,
        &[key("a")],
    ),
    bind(
        C::ListMute,
        "Mute or unmute it",
        S::ChatList,
        W::List,
        &[key("m")],
    ),
    bind(
        C::ListToggleRead,
        "Mark it as read or unread",
        S::ChatList,
        W::List,
        &[key("u")],
    ),
    bind(
        C::ListMenu,
        "The chat's menu",
        S::ChatList,
        W::List,
        &[key("menu"), shift("f10")],
    ),
    bind(
        C::ListClear,
        "Empty the search",
        S::ChatList,
        W::List,
        &[key("escape")],
    ),
    // Messages
    bind(
        C::FocusMessages,
        "Go to the newest message",
        S::Messages,
        W::Chat,
        &[alt("up")],
    ),
    bind(
        C::MessageUp,
        "Message above",
        S::Messages,
        W::Message,
        &[key("up"), key("k")],
    ),
    bind(
        C::MessageDown,
        "Message below",
        S::Messages,
        W::Message,
        &[key("down"), key("j")],
    ),
    bind(
        C::MessageFirst,
        "Oldest message",
        S::Messages,
        W::Message,
        &[key("home")],
    ),
    bind(
        C::MessageLast,
        "Newest message",
        S::Messages,
        W::Message,
        &[key("end")],
    ),
    bind(
        C::PageUp,
        "A screen up",
        S::Messages,
        W::Message,
        &[key("pageup")],
    ),
    bind(
        C::PageDown,
        "A screen down",
        S::Messages,
        W::Message,
        &[key("pagedown")],
    ),
    bind(
        C::ExtendUp,
        "Select the message above too",
        S::Messages,
        W::Message,
        &[shift("up")],
    ),
    bind(
        C::ExtendDown,
        "Select the message below too",
        S::Messages,
        W::Message,
        &[shift("down")],
    ),
    bind(
        C::BackToComposer,
        "Back to the composer",
        S::Messages,
        W::Message,
        &[key("escape")],
    ),
    // The message in focus
    bind(
        C::Reply,
        "Reply",
        S::Message,
        W::Message,
        &[key("enter"), key("r")],
    ),
    bind(
        C::React,
        "React",
        S::Message,
        W::Message,
        &[key("+"), key(":")],
    ),
    bind(C::Edit, "Edit", S::Message, W::Message, &[key("e")]),
    bind(
        C::Copy,
        "Copy",
        S::Message,
        W::Message,
        &[secondary("c"), key("c")],
    ),
    bind(
        C::CopyLink,
        "Copy link or number",
        S::Message,
        W::Message,
        &[shift("c")],
    ),
    bind(
        C::CopyImage,
        "Copy image",
        S::Message,
        W::Message,
        &[secondary_shift("c")],
    ),
    bind(
        C::SelectText,
        "Select the message's text",
        S::Message,
        W::Message,
        &[secondary("a")],
    ),
    bind(C::Forward, "Forward", S::Message, W::Message, &[key("f")]),
    bind(
        C::Star,
        "Star or unstar",
        S::Message,
        W::Message,
        &[key("s")],
    ),
    bind(
        C::Reactions,
        "Who reacted",
        S::Message,
        W::Message,
        &[key("w")],
    ),
    bind(C::Info, "Message info", S::Message, W::Message, &[key("i")]),
    bind(
        C::Open,
        "Open the file or link",
        S::Message,
        W::Message,
        &[key("o")],
    ),
    bind(
        C::Download,
        "Download the file",
        S::Message,
        W::Message,
        &[key("d")],
    ),
    bind(
        C::SelectMessages,
        "Select messages",
        S::Message,
        W::Message,
        &[key("x")],
    ),
    bind(
        C::Delete,
        "Delete",
        S::Message,
        W::Message,
        &[key("backspace"), key("delete")],
    ),
    bind(
        C::SaveAs,
        "Save as…",
        S::Message,
        W::Message,
        &[secondary("s")],
    ),
    bind(
        C::SaveToDownloads,
        "Save to Downloads",
        S::Message,
        W::Message,
        &[secondary_shift("s")],
    ),
    bind(
        C::SaveSticker,
        "Add the sticker to favorites",
        S::Message,
        W::Message,
        &[key("v")],
    ),
    bind(
        C::SaveGif,
        "Save the GIF",
        S::Message,
        W::Message,
        &[key("g")],
    ),
    bind(
        C::MessageMenu,
        "Message menu",
        S::Message,
        W::Message,
        &[key("m")],
    ),
    // Status: getting to it, and what is done to a status.
    bind(
        C::ShowStatus,
        "Show Status",
        S::Status,
        W::Anywhere,
        &[alt("s")],
    ),
    bind(
        C::ShowChats,
        "Show chats",
        S::Status,
        W::Anywhere,
        &[alt("c")],
    ),
    bind(
        C::NewStatus,
        "New status",
        S::Status,
        W::Anywhere,
        &[alt("n")],
    ),
    bind(
        C::SettingsStatus,
        "Who sees my status",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleStoryReceipts,
        "Status view receipts",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleStatusStrip,
        "Show or fold the status updates above the chats",
        S::Status,
        W::Anywhere,
        &[],
    ),
    // The status list.
    bind(C::StatusUp, "Row above", S::Status, W::Status, &[key("up")]),
    bind(
        C::StatusDown,
        "Row below",
        S::Status,
        W::Status,
        &[key("down")],
    ),
    bind(
        C::StatusFirst,
        "First row",
        S::Status,
        W::Status,
        &[key("home")],
    ),
    bind(
        C::StatusLast,
        "Last row",
        S::Status,
        W::Status,
        &[key("end")],
    ),
    bind(
        C::StatusOpen,
        "Open the status",
        S::Status,
        W::Status,
        &[key("enter")],
    ),
    bind(
        C::StatusMute,
        "Mute or unmute their status",
        S::Status,
        W::Status,
        &[key("m")],
    ),
    bind(
        C::StatusToggleMuted,
        "Show or hide muted updates",
        S::Status,
        W::Status,
        &[key("u")],
    ),
    bind(
        C::StatusMine,
        "Who saw my status",
        S::Status,
        W::Status,
        &[key("i")],
    ),
    bind(
        C::StatusRefresh,
        "Check for status updates again",
        S::Status,
        W::Status,
        &[key("r")],
    ),
    // Viewing a status.
    bind(
        C::StoryNext,
        "Next story",
        S::Status,
        W::Story,
        &[key("right")],
    ),
    bind(
        C::StoryPrevious,
        "Previous story",
        S::Status,
        W::Story,
        &[key("left")],
    ),
    bind(
        C::StoryNextAuthor,
        "Next person's stories",
        S::Status,
        W::Story,
        &[key("down"), control("right")],
    ),
    bind(
        C::StoryPreviousAuthor,
        "Previous person's stories",
        S::Status,
        W::Story,
        &[key("up"), control("left")],
    ),
    bind(
        C::StoryPause,
        "Pause or go on",
        S::Status,
        W::Story,
        &[key("space")],
    ),
    bind(
        C::StoryClose,
        "Close the status",
        S::Status,
        W::Story,
        &[key("escape")],
    ),
    bind(
        C::StoryMute,
        "Mute their status",
        S::Status,
        W::Story,
        &[key("m")],
    ),
    bind(
        C::StoryInfo,
        "Who saw it, and when it goes",
        S::Status,
        W::Story,
        &[key("i")],
    ),
    bind(
        C::StoryReply,
        "Reply to the status",
        S::Status,
        W::Story,
        &[key("r")],
    ),
    bind(
        C::StoryReact,
        "React to the status",
        S::Status,
        W::Story,
        &[key("e")],
    ),
    bind(
        C::StoryOpenVideo,
        "Play the video",
        S::Status,
        W::Story,
        &[key("o")],
    ),
    bind(
        C::StoryDelete,
        "Delete my status",
        S::Status,
        W::Story,
        &[key("delete"), key("backspace")],
    ),
    // The picture viewer. (Copy, Save as… and Save to Downloads have
    // their keys there too: the ones they have on a message.)
    bind(
        C::ViewerNext,
        "Next picture",
        S::Viewer,
        W::Viewer,
        &[key("right")],
    ),
    bind(
        C::ViewerPrevious,
        "Previous picture",
        S::Viewer,
        W::Viewer,
        &[key("left")],
    ),
    bind(
        C::ZoomIn,
        "Zoom in",
        S::Viewer,
        W::Viewer,
        &[key("+"), key("=")],
    ),
    bind(C::ZoomOut, "Zoom out", S::Viewer, W::Viewer, &[key("-")]),
    bind(
        C::ZoomFit,
        "Fit the window",
        S::Viewer,
        W::Viewer,
        &[key("0")],
    ),
    bind(
        C::ZoomActual,
        "Actual size",
        S::Viewer,
        W::Viewer,
        &[key("1")],
    ),
    // View
    bind(
        C::ToggleTheme,
        "Light or dark",
        S::View,
        W::Anywhere,
        &[secondary_shift("d")],
    ),
    bind(
        C::Larger,
        "Larger",
        S::View,
        W::Anywhere,
        &[secondary("+"), secondary("=")],
    ),
    bind(
        C::Smaller,
        "Smaller",
        S::View,
        W::Anywhere,
        &[secondary("-")],
    ),
    bind(
        C::ActualSize,
        "Actual size",
        S::View,
        W::Anywhere,
        &[secondary("0")],
    ),
    // Application
    bind(
        C::Settings,
        "Settings",
        S::Application,
        W::Anywhere,
        &[secondary(",")],
    ),
    bind(
        C::SettingsAppearance,
        "Settings: Appearance",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsSync,
        "Settings: Sync",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsNotifications,
        "Settings: Notifications",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsAudio,
        "Settings: Audio",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsStickers,
        "Settings: Stickers and GIFs",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsKeyboard,
        "Settings: Keyboard",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::Shortcuts,
        "Keyboard shortcuts",
        S::Application,
        W::Anywhere,
        // "?" on its own too, when nothing is being typed.
        &[secondary("/"), secondary("?"), key("?")],
    ),
    bind(
        C::AddNumber,
        "Add a number",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(C::SignOut, "Sign out", S::Application, W::Anywhere, &[]),
    bind(
        C::Diagnostics,
        "Check the connection",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SettingsAccount,
        "Settings: Account",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(C::SettingsAbout, "About", S::Application, W::Anywhere, &[]),
    bind(
        C::CheckForUpdates,
        "Check for updates",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(
        C::RestartToUpdate,
        "Restart to update",
        S::Application,
        W::Anywhere,
        &[],
    ),
    bind(C::Quit, "Quit", S::Application, W::Anywhere, &[]),
    // Reached from the palette: no keys of their own.
    bind(
        C::ShowAllChats,
        "Show all chats",
        S::Navigation,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ShowUnread,
        "Show unread chats",
        S::Navigation,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ShowGroups,
        "Show groups",
        S::Navigation,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SwitchNumber,
        "Switch number…",
        S::Navigation,
        W::Anywhere,
        &[],
    ),
    bind(
        C::JumpToDate,
        "Go to a date in the conversation…",
        S::Navigation,
        W::Chat,
        &[],
    ),
    bind(
        C::JumpToFirstUnread,
        "Go to the first unread message",
        S::Navigation,
        W::Chat,
        &[],
    ),
    bind(
        C::JumpToNewest,
        "Go to the newest message",
        S::Navigation,
        W::Chat,
        &[],
    ),
    bind(
        C::FocusConversation,
        "Focus the conversation",
        S::Panes,
        W::Anywhere,
        &[],
    ),
    bind(C::FocusRail, "Focus the rail", S::Panes, W::Anywhere, &[]),
    bind(C::MarkRead, "Mark the chat as read", S::Chats, W::Chat, &[]),
    bind(C::DeleteChat, "Delete the chat", S::Chats, W::Chat, &[]),
    bind(C::MuteFor, "Mute the chat for…", S::Chats, W::Chat, &[]),
    bind(
        C::PinAChat,
        "Pin or unpin a chat…",
        S::Chats,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ArchiveAChat,
        "Archive or unarchive a chat…",
        S::Chats,
        W::Anywhere,
        &[],
    ),
    bind(C::MuteAChat, "Mute a chat…", S::Chats, W::Anywhere, &[]),
    bind(
        C::MarkAChat,
        "Mark a chat as read or unread…",
        S::Chats,
        W::Anywhere,
        &[],
    ),
    bind(C::Paste, "Paste", S::Compose, W::Chat, &[]),
    bind(
        C::InsertMention,
        "Mention somebody",
        S::Compose,
        W::Chat,
        &[],
    ),
    bind(
        C::LinkNumber,
        "Link this number",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::RenameNumber,
        "Rename this number",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ReconnectNumber,
        "Reconnect this number",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::EditProfile,
        "Edit this number's WhatsApp profile",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::MoveNumberToGroup,
        "Move this number to a group…",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::LogOutNumber,
        "Log this number out",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::NumberLook,
        "Change this number's icon and colour",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::GroupNumberWith,
        "New group of numbers with…",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::LeaveRailGroup,
        "Remove this number from its group",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::MoveNumberUp,
        "Move this number up",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::MoveNumberDown,
        "Move this number down",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::MuteNumber,
        "Mute or unmute this number's notifications",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::RemoveNumber,
        "Remove this number",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleRailGroup,
        "Expand or collapse a group of numbers…",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::EditRailGroup,
        "Rename a group of numbers…",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(
        C::Ungroup,
        "Ungroup a group of numbers…",
        S::Numbers,
        W::Anywhere,
        &[],
    ),
    bind(C::SetTheme, "Theme…", S::Settings, W::Anywhere, &[]),
    bind(
        C::SetInterfaceSize,
        "Interface size…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SetBubbles,
        "Message bubbles…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SetWallpaper,
        "Chat wallpaper…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(C::SetMotion, "Motion…", S::Settings, W::Anywhere, &[]),
    bind(
        C::SetEmojiLanguage,
        "Emoji search language…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SetMediaDownload,
        "Download media automatically…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::SetHistory,
        "History to fetch…",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleReadReceipts,
        "Read receipts",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleDesktopNotifications,
        "Desktop notifications",
        S::Settings,
        W::Anywhere,
        &[],
    ),
    bind(
        C::ToggleMessagePreviews,
        "Message previews in notifications",
        S::Settings,
        W::Anywhere,
        &[],
    ),
];

/// Cmd or Ctrl with Enter in the palette: runs, and the palette stays.
pub const RUN_AND_STAY: Chord = secondary("enter");

/// The binding of a command.
pub fn binding(command: Command) -> Option<&'static Binding> {
    BINDINGS.iter().find(|binding| binding.command == command)
}

/// What a command is called.
pub fn label(command: Command) -> &'static str {
    binding(command).map_or("", |binding| binding.label)
}

/// The keys shown next to a command, written as this platform does; `None`
/// for a command without keys.
pub fn keys_label(command: Command) -> Option<String> {
    binding(command)?.chords.first().map(Chord::label)
}

/// What is true of the window when a key is pressed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Context {
    /// A conversation is open.
    pub chat: bool,
    /// A message has the keyboard (and so nothing is being typed).
    pub message: bool,
    /// A text field has the keyboard: a key on its own is a character.
    pub typing: bool,
    /// The picture viewer is open: its keys come first, and what acts on
    /// a message's file acts on the picture.
    pub viewer: bool,
    /// The chat list has the keyboard.
    pub list: bool,
    /// A number or a group of the rail has the keyboard.
    pub rail: bool,
    /// A voice note is being recorded, or waits to be sent.
    pub recording: bool,
    /// The sheet of files about to be sent is open.
    pub attaching: bool,
    /// The status list has the keyboard.
    pub status: bool,
    /// A status is being viewed.
    pub story: bool,
}

/// The command a keystroke stands for, given what is true of the window.
/// The narrowest context wins: a key that means something on a message
/// means that while a message is in focus.
pub fn resolve(stroke: &Keystroke, context: Context) -> Option<Command> {
    let applies = |when: When| match when {
        When::Viewer => context.viewer,
        // In the viewer the picture stands for the message in focus.
        When::Message => context.message || context.viewer,
        When::List => context.list,
        When::Rail => context.rail,
        When::Recording => context.recording,
        When::Attach => context.attaching,
        When::Status => context.status,
        When::Story => context.story,
        When::Chat => context.chat,
        When::Anywhere => true,
    };
    [
        When::Story,
        When::Viewer,
        When::Attach,
        When::Recording,
        When::Status,
        When::Message,
        When::List,
        When::Rail,
        When::Chat,
        When::Anywhere,
    ]
    .into_iter()
    .filter(|when| applies(*when))
    .find_map(|when| {
        BINDINGS
            .iter()
            .filter(|binding| binding.when == when)
            .find(|binding| {
                binding.chords.iter().any(|chord| {
                    // A bare key is a command only where it cannot be
                    // a character: on a message, or with no text field
                    // in focus.
                    let bare = !chord.secondary && !chord.control && !chord.alt;
                    let typed = bare && context.typing && chord.key != "escape";
                    // Ctrl with Left or Right moves the caret by a word:
                    // in a text field it is the field's, whatever it means
                    // around it (the next person's stories, in the viewer).
                    let caret =
                        context.typing && chord.control && matches!(chord.key, "left" | "right");
                    !typed && !caret && chord.matches(stroke)
                })
            })
            .map(|binding| binding.command)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn stroke(source: &str) -> Keystroke {
        Keystroke::parse(source).expect("a keystroke")
    }

    /// A character as the platform reports it when it is typed.
    fn typed(key: &str, character: &str, shift: bool) -> Keystroke {
        let mut stroke = Keystroke::parse(key).expect("a keystroke");
        stroke.modifiers.shift = shift;
        stroke.key_char = Some(character.to_owned());
        stroke
    }

    const SECONDARY: &str = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };

    #[test]
    fn every_command_is_bound_once_and_named() {
        let mut seen = HashSet::new();
        for binding in BINDINGS {
            assert!(
                seen.insert(binding.command),
                "{:?} is listed twice",
                binding.command
            );
            assert!(
                !binding.label.trim().is_empty(),
                "{:?} has no name",
                binding.command
            );
            assert!(Section::ALL.contains(&binding.section));
        }
    }

    #[test]
    fn no_two_commands_share_a_key_where_both_apply() {
        // Two bindings clash when the same chord is theirs in the same
        // context. A message's keys may repeat an "anywhere" chord only on
        // purpose (Ctrl+C, Ctrl+A): then the message's wins, by `resolve`.
        let mut taken: HashMap<(When, Chord), Command> = HashMap::new();
        for binding in BINDINGS {
            for chord in binding.chords {
                if let Some(other) = taken.insert((binding.when, *chord), binding.command) {
                    panic!(
                        "{} is both {other:?} and {:?} in {:?}",
                        chord.label_for(false),
                        binding.command,
                        binding.when
                    );
                }
            }
        }
        // And across contexts: a chord of the chat or of a message that is
        // also an "anywhere" chord would hide it.
        for binding in BINDINGS
            .iter()
            .filter(|binding| !matches!(binding.when, When::Anywhere | When::Viewer))
        {
            for chord in binding.chords {
                assert!(
                    !taken.contains_key(&(When::Anywhere, *chord)),
                    "{} of {:?} hides a shortcut that applies anywhere",
                    chord.label_for(false),
                    binding.command
                );
                // Escape is the one key with two meanings: on a message it
                // goes back to the composer before it closes anything.
                if binding.when == When::Message && chord.key != "escape" {
                    assert!(
                        !taken.contains_key(&(When::Chat, *chord)),
                        "{} of {:?} hides a chat shortcut",
                        chord.label_for(false),
                        binding.command
                    );
                }
            }
        }
    }

    #[test]
    fn nothing_clashes_with_editing_text() {
        // What a text field does with Cmd/Ctrl held is the field's. Only a
        // message in focus may take C and A: nothing is being typed then.
        let editing = [
            "a",
            "c",
            "v",
            "x",
            "z",
            "y",
            "left",
            "right",
            "backspace",
            "delete",
        ];
        for binding in BINDINGS.iter().filter(|binding| {
            // On a message, in the viewer, on the chat list, on the
            // rail and on a status the keyboard is not a text field's.
            !matches!(
                binding.when,
                When::Message
                    | When::Viewer
                    | When::List
                    | When::Rail
                    | When::Recording
                    | When::Attach
                    | When::Status
                    | When::Story
            )
        }) {
            for chord in binding
                .chords
                .iter()
                .filter(|chord| chord.secondary && !chord.shift)
            {
                assert!(
                    !editing.contains(&chord.key),
                    "{:?} takes {} from text fields",
                    binding.command,
                    chord.label_for(false)
                );
            }
            // And nothing outside a message is a bare key, but Escape.
            for chord in binding
                .chords
                .iter()
                .filter(|chord| chord.key != "escape" && chord.key != "?")
            {
                assert!(
                    chord.secondary || chord.control || chord.alt,
                    "{:?} would fire while typing",
                    binding.command
                );
            }
        }
    }

    #[test]
    fn keys_resolve_by_context_and_never_with_stray_modifiers() {
        let anywhere = Context::default();
        let chat = Context {
            chat: true,
            message: false,
            typing: true,
            viewer: false,
            list: false,
            rail: false,
            recording: false,
            attaching: false,
            status: false,
            story: false,
        };
        let message = Context {
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
        // "?" lists the shortcuts, but in a text field it is a question
        // mark.
        let mut question = stroke("shift-/");
        question.key_char = Some("?".into());
        assert_eq!(resolve(&question, anywhere), Some(C::Shortcuts));
        assert_eq!(resolve(&question, message), Some(C::Shortcuts));
        assert_eq!(resolve(&question, chat), None);
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-k")), anywhere),
            Some(C::Palette)
        );
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-shift-f")), chat),
            Some(C::SearchMessages)
        );
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-f")), chat),
            Some(C::Find)
        );
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-3")), anywhere),
            Some(C::Account(3))
        );
        assert_eq!(resolve(&stroke("ctrl-tab"), anywhere), Some(C::NextChat));
        assert_eq!(
            resolve(&stroke("ctrl-shift-tab"), anywhere),
            Some(C::PreviousChat)
        );
        assert_eq!(resolve(&stroke("alt-up"), chat), Some(C::FocusMessages));
        assert_eq!(
            resolve(&stroke("alt-up"), anywhere),
            None,
            "no chat, no messages"
        );

        // Single keys belong to a message in focus, and to nothing else.
        for (key, command) in [
            ("e", C::Edit),
            ("r", C::Reply),
            ("enter", C::Reply),
            ("c", C::Copy),
            ("f", C::Forward),
            ("s", C::Star),
            ("i", C::Info),
            ("w", C::Reactions),
            ("o", C::Open),
            ("d", C::Download),
            ("m", C::MessageMenu),
            ("backspace", C::Delete),
            ("delete", C::Delete),
            ("up", C::MessageUp),
            ("down", C::MessageDown),
            ("home", C::MessageFirst),
            ("end", C::MessageLast),
            ("escape", C::BackToComposer),
            ("shift-up", C::ExtendUp),
        ] {
            assert_eq!(resolve(&stroke(key), message), Some(command), "{key}");
            // While typing none of them is a command; Escape alone means
            // something there too (it closes the search, or goes to the
            // chat list).
            let typing = resolve(&stroke(key), chat);
            assert_eq!(
                typing,
                (key == "escape").then_some(C::LeaveConversation),
                "{key}"
            );
        }
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-c")), message),
            Some(C::Copy)
        );
        assert_eq!(resolve(&stroke(&format!("{SECONDARY}-c")), chat), None);
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-a")), message),
            Some(C::SelectText)
        );

        // A modifier that was not asked for is another key.
        assert_eq!(resolve(&stroke("alt-e"), message), None);
        assert_eq!(resolve(&stroke(&format!("{SECONDARY}-r")), message), None);
        // Cmd or Ctrl with E is a command of its own, not "Edit".
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-e")), message),
            Some(C::EmojiPicker)
        );
        assert_eq!(resolve(&stroke("shift-e"), message), None);
        assert_eq!(resolve(&stroke("shift-c"), message), Some(C::CopyLink));
    }

    #[test]
    fn the_files_of_the_attach_sheet_are_moved_by_keys_a_caption_does_not_use() {
        let sheet = |typing: bool| Context {
            chat: true,
            typing,
            attaching: true,
            ..Context::default()
        };
        let caption = sheet(true);
        let strip = sheet(false);
        let page = |down: bool, extra: &str| {
            stroke(&format!(
                "{SECONDARY}-{extra}{}",
                if down { "pagedown" } else { "pageup" }
            ))
        };
        // Cmd or Ctrl with Page Up and Page Down, from the caption.
        assert_eq!(resolve(&page(false, ""), caption), Some(C::AttachPrevious));
        assert_eq!(resolve(&page(true, ""), caption), Some(C::AttachNext));
        assert_eq!(
            resolve(&page(false, "shift-"), caption),
            Some(C::AttachMoveEarlier)
        );
        assert_eq!(
            resolve(&page(true, "shift-"), caption),
            Some(C::AttachMoveLater)
        );
        // The caret's keys are the caption's: the arrows and Delete mean
        // nothing to the sheet while text is being typed...
        for key in ["left", "right", "delete", "backspace"] {
            assert_eq!(resolve(&stroke(key), caption), None, "{key}");
            assert_eq!(
                resolve(&stroke(&format!("{SECONDARY}-{key}")), caption),
                None,
                "{SECONDARY}-{key}"
            );
        }
        // ...and walk the strip when it has the keyboard.
        assert_eq!(resolve(&stroke("left"), strip), Some(C::AttachPrevious));
        assert_eq!(resolve(&stroke("right"), strip), Some(C::AttachNext));
        assert_eq!(resolve(&stroke("delete"), strip), Some(C::AttachRemove));
        assert_eq!(resolve(&stroke("backspace"), strip), Some(C::AttachRemove));
        // Without the sheet none of it is a command.
        let chat = Context {
            chat: true,
            typing: true,
            ..Context::default()
        };
        assert_eq!(resolve(&page(true, ""), chat), None);
        assert_eq!(resolve(&stroke("left"), Context::default()), None);
        // Adding files from the sheet is the chat's own key.
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-o")), caption),
            Some(C::Attach)
        );
        // Written as the platform writes them.
        let next = binding(C::AttachNext).unwrap().chords[0];
        assert_eq!(next.label_for(false), "Ctrl+PgDn");
    }

    #[test]
    fn characters_go_by_what_was_typed_not_by_where_the_key_is() {
        let message = Context {
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
        // "+" is Shift and "=" on a US keyboard and a key of its own on
        // others: either way it is "+".
        assert_eq!(resolve(&typed("=", "+", true), message), Some(C::React));
        assert_eq!(resolve(&typed("+", "+", false), message), Some(C::React));
        assert_eq!(resolve(&typed(";", ":", true), message), Some(C::React));
        // A layout whose keys are not Latin still types "e" somewhere.
        assert_eq!(resolve(&typed("e", "e", false), message), Some(C::Edit));
        // Caps Lock does not make "E" a different command.
        assert_eq!(resolve(&typed("e", "E", false), message), Some(C::Edit));
        // With Ctrl held nothing is typed: Ctrl+Shift+"/" is Ctrl+"?".
        let mut help = stroke(&format!("{SECONDARY}-shift-/"));
        help.key_char = None;
        assert_eq!(resolve(&help, Context::default()), Some(C::Shortcuts));
        assert_eq!(
            resolve(&stroke(&format!("{SECONDARY}-/")), Context::default()),
            Some(C::Shortcuts)
        );
    }

    #[test]
    fn in_a_text_field_of_the_story_viewer_control_and_an_arrow_are_the_fields() {
        let watching = Context {
            story: true,
            ..Context::default()
        };
        let replying = Context {
            typing: true,
            ..watching
        };
        assert_eq!(
            resolve(&stroke("ctrl-right"), watching),
            Some(C::StoryNextAuthor)
        );
        assert_eq!(
            resolve(&stroke("ctrl-left"), watching),
            Some(C::StoryPreviousAuthor)
        );
        // In the reply field they move the caret by a word: the person
        // being answered must not change under what is being written.
        assert_eq!(resolve(&stroke("ctrl-right"), replying), None);
        assert_eq!(resolve(&stroke("ctrl-left"), replying), None);
        // Escape still leaves the field.
        assert_eq!(resolve(&stroke("escape"), replying), Some(C::StoryClose));
    }

    #[test]
    fn status_and_the_pickers_and_the_attach_sheet_keep_their_keys_side_by_side() {
        // Status is reached from anywhere, a text field included.
        let typing = Context {
            chat: true,
            typing: true,
            ..Context::default()
        };
        assert_eq!(resolve(&stroke("alt-s"), typing), Some(C::ShowStatus));
        assert_eq!(resolve(&stroke("alt-c"), typing), Some(C::ShowChats));
        assert_eq!(resolve(&stroke("alt-n"), typing), Some(C::NewStatus));
        // The pickers and attaching are a conversation's.
        let stickers = stroke(&format!("{SECONDARY}-shift-e"));
        let gifs = stroke(&format!("{SECONDARY}-shift-j"));
        let attach = stroke(&format!("{SECONDARY}-o"));
        assert_eq!(resolve(&stickers, typing), Some(C::StickerPicker));
        assert_eq!(resolve(&gifs, typing), Some(C::GifPicker));
        assert_eq!(resolve(&attach, typing), Some(C::Attach));
        // With Status in the conversation's place there is no chat for
        // them, on the list or in front of a story, typing or not.
        let list = Context {
            status: true,
            ..Context::default()
        };
        let watching = Context {
            story: true,
            ..Context::default()
        };
        let replying = Context {
            typing: true,
            ..watching
        };
        for context in [list, watching, replying] {
            for stroke in [&stickers, &gifs, &attach] {
                assert_eq!(resolve(stroke, context), None);
            }
            // Status itself is still a key away.
            assert_eq!(resolve(&stroke("alt-n"), context), Some(C::NewStatus));
        }
        // The same bare keys mean what is on screen: a story, the strip
        // of files, or a message.
        assert_eq!(resolve(&stroke("left"), watching), Some(C::StoryPrevious));
        assert_eq!(resolve(&stroke("delete"), watching), Some(C::StoryDelete));
        assert_eq!(resolve(&stroke("e"), watching), Some(C::StoryReact));
        let strip = Context {
            chat: true,
            attaching: true,
            ..Context::default()
        };
        assert_eq!(resolve(&stroke("left"), strip), Some(C::AttachPrevious));
        assert_eq!(resolve(&stroke("delete"), strip), Some(C::AttachRemove));
        let message = Context {
            chat: true,
            message: true,
            ..Context::default()
        };
        assert_eq!(resolve(&stroke("f"), message), Some(C::Forward));
        assert_eq!(resolve(&stroke("v"), message), Some(C::SaveSticker));
        assert_eq!(resolve(&stroke("g"), message), Some(C::SaveGif));
        for key in ["f", "v", "g"] {
            assert_eq!(resolve(&stroke(key), watching), None, "{key}");
            assert_eq!(resolve(&stroke(key), list), None, "{key}");
        }
        // No key of a story or of the status list is also a key of the
        // attach sheet's caption, of a chat or of anywhere with the same
        // modifiers: each context's bare keys are its own. (Escape closes
        // whatever is in front, the story first.)
        for binding in BINDINGS
            .iter()
            .filter(|binding| matches!(binding.when, When::Story | When::Status))
        {
            for chord in binding.chords.iter().filter(|chord| chord.key != "escape") {
                let elsewhere = BINDINGS.iter().find(|other| {
                    matches!(other.when, When::Chat | When::Anywhere)
                        && other.chords.contains(chord)
                });
                assert!(
                    elsewhere.is_none(),
                    "{} of {:?} is also {:?}",
                    chord.label_for(false),
                    binding.command,
                    elsewhere.map(|other| other.command)
                );
            }
        }
    }

    #[test]
    fn keys_are_written_as_the_platform_writes_them() {
        let palette = binding(C::Palette).unwrap().chords[0];
        assert_eq!(palette.label_for(false), "Ctrl+P");
        assert_eq!(palette.label_for(true), "⌘P");
        let search = binding(C::SearchMessages).unwrap().chords[0];
        assert_eq!(search.label_for(false), "Ctrl+Shift+F");
        assert_eq!(search.label_for(true), "⇧⌘F");
        let next = binding(C::NextChat).unwrap().chords[0];
        assert_eq!(next.label_for(false), "Ctrl+Tab");
        assert_eq!(next.label_for(true), "⌃⇥");
        assert_eq!(
            binding(C::Reply).unwrap().chords[0].label_for(false),
            "Enter"
        );
        assert_eq!(binding(C::Reply).unwrap().chords[0].label_for(true), "↩");
        assert_eq!(
            binding(C::Delete).unwrap().chords[0].label_for(false),
            "Backspace"
        );
        assert_eq!(
            binding(C::FocusMessages).unwrap().chords[0].label_for(false),
            "Alt+↑"
        );
        assert_eq!(
            binding(C::FocusMessages).unwrap().chords[0].label_for(true),
            "⌥↑"
        );
        assert_eq!(binding(C::Edit).unwrap().chords[0].label_for(false), "E");
        // A command without keys has none to show.
        assert_eq!(keys_label(C::SignOut), None);
        assert_eq!(label(C::SignOut), "Sign out");
    }
}
