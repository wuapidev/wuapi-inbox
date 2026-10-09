//! The sticker and GIF library, from the user's side: keeping what is in a
//! conversation, bringing files in from disk, the commands of the
//! registry, and Settings > Stickers and GIFs.
//!
//! The library itself, its limits and its sync with the phone are
//! `client-core`'s (see `docs/ARCHITECTURE.md`, "Stickers and GIFs");
//! the picker is `picker.rs`.

use super::media_out::Out;
use super::panels::{choice_row, section};
use super::picker::{Source, Tab};
use super::shell::{Overlay, PickKind, SettingsSection, Shell};
use super::widgets::{mono, switch, text_button};
use crate::gifs::MemorySecret;
use crate::keys::Command;
use crate::settings;
use crate::theme::{metrics, px, Palette};
use client_core::{LibraryItem, LibraryKind, LibrarySource};
use client_provider::{Media, MediaKind};
use gpui_kit::component::input::InputState;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, SharedString, Window};
use std::path::PathBuf;
use std::sync::Arc;

/// The biggest file read to make a sticker of (a photograph, say).
const SOURCE_MAX: u64 = 40 * 1024 * 1024;

/// What a file kept from a conversation becomes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Keep {
    pub(super) kind: LibraryKind,
    /// Starred at once.
    pub(super) favorite: bool,
    /// It was sent from here.
    pub(super) sent: bool,
}

/// Whether a message's file is a GIF: a video that says so, an MP4 (the
/// provider may not say, and a GIF on WhatsApp is one), or a `.gif`.
pub(super) fn is_gif(media: &Media) -> bool {
    match media.kind {
        MediaKind::Video => {
            media.gif
                || media
                    .mime_type
                    .as_deref()
                    .is_none_or(|mime| mime == "video/mp4")
        }
        MediaKind::Image => media.mime_type.as_deref() == Some("image/gif"),
        _ => false,
    }
}

/// What the settings panel holds beside the settings themselves.
pub(super) struct LibraryUi {
    /// Where the API key is typed.
    pub(super) key_input: Entity<InputState>,
    /// What the clear button asked: once, and now it asks again.
    pub(super) confirm_clear: Option<bool>,
    /// What the last action of the panel said.
    pub(super) said: Option<SharedString>,
}

impl LibraryUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            key_input: cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Paste your API key")
                    .masked(true)
            }),
            confirm_clear: None,
            said: None,
        }
    }
}

/// The result of bringing files in.
#[derive(Default)]
struct Imported {
    added: Vec<LibraryItem>,
    notes: Vec<&'static str>,
    failed: Vec<(String, String)>,
}

fn name_of(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reads and converts files; blocking.
fn import_files(
    engine: &client_core::SyncEngine,
    paths: Vec<PathBuf>,
    kind: LibraryKind,
) -> Imported {
    let mut done = Imported::default();
    // Several stickers at once are a set: a pack, named for their folder.
    let pack = (kind == LibraryKind::Sticker && paths.len() > 1)
        .then(|| {
            let folder = paths
                .first()
                .and_then(|path| path.parent())
                .and_then(|folder| folder.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "Imported".to_owned());
            let id = format!(
                "import-{}",
                &client_core::content_id(
                    paths
                        .iter()
                        .map(|path| path.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join("\n")
                        .as_bytes()
                )[..16]
            );
            engine
                .store()
                .library_pack(&id, &folder, client_provider::Timestamp::now())
                .ok()
        })
        .flatten();
    for path in paths {
        let name = name_of(&path);
        let shown = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let limit = if kind == LibraryKind::Gif {
            client_core::GIF_MAX as u64
        } else {
            SOURCE_MAX
        };
        let bytes = match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > limit => {
                done.failed.push((shown, "The file is too large.".into()));
                continue;
            }
            Ok(_) => match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    done.failed.push((shown, error.to_string()));
                    continue;
                }
            },
            Err(error) => {
                done.failed.push((shown, error.to_string()));
                continue;
            }
        };
        let made = match kind {
            LibraryKind::Sticker => engine
                .library_import_sticker(
                    &bytes,
                    Some(name),
                    pack.as_ref().map(|pack| pack.id.clone()),
                )
                .map(|(item, notes)| {
                    done.notes.extend(notes);
                    item
                }),
            LibraryKind::Gif => {
                engine.library_import_gif(bytes, Some(name), LibrarySource::Imported)
            }
        };
        match made {
            Ok(item) => done.added.push(item),
            Err(error) => done.failed.push((shown, error.to_string())),
        }
    }
    done
}

impl Shell {
    /// Whether there is a sticker to star: the picker is on one, or the
    /// message in focus is one.
    pub(super) fn sticker_in_hand(&self) -> bool {
        if self.overlay == Overlay::EmojiPicker && self.picker.tab == Tab::Stickers {
            return self
                .picker
                .current()
                .is_some_and(|tile| matches!(tile.kind, super::picker::TileKind::Sticker(_)));
        }
        self.focused_message().is_some_and(|message| {
            matches!(&message.content, client_provider::MessageContent::Media(media)
                if media.kind == MediaKind::Sticker && media.source.is_some())
        })
    }

    /// The commands of stickers and GIFs. `None` when `command` is not one
    /// of them.
    pub(super) fn run_picker_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use Command as C;
        // A sticker or a GIF is sent to the conversation on screen: with
        // Status in its place there is none, and the picker stays shut.
        let in_chat = self.conversation_showing();
        Some(match command {
            C::EmojiPicker if in_chat => {
                self.open_media_picker(Tab::Emoji, window, cx);
                true
            }
            C::StickerPicker if in_chat => {
                self.open_media_picker(Tab::Stickers, window, cx);
                true
            }
            C::GifPicker if in_chat => {
                self.open_media_picker(Tab::Gifs, window, cx);
                true
            }
            C::EmojiPicker | C::StickerPicker | C::GifPicker => false,
            C::FavoriteSticker => {
                if self.overlay == Overlay::EmojiPicker && self.picker.tab == Tab::Stickers {
                    self.picker_toggle_favorite(window, cx)
                } else {
                    self.save_message_sticker(cx)
                }
            }
            C::SaveSticker => self.save_message_sticker(cx),
            C::SaveGif => self.save_message_gif(cx),
            C::ImportSticker => {
                self.import_stickers(cx);
                true
            }
            C::ImportGif => {
                self.import_gifs(cx);
                true
            }
            C::SettingsStickers => {
                self.open_overlay(Overlay::Settings, window, cx);
                self.settings_section = SettingsSection::Stickers;
                true
            }
            _ => return None,
        })
    }

    // ----- from a conversation -------------------------------------------------------

    /// What the message in focus carries, when it is a file of this kind
    /// the library takes.
    fn focused_file(&self, want_sticker: bool) -> Option<(Media, bool)> {
        let message = self.focused_message()?;
        let media = super::media_out::media_of(&message)?.clone();
        let fits = if want_sticker {
            media.kind == MediaKind::Sticker
        } else {
            is_gif(&media)
        };
        fits.then(|| {
            let sent = message.direction == client_provider::Direction::Outgoing;
            (media, sent)
        })
    }

    /// Stars the sticker of the message in focus, keeping it in the
    /// library first: the original file, fetched when only a thumbnail is
    /// here.
    pub(super) fn save_message_sticker(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((media, sent)) = self.focused_file(true) else {
            return false;
        };
        self.send_out(
            media,
            Out::Keep(Keep {
                kind: LibraryKind::Sticker,
                favorite: true,
                sent,
            }),
            cx,
        );
        true
    }

    /// Saves the GIF of the message in focus.
    pub(super) fn save_message_gif(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((media, sent)) = self.focused_file(false) else {
            return false;
        };
        self.send_out(
            media,
            Out::Keep(Keep {
                kind: LibraryKind::Gif,
                favorite: false,
                sent,
            }),
            cx,
        );
        true
    }

    /// Keeps the file of a message in the library (the buttons on a
    /// picture): a sticker is starred, a GIF is saved.
    pub(super) fn keep_message_file(
        &mut self,
        media: Media,
        kind: LibraryKind,
        sent: bool,
        cx: &mut Context<Self>,
    ) {
        self.send_out(
            media,
            Out::Keep(Keep {
                kind,
                favorite: kind == LibraryKind::Sticker,
                sent,
            }),
            cx,
        );
    }

    /// A file of a conversation arrived whole: it goes in the library.
    pub(super) fn keep_in_library(
        &mut self,
        keep: Keep,
        bytes: Vec<u8>,
        mime: Option<String>,
        seen_in: Option<client_provider::MediaRef>,
        cx: &mut Context<Self>,
    ) {
        let engine = self.engine.clone();
        // The file is a message's, of the number on screen: a sticker
        // remembers which, so that starring it on the phone can name the
        // message instead of sending the file.
        let seen_in = self.account.clone().zip(seen_in);
        let source = if keep.sent {
            LibrarySource::Sent
        } else {
            LibrarySource::Received
        };
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move {
                    match keep.kind {
                        LibraryKind::Sticker => engine
                            .library_save_sticker(
                                bytes,
                                mime.as_deref().unwrap_or("image/webp"),
                                source,
                                false,
                            )
                            .and_then(|(item, created)| {
                                if let Some((account, media)) = &seen_in {
                                    engine
                                        .library_seen_in(&item.id, account, media)
                                        .map_err(client_core::LibraryError::from)?;
                                }
                                if keep.favorite {
                                    engine
                                        .set_library_favorite(&item.id, true)
                                        .map_err(client_core::LibraryError::from)?;
                                }
                                Ok((item, created))
                            }),
                        LibraryKind::Gif => engine.library_save_gif(bytes, source),
                    }
                })
                .await;
            this.update(cx, |this, cx| match saved {
                Ok((item, created)) => {
                    let text = match (keep.kind, created, keep.favorite) {
                        (LibraryKind::Sticker, _, true) => "Added to your favorite stickers",
                        (LibraryKind::Sticker, true, false) => "Saved to your stickers",
                        (LibraryKind::Sticker, false, false) => "Already in your stickers",
                        (LibraryKind::Gif, true, _) => "Saved to your GIFs",
                        (LibraryKind::Gif, false, _) => "Already in your GIFs",
                    };
                    let _ = item;
                    this.show_notice(text, None, cx);
                }
                Err(error) => this.show_problem(error.to_string(), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Keeps an online GIF in the library without sending it.
    pub(super) fn keep_online_gif(&mut self, hit: crate::gifs::GifHit, cx: &mut Context<Self>) {
        let Some(key) = self.picker.services.key() else {
            return;
        };
        let giphy = crate::gifs::Giphy::new(&self.picker.services.base, &key);
        let engine = self.engine.clone();
        let runtime = engine.runtime().clone();
        cx.spawn(async move |this, cx| {
            let got = runtime
                .spawn(async move { giphy.download(&hit).await.map(|bytes| (bytes, hit)) })
                .await;
            let (bytes, hit) = match got {
                Ok(Ok(got)) => got,
                Ok(Err(error)) => {
                    this.update(cx, |this, cx| this.picker_say(error.to_string(), cx))
                        .ok();
                    return;
                }
                Err(_) => return,
            };
            let kept = cx
                .background_spawn(async move {
                    engine.library_import_gif(bytes, Some(hit.title), LibrarySource::Online)
                })
                .await;
            this.update(cx, |this, cx| match kept {
                Ok(_) => this.picker_say("Saved to your GIFs", cx),
                Err(error) => this.picker_say(error.to_string(), cx),
            })
            .ok();
        })
        .detach();
    }

    // ----- from disk --------------------------------------------------------------------

    /// "Make a sticker from a picture…": the system's file dialog, then
    /// each picture becomes a 512 by 512 WebP in the library.
    pub(super) fn import_stickers(&mut self, cx: &mut Context<Self>) {
        self.import_files(LibraryKind::Sticker, cx);
    }

    /// "Add GIFs from files…".
    pub(super) fn import_gifs(&mut self, cx: &mut Context<Self>) {
        self.import_files(LibraryKind::Gif, cx);
    }

    fn import_files(&mut self, kind: LibraryKind, cx: &mut Context<Self>) {
        let picked = (self.pick_files)(PickKind::Files, cx);
        let engine = self.engine.clone();
        cx.spawn(async move |this, cx| {
            let Some(paths) = picked.await else { return };
            if paths.is_empty() {
                return;
            }
            let done = cx
                .background_spawn(async move { import_files(&engine, paths, kind) })
                .await;
            this.update(cx, |this, cx| this.imported(kind, done, cx))
                .ok();
        })
        .detach();
    }

    fn imported(&mut self, kind: LibraryKind, done: Imported, cx: &mut Context<Self>) {
        let what = if kind == LibraryKind::Sticker {
            "sticker"
        } else {
            "GIF"
        };
        let mut said = match done.added.len() {
            0 => String::new(),
            1 => format!("Added 1 {what}"),
            n => format!("Added {n} {what}s"),
        };
        if let Some(note) = done.notes.first() {
            said.push_str(&format!(". {note}"));
        }
        if !said.is_empty() {
            if self.overlay == Overlay::EmojiPicker {
                self.picker_say(said.clone(), cx);
            }
            self.show_notice(said, None, cx);
        }
        if let Some((name, why)) = done.failed.first() {
            let more = done.failed.len() - 1;
            let tail = if more > 0 {
                format!(" ({more} more did not work)")
            } else {
                String::new()
            };
            self.show_problem(format!("{name}: {why}{tail}"), cx);
        }
        self.picker_library_changed(cx);
        cx.notify();
    }

    // ----- Settings > Stickers and GIFs ------------------------------------------------

    /// The settings of the library and of the online GIF search.
    pub(super) fn render_stickers_settings(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let chosen = settings::get(cx);
        let stats = self.store.library_stats().unwrap_or_default();
        let stickers = self
            .store
            .library_items(LibraryKind::Sticker)
            .map(|items| items.len())
            .unwrap_or(0);
        let gifs = stats.items as usize - stickers.min(stats.items as usize);
        let size = if stats.bytes >= 1024 * 1024 {
            format!("{:.1} MB", stats.bytes as f64 / (1024.0 * 1024.0))
        } else {
            format!("{} KB", stats.bytes.div_ceil(1024))
        };
        let budget = client_core::LIBRARY_BUDGET / (1024 * 1024);
        let summary: SharedString = format!(
            "{stickers} stickers and {gifs} GIFs, {size} of {budget} MB. {} favorites are never \
             removed; when the library is full, what was used longest ago goes first.",
            stats.favorites
        )
        .into();
        let confirm = self.library.confirm_clear;
        let clear =
            |id: &'static str, label: &'static str, everything: bool, cx: &mut Context<Self>| {
                let asked = confirm == Some(everything);
                text_button(
                    id,
                    if asked { "Click again to clear" } else { label },
                    None,
                    asked,
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.clear_library(everything, cx);
                }))
            };
        let key_set = self.picker.services.key().is_some();
        let online = chosen.gif_online;
        let key_state: SharedString = if key_set {
            "A key is saved in the system keychain.".into()
        } else {
            "No key yet.".into()
        };
        let key_row = div()
            .debug_selector(|| "stickers-key-row".into())
            .py_2()
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "stickers-key".into())
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
                            .child(super::widgets::field(&self.library.key_input)),
                    )
                    .child(
                        text_button("stickers-key-save", "Save key", None, false, palette)
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.save_gif_key(window, cx);
                            })),
                    )
                    .when(key_set, |this| {
                        this.child(
                            text_button("stickers-key-remove", "Remove", None, false, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_gif_key(cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "stickers-key-state".into())
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(self.library.said.clone().unwrap_or(key_state)),
            );
        section("Stickers and GIFs", palette)
            .child(choice_row(
                "Your library",
                "Stickers and GIFs you keep, in the encrypted database on this computer. \
                 Signing out deletes them with the rest.",
                div()
                    .debug_selector(|| "stickers-clear".into())
                    .flex_none()
                    .flex()
                    .gap_2()
                    .child(clear("stickers-clear-keep", "Clear", false, cx))
                    .child(clear("stickers-clear-all", "Clear all", true, cx)),
                palette,
            ))
            .child(
                div()
                    .debug_selector(|| "stickers-summary".into())
                    .pb_2()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text_muted)
                    .child(summary),
            )
            .child(choice_row(
                "Animate stickers and GIFs on hover",
                "In the picker, the one tile the pointer or the keyboard is on moves; the rest \
                 stay still. Reduced motion (Appearance) keeps everything still.",
                switch("stickers-animate", chosen.sticker_hover_animation, palette).on_click(
                    |_, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| {
                            settings.sticker_hover_animation = !settings.sticker_hover_animation
                        });
                    },
                ),
                palette,
            ))
            .child(
                div()
                    .pt_4()
                    .pb_2()
                    .child(mono("[ ONLINE GIF SEARCH ]").text_color(palette.text_muted)),
            )
            .child(choice_row(
                "Search GIFs online with Giphy",
                "Off by default. Searches are sent to Giphy (api.giphy.com), a third party, \
                 with your own API key and the words you type, and the GIFs you pick are \
                 downloaded from it. Nothing is sent to anyone while this is off.",
                switch("stickers-online", online, palette).on_click(|_, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.gif_online = !settings.gif_online);
                }),
                palette,
            ))
            .child(
                div()
                    .pb_1()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text_muted)
                    .child(
                        "Get a key at developers.giphy.com. It is kept in the system keychain, \
                         never in the settings file, and no key comes with this application.",
                    ),
            )
            .child(key_row)
    }

    fn clear_library(&mut self, everything: bool, cx: &mut Context<Self>) {
        if self.library.confirm_clear != Some(everything) {
            self.library.confirm_clear = Some(everything);
            return cx.notify();
        }
        self.library.confirm_clear = None;
        match self.store.library_clear(!everything) {
            Ok(removed) => self.library.said = Some(format!("{removed} items removed.").into()),
            Err(error) => self.show_problem(error.to_string(), cx),
        }
        self.picker_library_changed(cx);
        cx.notify();
    }

    fn save_gif_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.library.key_input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            self.library.said = Some("Paste a key first.".into());
            return cx.notify();
        }
        match self.picker.services.secrets.set(&typed) {
            Ok(()) => {
                self.library.said = Some("Key saved in the system keychain.".into());
                self.library
                    .key_input
                    .update(cx, |field, cx| field.set_value("", window, cx));
            }
            Err(error) => {
                self.library.said =
                    Some(format!("The keychain did not take the key: {error}").into())
            }
        }
        self.picker.services.forget_key();
        cx.notify();
    }

    fn remove_gif_key(&mut self, cx: &mut Context<Self>) {
        match self.picker.services.secrets.delete() {
            Ok(()) => self.library.said = Some("Key removed.".into()),
            Err(error) => {
                self.library.said = Some(format!("The keychain did not answer: {error}").into())
            }
        }
        self.picker.services.forget_key();
        self.picker_set_source_saved_if_needed(cx);
        cx.notify();
    }

    /// With no key there is nothing online to show.
    fn picker_set_source_saved_if_needed(&mut self, cx: &mut Context<Self>) {
        if self.picker.source == Source::Online {
            self.picker.source = Source::Saved;
            self.picker.gifs.cursor = 0;
            self.picker_load(cx);
        }
    }
}

/// The key of the online search, where a session without a keychain
/// keeps it: in memory, for as long as it runs.
pub(super) fn session_secret() -> Arc<dyn crate::gifs::SecretStore> {
    if cfg!(test) {
        // Tests never touch the operating system's keychain.
        Arc::new(MemorySecret::default())
    } else {
        Arc::new(crate::gifs::KeychainSecret::new())
    }
}
