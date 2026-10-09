//! The macOS disk image people download: the bundle, a way to the
//! Applications folder, and a window that says what to do with the two.
//!
//! The picture behind the icons is `crates/app/assets/brand/dmg-background.svg`,
//! rendered here at 1x and 2x so that no binary of it is kept in the
//! repository. The window is laid out by `dmgbuild` from
//! `packaging/macos/dmg-settings.py`, which gets its numbers from this
//! file: the icons are where the picture expects them because both are
//! told by the same constants. Finder is never scripted.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name of the mounted volume, and of its window, unless another is
/// asked for (a trial image next to a mounted release).
pub const VOLUME: &str = "Wuapi";

/// The picture, and so the inside of the window, in points.
pub const CONTENT: (u32, u32) = (660, 440);
/// What the window's title bar adds to its height. It is taller on the
/// newest systems (32): there the picture loses its last four points,
/// which hold nothing, where a larger number would show older systems a
/// strip of Finder's own white under it.
pub const TITLE_BAR: u32 = 28;
/// Where the window opens, from the top left of the screen.
pub const WINDOW_AT: (u32, u32) = (200, 120);
/// The icons' size, and the size of the names under them.
pub const ICON_SIZE: u32 = 128;
/// The size of the names.
pub const TEXT_SIZE: u32 = 13;
/// The middle of the application's icon: on the left, where reading starts.
pub const APP_AT: (u32, u32) = (180, 166);
/// The middle of the Applications folder's: on the right, where the arrow
/// of the picture points.
pub const APPLICATIONS_AT: (u32, u32) = (480, 166);

/// The picture's source.
pub const BACKGROUND: &str = include_str!("../../app/assets/brand/dmg-background.svg");
/// The window's settings, for `dmgbuild`.
pub const SETTINGS: &str = include_str!("../../../packaging/macos/dmg-settings.py");
/// The application's own typeface, for the picture's line of words: the
/// fonts of the machine that builds are never looked at.
const TYPEFACE: &[u8] = include_bytes!("../../app/assets/fonts/Inter-Medium.ttf");

/// What the settings file is told about the layout (`-D name=value`).
pub fn layout() -> Vec<(&'static str, u32)> {
    vec![
        ("window_x", WINDOW_AT.0),
        ("window_y", WINDOW_AT.1),
        ("window_width", CONTENT.0),
        ("window_height", CONTENT.1 + TITLE_BAR),
        ("icon_size", ICON_SIZE),
        ("text_size", TEXT_SIZE),
        ("app_x", APP_AT.0),
        ("app_y", APP_AT.1),
        ("applications_x", APPLICATIONS_AT.0),
        ("applications_y", APPLICATIONS_AT.1),
    ]
}

/// The picture at `scale` times its size (1 for ordinary screens, 2 for
/// Retina ones).
pub fn render(scale: u32) -> Result<resvg::tiny_skia::Pixmap, String> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_font_data(TYPEFACE.to_vec());
    let tree = resvg::usvg::Tree::from_str(BACKGROUND, &options)
        .map_err(|error| format!("the disk image's picture is not good SVG: {error}"))?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(CONTENT.0 * scale, CONTENT.1 * scale)
        .ok_or("the disk image's picture has no size")?;
    let scale = scale as f32;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Ok(pixmap)
}

/// Writes the picture into `dir` at 1x and 2x, under the names that say
/// which is which, and returns the two paths.
pub fn write_background(dir: &Path) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let (small, large) = (dir.join("background.png"), dir.join("background@2x.png"));
    for (scale, path) in [(1, &small), (2, &large)] {
        render(scale)?
            .save_png(path)
            .map_err(|error| format!("{}: {error}", path.display()))?;
    }
    Ok((small, large))
}

/// What is asked of a bundle before it is put in an image.
#[derive(Debug)]
struct Bundle {
    path: PathBuf,
    icon: PathBuf,
}

fn bundle(input: &Path) -> Result<Bundle, String> {
    let is_bundle = input
        .extension()
        .is_some_and(|extension| extension == "app")
        && input.join("Contents/MacOS").is_dir();
    if !is_bundle {
        return Err(format!("{} is not an application bundle", input.display()));
    }
    let resources = input.join("Contents/Resources");
    let mut icons: Vec<PathBuf> = std::fs::read_dir(&resources)
        .map_err(|error| format!("{}: {error}", resources.display()))?
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "icns")
        })
        .collect();
    icons.sort();
    let icon = icons
        .into_iter()
        .next()
        .ok_or(format!("{} has no icon", input.display()))?;
    Ok(Bundle {
        path: input.to_owned(),
        icon,
    })
}

/// The arguments `dmgbuild` is run with.
pub fn dmgbuild_arguments(
    settings: &Path,
    app: &Path,
    icon: &Path,
    background: &Path,
    volume: &str,
    out: &Path,
) -> Vec<String> {
    let mut arguments = vec!["-s".to_owned(), settings.display().to_string()];
    let paths = [("app", app), ("icon", icon), ("background", background)];
    let defines = paths
        .into_iter()
        .map(|(name, path)| format!("{name}={}", path.display()))
        .chain(
            layout()
                .into_iter()
                .map(|(name, value)| format!("{name}={value}")),
        );
    for define in defines {
        arguments.extend(["-D".to_owned(), define]);
    }
    arguments.extend([volume.to_owned(), out.display().to_string()]);
    arguments
}

/// Runs a tool to its end. A tool that is not there, or that fails, is an
/// error with what it said: an image is never made without its looks.
fn tool(program: &str, arguments: &[String]) -> Result<String, String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(|error| format!("`{program}` could not be run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "`{program}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a finished image must hold for its window to look as designed.
pub const HELD: [&str; 4] = [
    ".DS_Store",
    ".background.tiff",
    ".VolumeIcon.icns",
    "Applications",
];

/// Mounts the image out of sight and looks for everything its window
/// needs: `dmgbuild` saying nothing is not taken for a styled image.
fn check(image: &Path, app_name: &std::ffi::OsStr, scratch: &Path) -> Result<(), String> {
    let mount = scratch.join("mount");
    std::fs::create_dir_all(&mount).map_err(|error| error.to_string())?;
    let at = |path: &Path| path.display().to_string();
    tool(
        "hdiutil",
        &[
            "attach".into(),
            "-nobrowse".into(),
            "-readonly".into(),
            "-noautoopen".into(),
            "-mountpoint".into(),
            at(&mount),
            at(image),
        ],
    )?;
    let missing: Vec<String> = HELD
        .iter()
        .map(|name| mount.join(name))
        .chain([mount.join(app_name).join("Contents/MacOS")])
        .filter(|path| std::fs::symlink_metadata(path).is_err())
        .map(|path| at(path.strip_prefix(&mount).unwrap_or(&path)))
        .collect();
    tool("hdiutil", &["detach".into(), at(&mount)])?;
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "the disk image was built without {}",
            missing.join(", ")
        ))
    }
}

/// Builds the disk image of `input` (a `.app` bundle, as it was signed)
/// at `out`. `dmgbuild` is the program of that name unless another is
/// given.
pub fn build(
    input: &Path,
    out: &Path,
    volume: Option<&str>,
    dmgbuild: Option<&str>,
) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("a disk image is only built on macOS".into());
    }
    let bundle = bundle(input)?;
    let app_name = bundle
        .path
        .file_name()
        .ok_or("the bundle has no name")?
        .to_owned();
    let scratch = std::env::temp_dir().join(format!("wuapi-inbox-dmg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let made = (|| {
        let (small, large) = write_background(&scratch)?;
        let at = |path: &Path| path.display().to_string();
        // One file with both sizes: Finder takes the one the screen wants.
        let background = scratch.join("background.tiff");
        tool(
            "tiffutil",
            &[
                "-cathidpicheck".into(),
                at(&small),
                at(&large),
                "-out".into(),
                at(&background),
            ],
        )?;
        let settings = scratch.join("dmg-settings.py");
        std::fs::write(&settings, SETTINGS).map_err(|error| error.to_string())?;
        if let Some(dir) = out.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
        }
        if out.exists() {
            std::fs::remove_file(out).map_err(|error| error.to_string())?;
        }
        tool(
            dmgbuild.unwrap_or("dmgbuild"),
            &dmgbuild_arguments(
                &settings,
                &bundle.path,
                &bundle.icon,
                &background,
                volume.unwrap_or(VOLUME),
                out,
            ),
        )
        .map_err(|error| format!("{error}\n(dmgbuild is installed with `pip install dmgbuild`)"))?;
        check(out, &app_name, &scratch)
    })();
    let _ = std::fs::remove_dir_all(&scratch);
    if made.is_err() {
        // Half an image is not left where a release would pick it up.
        let _ = std::fs::remove_file(out);
    }
    made
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dark theme's colours the picture may use
    /// (`crates/app/src/theme.rs`): BLACK, PAPER, STONE_400, LIME, and on
    /// the tray INK and OLIVE.
    const THEME: [&str; 6] = [
        "#0A0A0A", "#FAFAF9", "#A8A29E", "#D4FF3F", "#0C0A09", "#4D7C0F",
    ];

    /// How far under an icon's middle Finder puts the baseline of its
    /// name, at these sizes: measured on a mounted image, not asked of
    /// Finder.
    const NAME_BASELINE: u32 = 86;
    /// The light tray of the picture that holds both icons and their
    /// names: left, top, width, height.
    const TRAY: (u32, u32, u32, u32) = (70, 70, 520, 230);

    /// Every `name="value"` of the picture's source.
    fn attributes(name: &str) -> Vec<&'static str> {
        let opening = format!("{name}=\"");
        BACKGROUND
            .split(opening.as_str())
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect()
    }

    fn pixel(pixmap: &resvg::tiny_skia::Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        let colour = pixmap.pixel(x, y).expect("inside the picture");
        (colour.red(), colour.green(), colour.blue())
    }

    #[test]
    fn the_window_and_the_picture_are_laid_out_from_the_same_numbers() {
        let (width, height) = CONTENT;
        assert!(BACKGROUND.contains(&format!("viewBox=\"0 0 {width} {height}\"")));
        // The application on the left, the folder on the right, level, and
        // as far from the middle as each other.
        assert_eq!(APP_AT.1, APPLICATIONS_AT.1);
        assert!(APP_AT.0 < APPLICATIONS_AT.0);
        assert_eq!(APP_AT.0 + APPLICATIONS_AT.0, width);
        // Both whole inside the window, names included.
        let half = ICON_SIZE / 2;
        assert!(APP_AT.0 > half && APPLICATIONS_AT.0 + half < width);
        assert!(APP_AT.1 > half && APP_AT.1 + half + 3 * TEXT_SIZE < height);
        // The picture's own comment says where the icons are.
        for (x, y) in [APP_AT, APPLICATIONS_AT] {
            assert!(BACKGROUND.contains(&format!("{x},{y}")), "{x},{y}");
        }
        let window: Vec<u32> = layout()
            .into_iter()
            .filter(|(name, _)| name.starts_with("window_"))
            .map(|(_, value)| value)
            .collect();
        assert_eq!(window, [200, 120, 660, 468]);
    }

    #[test]
    fn the_tray_holds_both_icons_and_their_names_with_room_around() {
        let (left, top, wide, tall) = TRAY;
        assert!(BACKGROUND.contains(&format!(
            "<rect x=\"{left}\" y=\"{top}\" width=\"{wide}\" height=\"{tall}\" rx=\"24\" fill=\"#FAFAF9\"/>"
        )));
        // In the middle of the window, and the icons in the middle of it.
        assert_eq!(2 * left + wide, CONTENT.0);
        let half = ICON_SIZE / 2;
        assert!(APP_AT.0 - half - left >= 40);
        assert!(left + wide - (APPLICATIONS_AT.0 + half) >= 40);
        assert!(APP_AT.1 - half - top >= 28);
        assert!(top + tall - (APP_AT.1 + NAME_BASELINE) >= 22);
        // Clear of the mark above it and of the words under it, and
        // nothing that matters in the last points of the window, which the
        // taller title bar of the newest systems cuts off.
        assert!(top >= 26 + 26 + 16);
        assert!(352 - 16 - (top + tall) >= 30);
        assert!(CONTENT.1 - 376 >= 8 + 40);
    }

    #[test]
    fn the_settings_are_told_everything_they_read() {
        let asked = |opening: &str| -> Vec<&str> {
            SETTINGS
                .split(opening)
                .skip(1)
                .filter_map(|rest| rest.split('"').next())
                .collect()
        };
        let told = dmgbuild_arguments(
            Path::new("settings.py"),
            Path::new("work/Wuapi.app"),
            Path::new("AppIcon.icns"),
            Path::new("background.tiff"),
            VOLUME,
            Path::new("dist/x.dmg"),
        );
        for name in asked("number(\"").into_iter().chain(asked("defines[\"")) {
            assert!(
                told.iter()
                    .any(|argument| argument.starts_with(&format!("{name}="))),
                "the settings read `{name}`, which nothing gives them"
            );
        }
        assert_eq!(told[..2], ["-s", "settings.py"]);
        assert!(told.contains(&"app=work/Wuapi.app".to_owned()));
        assert_eq!(told[told.len() - 2..], [VOLUME, "dist/x.dmg"]);
        assert_eq!(VOLUME, "Wuapi");
        assert!(BACKGROUND.contains(">Drag Wuapi to Applications<"));
        // No toolbar, no sidebar, nothing but the two icons; and Finder
        // is not asked to arrange them.
        for line in [
            "default_view = \"icon-view\"",
            "show_toolbar = False",
            "show_status_bar = False",
            "show_pathbar = False",
            "show_sidebar = False",
            "arrange_by = None",
            "symlinks = {\"Applications\": \"/Applications\"}",
        ] {
            assert!(SETTINGS.contains(line), "{line}");
        }
    }

    #[test]
    fn the_picture_is_in_the_products_own_colours_and_pattern() {
        for colour in attributes("fill")
            .into_iter()
            .chain(attributes("stroke"))
            .chain(attributes("stop-color"))
        {
            assert!(
                colour == "none" || colour.starts_with("url(") || THEME.contains(&colour),
                "{colour} is not a colour of the theme"
            );
        }
        assert_eq!(attributes("font-family"), ["Inter", "Inter"]);
        // The pattern is the wallpaper's tile, line for line.
        let wallpaper = include_str!("../../app/assets/brand/wallpaper-lines.svg");
        let drawn: Vec<&str> = wallpaper
            .split(" d=\"")
            .skip(1)
            .filter_map(|rest| rest.split('"').next())
            .collect();
        assert!(drawn.len() > 5);
        for path in drawn {
            assert!(BACKGROUND.contains(path), "the tile's `{path}` is missing");
        }
        // And the mark is the mark.
        let mark = include_str!("../../app/assets/brand/mark-color.svg");
        let outline = mark
            .split(" d=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next());
        assert!(BACKGROUND.contains(outline.expect("the mark has an outline")));
    }

    #[test]
    fn the_picture_is_rendered_sharp_for_both_kinds_of_screen() {
        const BLACK: (u8, u8, u8) = (0x0a, 0x0a, 0x0a);
        const PAPER: (u8, u8, u8) = (0xfa, 0xfa, 0xf9);
        const OLIVE: (u8, u8, u8) = (0x4d, 0x7c, 0x0f);
        for scale in [1, 2] {
            let pixmap = render(scale).unwrap();
            assert_eq!(
                (pixmap.width(), pixmap.height()),
                (CONTENT.0 * scale, CONTENT.1 * scale)
            );
            for (x, y) in [APP_AT, APPLICATIONS_AT] {
                // Where the icons go, around them, and under them where
                // Finder writes their names: the tray's paper alone.
                let half = ICON_SIZE / 2;
                let names = (y + half)..(y + half + 3 * TEXT_SIZE);
                for down in ((y - half)..(y + half)).chain(names) {
                    for across in (x - half - 8)..(x + half + 8) {
                        assert_eq!(
                            pixel(&pixmap, across * scale, down * scale),
                            PAPER,
                            "the tray is not plain at {across},{down}"
                        );
                    }
                }
            }
            // The arrow, between them and level with them.
            let middle = (CONTENT.0 / 2) * scale;
            assert_eq!(pixel(&pixmap, middle, APP_AT.1 * scale), OLIVE);
            // The words were drawn: without the typeface the row is empty.
            let lit = (0..pixmap.width())
                .filter(|x| {
                    let (red, ..) = pixel(&pixmap, *x, 347 * scale);
                    red > 0x80
                })
                .count();
            assert!(lit > 20, "no words in the picture at {scale}x");
            // And nothing of the pattern runs into them.
            for (x, y) in [(200, 334), (460, 334), (200, 384), (460, 384), (452, 340)] {
                assert_eq!(pixel(&pixmap, x * scale, y * scale), BLACK);
            }
            assert_eq!(pixel(&pixmap, 4 * scale, 400 * scale), BLACK);
        }
        let dir = tempfile::tempdir().unwrap();
        let (small, large) = write_background(dir.path()).unwrap();
        assert!(small.ends_with("background.png") && large.ends_with("background@2x.png"));
        assert!(
            std::fs::metadata(&large).unwrap().len() > std::fs::metadata(&small).unwrap().len()
        );
        // Twice the same: nothing of the machine is in it.
        let again = tempfile::tempdir().unwrap();
        let (same, _) = write_background(again.path()).unwrap();
        assert_eq!(std::fs::read(small).unwrap(), std::fs::read(same).unwrap());
    }

    #[test]
    fn only_a_bundle_with_an_icon_goes_into_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("wuapi-inbox");
        std::fs::write(&file, "x").unwrap();
        assert!(bundle(&file)
            .unwrap_err()
            .contains("not an application bundle"));
        let app = dir.path().join("Wuapi.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        assert!(bundle(&app).unwrap_err().contains("has no icon"));
        std::fs::write(app.join("Contents/Resources/AppIcon.icns"), "icns").unwrap();
        let found = bundle(&app).unwrap();
        assert_eq!(found.icon, app.join("Contents/Resources/AppIcon.icns"));
        assert_eq!(found.path, app);
    }
}
