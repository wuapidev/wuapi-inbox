//! The wuapi brand assets bundled with the application: the logo, the
//! application icon and the two font families.
//!
//! Everything is compiled into the binary from `crates/app/assets`, whose
//! `ASSETS.md` records where each file comes from and its licence. Nothing
//! is read from disk or from the network at run time.

use std::borrow::Cow;
use std::sync::Arc;

/// The lime mark on a transparent ground, for dark surfaces.
pub const MARK: &str = "brand/mark-color.svg";
/// The lime mark on its rounded ink tile, for light surfaces.
pub const APP_ICON: &str = "brand/app-icon-rounded-color.svg";

const FILES: [(&str, &[u8]); 2] = [
    (MARK, include_bytes!("../assets/brand/mark-color.svg")),
    (
        APP_ICON,
        include_bytes!("../assets/brand/app-icon-rounded-color.svg"),
    ),
];

/// The bytes of a bundled brand asset.
pub fn load(path: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(name, _)| *name == path)
        .map(|(_, bytes)| *bytes)
}

/// The paths of the bundled brand assets.
pub fn paths() -> impl Iterator<Item = &'static str> {
    FILES.iter().map(|(name, _)| *name)
}

/// The font files: Inter (regular, italic, medium, semibold) and JetBrains
/// Mono (regular, medium).
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    [
        &include_bytes!("../assets/fonts/Inter-Regular.ttf")[..],
        include_bytes!("../assets/fonts/Inter-Italic.ttf"),
        include_bytes!("../assets/fonts/Inter-Medium.ttf"),
        include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        include_bytes!("../assets/fonts/JetBrainsMono-Medium.ttf"),
    ]
    .into_iter()
    .map(Cow::Borrowed)
    .collect()
}

/// The application icon for the window, where the platform takes one from
/// the application. Wayland takes it from the desktop entry instead
/// (`assets/linux`).
pub fn window_icon() -> Option<Arc<image::RgbaImage>> {
    let png = include_bytes!("../assets/icons/app-icon-256.png");
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()
        .map(|icon| Arc::new(icon.into_rgba8()))
}
