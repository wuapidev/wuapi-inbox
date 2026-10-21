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
pub(super) const MENU: [(Command, &str, IconName); 20] = [
    (Command::Reply, "message-reply", IconName::MessageSquareText),
    (
        Command::ReplyPrivately,
        "message-reply-privately",
        IconName::Reply,
    ),
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
    (Command::Pin, "message-pin", IconName::Pin),
    (Command::CreateTask, "message-task", IconName::SquareCheck),
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
        let caps = self.caps_here();
        let own = message.direction == Direction::Outgoing;
        let media = match &message.content {
            MessageContent::Media(media) if !message.extras.view_once => Some(media),
            _ => None,
        };
        let deleted = crate::i18n::t("chat.message_menu.deleted");
        let not_sent = crate::i18n::t("chat.message_menu.not_sent");
        if message.deleted
            && !matches!(
                command,
                C::Info | C::SelectMessages | C::Delete | C::MessageMenu
            )
        {
            return match command {
                C::CopyLink | C::CopyImage | C::Open | C::Download | C::Reactions => Hidden,
                _ => Disabled(deleted),
            };
        }
        match command {
            C::Copy => match copy_text(message) {
                Some(_) => Enabled,
                None => Disabled(crate::i18n::t("chat.message_menu.nothing_to_copy")),
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
                Some(_) => Disabled(crate::i18n::t("chat.message_menu.no_file")),
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
                    Disabled(crate::i18n::t("chat.message_menu.no_file"))
                }
                _ => Hidden,
            },
            C::SaveGif => match media {
                Some(media) if super::library_ui::is_gif(media) && media.source.is_some() => {
                    Enabled
                }
                Some(media) if super::library_ui::is_gif(media) => {
                    Disabled(crate::i18n::t("chat.message_menu.no_file"))
                }
                _ => Hidden,
            },
            C::SelectText | C::MessageMenu | C::Info | C::SelectMessages => Enabled,
            C::Reply if !caps.replies => Hidden,
            C::Reply if !sent(message) => Disabled(not_sent),
            C::Reply => Enabled,
            // Only what somebody else wrote in a group has a chat to be
            // answered in apart from this one.
            C::ReplyPrivately if !caps.replies || !caps.private_replies => Hidden,
            C::ReplyPrivately if own || !self.in_a_group() => Hidden,
            C::ReplyPrivately if !sent(message) => Disabled(not_sent),
            C::ReplyPrivately => Enabled,
            C::React if !caps.reactions => Hidden,
            C::React if !sent(message) => Disabled(not_sent),
            C::React => Enabled,
            // Only where somebody did.
            C::Reactions if self.has_reactions(&message.id) => Enabled,
            C::Reactions => Hidden,
            C::Edit if !caps.edits => Hidden,
            C::Edit if !own => Disabled(crate::i18n::t("chat.message_menu.only_own")),
            C::Edit if !matches!(message.content, MessageContent::Text { .. }) => {
                Disabled(crate::i18n::t("chat.message_menu.only_text"))
            }
            C::Edit if matches!(message.status, DeliveryStatus::Failed { .. }) => {
                Disabled(crate::i18n::t("chat.message_menu.not_delivered"))
            }
            C::Edit => Enabled,
            C::Star if !caps.stars => Hidden,
            C::Star if !sent(message) => Disabled(not_sent),
            C::Star => Enabled,
            C::Pin if !caps.pins => Hidden,
            C::Pin if !sent(message) => Disabled(not_sent),
            C::Pin if matches!(message.content, MessageContent::System(_)) => Hidden,
            C::Pin => Enabled,
            // Kept here, so anything with something to say will do,
            // sent or not.
            C::CreateTask if matches!(message.content, MessageContent::System(_)) => Hidden,
            C::CreateTask => Enabled,
            C::Forward if !caps.forwards => Hidden,
            C::Forward if !sent(message) => Disabled(not_sent),
            C::Forward => match self.forwardable(message) {
                Ok(()) => Enabled,
                Err(why) => Disabled(why),
            },
            C::Delete if own && !caps.deletes && !caps.delete_for_me => Hidden,
            C::Delete if !own && !caps.delete_received => {
                Disabled(crate::i18n::t("chat.message_menu.only_own"))
            }
            C::Delete => Enabled,
            _ => Hidden,
        }
    }

    /// What an entry of the menu is called for this message.
    fn menu_label(command: Command, message: &Message) -> &'static str {
        match command {
            Command::SaveSticker => crate::i18n::t("chat.message_menu.add_favorite"),
            Command::SaveGif => crate::i18n::t("chat.message_menu.save_gif"),
            Command::Star if message.extras.starred => crate::i18n::t("chat.message_menu.unstar"),
            Command::Star => crate::i18n::t("chat.message_menu.star"),
            Command::Pin if message.extras.pinned_at(client_provider::Timestamp::now()) => {
                crate::i18n::t("chat.message_menu.unpin")
            }
            Command::Pin => crate::i18n::t("chat.message_menu.pin"),
            Command::CopyLink => match &message.content {
                MessageContent::Contacts { .. } => crate::i18n::t("chat.message_menu.copy_number"),
                _ => crate::i18n::t("chat.message_menu.copy_link"),
            },
            Command::Open => match &message.content {
                MessageContent::Media(media)
                    if matches!(media.kind, MediaKind::Voice | MediaKind::Audio) =>
                {
                    crate::i18n::t("chat.message_menu.play")
                }
                MessageContent::Media(_) => crate::i18n::t("chat.message_menu.open"),
                _ => crate::i18n::t("chat.message_menu.open_link"),
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
