//! Voice notes as text, from the user's side: the "Transcribe" command, the
//! words under the waveform, and Settings > Audio > Transcription.
//!
//! What turns audio into text is `transcribe.rs`. Here: asking for a file
//! that is not here yet, handing it over off the UI thread, keeping the
//! answer in the encrypted store (so reopening a chat never transcribes, or
//! pays for, the same note twice), and the form of the settings.

use super::media::FileState;
use super::panels::choice_row;
use super::shell::Shell;
use super::widgets::{mono, text_button};
use crate::gifs::{KeychainSecret, MemorySecret, SecretStore};
use crate::settings;
use crate::theme::{metrics, px, Palette};
use crate::transcribe::{self, Audio, Config, Mode, TranscribeError, Transcriber};
use client_provider::{Media, MediaKind};
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, SharedString, Task, Window};
use std::collections::HashSet;
use std::sync::Arc;

/// Where a voice note's transcript stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Transcript {
    /// Being fetched, or with the service.
    Working,
    /// The words.
    Done(SharedString),
    /// Why it did not work: asking again retries.
    Failed(SharedString),
}

/// Presets of the form: the services people ask for most. The model is the
/// one each calls its Whisper.
const PRESETS: [(&str, &str, &str, &str); 2] = [
    (
        "transcription-preset-openai",
        "OpenAI",
        transcribe::OPENAI_BASE,
        transcribe::DEFAULT_MODEL,
    ),
    (
        "transcription-preset-groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "whisper-large-v3-turbo",
    ),
];

/// What the transcription settings and requests hold.
pub(super) struct TranscribeUi {
    /// The key, in the keychain (memory in tests).
    pub(super) secrets: Arc<dyn SecretStore>,
    /// The server's coordinates, as saved.
    pub(super) config: Config,
    pub(super) base_input: Entity<InputState>,
    pub(super) model_input: Entity<InputState>,
    pub(super) language_input: Entity<InputState>,
    pub(super) key_input: Entity<InputState>,
    /// The mode of the settings, mirrored: menus decide without a context.
    pub(super) mode: Mode,
    /// What the last action of the form said.
    pub(super) said: Option<SharedString>,
    /// Voice notes whose file was asked for because "Transcribe" was
    /// pressed: they go on when it arrives.
    waiting: HashSet<String>,
    /// Answers instead of the configured service: for tests.
    #[cfg(test)]
    pub(super) fake: Option<Arc<dyn Transcriber>>,
    tasks: Vec<Task<()>>,
}

impl TranscribeUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let config = settings::sibling(transcribe::FILE_NAME, cx)
            .map(|path| Config::load(&path))
            .unwrap_or_default();
        let field = |value: String,
                     placeholder: &'static str,
                     window: &mut Window,
                     cx: &mut Context<Shell>| {
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                state.set_value(value, window, cx);
                state
            })
        };
        Self {
            secrets: session_secret(),
            base_input: field(
                config.base_url.clone(),
                "https://api.openai.com/v1",
                window,
                cx,
            ),
            model_input: field(config.model.clone(), "whisper-1", window, cx),
            language_input: field(config.language.clone(), "Detect automatically", window, cx),
            key_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste your API key")
                    .masked(true)
            }),
            mode: settings::get(cx).transcription,
            config,
            said: None,
            waiting: HashSet::new(),
            #[cfg(test)]
            fake: None,
            tasks: Vec::new(),
        }
    }
}

/// Where the key is kept: the OS keychain; in memory for tests.
fn session_secret() -> Arc<dyn SecretStore> {
    if cfg!(test) {
        Arc::new(MemorySecret::default())
    } else {
        Arc::new(KeychainSecret::named("transcription-key"))
    }
}

/// Whether a message's audio can be transcribed.
pub(super) fn is_speech(media: &Media) -> bool {
    matches!(media.kind, MediaKind::Voice | MediaKind::Audio)
}

/// Reads back what an earlier session transcribed, for the voice notes on
/// screen: one store read each, once.
pub(super) fn load_transcript(
    audio: &mut super::shell::AudioDeck,
    media: &super::media::MediaShelf,
    url: &str,
) {
    if audio.transcripts.contains_key(url) || !audio.transcripts_checked.insert(url.to_owned()) {
        return;
    }
    if let Some(text) = media.transcript(url) {
        audio
            .transcripts
            .insert(url.to_owned(), Transcript::Done(text.into()));
    }
}

impl Shell {
    /// The service for the mode the settings ask for.
    fn transcriber(&self, cx: &gpui_kit::App) -> Result<Arc<dyn Transcriber>, TranscribeError> {
        #[cfg(test)]
        if let Some(fake) = &self.transcription.fake {
            return Ok(fake.clone());
        }
        match settings::get(cx).transcription {
            Mode::Off => Err(TranscribeError::Unsupported(
                "Transcription is off. Turn it on in Settings > Audio.".into(),
            )),
            Mode::System => transcribe::system::new(),
            Mode::Online => {
                let key = self
                    .transcription
                    .secrets
                    .get()
                    .map_err(|error| {
                        TranscribeError::Setup(format!("The keychain did not answer: {error}"))
                    })?
                    .unwrap_or_default();
                let config = &self.transcription.config;
                Ok(Arc::new(transcribe::OpenAiCompatible::new(
                    &config.base_url,
                    &key,
                    &config.model,
                )?))
            }
        }
    }

    /// Whether transcribing is turned off in the settings.
    pub(super) fn transcription_off(&self) -> bool {
        #[cfg(test)]
        if self.transcription.fake.is_some() {
            return false;
        }
        self.transcription.mode == Mode::Off
    }

    /// "Transcribe" on a voice note: from the store when it was done
    /// before, else fetch the file if need be and ask the service.
    pub(super) fn transcribe_audio(&mut self, media: Media, cx: &mut Context<Self>) {
        let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
            return;
        };
        if settings::get(cx).transcription == Mode::Off || !is_speech(&media) {
            return;
        }
        if matches!(
            self.audio.transcripts.get(&url),
            Some(Transcript::Working | Transcript::Done(_))
        ) {
            return;
        }
        if let Some(text) = self.media.transcript(&url) {
            self.audio
                .transcripts
                .insert(url, Transcript::Done(text.into()));
            return cx.notify();
        }
        let Some(account) = self.account.clone() else {
            return;
        };
        self.audio
            .transcripts
            .insert(url.clone(), Transcript::Working);
        match self.media.file_state(&url) {
            FileState::Ready => self.run_transcription(&url, cx),
            FileState::Unavailable(_) => {
                self.transcription.waiting.insert(url.clone());
                self.media.retry_file(&account, &url);
            }
            _ => {
                self.transcription.waiting.insert(url.clone());
                self.engine.want_file(&account, &url);
            }
        }
        cx.notify();
    }

    /// A file arrived (or could not): go on with the transcriptions that
    /// were waiting for it.
    pub(super) fn transcription_file_arrived(&mut self, key: &str, cx: &mut Context<Self>) {
        let urls: Vec<String> = self
            .transcription
            .waiting
            .iter()
            .filter(|url| client_core::file_key(url) == key)
            .cloned()
            .collect();
        for url in urls {
            match self.media.file_state(&url) {
                FileState::Ready => {
                    self.transcription.waiting.remove(&url);
                    self.run_transcription(&url, cx);
                }
                FileState::Unavailable(reason) => {
                    self.transcription.waiting.remove(&url);
                    self.audio
                        .transcripts
                        .insert(url, Transcript::Failed(reason));
                }
                _ => {}
            }
        }
        cx.notify();
    }

    /// Sends the downloaded file to the transcriber, off this thread.
    fn run_transcription(&mut self, url: &str, cx: &mut Context<Self>) {
        let fail = |this: &mut Self, reason: String| {
            this.audio
                .transcripts
                .insert(url.to_owned(), Transcript::Failed(reason.into()));
        };
        let Some((bytes, mime)) = self.media.file(url) else {
            return fail(self, "The file is not here.".into());
        };
        let transcriber = match self.transcriber(cx) {
            Ok(transcriber) => transcriber,
            Err(error) => return fail(self, error.to_string()),
        };
        let language = self.transcription.config.language_hint().map(str::to_owned);
        let work = self.engine.runtime().spawn(async move {
            transcriber
                .transcribe(Audio { bytes, mime }, language.as_deref())
                .await
        });
        let url = url.to_owned();
        self.transcription
            .tasks
            .push(cx.spawn(async move |this, cx| {
                let answer = work.await;
                this.update(cx, |this, cx| {
                    let transcript = match answer {
                        Ok(Ok(text)) if text.is_empty() => {
                            Transcript::Failed("Nothing was heard in it.".into())
                        }
                        Ok(Ok(text)) => {
                            this.media.keep_transcript(&url, &text);
                            Transcript::Done(text.into())
                        }
                        Ok(Err(error)) => Transcript::Failed(error.to_string().into()),
                        Err(error) => Transcript::Failed(error.to_string().into()),
                    };
                    this.audio.transcripts.insert(url, transcript);
                    cx.notify();
                })
                .ok();
            }));
    }

    /// Reads back what an earlier session transcribed, for one voice note.
    #[cfg(test)]
    pub(super) fn load_transcript(&mut self, url: &str) {
        load_transcript(&mut self.audio, &self.media, url);
    }

    // ----- Settings > Audio > Transcription -----------------------------

    pub(super) fn render_transcription_settings(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = settings::get(cx).transcription;
        let mode =
            |id: &'static str, label: &'static str, this_mode: Mode, cx: &mut Context<Self>| {
                text_button(id, label, None, chosen == this_mode, palette).on_click(cx.listener(
                    move |this, _, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| settings.transcription = this_mode);
                        this.transcription.mode = this_mode;
                        cx.notify();
                    },
                ))
            };
        let system = transcribe::system::available();
        let key_set = self
            .transcription
            .secrets
            .get()
            .ok()
            .flatten()
            .is_some_and(|key| !key.is_empty());
        let field = |id: &'static str, label: &'static str, input: &Entity<InputState>| {
            div()
                .debug_selector(move || id.into())
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h(metrics::BUTTON())
                        .px_2()
                        .rounded(metrics::RADIUS())
                        .border_1()
                        .border_color(palette.border)
                        .bg(palette.surface)
                        .flex()
                        .items_center()
                        .child(super::widgets::field(input)),
                )
        };
        let state: SharedString = self.transcription.said.clone().unwrap_or_else(|| {
            if key_set {
                "A key is saved in the system keychain.".into()
            } else {
                "No key yet. A server you run on this computer needs none.".into()
            }
        });
        let mut presets = div().flex().items_center().gap_2();
        for (id, label, base, model) in PRESETS {
            presets = presets.child(text_button(id, label, None, false, palette).on_click(
                cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.transcription
                        .base_input
                        .update(cx, |field, cx| field.set_value(base, window, cx));
                    this.transcription
                        .model_input
                        .update(cx, |field, cx| field.set_value(model, window, cx));
                }),
            ));
        }
        let online = chosen == Mode::Online;
        div()
            .pt_4()
            .flex()
            .flex_col()
            .child(
                div()
                    .pb_2()
                    .child(mono("[ TRANSCRIPTION ]").text_color(palette.text_muted)),
            )
            .child(choice_row(
                "Voice notes as text",
                "Off by default. When on, a voice note gets a Transcribe action. Only the note \
                 you ask for is sent, and only to the place chosen here.",
                div()
                    .debug_selector(|| "transcription-mode".into())
                    .flex_none()
                    .flex()
                    .gap_2()
                    .child(mode("transcription-off", "Off", Mode::Off, cx))
                    .when(system, |this| {
                        this.child(mode(
                            "transcription-system",
                            "This computer",
                            Mode::System,
                            cx,
                        ))
                    })
                    .child(mode("transcription-online", "Server", Mode::Online, cx)),
                palette,
            ))
            .when(system && chosen == Mode::System, |this| {
                this.child(
                    div()
                        .debug_selector(|| "transcription-system-note".into())
                        .pb_2()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child(
                            "Uses the speech recogniser installed with Windows, on this \
                             computer: nothing is sent anywhere. It understands only the \
                             languages whose speech pack is installed in Windows, and it is \
                             less accurate than a server's model.",
                        ),
                )
            })
            .when(!system, |this| {
                this.child(
                    div()
                        .pb_2()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child(
                            "This system has no speech recognition the application can use \
                             yet. Point it at a server instead: your own (whisper.cpp, \
                             faster-whisper on http://localhost) keeps the audio on this \
                             computer.",
                        ),
                )
            })
            .when(online, |this| {
                this.child(
                    div()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(
                            div()
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(18.))
                                .text_color(palette.text_muted)
                                .child(
                                    "Any server that speaks OpenAI's /audio/transcriptions: \
                                     the audio and the key are sent to it, and to no one else. \
                                     The key stays in the system keychain, never in a file.",
                                ),
                        )
                        .child(presets)
                        .child(field(
                            "transcription-base",
                            "Address",
                            &self.transcription.base_input,
                        ))
                        .child(field(
                            "transcription-model",
                            "Model",
                            &self.transcription.model_input,
                        ))
                        .child(field(
                            "transcription-language",
                            "Language",
                            &self.transcription.language_input,
                        ))
                        .child(field(
                            "transcription-key",
                            "API key",
                            &self.transcription.key_input,
                        ))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    text_button("transcription-save", "Save", None, true, palette)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.save_transcription(window, cx);
                                        })),
                                )
                                .when(key_set, |this| {
                                    this.child(
                                        text_button(
                                            "transcription-key-remove",
                                            "Remove key",
                                            None,
                                            false,
                                            palette,
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.remove_transcription_key(cx);
                                            }),
                                        ),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .debug_selector(|| "transcription-state".into())
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(palette.text_muted)
                                .child(state),
                        ),
                )
            })
    }

    fn save_transcription(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let read = |input: &Entity<InputState>, cx: &mut Context<Self>| {
            input.read(cx).value().trim().to_owned()
        };
        let config = Config {
            base_url: read(&self.transcription.base_input, cx),
            model: read(&self.transcription.model_input, cx),
            language: read(&self.transcription.language_input, cx).to_lowercase(),
        };
        let typed = read(&self.transcription.key_input, cx);
        if let Err(error) = transcribe::check_base(&config.base_url) {
            self.transcription.said = Some(error.to_string().into());
            return cx.notify();
        }
        if config.model.is_empty() {
            self.transcription.said = Some("Enter the model's name.".into());
            return cx.notify();
        }
        let mut said = String::from("Saved.");
        if let Some(path) = settings::sibling(transcribe::FILE_NAME, cx) {
            if let Err(error) = config.save(&path) {
                said = format!("It could not be saved: {error}");
            }
        }
        if !typed.is_empty() {
            match self.transcription.secrets.set(&typed) {
                Ok(()) => {
                    said = "Saved; the key is in the system keychain.".into();
                    self.transcription
                        .key_input
                        .update(cx, |field, cx| field.set_value("", window, cx));
                }
                Err(error) => said = format!("The keychain did not take the key: {error}"),
            }
        }
        self.transcription.config = config;
        self.transcription.said = Some(said.into());
        // A failure may have been about the old coordinates.
        self.audio
            .transcripts
            .retain(|_, transcript| !matches!(transcript, Transcript::Failed(_)));
        cx.notify();
    }

    fn remove_transcription_key(&mut self, cx: &mut Context<Self>) {
        self.transcription.said = Some(match self.transcription.secrets.delete() {
            Ok(()) => "Key removed.".into(),
            Err(error) => format!("The keychain did not answer: {error}").into(),
        });
        cx.notify();
    }
}
