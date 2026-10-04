//! The picture viewer's geometry: how large the picture is shown, where,
//! and how far it may be moved.
//!
//! The scale is always relative to the picture's natural pixels: 100 % is
//! one pixel of the picture on one logical pixel of the window (so on a
//! display scaled 2x a picture at 100 % covers two device pixels a side,
//! like everything else in the window). A picture opens whole: fitted
//! inside the area under the toolbar, never enlarged past 100 %.
//!
//! What is drawn is decoded here, off the interface's thread: turned
//! upright as its EXIF orientation says, and scaled down when it is
//! larger than any screen, which bounds the memory a picture takes. Its
//! natural size stays that of the file.

use super::media::{image_of, MediaShelf, MediaVisual, PLACEHOLDER};
use super::shell::Shell;
use crate::theme::px;
use gpui_kit::{point, size, AppContext as _, Bounds, Context, Image, Pixels, Point, RenderImage};
use std::sync::Arc;

/// The closest a picture is looked at: 800 %.
pub const MOST: f32 = 8.;
/// The scales "+" and "-" stop at.
pub const STEPS: [f32; 13] = [
    0.10, 0.25, 0.33, 0.50, 0.67, 0.75, 1., 1.25, 1.5, 2., 3., 4., 8.,
];
/// What one line of the wheel scales by.
pub const WHEEL: f32 = 1.25;
/// The longest side a picture is decoded at for the viewer: 64 MB of
/// pixels at the very most, whatever the file holds.
pub const MAX_SIDE: u32 = 4096;

/// How the picture is scaled.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Zoom {
    /// Whole, inside the area; follows the window.
    #[default]
    Fit,
    /// At this scale, whatever the window does.
    At(f32),
}

/// The scale at which a picture of `natural` size is whole inside `area`,
/// never above its natural size.
pub fn fit(natural: (f32, f32), area: (f32, f32)) -> f32 {
    if natural.0 <= 0. || natural.1 <= 0. {
        return 1.;
    }
    (area.0 / natural.0)
        .min(area.1 / natural.1)
        .clamp(0.001, 1.)
}

/// The furthest a picture can be looked at from: 10 %, or its fitted
/// scale when that is smaller still.
pub fn least(fit: f32) -> f32 {
    fit.min(STEPS[0])
}

/// The next stop from `scale`, closer or further.
pub fn step(scale: f32, closer: bool, fit: f32) -> f32 {
    let next = if closer {
        STEPS.iter().copied().find(|stop| *stop > scale * 1.005)
    } else {
        STEPS
            .iter()
            .rev()
            .copied()
            .find(|stop| *stop < scale * 0.995)
    };
    next.unwrap_or(if closer { MOST } else { 0. })
        .clamp(least(fit), MOST)
}

/// How far the middle of the picture may be from the middle of the area:
/// nowhere on an axis where the picture is no larger than the area (it
/// stays centred), and otherwise only until its edge meets the area's.
pub fn clamp_pan(pan: (f32, f32), shown: (f32, f32), area: (f32, f32)) -> (f32, f32) {
    let axis = |pan: f32, shown: f32, area: f32| {
        let room = ((shown - area) / 2.).max(0.);
        pan.clamp(-room, room)
    };
    (axis(pan.0, shown.0, area.0), axis(pan.1, shown.1, area.1))
}

/// The pan after the scale goes `from` → `to` with the point of the
/// picture under `anchor` (measured from the middle of the area) staying
/// where it is.
pub fn pan_around(pan: (f32, f32), anchor: (f32, f32), from: f32, to: f32) -> (f32, f32) {
    let ratio = to / from;
    (
        anchor.0 - (anchor.0 - pan.0) * ratio,
        anchor.1 - (anchor.1 - pan.1) * ratio,
    )
}

/// What is drawn of the original.
#[derive(Clone)]
pub enum Drawn {
    /// Its pixels, upright.
    Still(Arc<RenderImage>),
    /// The file itself: a GIF or a WebP that moves.
    Moving(Arc<Image>),
}

/// The original of the picture in the viewer, decoded.
pub struct Shown {
    url: String,
    /// The size of the picture in its own pixels, upright.
    pub natural: (u32, u32),
    /// What to draw.
    pub drawn: Drawn,
}

/// The viewer's state.
#[derive(Default)]
pub struct Viewer {
    /// How the picture is scaled.
    pub zoom: Zoom,
    /// How far its middle was moved from the middle of the area.
    pub pan: (f32, f32),
    /// Where the pointer was last seen.
    pub pointer: Option<Point<Pixels>>,
    /// The decoded original, once there is one.
    pub shown: Option<Shown>,
    /// The picture being decoded.
    decoding: Option<String>,
    /// Pictures let go, which the window still holds.
    stale: Vec<Arc<RenderImage>>,
}

impl Viewer {
    /// Another picture, or none: fitted again, and the last one let go.
    pub fn reset(&mut self) {
        self.zoom = Zoom::Fit;
        self.pan = (0., 0.);
        self.pointer = None;
        self.decoding = None;
        if let Some(Shown {
            drawn: Drawn::Still(image),
            ..
        }) = self.shown.take()
        {
            self.stale.push(image);
        }
    }

    /// Frees what the window holds of pictures that were let go.
    pub fn release(&mut self, window: &mut gpui_kit::Window) {
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

/// Where the picture is, for one size of the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The room the picture has: the window less the toolbar and a margin.
    pub area: Bounds<Pixels>,
    /// Where the picture is drawn.
    pub rect: Bounds<Pixels>,
    /// Its scale against its natural pixels; `None` while its natural
    /// size is not known (the thumbnail stands in, fitted).
    pub scale: Option<f32>,
    /// The scale at which it is whole.
    pub fit: f32,
}

/// Decodes the original for the viewer. Blocking.
fn decode(bytes: Vec<u8>, mime: Option<String>, moving: bool) -> Option<((u32, u32), Drawn)> {
    if moving {
        let natural = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        return Some((natural, Drawn::Moving(image_of(bytes, mime.as_deref())?)));
    }
    let upright = client_core::upright(&bytes, MAX_SIDE).ok()?;
    let mut pixels = upright.image.into_rgba8();
    // The renderer takes BGRA.
    let raw: &mut [u8] = &mut pixels;
    for red in (0..raw.len().saturating_sub(3)).step_by(4) {
        raw.swap(red, red + 2);
    }
    let image = RenderImage::new(vec![image::Frame::new(pixels)]);
    Some((upright.natural, Drawn::Still(Arc::new(image))))
}

impl Shell {
    /// The room the picture has in a window of this size.
    pub(super) fn viewer_area(viewport: gpui_kit::Size<Pixels>) -> Bounds<Pixels> {
        let (side, top, bottom) = (px(32.), px(64.), px(24.));
        Bounds {
            origin: point(side, top),
            size: size(
                (viewport.width - side * 2.).max(px(1.)),
                (viewport.height - top - bottom).max(px(1.)),
            ),
        }
    }

    /// The decoded original of the picture in the viewer.
    pub(super) fn viewer_shown(&self) -> Option<&Shown> {
        let url = self.viewing.as_ref()?.source.as_ref()?.as_str();
        self.viewer.shown.as_ref().filter(|shown| shown.url == url)
    }

    /// The natural size of the picture in the viewer: the original's once
    /// it is decoded, what the message says until then.
    fn viewer_natural(&self) -> Option<(f32, f32)> {
        if let Some(shown) = self.viewer_shown() {
            return Some((shown.natural.0 as f32, shown.natural.1 as f32));
        }
        let media = self.viewing.as_ref()?;
        match (media.width, media.height) {
            (Some(width), Some(height)) if width > 0 && height > 0 => {
                Some((width as f32, height as f32))
            }
            _ => None,
        }
    }

    /// Where the picture is in a window of this size.
    pub(super) fn viewer_frame(&self, viewport: gpui_kit::Size<Pixels>) -> Frame {
        let area = Self::viewer_area(viewport);
        let room = (area.size.width.as_f32(), area.size.height.as_f32());
        let (natural, scale, fit) = match self.viewer_natural() {
            Some(natural) => {
                let fit = fit(natural, room);
                let scale = match self.viewer.zoom {
                    Zoom::Fit => fit,
                    Zoom::At(scale) => scale.clamp(least(fit), MOST),
                };
                (natural, Some(scale), fit)
            }
            None => (self.viewer_stand_in(), None, 1.),
        };
        // A thumbnail of unknown natural size fills the area, whole.
        let drawn = scale.unwrap_or_else(|| (room.0 / natural.0).min(room.1 / natural.1));
        let shown = (natural.0 * drawn, natural.1 * drawn);
        let pan = match scale {
            Some(_) => clamp_pan(self.viewer.pan, shown, room),
            None => (0., 0.),
        };
        let middle = area.center();
        Frame {
            area,
            rect: Bounds {
                origin: point(
                    middle.x + gpui_kit::px(pan.0 - shown.0 / 2.),
                    middle.y + gpui_kit::px(pan.1 - shown.1 / 2.),
                ),
                size: size(gpui_kit::px(shown.0), gpui_kit::px(shown.1)),
            },
            scale,
            fit,
        }
    }

    /// The proportions of the thumbnail, for a picture nothing else is
    /// known of yet.
    fn viewer_stand_in(&self) -> (f32, f32) {
        let fallback = (PLACEHOLDER().width.as_f32(), PLACEHOLDER().height.as_f32());
        let Some(url) = self
            .viewing
            .as_ref()
            .and_then(|media| media.source.as_ref())
        else {
            return fallback;
        };
        match self
            .engine
            .store()
            .media_size(&client_core::thumbnail_key(url.as_str()))
        {
            Ok(Some((width, height))) if width > 0 && height > 0 => (width as f32, height as f32),
            _ => fallback,
        }
    }

    /// What to draw in the viewer: the original, or the thumbnail until
    /// it is there.
    pub(super) fn viewer_image(&self) -> Option<gpui_kit::ImageSource> {
        if let Some(shown) = self.viewer_shown() {
            return Some(match &shown.drawn {
                Drawn::Still(image) => image.clone().into(),
                Drawn::Moving(image) => image.clone().into(),
            });
        }
        let (account, media) = (self.account.as_ref()?, self.viewing.as_ref()?);
        match self.media.visual(account, media) {
            MediaVisual::Image(image) => Some(image.into()),
            _ => None,
        }
    }

    /// Scales the picture to `to`, keeping what is under `anchor` (a
    /// point of the window; the middle of the area when `None`) in place.
    pub(super) fn zoom_viewer_to(
        &mut self,
        to: Zoom,
        anchor: Option<Point<Pixels>>,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let frame = self.viewer_frame(window.viewport_size());
        let Some(from) = frame.scale else {
            return;
        };
        match to {
            Zoom::Fit => {
                self.viewer.zoom = Zoom::Fit;
                self.viewer.pan = (0., 0.);
            }
            Zoom::At(to) => {
                let to = to.clamp(least(frame.fit), MOST);
                let middle = frame.area.center();
                let anchor = anchor
                    .map(|at| ((at.x - middle.x).as_f32(), (at.y - middle.y).as_f32()))
                    .unwrap_or((0., 0.));
                // From where the picture really is, not from a pan that
                // an earlier, larger scale left behind.
                let now = frame.rect.center();
                let pan = ((now.x - middle.x).as_f32(), (now.y - middle.y).as_f32());
                self.viewer.zoom = Zoom::At(to);
                self.viewer.pan = pan_around(pan, anchor, from, to);
            }
        }
        self.settle_viewer(window);
        cx.notify();
    }

    /// Keeps the pan within what the picture's size allows.
    fn settle_viewer(&mut self, window: &gpui_kit::Window) {
        let frame = self.viewer_frame(window.viewport_size());
        let middle = frame.area.center();
        let now = frame.rect.center();
        self.viewer.pan = ((now.x - middle.x).as_f32(), (now.y - middle.y).as_f32());
    }

    /// A step closer or further, around the middle; `None` fits.
    pub(super) fn zoom_viewer(
        &mut self,
        closer: Option<bool>,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let frame = self.viewer_frame(window.viewport_size());
        let to = match (closer, frame.scale) {
            (Some(closer), Some(scale)) => Zoom::At(step(scale, closer, frame.fit)),
            _ => Zoom::Fit,
        };
        self.zoom_viewer_to(to, None, window, cx);
    }

    /// The wheel: scales by `factor` around the pointer.
    pub(super) fn zoom_viewer_by(
        &mut self,
        factor: f32,
        at: Point<Pixels>,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let frame = self.viewer_frame(window.viewport_size());
        if let Some(scale) = frame.scale {
            self.zoom_viewer_to(Zoom::At(scale * factor), Some(at), window, cx);
        }
    }

    /// A double click: actual size around the click, or back to fit.
    pub(super) fn toggle_viewer_zoom(
        &mut self,
        at: Point<Pixels>,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        let frame = self.viewer_frame(window.viewport_size());
        let fitted = frame
            .scale
            .is_some_and(|scale| (scale - frame.fit).abs() < 0.0005);
        // A picture that fits at its natural size has nothing to toggle.
        if fitted && frame.fit < 1. {
            self.zoom_viewer_to(Zoom::At(1.), Some(at), window, cx);
        } else {
            self.zoom_viewer_to(Zoom::Fit, None, window, cx);
        }
    }

    /// Moves the picture with the pointer, as far as its size allows.
    pub(super) fn drag_viewer(
        &mut self,
        by: Point<Pixels>,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        self.settle_viewer(window);
        self.viewer.pan.0 += by.x.as_f32();
        self.viewer.pan.1 += by.y.as_f32();
        self.settle_viewer(window);
        cx.notify();
    }

    /// Decodes the original of the picture in the viewer, once it is
    /// here, off the interface's thread.
    pub(super) fn load_viewed(&mut self, cx: &mut Context<Self>) {
        let Some(media) = self.viewing.clone() else {
            return;
        };
        let Some(url) = media
            .source
            .as_ref()
            .map(|source| source.as_str().to_owned())
        else {
            return;
        };
        if self.viewer_shown().is_some() || self.viewer.decoding.as_deref() == Some(url.as_str()) {
            return;
        }
        let Some((bytes, mime)) = self.media.file(&url) else {
            return;
        };
        let moving = MediaShelf::may_move(&media) && !crate::settings::reduce_motion(cx);
        self.viewer.decoding = Some(url.clone());
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(async move { decode(bytes, mime, moving) })
                .await;
            this.update(cx, |this, cx| {
                if this.viewer.decoding.as_deref() != Some(url.as_str()) {
                    // Another picture is being looked at by now.
                    if let Some((_, Drawn::Still(image))) = made {
                        this.viewer.stale.push(image);
                    }
                    return;
                }
                this.viewer.decoding = None;
                if let Some(Shown {
                    drawn: Drawn::Still(image),
                    ..
                }) = this.viewer.shown.take()
                {
                    this.viewer.stale.push(image);
                }
                this.viewer.shown = made.map(|(natural, drawn)| Shown {
                    url,
                    natural,
                    drawn,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_fitted_whole_and_never_enlarged() {
        // The tall screenshot in a wide window: its height decides.
        let scale = fit((1080., 2340.), (1216., 712.));
        assert!((scale - 712. / 2340.).abs() < 1e-6);
        // Wide in a tall window: its width does.
        let scale = fit((2340., 1080.), (736., 1112.));
        assert!((scale - 736. / 2340.).abs() < 1e-6);
        // Smaller than the area: its own size.
        assert_eq!(fit((64., 64.), (1216., 712.)), 1.);
    }

    #[test]
    fn the_steps_go_from_the_least_to_the_most() {
        let fit = 0.304;
        // Out from fit: the stops below it, down to 10 %, and no further.
        assert_eq!(step(fit, false, fit), 0.25);
        assert_eq!(step(0.25, false, fit), 0.10);
        assert_eq!(step(0.10, false, fit), 0.10);
        // In from fit: the stops above it, up to 800 %.
        assert_eq!(step(fit, true, fit), 0.33);
        let mut scale = fit;
        for _ in 0..20 {
            scale = step(scale, true, fit);
        }
        assert_eq!(scale, MOST);
        // A picture fitted below 10 % goes back to its fit, no further.
        assert_eq!(least(0.04), 0.04);
        assert_eq!(step(0.10, false, 0.04), 0.04);
        assert_eq!(step(0.04, false, 0.04), 0.04);
        assert_eq!(step(0.04, true, 0.04), 0.10);
    }

    #[test]
    fn a_picture_is_moved_only_as_far_as_it_is_larger() {
        // Smaller than the area on both axes: it stays in the middle.
        assert_eq!(
            clamp_pan((300., -200.), (400., 300.), (800., 600.)),
            (0., 0.)
        );
        // Wider only: it moves sideways, until its edge meets the area's.
        assert_eq!(
            clamp_pan((900., 50.), (1000., 300.), (800., 600.)),
            (100., 0.)
        );
        assert_eq!(
            clamp_pan((-900., 0.), (1000., 300.), (800., 600.)),
            (-100., 0.)
        );
    }

    #[test]
    fn what_is_under_the_pointer_stays_there() {
        let (pan, anchor, from, to) = ((40., -10.), (120., 80.), 0.5, 2.);
        let after = pan_around(pan, anchor, from, to);
        // The point of the picture under the anchor, in its own pixels.
        let before = ((anchor.0 - pan.0) / from, (anchor.1 - pan.1) / from);
        let now = ((anchor.0 - after.0) / to, (anchor.1 - after.1) / to);
        assert!((before.0 - now.0).abs() < 1e-4 && (before.1 - now.1).abs() < 1e-4);
    }
}
