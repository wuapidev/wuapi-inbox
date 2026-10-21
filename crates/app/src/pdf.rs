//! A PDF read inside the application: which files are one, how its pages
//! are laid out one under the other, and their pixels.
//!
//! The pages are drawn by `hayro`, a rasterizer in pure Rust: nothing of
//! the system's is asked for, so a PDF reads the same on Linux, macOS and
//! Windows. Drawing is slow work (tens of milliseconds a page, more for a
//! scanned one) and is always called off the interface's thread.
//!
//! What it does not do: a file that asks for a password is not opened,
//! and its text cannot be selected or searched. Both are said by the
//! reader, which offers the system's application for the file instead.

use crate::i18n;
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings};
use std::sync::Arc;

/// The widest a page is drawn, in device pixels: an A4 page is then 14 MB
/// of pixels, whatever the zoom and the display's scale.
pub const MAX_WIDTH: u32 = 1600;
/// The tallest a page is drawn. A page longer than this for its width (a
/// receipt, a chat exported as one page) is drawn narrower.
pub const MAX_HEIGHT: u32 = 6400;

/// Whether a file is a PDF, by what the message says of it: its type, or
/// the name it was sent under.
pub fn is_pdf(mime: Option<&str>, name: Option<&str>) -> bool {
    let by_type = mime.is_some_and(|mime| {
        let mime = mime.split(';').next().unwrap_or(mime).trim();
        mime.eq_ignore_ascii_case("application/pdf")
    });
    let by_name = name.is_some_and(|name| {
        name.rsplit_once('.')
            .is_some_and(|(_, ending)| ending.eq_ignore_ascii_case("pdf"))
    });
    by_type || by_name
}

/// A PDF that was opened.
#[derive(Clone)]
pub struct Document {
    pdf: Arc<Pdf>,
    /// The size of each page in its own units (points), as it is read:
    /// turned the way the page says.
    pub pages: Arc<[(f32, f32)]>,
}

/// Opens a PDF. Blocking, though only the file's table is read here: the
/// pages are read when they are drawn.
pub fn open(bytes: Vec<u8>) -> Result<Document, String> {
    use hayro::hayro_syntax::LoadPdfError;
    let pdf = Pdf::new(bytes).map_err(|error| match error {
        LoadPdfError::Decryption(_) => i18n::t("common.pdf.password").to_owned(),
        LoadPdfError::Invalid => i18n::t("common.pdf.invalid").to_owned(),
    })?;
    let pages: Arc<[(f32, f32)]> = pdf
        .pages()
        .iter()
        .map(|page| page.render_dimensions())
        .collect();
    if pages.is_empty() {
        return Err(i18n::t("common.pdf.no_pages").to_owned());
    }
    Ok(Document {
        pdf: Arc::new(pdf),
        pages,
    })
}

/// The pixels of a page: BGRA, as the renderer of the window takes them.
pub struct Drawn {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// The size a page of `natural` points is drawn at to be `width` device
/// pixels wide, within [`MAX_WIDTH`] and [`MAX_HEIGHT`].
pub fn drawn_size(natural: (f32, f32), width: u32) -> (u32, u32) {
    let (wide, tall) = (natural.0.max(1.), natural.1.max(1.));
    let mut width = (width.clamp(1, MAX_WIDTH)) as f32;
    if width * tall / wide > MAX_HEIGHT as f32 {
        width = MAX_HEIGHT as f32 * wide / tall;
    }
    let width = width.round().max(1.);
    (width as u32, (width * tall / wide).round().max(1.) as u32)
}

impl Document {
    /// Draws the page at `index` on white, `width` device pixels wide (or
    /// as wide as the limits allow). Blocking, and slow.
    pub fn draw(&self, index: usize, width: u32) -> Option<Drawn> {
        let page = self.pdf.pages().get(index)?;
        let natural = page.render_dimensions();
        let (width, _) = drawn_size(natural, width);
        let scale = width as f32 / natural.0.max(1.);
        let pixmap = hayro::render(
            page,
            &RenderCache::new(),
            &InterpreterSettings::default(),
            &RenderSettings::default(),
            &PixmapSettings {
                x_scale: scale,
                y_scale: scale,
                bg_color: WHITE,
            },
        );
        let (width, height) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        if width == 0 || height == 0 {
            return None;
        }
        // Drawn on white, so nothing is see-through and the colours are
        // as they are; only their order changes.
        let mut bgra = pixmap.data_as_u8_slice().to_vec();
        for red in (0..bgra.len().saturating_sub(3)).step_by(4) {
            bgra.swap(red, red + 2);
        }
        Some(Drawn {
            width,
            height,
            bgra,
        })
    }
}

/// Where each page stands when they are all `width` wide, one under the
/// other with `gap` between them: its top and its height.
pub fn layout(pages: &[(f32, f32)], width: f32, gap: f32) -> Vec<(f32, f32)> {
    let mut top = 0.;
    pages
        .iter()
        .map(|(wide, tall)| {
            let height = (width * tall.max(1.) / wide.max(1.)).max(1.);
            let placed = (top, height);
            top += height + gap;
            placed
        })
        .collect()
}

/// The pages that show between `from` and `from + room` of the column
/// (first and last, both included). `None` when there is no page.
pub fn visible(layout: &[(f32, f32)], from: f32, room: f32) -> Option<(usize, usize)> {
    if layout.is_empty() {
        return None;
    }
    let last = layout.len() - 1;
    let first = layout
        .iter()
        .position(|(top, height)| top + height > from)
        .unwrap_or(last);
    let end = layout
        .iter()
        .rposition(|(top, _)| *top < from + room)
        .unwrap_or(0)
        .max(first);
    Some((first, end.min(last)))
}

/// The page being read when the column is scrolled to `from`: the one
/// that holds the line a third of the way down the room.
pub fn current(layout: &[(f32, f32)], from: f32, room: f32) -> usize {
    let line = from + room / 3.;
    layout
        .iter()
        .rposition(|(top, _)| *top <= line)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A PDF made on the spot: `pages` blank pages of `size` points, the
    /// first with a filled square in its lower left corner.
    pub(crate) fn sample(pages: usize, size: (u32, u32)) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let kids: Vec<String> = (0..pages)
            .map(|page| format!("{} 0 R", 3 + page * 2))
            .collect();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {pages} >>",
            kids.join(" ")
        ));
        for page in 0..pages {
            let content = if page == 0 {
                "0 0 0 rg 0 0 20 20 re f"
            } else {
                ""
            };
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R >>",
                size.0,
                size.1,
                4 + page * 2
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn a_pdf_is_told_by_its_type_or_its_name() {
        assert!(is_pdf(Some("application/pdf"), None));
        assert!(is_pdf(Some("Application/PDF; qs=0.9"), None));
        assert!(is_pdf(None, Some("Contract 2026.PDF")));
        assert!(is_pdf(Some("application/octet-stream"), Some("scan.pdf")));
        assert!(!is_pdf(Some("image/png"), Some("pdf")));
        assert!(!is_pdf(None, Some("notes.pdf.txt")));
        assert!(!is_pdf(None, None));
    }

    #[test]
    fn its_pages_are_counted_and_drawn_on_white() {
        let document = open(sample(3, (200, 400))).expect("a PDF");
        assert_eq!(&*document.pages, &[(200., 400.); 3]);
        let drawn = document.draw(0, 100).expect("the first page");
        assert_eq!((drawn.width, drawn.height), (100, 200));
        assert_eq!(drawn.bgra.len(), 100 * 200 * 4);
        // White at the top, and the square in the lower left corner.
        assert_eq!(&drawn.bgra[..4], &[255, 255, 255, 255]);
        let at = |x: usize, y: usize| &drawn.bgra[(y * 100 + x) * 4..][..4];
        assert_eq!(at(4, 195), &[0, 0, 0, 255]);
        assert_eq!(at(50, 195), &[255, 255, 255, 255]);
        // A page that is not there is not drawn.
        assert!(document.draw(3, 100).is_none());
    }

    #[test]
    fn what_is_not_a_pdf_is_said_in_words() {
        assert_eq!(
            open(b"not a pdf at all".to_vec()).err().as_deref(),
            Some("This file could not be read as a PDF.")
        );
    }

    #[test]
    fn a_page_is_never_drawn_past_the_limits() {
        assert_eq!(drawn_size((595., 842.), 920), (920, 1302));
        assert_eq!(drawn_size((595., 842.), 9000).0, MAX_WIDTH);
        // A strip a hundred times as tall as it is wide: its height
        // decides.
        let (width, height) = drawn_size((100., 10_000.), 1000);
        assert_eq!((width, height), (64, MAX_HEIGHT));
    }

    #[test]
    fn the_pages_stand_one_under_the_other() {
        let placed = layout(&[(100., 200.), (100., 100.), (200., 100.)], 50., 10.);
        assert_eq!(placed, [(0., 100.), (110., 50.), (170., 25.)]);
        // From the top, a room of 120 shows the first two.
        assert_eq!(visible(&placed, 0., 120.), Some((0, 1)));
        // In the gap after the first: the second on.
        assert_eq!(visible(&placed, 105., 30.), Some((1, 1)));
        assert_eq!(visible(&placed, 105., 80.), Some((1, 2)));
        // Past the end: the last.
        assert_eq!(visible(&placed, 500., 80.), Some((2, 2)));
        assert_eq!(visible(&[], 0., 80.), None);
        // The page being read is the one a third of the way down.
        assert_eq!(current(&placed, 0., 120.), 0);
        assert_eq!(current(&placed, 80., 120.), 1);
        assert_eq!(current(&placed, 190., 120.), 2);
    }
}
