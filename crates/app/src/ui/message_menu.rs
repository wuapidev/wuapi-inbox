//! What can be done to a message, and the menu that lists it.
//!
//! [`Shell::command_state`] is the one place that says whether a command
//! applies to a message: the keys, the menu and the bar of a selection all
//! ask it. What the provider cannot do at all is hidden; what it can do
//! but not to this message is shown disabled, with the reason.

use super::shell::{MenuAction, MenuItem, Shell};
use super::thread_keys::{copy_link, copy_text};
use crate::icons::IconName;
use crate::keys::Command;
use client_provider::{DeliveryStatus, Direction, MediaKind, Message, MessageContent};

/// Whether a command can be run on a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandState {
    /// The provider cannot do it: it is not offered.
    Hidden,
    /// Not on this message, and why.
    Disabled(&'static str),
    /// It can be run.
    Enabled,
}

/// The commands of a message's menu, in its order, with their glyphs.
pub(super) const MENU: [(Command, &str, IconName); 17] = [
    (Command::Reply, "message-reply", IconName::MessageSquareText),
    (
        Command::React,
        "message-react",
        IconName::FaceSlightlySmiling,
    ),
    (Command::Reactions, "message-reactors", IconName::Users),
    (Command::Copy, "message-copy", IconName::Copy),
    (Command::CopyLink, "message-copy-link", IconName::Link),
    (Command::CopyImage, "message-copy-image", IconName::Image),
    (Command::SaveAs, "message-save", IconName::ArrowDownToLine),
    (
        Command::SaveToDownloads,
        "message-save-downloads",
        IconName::ArrowDownToLine,
    ),
    (Command::SaveSticker, "message-save-sticker", IconName::Star),
    (Command::SaveGif, "message-save-gif", IconName::Film),
    (Command::Forward, "message-forward", IconName::Forward),
    (Command::Star, "message-star", IconName::Star),
    (Command::Edit, "message-edit", IconName::Pencil),
    (Command::Info, "message-info", IconName::Info),
    (Command::Open, "message-open", IconName::ExternalLink),
    (
        Command::SelectMessages,
        "message-select",
        IconName::SquareCheck,
    ),
    (Command::Delete, "message-delete", IconName::Trash),
];

/// Whether a message is on WhatsApp: only then can it be answered,
/// reacted to, starred or passed on. One that is still waiting here, or
/// that the provider has accepted and not sent yet, is not.
fn sent(message: &Message) -> bool {
    !matches!(
        message.status,
        DeliveryStatus::Pending | DeliveryStatus::Accepted | DeliveryStatus::Failed { .. }
    )
}

impl Shell {
    /// Whether `command` can be run on `message` right now.
    pub(super) fn command_state(&self, command: Command, message: &Message) -> CommandState {
        use Command as C;
        use CommandState::{Disabled, Enabled, Hidden};
        let caps = self.engine.capabilities();
        let own = message.direction == Direction::Outgoing;
        let media = match &message.content {
            MessageContent::Media(media) if !message.extras.view_once => Some(media),
            _ => None,
        };
        const DELETED: &str = "The message was deleted";
        const NOT_SENT: &str = "Not sent yet";
        if message.deleted
            && !matches!(
                command,
                C::Info | C::SelectMessages | C::Delete | C::MessageMenu
            )
        {
            return match command {
                C::CopyLink | C::CopyImage | C::Open | C::Download | C::Reactions => Hidden,
                _ => Disabled(DELETED),
            };
        }
        match command {
            C::Copy => match copy_text(message) {
                Some(_) => Enabled,
                None => Disabled("Nothing to copy"),
            },
            C::CopyLink => match copy_link(message) {
                Some(_) => Enabled,
                None => Hidden,
            },
            C::CopyImage => match media {
                Some(media) if matches!(media.kind, MediaKind::Image | MediaKind::Sticker) => {
                    Enabled
                }
                _ => Hidden,
            },
            C::Open => match (media, copy_link(message)) {
                (Some(media), _) if media.kind != MediaKind::Sticker => Enabled,
                (None, Some(link)) if link.starts_with("http") => Enabled,
                _ => Hidden,
            },
            C::SaveAs | C::SaveToDownloads => match media {
                Some(media) if media.source.is_some() => Enabled,
                Some(_) => Disabled("The provider has no file for it"),
                None => Hidden,
            },
            C::Download => match media {
                Some(media) if media.source.is_some() => Enabled,
                _ => Hidden,
            },
            C::SaveSticker => match media {
                Some(media) if media.kind == MediaKind::Sticker && media.source.is_some() => {
                    Enabled
                }
                Some(media) if media.kind == MediaKind::Sticker => {
                    Disabled("The provider has no file for it")
                }
                _ => Hidden,
            },
            C::SaveGif => match media {
                Some(media) if super::library_ui::is_gif(media) && media.source.is_some() => {
                    Enabled
                }
                Some(media) if super::library_ui::is_gif(media) => {
                    Disabled("The provider has no file for it")
                }
                _ => Hidden,
            },
            C::SelectText | C::MessageMenu | C::Info | C::SelectMessages => Enabled,
            C::Reply if !caps.replies => Hidden,
            C::Reply if !sent(message) => Disabled(NOT_SENT),
            C::Reply => Enabled,
            C::React if !caps.reactions => Hidden,
            C::React if !sent(message) => Disabled(NOT_SENT),
            C::React => Enabled,
            // Only where somebody did.
            C::Reactions if self.has_reactions(&message.id) => Enabled,
            C::Reactions => Hidden,
            C::Edit if !caps.edits => Hidden,
            C::Edit if !own => Disabled("Only your own messages"),
            C::Edit if !matches!(message.content, MessageContent::Text { .. }) => {
                Disabled("Only text can be edited")
            }
            C::Edit if matches!(message.status, DeliveryStatus::Failed { .. }) => {
                Disabled("It was not sent")
            }
            C::Edit => Enabled,
            C::Star if !caps.stars => Hidden,
            C::Star if !sent(message) => Disabled(NOT_SENT),
            C::Star => Enabled,
            C::Forward if !caps.forwards => Hidden,
            C::Forward if !sent(message) => Disabled(NOT_SENT),
            C::Forward => match self.forwardable(message) {
                Ok(()) => Enabled,
                Err(why) => Disabled(why),
            },
            C::Delete if own && !caps.deletes && !caps.delete_for_me => Hidden,
            C::Delete if !own && !caps.delete_received => Disabled("Only your own messages"),
            C::Delete => Enabled,
            _ => Hidden,
        }
    }

    /// What an entry of the menu is called for this message.
    fn menu_label(command: Command, message: &Message) -> &'static str {
        match command {
            Command::SaveSticker => "Add to favorites",
            Command::SaveGif => "Save GIF",
            Command::Star if message.extras.starred => "Unstar",
            Command::Star => "Star",
            Command::CopyLink => match &message.content {
                MessageContent::Contacts { .. } => "Copy number",
                _ => "Copy link",
            },
            Command::Open => match &message.content {
                MessageContent::Media(media)
                    if matches!(media.kind, MediaKind::Voice | MediaKind::Audio) =>
                {
                    "Play"
                }
                MessageContent::Media(_) => "Open",
                _ => "Open link",
            },
            other => crate::keys::label(other),
        }
    }

    /// The entries of the menu of the message in focus.
    pub(super) fn message_menu_items(&self) -> Vec<MenuItem> {
        let Some(message) = self.focused_message() else {
            return Vec::new();
        };
        MENU.iter()
            .filter_map(|(command, id, icon)| {
                let (action, hint) = match self.command_state(*command, &message) {
                    CommandState::Hidden => return None,
                    CommandState::Disabled(why) => (None, Some(why)),
                    CommandState::Enabled => (Some(MenuAction::Run(*command)), None),
                };
                Some(MenuItem {
                    id,
                    label: Self::menu_label(*command, &message),
                    icon: *icon,
                    action,
                    hint,
                    keys: Some(*command),
                })
            })
            .collect()
    }
}
