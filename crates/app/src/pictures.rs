//! Choosing a picture from this computer: the profile picture of a number,
//! the picture of a group.
//!
//! The file dialog is the operating system's (the desktop portal on Linux,
//! so it works under Wayland; the native panels on macOS and Windows),
//! opened through GPUI, which answers asynchronously: the window keeps
//! drawing while it is open. It sits behind [`PicturePicker`] so that no
//! test ever opens one.

use gpui_kit::{App, PathPromptOptions};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

/// The file the user picked, or `None` when they closed the dialog.
pub type Picked = Pin<Box<dyn Future<Output = Option<PathBuf>>>>;

/// Something that asks the user for one image file.
pub trait PicturePicker {
    /// Asks. Must not block: the answer comes through the future.
    fn pick(&self, cx: &mut App) -> Picked;
}

/// The operating system's file dialog.
pub struct SystemPicker;

impl PicturePicker for SystemPicker {
    fn pick(&self, cx: &mut App) -> Picked {
        let asked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose picture".into()),
        });
        Box::pin(async move {
            match asked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) => None,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "the file dialog could not be opened");
                    None
                }
                Err(_) => None,
            }
        })
    }
}

/// Reads the picked file and turns it into the square JPEG WhatsApp takes
/// (see `client_core::profile_picture`). Blocking: call it off the UI
/// thread. The error is a sentence for the user.
pub fn load(path: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|_| "The file could not be read.".to_owned())?
        .len();
    if size > client_core::PICTURE_MAX_FILE {
        return Err("The file is larger than 25 MB.".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|_| "The file could not be read.".to_owned())?;
    client_core::profile_picture(&bytes)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A picker that answers what a test queued, and counts the asks.
    #[derive(Clone, Default)]
    pub struct FakePicker {
        answers: Rc<RefCell<VecDeque<Option<PathBuf>>>>,
        asked: Rc<RefCell<usize>>,
    }

    impl FakePicker {
        /// The next dialog answers this file (`None`: it is dismissed).
        pub fn answer(&self, path: Option<PathBuf>) {
            self.answers.borrow_mut().push_back(path);
        }

        /// How many times a dialog was asked for.
        pub fn asked(&self) -> usize {
            *self.asked.borrow()
        }
    }

    impl PicturePicker for FakePicker {
        fn pick(&self, _: &mut App) -> Picked {
            *self.asked.borrow_mut() += 1;
            let answer = self.answers.borrow_mut().pop_front().flatten();
            Box::pin(async move { answer })
        }
    }

    #[test]
    fn a_picked_file_becomes_a_square_jpeg_or_a_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let image = image::RgbImage::from_pixel(300, 180, image::Rgb([20, 120, 220]));
        let path = dir.path().join("photo.png");
        image.save(&path).unwrap();
        let jpeg = load(&path).unwrap();
        let back = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((back.width(), back.height()), (180, 180));

        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "not a picture").unwrap();
        assert_eq!(
            load(&text).unwrap_err(),
            "The file is not an image this app can read."
        );
        assert_eq!(
            load(&dir.path().join("missing.png")).unwrap_err(),
            "The file could not be read."
        );
    }
}
