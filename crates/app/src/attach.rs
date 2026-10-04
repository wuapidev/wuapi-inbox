//! Attachments before they are sent: what a paste or a drop holds, what
//! kind of file something is, and reading it off the disk.
//!
//! Nothing here draws or sends. The preview sheet is `ui/attach.rs`; the
//! sending is the engine's (`SyncEngine::send_media`).

use client_provider::MediaKind;
use gpui_kit::{ClipboardEntry, ClipboardItem, ImageFormat};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How many files one paste, drop or pick may bring. More than this is a
/// mistake more often than an intention.
pub const MAX_FILES: usize = 20;

/// A file ready to be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    /// Its name, as the recipient will see it for a document.
    pub name: String,
    /// Its MIME type.
    pub mime: String,
    /// Its bytes.
    pub bytes: Arc<Vec<u8>>,
}

impl Attachment {
    /// Whether it is a picture WhatsApp shows inline.
    pub fn is_image(&self) -> bool {
        matches!(
            self.mime.as_str(),
            "image/jpeg" | "image/png" | "image/webp" | "image/gif"
        )
    }

    /// What it is sent as. A picture or a video goes as what it is unless
    /// the user asked for documents; everything else is a document.
    pub fn kind(&self, as_document: bool) -> MediaKind {
        if as_document {
            return MediaKind::Document;
        }
        if self.is_image() {
            MediaKind::Image
        } else if self.mime == "video/mp4" {
            MediaKind::Video
        } else if self.mime.starts_with("audio/") {
            MediaKind::Audio
        } else {
            MediaKind::Document
        }
    }
}

/// What a paste holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pasted {
    /// Files copied in a file manager.
    Files(Vec<PathBuf>),
    /// A picture (a screenshot, "copy image").
    Image(Attachment),
    /// Text, or nothing this application attaches: the field pastes it.
    Text,
}

/// What to do with a paste. The rule, in order:
///
/// 1. copied files are files;
/// 2. text that is nothing but paths or `file://` addresses of files that
///    exist is those files (how Linux file managers put a copy on the
///    clipboard, which the toolkit hands over as text);
/// 3. a picture is a picture, unless the clipboard also holds text that
///    says something: then the text is what was meant (a spreadsheet's
///    cells, a paragraph from a word processor, come as both);
/// 4. everything else is text.
pub fn classify_paste(item: &ClipboardItem, is_file: impl Fn(&Path) -> bool) -> Pasted {
    let mut paths = Vec::new();
    let mut image = None;
    let mut text = String::new();
    for entry in item.entries() {
        match entry {
            ClipboardEntry::ExternalPaths(files) => paths.extend(files.paths().iter().cloned()),
            ClipboardEntry::Image(picture) if image.is_none() => image = Some(picture),
            ClipboardEntry::String(string) => text.push_str(string.text()),
            _ => {}
        }
    }
    if !paths.is_empty() {
        return Pasted::Files(paths);
    }
    if let Some(files) = paths_in_text(&text, &is_file) {
        return Pasted::Files(files);
    }
    match image {
        Some(picture) if text.trim().is_empty() => {
            match picture_attachment(picture.format, picture.bytes.clone()) {
                Some(attachment) => Pasted::Image(attachment),
                None => Pasted::Text,
            }
        }
        _ => Pasted::Text,
    }
}

/// The files a text names, when it names nothing else: one path or
/// `file://` address a line, each of a file that exists.
fn paths_in_text(text: &str, is_file: &impl Fn(&Path) -> bool) -> Option<Vec<PathBuf>> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        // Some file managers lead with the operation.
        .filter(|line| !line.is_empty() && *line != "copy" && *line != "cut")
        .collect();
    if lines.is_empty() || lines.len() > MAX_FILES {
        return None;
    }
    let mut paths = Vec::new();
    for line in lines {
        let path = match line.strip_prefix("file://") {
            Some(rest) => PathBuf::from(percent_decode(
                // `file://host/path`: only this machine's files.
                rest.strip_prefix("localhost").unwrap_or(rest),
            )),
            None if line.starts_with('/') => PathBuf::from(line),
            None => return None,
        };
        if !path.is_absolute() || !is_file(&path) {
            return None;
        }
        paths.push(path);
    }
    Some(paths)
}

/// `%20` and the like, as the bytes they stand for.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = |index: usize| {
            bytes
                .get(index)
                .and_then(|byte| (*byte as char).to_digit(16))
        };
        match (bytes[at], hex(at + 1), hex(at + 2)) {
            (b'%', Some(high), Some(low)) => {
                out.push((high * 16 + low) as u8);
                at += 3;
            }
            (byte, _, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A picture off the clipboard as a file to send. The formats WhatsApp
/// shows inline go as they are; anything else (a bitmap, a TIFF) is
/// encoded as PNG first.
fn picture_attachment(format: ImageFormat, bytes: Vec<u8>) -> Option<Attachment> {
    let (mime, extension) = match format {
        ImageFormat::Png => ("image/png", "png"),
        ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        ImageFormat::Webp => ("image/webp", "webp"),
        ImageFormat::Gif => ("image/gif", "gif"),
        _ => {
            let decoded = image::load_from_memory(&bytes).ok()?;
            let mut png = Vec::new();
            decoded
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .ok()?;
            return Some(Attachment {
                name: "pasted-image.png".to_owned(),
                mime: "image/png".to_owned(),
                bytes: Arc::new(png),
            });
        }
    };
    (!bytes.is_empty()).then(|| Attachment {
        name: format!("pasted-image.{extension}"),
        mime: mime.to_owned(),
        bytes: Arc::new(bytes),
    })
}

/// The MIME type of a file: by what its first bytes say, then by its
/// name, then "a file".
pub fn mime_of(name: &str, bytes: &[u8]) -> String {
    let magic: &[(&[u8], usize, &str)] = &[
        (b"\x89PNG\r\n\x1a\n", 0, "image/png"),
        (b"\xff\xd8\xff", 0, "image/jpeg"),
        (b"GIF87a", 0, "image/gif"),
        (b"GIF89a", 0, "image/gif"),
        (b"WEBP", 8, "image/webp"),
        (b"%PDF-", 0, "application/pdf"),
        (b"ftyp", 4, "video/mp4"),
        (b"OggS", 0, "audio/ogg"),
        (b"ID3", 0, "audio/mpeg"),
    ];
    for (signature, offset, mime) in magic {
        if bytes.len() >= offset + signature.len()
            && &bytes[*offset..offset + signature.len()] == *signature
        {
            return (*mime).to_owned();
        }
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" | "opus" => "audio/ogg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "pdf" => "application/pdf",
        "txt" | "log" | "md" => "text/plain",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    }
    .to_owned()
}

/// What reading one file gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    /// Its bytes, ready to be sent.
    Ready(Attachment),
    /// Larger than the provider takes: not read at all, but still shown,
    /// flagged, so it can be taken out (the other files are not held up).
    TooLarge {
        /// Its name.
        name: String,
        /// Its size on disk.
        size: u64,
    },
}

/// Why a file of `size` cannot go, in words.
pub fn too_large(size: u64, limit: u64) -> String {
    format!(
        "Too large: {}, and the most that can be sent is {}.",
        crate::format::file_size(size),
        crate::format::file_size(limit)
    )
}

/// Reads files off the disk to attach them, in the order given. Returns
/// what could be read (a file over `limit` is not read, only named) and,
/// in words, why each of the others could not: a folder, an empty file, a
/// file that cannot be opened.
///
/// Blocking: call it off the UI thread.
pub fn read_files(paths: &[PathBuf], limit: Option<u64>) -> (Vec<Read>, Vec<String>) {
    let (mut files, mut refused) = (Vec::new(), Vec::new());
    for path in paths.iter().take(MAX_FILES) {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_owned());
        let size = match std::fs::metadata(path) {
            Ok(meta) if meta.is_file() => meta.len(),
            Ok(_) => {
                refused.push(format!("{name} is a folder: only files can be sent."));
                continue;
            }
            Err(_) => {
                refused.push(format!("{name} could not be read."));
                continue;
            }
        };
        if size == 0 {
            refused.push(format!("{name} is empty."));
            continue;
        }
        if limit.is_some_and(|limit| size > limit) {
            files.push(Read::TooLarge { name, size });
            continue;
        }
        match std::fs::read(path) {
            Ok(bytes) => files.push(Read::Ready(Attachment {
                mime: mime_of(&name, &bytes),
                name,
                bytes: Arc::new(bytes),
            })),
            Err(_) => refused.push(format!("{name} could not be read.")),
        }
    }
    if paths.len() > MAX_FILES {
        refused.push(format!(
            "Only the first {MAX_FILES} files were taken: send the rest after these."
        ));
    }
    (files, refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{ExternalPaths, Image};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-rest";

    fn item(entries: Vec<ClipboardEntry>) -> ClipboardItem {
        ClipboardItem { entries }
    }

    fn picture(format: ImageFormat, bytes: &[u8]) -> ClipboardEntry {
        ClipboardEntry::Image(Image::from_bytes(format, bytes.to_vec()))
    }

    fn text(value: &str) -> ClipboardEntry {
        ClipboardItem::new_string(value.to_owned())
            .entries()
            .first()
            .cloned()
            .unwrap()
    }

    #[test]
    fn a_screenshot_is_a_picture_and_text_is_text() {
        let nothing_exists = |_: &Path| false;
        // A screenshot: only a picture.
        let pasted = classify_paste(&item(vec![picture(ImageFormat::Png, PNG)]), nothing_exists);
        let Pasted::Image(attachment) = pasted else {
            panic!("a picture");
        };
        assert_eq!(attachment.mime, "image/png");
        assert_eq!(attachment.name, "pasted-image.png");
        assert_eq!(attachment.bytes.as_slice(), PNG);
        assert!(attachment.is_image());
        // Plain text pastes as text.
        assert_eq!(
            classify_paste(&item(vec![text("hello there")]), nothing_exists),
            Pasted::Text
        );
        // A picture next to text that says something: the text was meant.
        assert_eq!(
            classify_paste(
                &item(vec![text("A1\tB1\nA2\tB2"), picture(ImageFormat::Png, PNG)]),
                nothing_exists
            ),
            Pasted::Text
        );
        // Next to nothing but whitespace: the picture.
        assert!(matches!(
            classify_paste(
                &item(vec![
                    text("  \n"),
                    picture(ImageFormat::Jpeg, b"\xff\xd8\xff-")
                ]),
                nothing_exists
            ),
            Pasted::Image(_)
        ));
        // An empty clipboard is nothing to attach.
        assert_eq!(classify_paste(&item(vec![]), nothing_exists), Pasted::Text);
    }

    #[test]
    fn a_picture_whatsapp_does_not_show_becomes_a_png() {
        let mut bmp = Vec::new();
        image::RgbaImage::from_pixel(3, 2, image::Rgba([10, 20, 30, 255]))
            .write_to(&mut std::io::Cursor::new(&mut bmp), image::ImageFormat::Bmp)
            .unwrap();
        let pasted = classify_paste(&item(vec![picture(ImageFormat::Bmp, &bmp)]), |_| false);
        let Pasted::Image(attachment) = pasted else {
            panic!("a picture");
        };
        assert_eq!(attachment.mime, "image/png");
        assert!(attachment.bytes.starts_with(b"\x89PNG"));
        // What cannot be decoded is not attached.
        assert_eq!(
            classify_paste(&item(vec![picture(ImageFormat::Tiff, b"junk")]), |_| false),
            Pasted::Text
        );
    }

    // The addresses and paths here are a Unix desktop's: on Windows a
    // `file:///home/…` address names no file (there is no drive in it).
    #[cfg_attr(windows, ignore = "the paths are a Unix desktop's")]
    #[test]
    fn copied_files_are_files_however_the_desktop_hands_them_over() {
        let known = |path: &Path| path.starts_with("/home/u");
        // macOS and Windows: a list of paths.
        let files = ExternalPaths(vec![PathBuf::from("/home/u/a.pdf")].into());
        assert_eq!(
            classify_paste(
                &item(vec![ClipboardEntry::ExternalPaths(files), text("a.pdf")]),
                known
            ),
            Pasted::Files(vec![PathBuf::from("/home/u/a.pdf")])
        );
        // Linux: addresses as text, one a line, spaces escaped.
        assert_eq!(
            classify_paste(
                &item(vec![text(
                    "file:///home/u/My%20Report.pdf\nfile:///home/u/b.png\n"
                )]),
                known
            ),
            Pasted::Files(vec![
                PathBuf::from("/home/u/My Report.pdf"),
                PathBuf::from("/home/u/b.png")
            ])
        );
        assert_eq!(
            classify_paste(&item(vec![text("copy\n/home/u/a.pdf")]), known),
            Pasted::Files(vec![PathBuf::from("/home/u/a.pdf")])
        );
        // A path that is not a file here is a sentence about a path.
        assert_eq!(
            classify_paste(&item(vec![text("/etc/hosts")]), known),
            Pasted::Text
        );
        // So is a path among other words.
        assert_eq!(
            classify_paste(&item(vec![text("see /home/u/a.pdf\n/home/u/a.pdf")]), known),
            Pasted::Text
        );
        // A file on another machine is not attached.
        assert_eq!(
            classify_paste(&item(vec![text("file://server/home/u/a.pdf")]), known),
            Pasted::Text
        );
    }

    #[test]
    fn a_file_says_what_it_is_and_how_it_goes() {
        assert_eq!(
            mime_of("x.bin", PNG),
            "image/png",
            "its bytes, not its name"
        );
        assert_eq!(mime_of("holiday.JPG", b""), "image/jpeg");
        assert_eq!(mime_of("clip.mp4", b"\0\0\0\x18ftypmp42"), "video/mp4");
        assert_eq!(mime_of("notes", b"plain words"), "application/octet-stream");
        let file = |name: &str, mime: &str| Attachment {
            name: name.into(),
            mime: mime.into(),
            bytes: Arc::new(vec![1]),
        };
        assert_eq!(file("a.png", "image/png").kind(false), MediaKind::Image);
        assert_eq!(file("a.png", "image/png").kind(true), MediaKind::Document);
        assert_eq!(file("a.mp4", "video/mp4").kind(false), MediaKind::Video);
        assert_eq!(file("a.mp3", "audio/mpeg").kind(false), MediaKind::Audio);
        assert_eq!(
            file("a.pdf", "application/pdf").kind(false),
            MediaKind::Document
        );
        // A picture WhatsApp does not show inline goes as a document.
        assert_eq!(
            file("a.svg", "image/svg+xml").kind(false),
            MediaKind::Document
        );
    }

    #[test]
    fn files_are_read_within_the_limit_and_the_rest_is_said() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        let paths = vec![
            write("small.png", PNG),
            write("large.bin", &[0; 2000]),
            write("empty.txt", b""),
            dir.path().to_path_buf(),
            dir.path().join("missing.pdf"),
        ];
        let (files, refused) = read_files(&paths, Some(1000));
        // In order: the one that fits, then the one over the limit, named
        // and not read.
        assert_eq!(files.len(), 2);
        let Read::Ready(small) = &files[0] else {
            panic!("the small file is read");
        };
        assert_eq!(
            (small.name.as_str(), small.mime.as_str()),
            ("small.png", "image/png")
        );
        assert_eq!(
            files[1],
            Read::TooLarge {
                name: "large.bin".into(),
                size: 2000
            }
        );
        assert_eq!(refused.len(), 3);
        assert!(refused[0].contains("empty"));
        assert!(refused[1].contains("folder"));
        assert!(refused[2].contains("could not be read"));
        // No limit: everything that is a file with something in it.
        let (everything, _) = read_files(&paths, None);
        assert!(everything.iter().all(|file| matches!(file, Read::Ready(_))));
        assert_eq!(everything.len(), 2);
        assert!(too_large(3009, 2000).contains("Too large"));
    }
}
