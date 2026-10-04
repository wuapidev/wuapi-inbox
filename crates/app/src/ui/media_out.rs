//! Pictures and files leaving the application on the user's word: a
//! picture copied to the clipboard, a file saved where they chose.
//!
//! Both take the ORIGINAL: what the provider has, byte for byte. When
//! only a thumbnail is here, the original is fetched first (the same
//! download, with the same progress and stop, as opening the file), and
//! the copy or the save happens when it arrives. A thumbnail is never
//! passed off as the picture.
//!
//! Saving is, with "Open", the one place content leaves the encrypted
//! store: only where the user said (the file dialog's answer, or their
//! Downloads folder on the one-click variant), with ordinary permissions,
//! written straight from the store with no copy left anywhere else.

use super::bubble::Row;
use super::media::{export_name, FileState};
use super::shell::{Overlay, Shell};
use crate::clipboard::{self, ImageClipboard, SystemClipboard};
use client_provider::{Media, MediaKind, Message, MessageContent, Timestamp};
use gpui_kit::{AppContext as _, Context, SharedString, Task, Window};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// The file that remembers the folder last saved to, next to the settings.
const FOLDER_FILE: &str = "save-folder";

/// Asks where to save a file, without blocking the interface: the folder
/// to start in and the name to offer. `None` when the dialog was dismissed.
pub type SaveDialog = Rc<dyn Fn(&Path, &str, &mut gpui_kit::App) -> Task<Option<PathBuf>>>;
/// Asks for a folder.
pub type FolderDialog = Rc<dyn Fn(&mut gpui_kit::App) -> Task<Option<PathBuf>>>;

/// How things leave the application: the system's clipboard and dialogs,
/// unless a test brings its own.
#[derive(Clone)]
pub struct MediaOut {
    /// Where pictures are copied to.
    pub clipboard: Arc<dyn ImageClipboard>,
    /// "Save as…".
    pub save_as: SaveDialog,
    /// "Save N items": the folder.
    pub pick_folder: FolderDialog,
    /// Where "Save to Downloads" saves.
    pub downloads: PathBuf,
}

impl MediaOut {
    /// The system's: its clipboard, its dialogs (the desktop portal on
    /// Linux, through the toolkit), its Downloads folder.
    pub fn system() -> Self {
        Self {
            clipboard: Arc::new(SystemClipboard::default()),
            save_as: Rc::new(|folder, name, cx| {
                let answer = cx.prompt_for_new_path(folder, Some(name));
                cx.spawn(async move |_| answer.await.ok()?.ok()?)
            }),
            pick_folder: Rc::new(|cx| {
                let answer = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
                    files: false,
                    directories: true,
                    multiple: false,
                    prompt: None,
                });
                cx.spawn(async move |_| answer.await.ok()?.ok()??.into_iter().next())
            }),
            downloads: dirs::download_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_else(std::env::temp_dir),
        }
    }
}

/// What is to be done with a file once it is here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Out {
    /// The picture goes on the clipboard.
    Copy,
    /// The file is written to this path, replacing what is there (the
    /// dialog asked).
    SaveTo(PathBuf),
    /// The file is written into this folder under this name, or the next
    /// free one like it.
    SaveInto(PathBuf, String),
    /// The file goes into the sticker and GIF library (see
    /// `library_ui.rs`).
    Keep(super::library_ui::Keep),
}

/// A line that says something went well, with the way to what it made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Notice {
    pub(super) text: SharedString,
    /// The file to show in its folder.
    pub(super) reveal: Option<PathBuf>,
}

/// The state of what is leaving.
#[derive(Default)]
pub(super) struct Leaving {
    /// What waits for its file: fetched, then done.
    pub(super) waiting: Vec<(Media, Out)>,
    /// What is said under the header.
    pub(super) notice: Option<Notice>,
    _notice: Option<Task<()>>,
    /// The file last shown in its folder (for tests: the file manager is
    /// not theirs to open).
    pub(super) revealed: Option<PathBuf>,
}

/// A name that is safe as a file's: no path, no control characters.
fn clean(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The name a file is offered under: the one the message gives it, else
/// the chat and when it was sent, with the extension of what it is.
pub(super) fn save_name(media: &Media, mime: Option<&str>, chat: &str, sent: Timestamp) -> String {
    if media
        .file_name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
    {
        return export_name(media, mime);
    }
    // `export_name` without a name answers "<kind>.<ext>": its extension.
    let generic = export_name(media, mime);
    let extension = match generic.rsplit_once('.').map_or("bin", |(_, ext)| ext) {
        // The type is not known yet (the file has not been fetched and
        // the message does not say): what its kind usually is.
        "bin" => match media.kind {
            MediaKind::Image => "jpg",
            MediaKind::Sticker => "webp",
            MediaKind::Video => "mp4",
            MediaKind::Voice => "ogg",
            MediaKind::Audio => "mp3",
            MediaKind::Document => "bin",
        },
        known => known,
    };
    let chat = clean(chat);
    let chat = if chat.is_empty() {
        "chat"
    } else {
        chat.as_str()
    };
    format!("{chat} {}.{extension}", crate::format::file_stamp(sent))
}

/// `name` in `folder`, or the next free name like it: `photo (1).jpg`.
pub(super) fn free_path(folder: &Path, name: &str) -> PathBuf {
    let first = folder.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    (1..10_000)
        .map(|n| match extension {
            Some(extension) => folder.join(format!("{stem} ({n}).{extension}")),
            None => folder.join(format!("{stem} ({n})")),
        })
        .find(|path| !path.exists())
        .unwrap_or(first)
}

/// What a message carries that can be saved or copied.
pub(super) fn media_of(message: &Message) -> Option<&Media> {
    match &message.content {
        MessageContent::Media(media)
            if !message.deleted && !message.extras.view_once && media.source.is_some() =>
        {
            Some(media)
        }
        _ => None,
    }
}

impl Shell {
    // ----- notices --------------------------------------------------------------------

    /// Says that something went well, for a few seconds.
    pub(super) fn show_notice(
        &mut self,
        text: impl Into<SharedString>,
        reveal: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        self.leaving.notice = Some(Notice {
            text: text.into(),
            reveal,
        });
        self.leaving._notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(8))
                .await;
            this.update(cx, |this, cx| {
                this.leaving.notice = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// "Show in folder": the system's file manager, on the file.
    pub(super) fn reveal_saved(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.leaving.notice.as_ref().and_then(|n| n.reveal.clone()) else {
            return;
        };
        if !cfg!(test) {
            cx.reveal_path(&path);
        }
        self.leaving.revealed = Some(path);
        cx.notify();
    }

    // ----- what a file is, and whose --------------------------------------------------

    /// The message of the open chat that carries this file: for its name
    /// when it is saved.
    fn message_of(&self, media: &Media) -> Option<Message> {
        let source = media.source.as_ref()?;
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) => match &row.stored.message.content {
                MessageContent::Media(known) if known.source.as_ref() == Some(source) => {
                    Some(row.stored.message.clone())
                }
                _ => None,
            },
            Row::Day(_) => None,
        })
    }

    /// The name a file of the open chat is saved under.
    fn name_for(&self, media: &Media) -> String {
        let mime = media
            .source
            .as_ref()
            .and_then(|source| self.media.file(source.as_str()))
            .and_then(|(_, mime)| mime);
        let chat = self
            .open
            .as_ref()
            .map(|open| open.chat.title.clone())
            .unwrap_or_default();
        let sent = self
            .message_of(media)
            .map_or_else(Timestamp::now, |message| message.timestamp);
        save_name(media, mime.as_deref(), &chat, sent)
    }

    /// The folder "Save as…" starts in: the one used last, else Downloads.
    fn save_folder(&self, cx: &gpui_kit::App) -> PathBuf {
        crate::settings::sibling(FOLDER_FILE, cx)
            .and_then(|file| std::fs::read_to_string(file).ok())
            .map(|text| PathBuf::from(text.trim()))
            .filter(|folder| folder.is_dir())
            .unwrap_or_else(|| self.out.downloads.clone())
    }

    fn remember_folder(folder: &Path, cx: &gpui_kit::App) {
        if let Some(file) = crate::settings::sibling(FOLDER_FILE, cx) {
            if let Err(error) = std::fs::write(file, folder.to_string_lossy().as_bytes()) {
                tracing::warn!(%error, "could not remember the folder");
            }
        }
    }

    // ----- doing it -------------------------------------------------------------------

    /// Does `out` with a file: now when it is here, else once it has been
    /// fetched, which starts now and shows where the file is drawn.
    pub(super) fn send_out(&mut self, media: Media, out: Out, cx: &mut Context<Self>) {
        let (Some(account), Some(source)) = (self.account.clone(), media.source.clone()) else {
            return;
        };
        match self.media.file_state(source.as_str()) {
            FileState::Ready => self.do_out(&media, out, cx),
            FileState::Unavailable(reason) if self.media.file_expired(source.as_str()) => {
                self.show_problem(format!("The file is no longer available: {reason}"), cx);
            }
            _ => {
                self.media.retry_file(&account, source.as_str());
                let what = match out {
                    Out::Copy => "Downloading the original to copy it…",
                    Out::Keep(_) => "Downloading the original to keep it…",
                    _ => "Downloading the original to save it…",
                };
                self.leaving.waiting.push((media, out));
                self.show_notice(what, None, cx);
            }
        }
        cx.notify();
    }

    /// A file arrived, or will not: what waited for it is done, or said.
    pub(super) fn out_arrived(&mut self, key: &str, cx: &mut Context<Self>) {
        let is_it = |media: &Media| {
            media
                .source
                .as_ref()
                .is_some_and(|source| client_core::file_key(source.as_str()) == key)
        };
        if !self.leaving.waiting.iter().any(|(media, _)| is_it(media)) {
            return;
        }
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.leaving.waiting)
            .into_iter()
            .partition(|(media, _)| is_it(media));
        self.leaving.waiting = waiting;
        for (media, out) in ready {
            let Some(source) = media.source.clone() else {
                continue;
            };
            match self.media.file_state(source.as_str()) {
                FileState::Ready => self.do_out(&media, out, cx),
                FileState::Unavailable(reason) => {
                    self.leaving.notice = None;
                    self.show_problem(format!("The file could not be downloaded: {reason}"), cx);
                }
                // Stopped, or still on its way: it waits on.
                _ => self.leaving.waiting.push((media, out)),
            }
        }
    }

    /// Does `out` with a file that is here.
    fn do_out(&mut self, media: &Media, out: Out, cx: &mut Context<Self>) {
        let Some((bytes, mime)) = media
            .source
            .as_ref()
            .and_then(|source| self.media.file(source.as_str()))
        else {
            return self.show_problem("The file is not here.".into(), cx);
        };
        match out {
            Out::Keep(keep) => self.keep_in_library(keep, bytes, mime, media.source.clone(), cx),
            Out::Copy => {
                let clipboard = self.out.clipboard.clone();
                let toolkit = clipboard.through_toolkit();
                cx.spawn(async move |this, cx| {
                    // Decoding, and on Linux the talk with the display
                    // server, off the interface's thread.
                    let made = cx
                        .background_spawn(async move {
                            let offer = clipboard::offer(bytes, mime.as_deref())?;
                            if !toolkit {
                                clipboard.copy(&offer)?;
                            }
                            Ok::<_, String>(offer)
                        })
                        .await;
                    this.update(cx, |this, cx| match made {
                        Ok(offer) => {
                            if toolkit {
                                clipboard::copy_on_toolkit(&offer, cx);
                            }
                            this.show_notice("Image copied", None, cx);
                        }
                        Err(error) => {
                            this.leaving.notice = None;
                            this.show_problem(
                                format!("The image could not be copied: {error}"),
                                cx,
                            );
                        }
                    })
                    .ok();
                })
                .detach();
            }
            Out::SaveTo(_) | Out::SaveInto(..) => {
                cx.spawn(async move |this, cx| {
                    let written = cx
                        .background_spawn(async move {
                            let path = match out {
                                Out::SaveTo(path) => path,
                                Out::SaveInto(folder, name) => free_path(&folder, &name),
                                Out::Copy | Out::Keep(_) => unreachable!("matched above"),
                            };
                            // The original bytes, as they are, where the
                            // user said and nowhere else.
                            std::fs::write(&path, &bytes)
                                .map(|()| path.clone())
                                .map_err(|error| format!("{}: {error}", path.display()))
                        })
                        .await;
                    this.update(cx, |this, cx| match written {
                        Ok(path) => {
                            let name = path
                                .file_name()
                                .map(|name| name.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            this.show_notice(format!("Saved {name}"), Some(path), cx);
                        }
                        Err(error) => {
                            this.leaving.notice = None;
                            this.show_problem(format!("The file could not be saved: {error}"), cx);
                        }
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    // ----- the commands ---------------------------------------------------------------

    /// The file an action is about: the one in the viewer, else the one
    /// the message in focus carries.
    fn out_subject(&self) -> Option<Media> {
        if self.overlay == Overlay::Viewer {
            return self.viewing.clone();
        }
        self.focused_message().as_ref().and_then(media_of).cloned()
    }

    /// Copies the picture an action is about: the original, fetched first
    /// when it is not here.
    pub(super) fn copy_image(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(media) = self
            .out_subject()
            .filter(|media| matches!(media.kind, MediaKind::Image | MediaKind::Sticker))
        else {
            return false;
        };
        self.send_out(media, Out::Copy, cx);
        true
    }

    /// Copies a given picture (the buttons on a picture).
    pub(super) fn copy_media(&mut self, media: Media, cx: &mut Context<Self>) {
        self.send_out(media, Out::Copy, cx);
    }

    /// "Save as…": asks where, then saves the original there.
    pub(super) fn save_as(&mut self, media: Option<Media>, cx: &mut Context<Self>) -> bool {
        let Some(media) = media.or_else(|| self.out_subject()) else {
            return false;
        };
        let (folder, name) = (self.save_folder(cx), self.name_for(&media));
        let asked = (self.out.save_as)(&folder, &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = asked.await else { return };
            this.update(cx, |this, cx| {
                if let Some(folder) = path.parent() {
                    Self::remember_folder(folder, cx);
                }
                this.send_out(media, Out::SaveTo(path), cx);
            })
            .ok();
        })
        .detach();
        true
    }

    /// "Save to Downloads": no dialog, a name that is free.
    pub(super) fn save_to_downloads(
        &mut self,
        media: Option<Media>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(media) = media.or_else(|| self.out_subject()) else {
            return false;
        };
        let name = self.name_for(&media);
        let folder = self.out.downloads.clone();
        if let Err(error) = std::fs::create_dir_all(&folder) {
            self.show_problem(format!("{}: {error}", folder.display()), cx);
            return true;
        }
        self.send_out(media, Out::SaveInto(folder, name), cx);
        true
    }

    /// "Save N items": every file among the messages picked, into one
    /// folder that is asked for once.
    pub(super) fn save_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let files: Vec<Media> = self
            .subjects()
            .iter()
            .filter_map(media_of)
            .cloned()
            .collect();
        if files.is_empty() {
            return false;
        }
        let asked = (self.out.pick_folder)(cx);
        cx.spawn(async move |this, cx| {
            let Some(folder) = asked.await else { return };
            this.update(cx, |this, cx| {
                Self::remember_folder(&folder, cx);
                for media in files {
                    let name = this.name_for(&media);
                    this.send_out(media, Out::SaveInto(folder.clone(), name), cx);
                }
                this.acting.selecting = None;
            })
            .ok();
        })
        .detach();
        true
    }

    // ----- the viewer -----------------------------------------------------------------

    /// The pictures of the open chat, oldest first: what Left and Right
    /// walk in the viewer.
    pub(super) fn pictures(&self) -> Vec<Media> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        open.rows
            .iter()
            .filter_map(|row| match row {
                Row::Message(row) => media_of(&row.stored.message),
                Row::Day(_) => None,
            })
            .filter(|media| media.kind == MediaKind::Image)
            .cloned()
            .collect()
    }

    /// The picture after (or before) the one in the viewer, if there is
    /// one: the viewer shows it.
    pub(super) fn step_viewer(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let pictures = self.pictures();
        let Some(at) = self.viewing.as_ref().and_then(|shown| {
            pictures
                .iter()
                .position(|media| media.source == shown.source)
        }) else {
            return false;
        };
        let next = if forward { at + 1 } else { at.wrapping_sub(1) };
        let Some(media) = pictures.get(next).cloned() else {
            return false;
        };
        if let (Some(account), Some(source)) = (&self.account, &media.source) {
            self.engine.want_file(account, source.as_str());
        }
        self.viewing = Some(media);
        // Each picture opens whole.
        self.viewer.reset();
        self.load_viewed(cx);
        cx.notify();
        true
    }

    /// Whether the viewer has a picture before and after the one shown.
    pub(super) fn viewer_neighbours(&self) -> (bool, bool) {
        let pictures = self.pictures();
        let at = self.viewing.as_ref().and_then(|shown| {
            pictures
                .iter()
                .position(|media| media.source == shown.source)
        });
        match at {
            Some(at) => (at > 0, at + 1 < pictures.len()),
            None => (false, false),
        }
    }

    /// Opens the viewer's picture with the system's application for it.
    pub(super) fn open_viewed(&mut self, cx: &mut Context<Self>) {
        if let Some(media) = self.viewing.clone() {
            self.open_file(media, cx);
        }
    }

    /// Opens the viewer on a picture, at its fitted size.
    pub(super) fn begin_viewing(
        &mut self,
        media: Media,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.viewer.reset();
        self.view_image(media, window, cx);
        self.load_viewed(cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(kind: MediaKind, name: Option<&str>, mime: Option<&str>) -> Media {
        let mut media = Media::new(kind);
        media.file_name = name.map(str::to_owned);
        media.mime_type = mime.map(str::to_owned);
        media
    }

    #[test]
    fn a_file_is_named_as_the_message_names_it_or_by_chat_and_time() {
        let sent = Timestamp::from_millis(1_790_000_000_000);
        let stamp = crate::format::file_stamp(sent);
        // The message's own name, without any path it may carry.
        assert_eq!(
            save_name(
                &media(MediaKind::Document, Some("../../contract.pdf"), None),
                None,
                "Ana",
                sent
            ),
            "contract.pdf"
        );
        // No name: the chat and when it was sent, with the type's
        // extension. Nothing of the chat's name can leave the folder.
        assert_eq!(
            save_name(
                &media(MediaKind::Image, None, Some("image/jpeg")),
                None,
                "Lisbon trip ✈️",
                sent
            ),
            format!("Lisbon trip ✈️ {stamp}.jpg")
        );
        assert_eq!(
            save_name(
                &media(MediaKind::Voice, None, None),
                Some("audio/ogg"),
                "a/b\\c:d",
                sent
            ),
            format!("a b c d {stamp}.ogg")
        );
        assert_eq!(
            save_name(
                &media(MediaKind::Video, None, Some("video/mp4")),
                None,
                "  ",
                sent
            ),
            format!("chat {stamp}.mp4")
        );
        assert!(!stamp.contains(':') && !stamp.contains('/'), "{stamp}");
    }

    #[test]
    fn a_name_that_is_taken_gets_a_number() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            free_path(dir.path(), "photo.jpg"),
            dir.path().join("photo.jpg")
        );
        std::fs::write(dir.path().join("photo.jpg"), b"1").unwrap();
        assert_eq!(
            free_path(dir.path(), "photo.jpg"),
            dir.path().join("photo (1).jpg")
        );
        std::fs::write(dir.path().join("photo (1).jpg"), b"2").unwrap();
        assert_eq!(
            free_path(dir.path(), "photo.jpg"),
            dir.path().join("photo (2).jpg")
        );
        std::fs::write(dir.path().join("notes"), b"3").unwrap();
        assert_eq!(free_path(dir.path(), "notes"), dir.path().join("notes (1)"));
    }
}
