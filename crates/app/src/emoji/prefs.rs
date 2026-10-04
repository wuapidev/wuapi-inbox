//! What the user's emoji picker remembers: the emoji used most, the skin
//! tone and the search language.
//!
//! A small JSON file, `emoji.json`, next to `settings.json`. Like the
//! settings it holds nothing secret and nothing of a provider's; a missing
//! or unreadable file means the defaults, and a write that fails is logged
//! while the choice still applies for the session.

use super::data::Tone;
use super::locale::LanguageChoice;
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The file's name inside the data directory.
pub const FILE_NAME: &str = "emoji.json";

/// How many used emoji are remembered.
pub const KEPT: usize = 54;

/// An emoji that was picked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Used {
    /// As it was picked, skin tone included.
    pub emoji: String,
    /// How many times.
    pub count: u32,
    /// When last: a counter that goes up with every pick, not a time.
    pub last: u64,
}

/// Everything the picker remembers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EmojiPrefs {
    /// The language the emoji are searched in, beside English.
    pub language: LanguageChoice,
    /// The skin tone emoji are offered in; `None` is the yellow default.
    pub tone: Option<Tone>,
    /// The emoji used, at most [`KEPT`] of them.
    pub used: Vec<Used>,
}

impl EmojiPrefs {
    /// Reads the file, falling back to the defaults.
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "the emoji file is unreadable; starting over");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the file whole, through a temporary one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let draft = path.with_extension("json.tmp");
        std::fs::write(&draft, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&draft, path)
    }

    /// An emoji was picked. When more than [`KEPT`] are remembered, the
    /// one used least, and longest ago among those, is forgotten.
    pub fn note(&mut self, emoji: &str) {
        let emoji = emoji.trim();
        if emoji.is_empty() {
            return;
        }
        let now = self.used.iter().map(|used| used.last).max().unwrap_or(0) + 1;
        match self.used.iter_mut().find(|used| used.emoji == emoji) {
            Some(used) => {
                used.count = used.count.saturating_add(1);
                used.last = now;
            }
            None => self.used.push(Used {
                emoji: emoji.to_owned(),
                count: 1,
                last: now,
            }),
        }
        while self.used.len() > KEPT {
            // Never the one just picked: a new emoji has to be able to
            // get in among fifty-four that were each used twice.
            let Some(least) = self
                .used
                .iter()
                .enumerate()
                .filter(|(_, used)| used.last != now)
                .min_by_key(|(_, used)| (used.count, used.last))
                .map(|(index, _)| index)
            else {
                break;
            };
            self.used.remove(least);
        }
    }

    /// The emoji used most, the most used first; among equals, the one
    /// used last.
    pub fn frequent(&self, limit: usize) -> Vec<&str> {
        let mut used: Vec<&Used> = self.used.iter().collect();
        used.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)));
        used.into_iter()
            .take(limit)
            .map(|used| used.emoji.as_str())
            .collect()
    }
}

/// The preferences in use, and where they are kept.
struct Active {
    values: EmojiPrefs,
    /// `None` keeps them in memory only (tests, a session that cannot
    /// write).
    path: Option<PathBuf>,
}

impl Global for Active {}

/// Reads the file next to the settings, once. Later calls do nothing.
pub fn init(cx: &mut App) {
    if cx.has_global::<Active>() {
        return;
    }
    let path = crate::settings::sibling(FILE_NAME, cx);
    let values = path.as_deref().map(EmojiPrefs::load).unwrap_or_default();
    cx.set_global(Active { values, path });
}

/// The preferences in use.
pub fn get(cx: &App) -> EmojiPrefs {
    cx.try_global::<Active>()
        .map(|active| active.values.clone())
        .unwrap_or_default()
}

/// The search language picked, without copying the rest.
pub fn language(cx: &App) -> LanguageChoice {
    cx.try_global::<Active>()
        .map(|active| active.values.language)
        .unwrap_or_default()
}

/// Changes the preferences and saves them.
pub fn update(cx: &mut App, change: impl FnOnce(&mut EmojiPrefs)) {
    init(cx);
    let active = cx.global_mut::<Active>();
    let before = active.values.clone();
    change(&mut active.values);
    if active.values != before {
        if let Some(path) = &active.path {
            if let Err(error) = active.values.save(path) {
                tracing::warn!(%error, "the emoji file could not be saved");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::emoji::data::Lang;

    #[test]
    fn the_most_used_come_first_and_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert_eq!(
            EmojiPrefs::load(&path),
            EmojiPrefs::default(),
            "no file yet"
        );

        let mut prefs = EmojiPrefs::default();
        for emoji in ["🔥", "👍🏽", "🔥", "😂", "🔥", "👍🏽", "🎉"] {
            prefs.note(emoji);
        }
        prefs.note("  ");
        // By count, then the one picked last.
        assert_eq!(prefs.frequent(10), ["🔥", "👍🏽", "🎉", "😂"]);
        assert_eq!(prefs.frequent(2), ["🔥", "👍🏽"]);
        prefs.tone = Some(Tone::Medium);
        prefs.language = LanguageChoice::Lang(Lang::Es);

        prefs.save(&path).unwrap();
        let loaded = EmojiPrefs::load(&path);
        assert_eq!(loaded, prefs);
        assert_eq!(loaded.frequent(10), ["🔥", "👍🏽", "🎉", "😂"]);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn what_is_remembered_is_bounded() {
        let mut prefs = EmojiPrefs::default();
        prefs.note("🔥");
        prefs.note("🔥");
        // Far more emoji than are kept, each used once.
        for index in 0..(KEPT as u32 * 3) {
            prefs.note(&char::from_u32(0x1F600 + index).unwrap().to_string());
        }
        assert_eq!(prefs.used.len(), KEPT);
        // The one used twice outlived all of them, and the newest is in.
        assert_eq!(prefs.frequent(1), ["🔥"]);
        let newest = char::from_u32(0x1F600 + KEPT as u32 * 3 - 1)
            .unwrap()
            .to_string();
        assert!(prefs.used.iter().any(|used| used.emoji == newest));
        // The oldest of those used once went first.
        assert!(!prefs.used.iter().any(|used| used.emoji == "😀"));
    }

    #[test]
    fn a_damaged_or_older_file_is_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(EmojiPrefs::load(&path), EmojiPrefs::default());

        // A file that knows less keeps what it has.
        std::fs::write(&path, r#"{"language":"es"}"#).unwrap();
        let loaded = EmojiPrefs::load(&path);
        assert_eq!(loaded.language, LanguageChoice::Lang(Lang::Es));
        assert_eq!(loaded.tone, None);
        assert!(loaded.used.is_empty());

        let defaults = EmojiPrefs::default();
        assert_eq!(defaults.language, LanguageChoice::System);
    }
}
