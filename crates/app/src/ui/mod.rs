//! The user interface: the three panes every chat client shares, drawn in
//! the wuapi design system (hairline rules, one accent, see `theme.rs`).
//!
//! ```text
//! ┌────┬──────────────┬──────────────────────────────┐
//! │rail│  chat list   │         conversation         │
//! └────┴──────────────┴──────────────────────────────┘
//! ```
//!
//! [`AppView`] is the root: it shows the welcome (`welcome`) on a first
//! run, the sign-in screen (`login`) until there is a session, then the [`Shell`](shell::Shell), which owns all
//! chat UI state. The other modules are rendering helpers for one pane
//! each. Everything shown comes from the
//! local store; the views never call a provider.

mod app;
mod attach;
mod bubble;
mod chat_list;
mod connection;
mod conversation;
mod emoji_picker;
mod groups;
mod hints;
mod library_ui;
mod link_qr;
mod login;
mod media;
mod media_out;
mod mentions;
mod menus;
mod message_actions;
mod message_menu;
mod numbers;
mod palette;
mod palette_steps;
mod panels;
mod panes;
mod picker;
mod picker_view;
mod pills;
mod poll_form;
mod profiles;
mod rail;
mod rail_menu;
mod reactors;
mod recovery;
mod select_text;
mod senders;
mod shell;
mod social;
mod status;
mod status_mine;
mod status_post;
mod status_strip;
mod status_view;
mod story_quote;
mod story_ring;
mod thread_keys;
mod tiles;
mod transfer;
mod updates;
mod viewer;
mod voice;
mod voice_record;
mod wallpaper;
mod welcome;
mod widgets;

pub use app::AppView;
pub use media::clear_exports;
pub use shell::SessionKind;

#[cfg(test)]
mod tests;
