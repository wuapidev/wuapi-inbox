//! The user's preferences, kept across restarts.
//!
//! One small JSON file, `settings.json`, in the application's data
//! directory (`--data-dir`, or the platform's: `~/.local/share/<name>` on
//! Linux, see [`product::data_dir`](crate::product::data_dir)). It holds
//! nothing secret and nothing a provider owns, so it sits next to the
//! databases rather than inside one: it has to be readable before anyone is
//! signed in.
//!
//! A missing or unreadable file means the defaults. A write that fails is
//! logged and the choice still applies for the session.

use crate::theme::{self, Appearance, BubbleStyle};
use client_core::HistoryMode;
use gpui_kit::{App, Global, WindowAppearance};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file's name inside the data directory.
pub const FILE_NAME: &str = "settings.json";

/// Which theme to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Always light.
    Light,
    /// Always dark.
    Dark,
    /// Whatever the desktop is set to.
    #[default]
    System,
}

/// Whether the interface moves: the logo's animation and the entrances.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionChoice {
    /// Follow the desktop's reduced-motion setting, where it has one.
    /// Where it cannot be read, motion is on.
    #[default]
    System,
    /// Animate.
    On,
    /// Keep everything still.
    Off,
}

/// How much message history is fetched without being asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryChoice {
    /// A chat's history is fetched when it is opened, and further back as
    /// the user scrolls. The chat list needs none of it.
    #[default]
    OnOpen,
    /// The newest page of the most recent chats, in the background.
    Recent,
    /// Every chat, all the way back, in the background.
    Everything,
}

/// Which media is downloaded without being asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaChoice {
    /// [`Everything`](Self::Everything) with the numbers of this
    /// computer, which fetch straight from WhatsApp;
    /// [`Images`](Self::Images) through Wuapi Cloud.
    #[default]
    Automatic,
    /// Images, stickers and GIFs, as they scroll into view. Other files
    /// wait for a click.
    Images,
    /// Nothing: every image and file waits for a click.
    Never,
    /// Images, and the files of videos, audio and documents too.
    Everything,
}

impl MediaChoice {
    /// What is fetched in a session that talks to WhatsApp `direct`ly or
    /// not: never [`Automatic`](Self::Automatic).
    pub fn resolved(self, direct: bool) -> Self {
        match self {
            Self::Automatic if direct => Self::Everything,
            Self::Automatic => Self::Images,
            chosen => chosen,
        }
    }
}

/// What is drawn behind the messages of a conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WallpaperChoice {
    /// The brand's pattern: the mark, crosshairs and ticks, barely there.
    #[default]
    Pattern,
    /// The plain page.
    Plain,
}

/// The tab the emoji, GIF and sticker picker opens on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickerTab {
    /// Emoji.
    #[default]
    Emoji,
    /// GIFs.
    Gifs,
    /// Stickers.
    Stickers,
}

/// Everything the settings screen can change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Light, dark or the desktop's.
    pub theme: ThemeChoice,
    /// The language of the interface: the system's, or one picked.
    pub language: crate::i18n::LanguageChoice,
    /// Animations.
    pub motion: MotionChoice,
    /// Show a desktop notification for a new message. Saved now; delivery
    /// is not implemented yet.
    pub desktop_notifications: bool,
    /// Include the message text in notifications. Saved now, as above.
    pub message_previews: bool,
    /// How much history to fetch in the background.
    pub history: HistoryChoice,
    /// With [`HistoryChoice::Recent`]: how many chats of each number.
    pub recent_chats: u32,
    /// Which media is downloaded without being asked. Kept under a key of
    /// its own since [`MediaChoice::Automatic`]: the old one held "images"
    /// for everybody who never chose, see [`Settings::load`].
    #[serde(rename = "media_download")]
    pub media: MediaChoice,
    /// The welcome screen has been seen through once: later starts go
    /// straight to the sign-in or the chats.
    pub welcome_done: bool,
    /// The size of the whole interface, in percent of the design: one of
    /// [`theme::SCALE_STEPS`].
    pub interface_scale: u16,
    /// The width the user dragged the chat list to, in design pixels (at
    /// 100 %). `None`: about 30 % of the window.
    pub list_width: Option<u16>,
    /// Tell the other side when their messages have been read. Off: a
    /// chat is still marked as read here and on the account's other
    /// devices, without the blue ticks.
    pub read_receipts: bool,
    /// Tell an author that their status was seen. Only while
    /// `read_receipts` is on too: off, viewing a status is local only.
    pub story_receipts: bool,
    /// A desktop notification when a contact posts a status. Off unless
    /// asked for. Saved now; delivery is not implemented yet.
    pub story_notifications: bool,
    /// How the account's own bubbles are drawn.
    pub bubbles: BubbleStyle,
    /// What is behind the messages.
    pub wallpaper: WallpaperChoice,
    /// How fast voice notes were last played: the next one starts at it.
    pub voice_speed: crate::audio::Speed,
    /// The tip that points at the palette was dismissed, or the palette
    /// was opened: it is not shown again.
    pub palette_tip_done: bool,
    /// The question of moving to the Applications folder was answered
    /// "not now": it is not asked again at the start. (Settings > About
    /// keeps the way there.)
    pub move_prompt_done: bool,
    /// The picker's tab last used: where the composer's button opens it.
    pub picker_tab: PickerTab,
    /// Search GIFs online, with the user's own API key (kept in the OS
    /// keychain, not here). Off: no request is ever made to any service.
    pub gif_online: bool,
    /// Stickers and GIFs move while they are hovered or have the
    /// keyboard in the picker (and only then).
    pub sticker_hover_animation: bool,
    /// Look for a new version now and then, download it in the
    /// background and install it at the next start. Off: only when asked
    /// (Settings > About > "Check now").
    pub auto_update: bool,
    /// The strip of status updates above the chats is open. Off: it is
    /// folded to one line.
    pub story_strip: bool,
    /// The tasks are open beside the conversation.
    pub tasks_open: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            language: crate::i18n::LanguageChoice::default(),
            motion: MotionChoice::default(),
            desktop_notifications: true,
            message_previews: true,
            history: HistoryChoice::default(),
            recent_chats: 20,
            media: MediaChoice::default(),
            welcome_done: false,
            interface_scale: 100,
            list_width: None,
            read_receipts: true,
            story_receipts: true,
            story_notifications: false,
            bubbles: BubbleStyle::default(),
            wallpaper: WallpaperChoice::default(),
            voice_speed: crate::audio::Speed::default(),
            palette_tip_done: false,
            move_prompt_done: false,
            picker_tab: PickerTab::default(),
            gif_online: false,
            sticker_hover_animation: true,
            auto_update: true,
            story_strip: true,
            tasks_open: false,
        }
    }
}

impl Settings {
    /// The engine's history mode for these settings.
    pub fn history_mode(&self) -> HistoryMode {
        match self.history {
            HistoryChoice::OnOpen => HistoryMode::OnOpen,
            HistoryChoice::Recent => HistoryMode::Recent(self.recent_chats.max(1) as usize),
            HistoryChoice::Everything => HistoryMode::Everything,
        }
    }

    /// Reads the file, falling back to the defaults for whatever is
    /// missing or unreadable.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        let read = serde_json::from_slice::<serde_json::Value>(&bytes).and_then(|mut value| {
            upgrade(&mut value);
            serde_json::from_value(value)
        });
        read.unwrap_or_else(|error| {
            tracing::warn!(%error, "the settings file is unreadable; using the defaults");
            Self::default()
        })
    }

    /// Writes the file whole, through a temporary one, so a crash midway
    /// never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let draft = path.with_extension("json.tmp");
        std::fs::write(&draft, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&draft, path)
    }
}

/// Carries what a file written by an earlier version chose over to the
/// keys of this one. "media" was "images" for whoever never chose, so only
/// a choice of nothing or everything is kept; the rest is automatic.
fn upgrade(value: &mut serde_json::Value) {
    let Some(object) = value.as_object_mut() else {
        return;
    };
    if let Some(legacy) = object.remove("media") {
        if !object.contains_key("media_download")
            && matches!(legacy.as_str(), Some("never" | "everything"))
        {
            object.insert("media_download".into(), legacy);
        }
    }
}

/// The settings in use, and where they are kept.
struct Active {
    values: Settings,
    /// `None` keeps them in memory only (tests).
    path: Option<PathBuf>,
    /// `--theme`: wins over the saved theme until the user picks one.
    theme_override: Option<Appearance>,
}

impl Global for Active {}

/// Loads the settings and applies them. Call once, before the first window
/// opens. `path` is the settings file; `None` keeps them in memory.
pub fn init(path: Option<PathBuf>, theme_override: Option<Appearance>, cx: &mut App) {
    let values = path.as_deref().map(Settings::load).unwrap_or_default();
    theme::set_scale(values.interface_scale);
    theme::set_bubbles(values.bubbles);
    apply_language(values.language);
    cx.set_global(Active {
        values,
        path,
        theme_override,
    });
    apply_theme(cx);
}

/// A file kept next to the settings file, by name. `None` when the
/// settings live in memory only (tests, a first run that cannot write).
pub fn sibling(name: &str, cx: &App) -> Option<PathBuf> {
    cx.try_global::<Active>()
        .and_then(|active| active.path.as_ref())
        .map(|path| path.with_file_name(name))
}

/// The settings in use.
pub fn get(cx: &App) -> Settings {
    cx.try_global::<Active>()
        .map(|active| active.values)
        .unwrap_or_default()
}

/// Changes the settings, saves them and applies what changed.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Settings)) {
    let active = cx.default_global_or_init();
    let before = active.values;
    change(&mut active.values);
    active.values.interface_scale = theme::nearest_step(active.values.interface_scale);
    let after = active.values;
    if after.theme != before.theme {
        // Picking a theme ends the command line's say.
        active.theme_override = None;
    }
    if after != before {
        if let Some(path) = &active.path {
            if let Err(error) = after.save(path) {
                tracing::warn!(%error, "the settings could not be saved");
            }
        }
    }
    if after.interface_scale != before.interface_scale || after.bubbles != before.bubbles {
        // Every size is read again at the new scale; the component
        // library's own sizes follow the theme. Another style of bubble
        // is another set of colours.
        theme::set_scale(after.interface_scale);
        theme::set_bubbles(after.bubbles);
        theme::apply(appearance(cx), cx);
    }
    if after.language != before.language {
        apply_language(after.language);
    }
    apply_theme(cx);
    cx.refresh_windows();
}

/// Puts the interface in the language a choice means on this system.
fn apply_language(choice: crate::i18n::LanguageChoice) {
    // Tests see English, like the emoji picker's search language.
    let system = if cfg!(test) {
        None
    } else {
        crate::locale::system()
    };
    crate::i18n::set_language(crate::i18n::resolve(choice, system.as_ref()));
}

trait OrInit {
    fn default_global_or_init(&mut self) -> &mut Active;
}

impl OrInit for App {
    fn default_global_or_init(&mut self) -> &mut Active {
        if !self.has_global::<Active>() {
            self.set_global(Active {
                values: Settings::default(),
                path: None,
                theme_override: None,
            });
        }
        self.global_mut::<Active>()
    }
}

/// The appearance the settings ask for right now.
pub fn appearance(cx: &App) -> Appearance {
    let (choice, forced) = cx
        .try_global::<Active>()
        .map(|active| (active.values.theme, active.theme_override))
        .unwrap_or_default();
    if let Some(forced) = forced {
        return forced;
    }
    match choice {
        ThemeChoice::Light => Appearance::Light,
        ThemeChoice::Dark => Appearance::Dark,
        ThemeChoice::System => match cx.window_appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
        },
    }
}

/// Installs the palette the settings ask for, if it is not the active one.
/// Also the answer to "the desktop switched between light and dark".
pub fn apply_theme(cx: &mut App) {
    let wanted = appearance(cx);
    if theme::try_palette(cx).map(|palette| palette.appearance) != Some(wanted) {
        theme::apply(wanted, cx);
    }
}

/// True when the interface should stay still: the logo shows its static
/// mark and nothing animates in.
pub fn reduce_motion(cx: &App) -> bool {
    match get(cx).motion {
        MotionChoice::On => false,
        MotionChoice::Off => true,
        // The component library reads the desktop's setting into this flag
        // (and keeps following it on Linux). Where the desktop cannot say,
        // it stays false: motion is on.
        MotionChoice::System => cx.reduce_motion(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_choice_of_media_from_an_earlier_version_is_kept_and_the_default_is_automatic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let load = |media: &str| {
            std::fs::write(
                &path,
                format!(r#"{{"media": "{media}", "recent_chats": 7}}"#),
            )
            .unwrap();
            Settings::load(&path)
        };
        assert_eq!(load("images").media, MediaChoice::Automatic, "nobody chose");
        assert_eq!(load("never").media, MediaChoice::Never);
        assert_eq!(load("everything").media, MediaChoice::Everything);
        assert_eq!(load("images").recent_chats, 7, "the rest is read as it was");

        assert_eq!(
            MediaChoice::Automatic.resolved(true),
            MediaChoice::Everything
        );
        assert_eq!(MediaChoice::Automatic.resolved(false), MediaChoice::Images);
        assert_eq!(MediaChoice::Never.resolved(true), MediaChoice::Never);
    }

    #[test]
    fn settings_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert_eq!(Settings::load(&path), Settings::default(), "no file yet");

        let chosen = Settings {
            theme: ThemeChoice::Dark,
            language: crate::i18n::LanguageChoice::Lang(crate::i18n::Language::Es),
            motion: MotionChoice::Off,
            desktop_notifications: false,
            message_previews: true,
            history: HistoryChoice::Recent,
            recent_chats: 50,
            media: MediaChoice::Never,
            welcome_done: true,
            interface_scale: 125,
            list_width: Some(300),
            read_receipts: false,
            story_receipts: false,
            story_notifications: true,
            bubbles: BubbleStyle::Lime,
            wallpaper: WallpaperChoice::Plain,
            voice_speed: crate::audio::Speed::Faster,
            palette_tip_done: true,
            move_prompt_done: true,
            picker_tab: PickerTab::Stickers,
            gif_online: true,
            sticker_hover_animation: false,
            auto_update: false,
            story_strip: false,
            tasks_open: true,
        };
        chosen.save(&path).unwrap();
        assert_eq!(Settings::load(&path), chosen);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn a_damaged_or_older_file_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());

        // A file from a version that knew fewer settings keeps what it has.
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let loaded = Settings::load(&path);
        assert_eq!(loaded.theme, ThemeChoice::Dark);
        assert_eq!(loaded.motion, MotionChoice::System);
        assert!(loaded.desktop_notifications && loaded.message_previews);
        assert!(!loaded.welcome_done);
        // What it never heard of is as a new install has it.
        assert_eq!(loaded.bubbles, BubbleStyle::Brand);
        assert_eq!(loaded.wallpaper, WallpaperChoice::Pattern);
        assert!(
            loaded.auto_update,
            "updates are looked for unless turned off"
        );
    }

    #[test]
    fn the_defaults_follow_the_system() {
        let defaults = Settings::default();
        assert_eq!(defaults.theme, ThemeChoice::System);
        assert_eq!(defaults.motion, MotionChoice::System);
        // History waits for a chat to be opened, unless asked otherwise.
        assert_eq!(defaults.history_mode(), HistoryMode::OnOpen);
        let recent = Settings {
            history: HistoryChoice::Recent,
            ..defaults
        };
        assert_eq!(recent.history_mode(), HistoryMode::Recent(20));
    }
}
