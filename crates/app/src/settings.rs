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
    /// Images, stickers and GIFs, as they scroll into view. Other files
    /// wait for a click.
    #[default]
    Images,
    /// Nothing: every image and file waits for a click.
    Never,
    /// Images, and the files of videos, audio and documents too.
    Everything,
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
    /// Which media is downloaded without being asked.
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
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
            picker_tab: PickerTab::default(),
            gif_online: false,
            sticker_hover_animation: true,
            auto_update: true,
            story_strip: true,
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
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "the settings file is unreadable; using the defaults");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
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
    apply_theme(cx);
    cx.refresh_windows();
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
    fn settings_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert_eq!(Settings::load(&path), Settings::default(), "no file yet");

        let chosen = Settings {
            theme: ThemeChoice::Dark,
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
            picker_tab: PickerTab::Stickers,
            gif_online: true,
            sticker_hover_animation: false,
            auto_update: false,
            story_strip: false,
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
