//! How a number's connection is said, in one place: the words every pane
//! uses for a state, the reason a provider gives turned into a sentence,
//! and the strips that carry them with the way to link the number again.

use super::shell::Shell;
use super::widgets::{led, status_colour};
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::{Account, ConnectionState};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, FontWeight, Hsla, SharedString, Stateful};

/// How loud a state is said.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tone {
    /// Connected: there to be checked, not to be read.
    Quiet,
    /// On its way: nothing to do but wait.
    Busy,
    /// Created and never linked: waiting for a phone.
    Unlinked,
    /// Off until somebody reconnects or links it again.
    Broken,
}

/// The words for an account's connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ConnectionWords {
    /// The state in a word or two: "Connected", "Not connected".
    pub label: &'static str,
    /// The state for a selector: `connected`, `logged-out`.
    pub slug: &'static str,
    /// The state with the reason it is in, when one is known:
    /// "Not connected: its proxy is paused".
    pub summary: String,
    /// The reason alone, in words, when the provider gave one.
    pub reason: Option<String>,
    /// What is said on top of the number's chats.
    pub sentence: String,
    /// What is said in an open chat: what happens to a message sent now.
    pub waiting: String,
    /// What the button that starts the reconnection says, for a state
    /// that needs somebody.
    pub action: Option<&'static str>,
    pub tone: Tone,
}

/// A provider's reason for a number being offline, in words. The codes
/// the API documents are translated; anything else is the provider's own
/// text and is shown as it is.
pub(super) fn reason_words(reason: &str) -> String {
    let reason = reason.trim();
    match reason.to_ascii_lowercase().as_str() {
        "logged_out" => "it was logged out on the phone".into(),
        "session_not_found" => "its session no longer exists on the server".into(),
        "link_timeout" => "it was not linked in time".into(),
        "free_limit_reached" => "the free plan's limit was reached".into(),
        "proxy_paused" => "its proxy is paused".into(),
        _ => reason.trim_end_matches('.').to_owned(),
    }
}

/// The words for `account`'s connection, the same in every pane.
pub(super) fn connection_words(account: &Account) -> ConnectionWords {
    const BACK: &str = "Messages you send will go out when it is back.";
    let said = |label: &'static str, slug, sentence: String, waiting: String, action, tone| {
        ConnectionWords {
            label,
            slug,
            summary: label.into(),
            reason: None,
            sentence,
            waiting,
            action,
            tone,
        }
    };
    match &account.connection {
        // Created and never linked: nothing broke, it is waiting for a
        // phone.
        _ if account.never_linked() => said(
            "Not linked yet",
            "unlinked",
            "This number was created and never linked to a phone.".into(),
            "This number is not linked yet. Messages you send will go out once it is linked."
                .into(),
            Some("Link"),
            Tone::Unlinked,
        ),
        ConnectionState::Connected => said(
            "Connected",
            "connected",
            "Connected".into(),
            "Connected".into(),
            None,
            Tone::Quiet,
        ),
        ConnectionState::Connecting => said(
            "Connecting",
            "connecting",
            "Connecting…".into(),
            format!("Connecting… {BACK}"),
            None,
            Tone::Busy,
        ),
        ConnectionState::Reconnecting => said(
            "Reconnecting",
            "reconnecting",
            format!("Reconnecting… {BACK}"),
            format!("Reconnecting… {BACK}"),
            None,
            Tone::Busy,
        ),
        ConnectionState::Disconnected { reason } => {
            let reason = reason
                .as_deref()
                .map(reason_words)
                .filter(|reason| !reason.is_empty());
            let summary = match &reason {
                Some(reason) => format!("Not connected: {reason}"),
                None => "Not connected".into(),
            };
            ConnectionWords {
                summary: summary.clone(),
                reason,
                ..said(
                    "Not connected",
                    "disconnected",
                    summary,
                    format!("This number is not connected. {BACK}"),
                    Some("Reconnect"),
                    Tone::Broken,
                )
            }
        }
        ConnectionState::LoggedOut => said(
            "Logged out",
            "logged-out",
            "This number was logged out. Link it again.".into(),
            "This number was logged out. Messages you send will go out once it is linked again."
                .into(),
            Some("Link again"),
            Tone::Broken,
        ),
    }
}

/// The button of a connection strip: the page's own colours, so it reads
/// on the strip's fill as it does on the page.
fn action_button(id: &'static str, text: &'static str, palette: &Palette) -> Stateful<Div> {
    let ring = palette.text;
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .h(px(26.))
        .px_2()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(palette.border)
        .bg(palette.background)
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(metrics::TEXT_SMALL())
        .font_weight(FontWeight::MEDIUM)
        .text_color(palette.text)
        .tab_index(0)
        .hover(|style| style.opacity(0.85))
        .focus_visible(move |style| style.border_color(ring))
        .child(text)
}

impl Shell {
    /// The account with this id, among the session's numbers.
    fn number(&self, id: Option<&client_provider::AccountId>) -> Option<&Account> {
        let id = id?;
        self.accounts.iter().find(|account| &account.id == id)
    }

    /// The button that reconnects `account`, or links it again: the same
    /// path as the settings' and the rail's menu, offered where the
    /// provider can manage its numbers.
    fn connection_action(
        &self,
        id: &'static str,
        account: &Account,
        words: &ConnectionWords,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let text = words
            .action
            .filter(|_| self.engine.capabilities().manage_accounts)?;
        let account = account.id.clone();
        Some(
            action_button(id, text, palette).on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.reconnect_number(account.clone(), window, cx);
            })),
        )
    }

    /// The strip on top of the chats that says whether their number is
    /// connected: one quiet line while it is, a strip nobody can miss,
    /// with the way to reconnect, while it is not.
    pub(super) fn render_connection_banner(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let account = self.number(self.account.as_ref())?;
        let words = connection_words(account);
        let slug = words.slug;
        let strip = div()
            .flex_none()
            .debug_selector(|| "connection-banner".into())
            .px_4()
            .border_b_1()
            .flex()
            .items_center()
            .gap_2()
            .text_size(metrics::TEXT_SMALL())
            .line_height(px(18.));
        let text = div()
            .debug_selector(move || format!("connection-state-{slug}"))
            .flex_1()
            .min_w_0()
            .child(SharedString::from(words.sentence.clone()));
        let action =
            self.connection_action("connection-banner-action", account, &words, palette, cx);
        let fill = |fill: Hsla, strip: Div, text: Div| {
            // The page's colour on the alarm's: the pair the theme keeps
            // readable in both palettes.
            strip
                .py(px(10.))
                .border_color(fill)
                .bg(fill)
                .text_color(palette.background)
                .font_weight(FontWeight::MEDIUM)
                .child(icon(IconName::CircleAlert, px(16.), palette.background))
                .child(text)
                .children(action)
        };
        Some(match words.tone {
            Tone::Quiet => strip
                .py(px(5.))
                .border_color(palette.border)
                .text_color(palette.text_muted)
                .child(led(status_colour(&account.connection, palette)))
                .child(text),
            Tone::Busy => strip
                .py(px(10.))
                .border_color(palette.border)
                .bg(palette.warning.opacity(0.16))
                .text_color(palette.text)
                .child(led(palette.warning))
                .child(text),
            Tone::Unlinked => fill(palette.warning, strip, text),
            Tone::Broken => fill(palette.danger, strip, text),
        })
    }

    /// The notice over the composer of a chat whose number is not
    /// connected. Writing is not held back: what is sent waits in the
    /// outbox and goes out when the number is back.
    pub(super) fn render_connection_notice(
        &self,
        account: &client_provider::AccountId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let account = self.number(Some(account))?;
        let words = connection_words(account);
        let light = match words.tone {
            Tone::Quiet => return None,
            Tone::Busy | Tone::Unlinked => palette.warning,
            Tone::Broken => palette.danger,
        };
        Some(
            div()
                .flex_none()
                .debug_selector(|| "conversation-connection".into())
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(palette.border)
                .bg(palette.muted)
                .flex()
                .items_center()
                .gap_2()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(palette.text)
                .child(led(light))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(words.waiting.clone())),
                )
                .children(self.connection_action(
                    "conversation-connection-action",
                    account,
                    &words,
                    palette,
                    cx,
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::{AccountId, AccountSettings};

    fn account(connection: ConnectionState) -> Account {
        Account {
            id: AccountId::new("acc"),
            display_name: "Sales".into(),
            phone: None,
            self_contact: None,
            connection,
            settings: AccountSettings::default(),
        }
    }

    fn stopped(reason: Option<&str>) -> Account {
        account(ConnectionState::Disconnected {
            reason: reason.map(Into::into),
        })
    }

    #[test]
    fn every_documented_reason_is_said_in_words() {
        for (code, words) in [
            ("logged_out", "it was logged out on the phone"),
            (
                "session_not_found",
                "its session no longer exists on the server",
            ),
            ("link_timeout", "it was not linked in time"),
            ("free_limit_reached", "the free plan's limit was reached"),
            ("proxy_paused", "its proxy is paused"),
        ] {
            assert_eq!(reason_words(code), words);
            let said = connection_words(&stopped(Some(code)));
            assert_eq!(said.summary, format!("Not connected: {words}"));
            assert_eq!(said.sentence, said.summary);
            assert_eq!(said.reason.as_deref(), Some(words));
            assert!(!said.summary.contains(code), "{code} is not for people");
        }
        assert_eq!(reason_words(" PROXY_PAUSED "), "its proxy is paused");
    }

    #[test]
    fn a_reason_nobody_documented_is_shown_as_it_is() {
        // A code this client has never heard of, and the provider's own
        // sentence: neither is dropped or rewritten.
        assert_eq!(reason_words("device_banned"), "device_banned");
        let said = connection_words(&stopped(Some("dial tcp: connection refused.")));
        assert_eq!(said.summary, "Not connected: dial tcp: connection refused");
        // None, or an empty one: nothing is made up.
        for none in [None, Some(""), Some("  ")] {
            let said = connection_words(&stopped(none));
            assert_eq!(said.summary, "Not connected");
            assert_eq!(said.reason, None);
        }
    }

    #[test]
    fn each_state_has_its_words_its_tone_and_its_button() {
        let states = [
            (ConnectionState::Connected, "Connected", Tone::Quiet, None),
            (ConnectionState::Connecting, "Connecting", Tone::Busy, None),
            (
                ConnectionState::Reconnecting,
                "Reconnecting",
                Tone::Busy,
                None,
            ),
            (
                ConnectionState::Disconnected { reason: None },
                "Not connected",
                Tone::Broken,
                Some("Reconnect"),
            ),
            (
                ConnectionState::LoggedOut,
                "Logged out",
                Tone::Broken,
                Some("Link again"),
            ),
        ];
        for (state, label, tone, action) in states {
            let said = connection_words(&account(state));
            assert_eq!((said.label, said.tone, said.action), (label, tone, action));
            assert!(said.summary.starts_with(label));
        }
        assert_eq!(
            connection_words(&account(ConnectionState::Reconnecting)).sentence,
            "Reconnecting… Messages you send will go out when it is back."
        );
        assert_eq!(
            connection_words(&account(ConnectionState::LoggedOut)).sentence,
            "This number was logged out. Link it again."
        );
        assert_eq!(
            connection_words(&stopped(None)).waiting,
            "This number is not connected. Messages you send will go out when it is back."
        );
    }

    #[test]
    fn a_number_that_never_linked_is_waiting_not_broken() {
        let mut never = stopped(Some("link_timeout"));
        never.settings.ever_linked = Some(false);
        let said = connection_words(&never);
        assert_eq!(said.label, "Not linked yet");
        assert_eq!(said.tone, Tone::Unlinked);
        assert_eq!(said.action, Some("Link"));
        assert_eq!(
            said.sentence,
            "This number was created and never linked to a phone."
        );
    }
}
