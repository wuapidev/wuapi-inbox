//! Which emoji the fonts at hand can draw.
//!
//! The application bundles Inter and JetBrains Mono, which have no emoji.
//! An emoji in a text is drawn by whatever colour-emoji font the system's
//! font fallback finds (Noto Color Emoji on most Linux desktops, Apple
//! Color Emoji, Segoe UI Emoji), and that font is usually older than the
//! newest emoji data. A sequence it does not know comes out as a missing
//! glyph, or as its parts side by side (a person, a hand, a tone swatch).
//!
//! So every sequence is shaped once, off the UI thread, with the text
//! system that will draw it, and the picker offers the ones that came out
//! whole.
//!
//! What was found on Linux with GPUI's text system (cosmic-text) and Noto
//! Color Emoji, and what the rule below is made for:
//!
//! * a sequence the emoji font knows is one glyph, whatever its length
//!   (a family, a flag, a skin tone);
//! * a character no font has is glyph 0;
//! * a sequence the font does not know is the glyphs of its parts;
//! * an emoji whose first character the interface font or a symbol font
//!   has (the red heart, the warning sign, the arrows, the digits of the
//!   keycaps: the ones that are text unless followed by the variation
//!   selector U+FE0F) is drawn by that font, in one colour, with one more
//!   glyph for the selector and one for the keycap. GPUI keeps the font
//!   that has the first character and does not read the selector, in the
//!   picker as in a message. These are offered: they are drawn, if plainly.

use super::data::EmojiSet;

/// The glyphs a text was shaped to, as the font numbers them; 0 is the
/// "missing glyph" of every font.
pub type Glyphs = Vec<u32>;

/// Whether a sequence that shaped to these glyphs is drawn whole: no
/// missing glyph, and one glyph for the emoji, beside those a font that
/// draws it as text spends on its variation selectors and its keycap.
/// More means the font drew the parts; none means nothing was drawn.
pub fn renders(text: &str, glyphs: &[u32]) -> bool {
    let beside = text
        .chars()
        .filter(|ch| matches!(ch, '\u{FE0F}' | '\u{20E3}'))
        .count();
    !glyphs.is_empty() && glyphs.len() <= 1 + beside && !glyphs.contains(&0)
}

/// Which emoji of a set can be drawn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Support {
    /// Per emoji of the set.
    bases: Vec<bool>,
    /// Per emoji, per skin-tone variant.
    variants: Vec<Vec<bool>>,
    /// The fonts were asked. `false`: everything is assumed to draw.
    pub checked: bool,
}

impl Support {
    /// Everything draws: for when the fonts cannot be asked.
    pub fn all(set: &EmojiSet) -> Self {
        Self {
            bases: vec![true; set.emojis.len()],
            variants: set
                .emojis
                .iter()
                .map(|emoji| vec![true; emoji.variants.len()])
                .collect(),
            checked: false,
        }
    }

    /// Shapes every sequence of the set with `shape`.
    ///
    /// When fewer than half of the oldest, simplest emoji come out as one
    /// glyph, it is the way of asking that does not work here (no emoji
    /// font at all, or a text system that answers differently), not the
    /// emoji: nothing is hidden then.
    pub fn probe(set: &EmojiSet, shape: impl Fn(&str) -> Glyphs) -> Self {
        let sample: Vec<&str> = set
            .emojis
            .iter()
            .filter(|emoji| emoji.text.chars().count() == 1)
            .take(40)
            .map(|emoji| &*emoji.text)
            .collect();
        let drawn = sample
            .iter()
            .filter(|text| renders(text, &shape(text)))
            .count();
        if drawn * 2 < sample.len() {
            tracing::warn!(
                drawn,
                of = sample.len(),
                "the fonts could not be asked which emoji they draw; showing all"
            );
            return Self::all(set);
        }
        Self {
            bases: set
                .emojis
                .iter()
                .map(|emoji| renders(&emoji.text, &shape(&emoji.text)))
                .collect(),
            variants: set
                .emojis
                .iter()
                .map(|emoji| {
                    emoji
                        .variants
                        .iter()
                        .map(|variant| renders(&variant.text, &shape(&variant.text)))
                        .collect()
                })
                .collect(),
            checked: true,
        }
    }

    /// Whether the emoji at `index` can be drawn.
    pub fn base(&self, index: usize) -> bool {
        self.bases.get(index).copied().unwrap_or(false)
    }

    /// Whether its skin-tone variant at `variant` can.
    pub fn variant(&self, index: usize, variant: usize) -> bool {
        self.variants
            .get(index)
            .and_then(|variants| variants.get(variant))
            .copied()
            .unwrap_or(false)
    }

    /// How many emoji and variants cannot be drawn.
    pub fn hidden(&self) -> (usize, usize) {
        (
            self.bases.iter().filter(|drawn| !**drawn).count(),
            self.variants
                .iter()
                .flatten()
                .filter(|drawn| !**drawn)
                .count(),
        )
    }
}

/// A shaper over the application's text system, in the family the
/// interface is set in: what the picker and the messages are drawn with.
/// It can be used off the UI thread.
pub fn system_shaper(
    text_system: std::sync::Arc<gpui_kit::TextSystem>,
) -> impl Fn(&str) -> Glyphs + Send + 'static {
    // Its own layout cache, so nothing of this stays with a window.
    let layouts = gpui_kit::WindowTextSystem::new(text_system);
    let font = gpui_kit::font(crate::theme::fonts::SANS);
    move |text: &str| {
        let run = gpui_kit::TextRun {
            len: text.len(),
            font: font.clone(),
            color: gpui_kit::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        layouts
            .layout_line(text, gpui_kit::px(16.), &[run], None)
            .runs
            .iter()
            .flat_map(|run| run.glyphs.iter().map(|glyph| glyph.id.0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_glyph_is_an_emoji_and_its_parts_are_not() {
        assert!(renders("🔥", &[812]));
        assert!(renders("👨‍👩‍👧‍👦", &[2023]), "a family the font knows");
        assert!(!renders("\u{1FADD}", &[0]), "the missing glyph");
        assert!(
            !renders("👨‍👩‍👧‍👦", &[812, 4, 907, 4, 300, 4, 301]),
            "a sequence drawn as its parts"
        );
        assert!(!renders("\u{1FAF9}🏻", &[0, 487]), "a tone beside a box");
        assert!(!renders("🔥", &[]));
        // Drawn as text by a font that spends a glyph on the selector,
        // and another on the keycap.
        assert!(renders("❤️", &[1868, 1777]));
        assert!(renders("❤️", &[1868]));
        assert!(renders("0️⃣", &[839, 681, 335]));
        assert!(!renders("❤️", &[1868, 1777, 12]));
        assert!(!renders("🏳️‍🌈", &[40, 3, 4, 41]), "a flag and a rainbow");
    }

    #[test]
    fn what_a_font_cannot_draw_is_told_apart() {
        let set = EmojiSet::load();
        // A font like those in use today: it knows nothing newer than
        // Emoji 15, and draws two-tone handshakes as two hands.
        let old = |text: &str| -> Glyphs {
            let new = text
                .chars()
                .any(|ch| matches!(ch, '\u{1FAE9}' | '\u{1FADD}' | '\u{1FAEB}'));
            let parts = text.contains('\u{1FAF1}') && text.contains('\u{1FAF2}');
            if new {
                vec![0]
            } else if parts {
                vec![70, 3, 71]
            } else {
                vec![5]
            }
        };
        let support = Support::probe(&set, old);
        assert!(support.checked);
        let fire = set.find("🔥").unwrap();
        assert!(support.base(fire));
        let pickle = set.find("\u{1FADD}").unwrap();
        assert!(!support.base(pickle), "tofu is hidden");
        let (shake, mixed) = set.locate("🫱🏻‍🫲🏿").unwrap();
        assert!(support.base(shake));
        assert!(!support.variant(shake, mixed.unwrap()), "split in parts");
        let (_, same) = set.locate("🤝🏿").unwrap();
        assert!(support.variant(shake, same.unwrap()));
        let (bases, variants) = support.hidden();
        assert!(bases >= 2 && variants == 20, "{bases} {variants}");
        assert!(!support.base(usize::MAX));
    }

    #[test]
    fn a_text_system_that_cannot_be_asked_hides_nothing() {
        let set = EmojiSet::load();
        // No emoji font at all: everything is the missing glyph.
        let none = Support::probe(&set, |_| vec![0]);
        assert!(!none.checked);
        assert_eq!(none.hidden(), (0, 0));
        // One glyph per character, as the test platform shapes.
        let per_char = Support::probe(&set, |text| vec![0; text.chars().count()]);
        assert_eq!(per_char, Support::all(&set));
    }
}
