//! The icons the UI uses, embedded in the binary.
//!
//! Only the icons listed here are compiled in (from the Lucide set bundled
//! with the component library), on top of the few the component library
//! itself needs.

use crate::brand;
pub use gpui_kit::assets::IconName;
use gpui_kit::assets::{icon_assets, Assets as ComponentAssets};
use gpui_kit::component::Icon;
use gpui_kit::{AssetSource, Hsla, Pixels, Result, SharedString, Styled};
use std::borrow::Cow;

icon_assets!(
    AppIcons,
    [
        Archive,
        ArrowDownToLine,
        ArrowLeft,
        Ban,
        Bell,
        BellOff,
        Calendar,
        CalendarPlus,
        Camera,
        ChartNoAxesColumn,
        Check,
        CheckCheck,
        ChevronDown,
        ChevronLeft,
        ChevronRight,
        ChevronUp,
        Circle,
        CircleDotDashed,
        CircleAlert,
        CircleCheck,
        CircleX,
        Copy,
        ExternalLink,
        Clock3,
        EllipsisVertical,
        FileText,
        Forward,
        Image,
        Keyboard,
        Info,
        Link,
        ListFilter,
        Lock,
        LogOut,
        MapPin,
        MessageCircle,
        MessageSquareText,
        Mic,
        Moon,
        Pause,
        Pencil,
        Phone,
        Pin,
        Play,
        Plus,
        RotateCw,
        Search,
        SendHorizontal,
        Settings,
        FaceSlightlySmiling,
        Square,
        SquareCheck,
        SquarePen,
        Star,
        Sun,
        Timer,
        Trash,
        TriangleAlert,
        UserPlus,
        Users,
        Video,
        WifiOff,
        X,
        ZoomIn,
        ZoomOut,
        Maximize,
        Scan,
        Sticker,
        Film,
    ]
);

/// The application's asset source: the brand assets and its own icons
/// first, then the component library's defaults.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = brand::load(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        match AppIcons.load(path)? {
            Some(bytes) => Ok(Some(bytes)),
            None => ComponentAssets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = brand::paths()
            .filter(|name| name.starts_with(path))
            .map(Into::into)
            .collect();
        names.extend(AppIcons.list(path)?);
        names.extend(ComponentAssets.list(path)?);
        Ok(names)
    }
}

/// An icon of the given size and colour.
pub fn icon(name: IconName, size: Pixels, colour: Hsla) -> Icon {
    Icon::new(name).size(size).text_color(colour)
}
