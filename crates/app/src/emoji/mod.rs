//! Emoji: every one Unicode lists, what they are called in the user's
//! language, and how they are found.
//!
//! Nothing here draws. [`data`] reads the tables compiled into the binary
//! (`assets/emoji`, built by `generate.py` there from Unicode's
//! `emoji-test.txt`, CLDR's annotations and gemoji's shortcodes), [`search`]
//! looks through them, [`locale`] decides which language the names are
//! searched in, [`font`] says which emoji the fonts at hand can draw, and
//! [`prefs`] keeps what the user picks most, their skin tone and their
//! language next to the settings. The picker itself is `ui/emoji_picker.rs`.
//!
//! The tables are unpacked the first time a picker opens, off the UI
//! thread, and kept for the life of the process; a language's names are
//! unpacked the first time that language is searched.

pub mod data;
pub mod font;
pub mod locale;
pub mod prefs;
pub mod search;
