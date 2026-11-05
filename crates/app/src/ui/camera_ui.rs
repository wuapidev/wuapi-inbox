//! The camera, from the composer's "+": what it sees, live, and a button
//! that takes the picture. The picture then goes to the sheet that sends
//! files, as a file pasted would: a caption, then the outbox.

use super::shell::Shell;
use super::widgets::text_button;
use crate::attach::Attachment;
use crate::icons::IconName;
use crate::theme::{metrics, px, Palette};
use client_provider::{CallPicture, CallScreen};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Context, Div, ObjectFit, RenderImage, SharedString, Stateful, Task};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// How often what the camera sees is drawn again.
const TICK: Duration = Duration::from_millis(66);
/// How wide the picture is shown.
const SHOWN_WIDTH: f32 = 560.;

/// A camera that is open: the pictures it shows, and why it shows none.
/// Whatever keeps it open is let go with it.
pub(crate) struct CameraFeed {
    pub(crate) screen: CallScreen,
    pub(crate) problem: Box<dyn Fn() -> Option<String>>,
    pub(crate) _open: Box<dyn std::any::Any>,
}

/// How the camera is found and opened.
pub(crate) trait CameraSource {
    /// Whether there is a camera to offer, without waiting.
    fn available(&self) -> bool;
    /// Opens `camera` (as chosen for calls), or the first there is.
    fn open(&self, camera: Option<String>) -> Result<CameraFeed, String>;
}

/// See [`CameraSource`].
#[derive(Clone)]
pub struct Cameras(pub(crate) Rc<dyn CameraSource>);

impl Cameras {
    pub(super) fn available(&self) -> bool {
        self.0.available()
    }
}

/// The machine's camera, through `ffmpeg`, as calls with video use it.
struct System;

#[cfg(feature = "provider-native")]
impl CameraSource for System {
    fn available(&self) -> bool {
        provider_native::camera::available_now()
    }

    fn open(&self, camera: Option<String>) -> Result<CameraFeed, String> {
        let finder = Rc::new(provider_native::camera::Viewfinder::open(camera)?);
        let asked = finder.clone();
        Ok(CameraFeed {
            screen: finder.screen().clone(),
            problem: Box::new(move || asked.problem()),
            _open: Box::new(finder),
        })
    }
}

/// Without the numbers of this computer there is no `ffmpeg` camera.
#[cfg(not(feature = "provider-native"))]
impl CameraSource for System {
    fn available(&self) -> bool {
        false
    }

    fn open(&self, _camera: Option<String>) -> Result<CameraFeed, String> {
        Err("There is no camera in this build.".into())
    }
}

/// The machine's own camera.
pub fn system_cameras() -> Cameras {
    #[cfg(feature = "provider-native")]
    provider_native::call_video::look_ahead();
    Cameras(Rc::new(System))
}

/// A picture the camera showed, as a JPEG to send.
pub(super) fn jpeg(picture: &CallPicture) -> Option<Vec<u8>> {
    let mut rgb = Vec::with_capacity((picture.width * picture.height * 3) as usize);
    for pixel in picture.bgra.as_chunks::<4>().0 {
        rgb.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let image = image::RgbImage::from_raw(picture.width, picture.height, rgb)?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90)
        .encode_image(&image)
        .ok()?;
    Some(out.into_inner())
}

/// The camera while its sheet is open.
pub(crate) struct CameraView {
    feed: Option<CameraFeed>,
    /// Why there is nothing to show.
    problem: Option<SharedString>,
    /// The picture as last drawn, with the count it had on the screen.
    drawn: RefCell<(u64, Option<Arc<RenderImage>>)>,
    /// Pictures that were replaced, which the window still holds.
    stale: RefCell<Vec<Arc<RenderImage>>>,
    /// The picture is being made into a file.
    taking: bool,
    _tick: Option<Task<()>>,
}

impl CameraView {
    fn picture(&self) -> Option<Arc<RenderImage>> {
        let feed = self.feed.as_ref()?;
        let (count, newest) = feed.screen.newest();
        let mut drawn = self.drawn.borrow_mut();
        if drawn.0 == count && drawn.1.is_some() {
            return drawn.1.clone();
        }
        let image = newest.as_deref().and_then(render_image);
        if let Some(previous) = std::mem::replace(&mut *drawn, (count, image.clone())).1 {
            self.stale.borrow_mut().push(previous);
        }
        image
    }

    /// Frees the pictures the window holds that were replaced.
    pub(super) fn release(&self, window: &mut gpui_kit::Window) {
        for image in self.stale.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
    }

    /// Lets the camera go, and every picture the window holds of it.
    pub(super) fn close(self, window: &mut gpui_kit::Window) {
        self.release(window);
        if let Some(image) = self.drawn.borrow_mut().1.take() {
            let _ = window.drop_image(image);
        }
    }
}

fn render_image(picture: &CallPicture) -> Option<Arc<RenderImage>> {
    let image = image::RgbaImage::from_raw(picture.width, picture.height, picture.bgra.clone())?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

impl Shell {
    /// Opens the camera, and draws what it sees until the sheet closes.
    pub(super) fn begin_camera(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let (feed, problem) = match self.cameras.0.open(self.call_camera.clone()) {
            Ok(feed) => (Some(feed), None),
            Err(problem) => {
                tracing::warn!(%problem, "the camera did not open");
                (None, Some(crate::i18n::t("chat.camera.no_camera").into()))
            }
        };
        let tick = feed.is_some().then(|| {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(TICK).await;
                let open = this
                    .update(cx, |this, cx| {
                        let Some(view) = &mut this.camera else {
                            return false;
                        };
                        if view.problem.is_none() {
                            if let Some(problem) = view.feed.as_ref().and_then(|f| (f.problem)()) {
                                tracing::warn!(%problem, "the camera shows nothing");
                                view.problem =
                                    Some(crate::i18n::t("chat.camera.no_picture").into());
                            }
                        }
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !open {
                    break;
                }
            })
        });
        self.camera = Some(CameraView {
            feed,
            problem,
            drawn: RefCell::default(),
            stale: RefCell::default(),
            taking: false,
            _tick: tick,
        });
        self.overlay_focus.focus(window, cx);
    }

    /// Takes what the camera shows now and hands it to the sheet that
    /// sends files.
    pub(super) fn take_photo(&mut self, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let Some(view) = &mut self.camera else {
            return;
        };
        if view.taking {
            return;
        }
        let Some(picture) = view.feed.as_ref().and_then(|feed| feed.screen.newest().1) else {
            return;
        };
        view.taking = true;
        cx.notify();
        let made = cx
            .background_executor()
            .spawn(async move { jpeg(&picture) });
        let name = chrono::Local::now()
            .format("IMG-%Y%m%d-%H%M%S.jpg")
            .to_string();
        cx.spawn_in(window, async move |this, cx| {
            let bytes = made.await;
            this.update_in(cx, |this, window, cx| {
                if this.camera.is_none() {
                    return;
                }
                this.close_overlay(window, cx);
                match bytes {
                    Some(bytes) => this.attach_pasted(
                        Attachment {
                            name,
                            mime: "image/jpeg".into(),
                            bytes: Arc::new(bytes),
                        },
                        cx,
                    ),
                    None => {
                        this.show_problem(crate::i18n::t("chat.camera.not_taken").to_owned(), cx)
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// The camera's sheet.
    pub(super) fn render_camera(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let view = self.camera.as_ref()?;
        let picture = view.picture();
        let ready = picture.is_some() && !view.taking;
        // As the camera shows it; 4:3 until it shows anything.
        let (width, height) = view
            .feed
            .as_ref()
            .and_then(|feed| feed.screen.newest().1)
            .map_or((4, 3), |picture| (picture.width.max(1), picture.height));
        let shown_height = SHOWN_WIDTH * height as f32 / width as f32;
        let screen = div()
            .debug_selector(|| "camera-screen".into())
            .w(px(SHOWN_WIDTH))
            .max_w_full()
            .h(px(shown_height))
            .rounded(metrics::RADIUS())
            .overflow_hidden()
            .bg(palette.background)
            .flex()
            .items_center()
            .justify_center()
            .text_size(metrics::TEXT_BODY())
            .text_color(palette.text_muted);
        let screen = match (&picture, &view.problem) {
            (Some(picture), _) => screen.child(
                img(picture.clone())
                    .debug_selector(|| "camera-picture".into())
                    .size_full()
                    .object_fit(ObjectFit::Contain),
            ),
            (None, Some(problem)) => screen.child(
                div()
                    .debug_selector(|| "camera-problem".into())
                    .px_4()
                    .text_color(palette.danger)
                    .child(problem.clone()),
            ),
            (None, None) => screen.child(crate::i18n::t("chat.camera.opening")),
        };
        Some(
            self.card("camera", palette)
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(screen)
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button(
                                "camera-cancel",
                                crate::i18n::t("chat.camera.cancel"),
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_overlay(window, cx);
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "camera-shoot",
                                crate::i18n::t("chat.camera.take"),
                                Some(IconName::Camera),
                                ready,
                                palette,
                            )
                            .when(!ready, |this| this.opacity(0.5))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.take_photo(window, cx);
                                },
                            )),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::jpeg;
    use client_provider::CallPicture;

    #[test]
    fn a_picture_is_kept_as_a_jpeg_of_its_size_and_colours() {
        // Blue, green, red, alpha: a red picture.
        let picture = CallPicture {
            width: 16,
            height: 8,
            bgra: [0u8, 0, 255, 255].repeat(16 * 8),
        };
        let bytes = jpeg(&picture).expect("a jpeg");
        assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        let back = image::load_from_memory(&bytes).unwrap().to_rgb8();
        assert_eq!(back.dimensions(), (16, 8));
        let [r, g, b] = back.get_pixel(4, 4).0;
        assert!(r > 200 && g < 60 && b < 60, "{r} {g} {b}");
    }
}
