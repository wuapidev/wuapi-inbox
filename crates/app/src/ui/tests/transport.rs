//! How a provider's live updates arrive (a stream, or polling) is its own
//! business. The palette's Connection panel says it in one line; nothing
//! else on screen says it, and nothing else changes when it switches.

use super::keyboard::palette_rows;
use super::*;
use crate::ui::palette::Item;
use async_trait::async_trait;
use client_provider::{
    Account, AccountId, AvatarAnswer, Capabilities, Chat, ChatChange, ChatId, Contact, ContactId,
    Cursor, EventStream, Feature, Group, LiveUpdates, MediaData, MediaRef, Message, MessageId,
    OutgoingMessage, Page, PollingReason, Provider, ProviderResult, SendReceipt, Story,
};

/// The mock, saying how its updates arrive as the test sets it. It answers
/// everything a window asks at start by the mock's own world, so what is on
/// screen is what the mock-backed windows show.
struct Reporting {
    inner: MockProvider,
    live: Arc<Mutex<Option<LiveUpdates>>>,
}

#[async_trait]
impl Provider for Reporting {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn unavailable(&self, account: &AccountId, feature: Feature) -> bool {
        self.inner.unavailable(account, feature)
    }
    fn live_updates(&self) -> Option<LiveUpdates> {
        *self.live.lock().unwrap()
    }
    fn recheck(&self, account: &AccountId, feature: Feature) {
        self.inner.recheck(account, feature)
    }
    fn mention_handle(&self, contact: &ContactId) -> String {
        self.inner.mention_handle(contact)
    }
    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        self.inner.list_accounts().await
    }
    async fn list_chats(&self, a: &AccountId, c: Option<Cursor>) -> ProviderResult<Page<Chat>> {
        self.inner.list_chats(a, c).await
    }
    async fn fetch_messages(
        &self,
        a: &AccountId,
        c: &ChatId,
        k: Option<Cursor>,
        n: u32,
    ) -> ProviderResult<Page<Message>> {
        self.inner.fetch_messages(a, c, k, n).await
    }
    async fn send(&self, m: OutgoingMessage) -> ProviderResult<SendReceipt> {
        self.inner.send(m).await
    }
    async fn mark_read(
        &self,
        a: &AccountId,
        c: &ChatId,
        up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        self.inner.mark_read(a, c, up_to).await
    }
    async fn mark_read_quietly(&self, a: &AccountId, c: &ChatId) -> ProviderResult<()> {
        self.inner.mark_read_quietly(a, c).await
    }
    async fn update_chat(
        &self,
        a: &AccountId,
        c: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        self.inner.update_chat(a, c, change).await
    }
    async fn download_media(&self, a: &AccountId, m: &MediaRef) -> ProviderResult<MediaData> {
        self.inner.download_media(a, m).await
    }
    async fn fetch_avatar(
        &self,
        a: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        self.inner.fetch_avatar(a, subject, known).await
    }
    async fn list_contacts(
        &self,
        a: &AccountId,
        c: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        self.inner.list_contacts(a, c).await
    }
    async fn list_groups(&self, a: &AccountId) -> ProviderResult<Vec<Group>> {
        self.inner.list_groups(a).await
    }
    async fn list_stories(&self, a: &AccountId) -> ProviderResult<Vec<Story>> {
        self.inner.list_stories(a).await
    }
    async fn muted_story_authors(&self, a: &AccountId) -> ProviderResult<Vec<ContactId>> {
        self.inner.muted_story_authors(a).await
    }
    async fn subscribe(&self) -> ProviderResult<EventStream> {
        self.inner.subscribe().await
    }
}

/// A window over a provider that says `first`, and the knob that changes it.
fn open_reporting(
    cx: &mut TestAppContext,
    first: Option<LiveUpdates>,
) -> (Harness, Arc<Mutex<Option<LiveUpdates>>>) {
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::quiet();
    let live = Arc::new(Mutex::new(first));
    let provider = Reporting {
        inner: mock.clone(),
        live: live.clone(),
    };
    let harness = open_over_provider(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
        Arc::new(provider),
    );
    (harness, live)
}

/// Opens the Connection panel from the palette and reads its "Live
/// updates" line, then closes it. `None` when the panel has no such line.
fn live_line(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "Check the connection", cx);
    let rows = palette_rows(harness, cx);
    assert_eq!(
        rows.first().map(|(kind, title)| (*kind, title.as_str())),
        Some(("command", "Check the connection")),
        "{rows:?}"
    );
    press(harness.window, "enter", cx);
    let line = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .find_map(|item| match item {
                Item::Line(name, value) if name.as_ref() == "Live updates" => {
                    Some(value.to_string())
                }
                _ => None,
            })
    });
    // Escape steps back out of the panel, and then out of the palette.
    for _ in 0..3 {
        if overlay_is_none(harness, cx) {
            break;
        }
        press(harness.window, "escape", cx);
    }
    assert!(overlay_is_none(harness, cx), "the palette closed");
    line
}

fn overlay_is_none(harness: &Harness, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| harness.shell.read(cx).overlay == Overlay::None)
}

#[gpui_kit::test]
fn the_connection_panel_says_stream_while_the_provider_streams(cx: &mut TestAppContext) {
    let (harness, _) = open_reporting(cx, Some(LiveUpdates::Stream));
    assert_eq!(live_line(&harness, cx).as_deref(), Some("Stream"));
}

#[gpui_kit::test]
fn the_connection_panel_says_polling_and_why_after_a_fallback(cx: &mut TestAppContext) {
    let (harness, _) = open_reporting(cx, Some(LiveUpdates::Polling(PollingReason::Unavailable)));
    assert_eq!(
        live_line(&harness, cx).as_deref(),
        Some("Polling (the stream is not available yet)")
    );
    // Chosen on purpose: no reason in parentheses.
    let (harness, _) = open_reporting(cx, Some(LiveUpdates::Polling(PollingReason::Chosen)));
    assert_eq!(live_line(&harness, cx).as_deref(), Some("Polling"));
}

#[gpui_kit::test]
fn the_panel_says_where_the_updates_are_each_time_it_opens(cx: &mut TestAppContext) {
    // One window, the provider changing its mind between two openings: the
    // line is read when the panel opens, not when the window did.
    let (harness, live) = open_reporting(cx, Some(LiveUpdates::Stream));
    let mut said = Vec::new();
    for now in [
        LiveUpdates::Stream,
        LiveUpdates::Polling(PollingReason::Failing),
        LiveUpdates::Polling(PollingReason::ConnectionLimit),
        LiveUpdates::Polling(PollingReason::Refused),
        LiveUpdates::Reconnecting,
        LiveUpdates::Stream,
    ] {
        *live.lock().unwrap() = Some(now);
        said.push(live_line(&harness, cx).unwrap());
    }
    assert_eq!(
        said,
        [
            "Stream",
            "Polling (the stream keeps failing)",
            "Polling (too many stream connections are open)",
            "Polling (the stream refused this key)",
            "Stream, reconnecting",
            "Stream",
        ]
    );
    // And a provider that does not say has no line at all.
    *live.lock().unwrap() = None;
    assert_eq!(live_line(&harness, cx), None);
}

/// What the main window shows, as far as a person looking at it would
/// tell: the strip's words and where it is, what else is drawn, what is
/// open on top, and what went wrong lately.
#[derive(Debug, PartialEq)]
struct MainWindow {
    sentence: String,
    overlay_none: bool,
    problem: Option<String>,
    drawn: Vec<(&'static str, Option<Bounds<gpui_kit::Pixels>>)>,
    chats: usize,
    open_chat: Option<ChatId>,
}

fn main_window(harness: &Harness, cx: &mut TestAppContext) -> MainWindow {
    let drawn = [
        "connection-banner",
        "connection-state-connected",
        "connection-state-reconnecting",
        "connection-state-connecting",
        "connection-banner-action",
        "conversation-connection",
    ]
    .into_iter()
    .map(|selector| {
        (
            selector,
            VisualTestContext::from_window(harness.window.into(), cx).debug_bounds(selector),
        )
    })
    .collect();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let account = shell
            .accounts
            .iter()
            .find(|account| Some(&account.id) == shell.account.as_ref())
            .expect("a number is on screen");
        MainWindow {
            sentence: crate::ui::connection::connection_words(account).sentence,
            overlay_none: shell.overlay == Overlay::None,
            problem: shell.problem.as_ref().map(|problem| problem.to_string()),
            drawn,
            chats: shell.list_rows.len(),
            open_chat: shell.open.as_ref().map(|open| open.chat.id.clone()),
        }
    })
}

#[gpui_kit::test]
fn a_switch_between_stream_and_polling_changes_nothing_on_the_main_window(cx: &mut TestAppContext) {
    let (harness, live) = open_reporting(cx, Some(LiveUpdates::Stream));
    let streaming = main_window(&harness, cx);
    // What is compared is something: the strip is up, the number connected,
    // a chat open, and nothing is wrong.
    assert_eq!(streaming.sentence, "Connected");
    assert!(streaming.overlay_none && streaming.problem.is_none());
    assert!(streaming.drawn[0].1.is_some(), "the strip is drawn");
    assert!(streaming.drawn[1].1.is_some(), "and says connected");
    assert!(streaming.open_chat.is_some() && streaming.chats > 0);

    for change in [
        // The stream fails and polling takes over, for each reason.
        LiveUpdates::Reconnecting,
        LiveUpdates::Polling(PollingReason::Failing),
        LiveUpdates::Polling(PollingReason::ConnectionLimit),
        LiveUpdates::Polling(PollingReason::Unavailable),
        // And it comes back.
        LiveUpdates::Stream,
    ] {
        *live.lock().unwrap() = Some(change);
        // Draw again, as any change of the window would.
        cx.update(|cx| harness.shell.update(cx, |_, cx| cx.notify()));
        harness.settle(cx);
        assert_eq!(main_window(&harness, cx), streaming, "after {change:?}");
    }
}

#[gpui_kit::test]
fn the_strip_follows_the_number_not_the_transport(cx: &mut TestAppContext) {
    // Triangulation: the same window does change when the number's own
    // connection does, whatever the transport says, so the comparison
    // above can tell a change from none.
    let (harness, live) = open_reporting(cx, Some(LiveUpdates::Polling(PollingReason::Failing)));
    let before = main_window(&harness, cx);
    harness
        .engine
        .apply_event(client_provider::ProviderEvent::ConnectionChanged {
            account_id: cx.update(|cx| harness.shell.read(cx).account.clone().unwrap()),
            state: client_provider::ConnectionState::Reconnecting,
        })
        .unwrap();
    harness.settle(cx);
    let after = main_window(&harness, cx);
    assert_ne!(before, after);
    assert!(after.sentence.starts_with("Reconnecting"), "{after:?}");
    // The transport going the other way changes nothing more.
    *live.lock().unwrap() = Some(LiveUpdates::Stream);
    cx.update(|cx| harness.shell.update(cx, |_, cx| cx.notify()));
    harness.settle(cx);
    assert_eq!(main_window(&harness, cx), after);
}
