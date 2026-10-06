# The layout of the macOS disk image, for dmgbuild (https://dmgbuild.readthedocs.io).
#
# Not run by hand: `cargo xtask dmg --input <bundle> --out <dmg>` renders the
# background, and gives this file every number it uses (`-D name=value`), so
# that the window and the picture behind it are laid out from the same ones
# (crates/xtask/src/dmg.rs). dmgbuild writes the window's settings itself;
# Finder is never scripted.

import os.path

app = defines["app"]
name = os.path.basename(app)


def number(key):
    return int(defines[key])


format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}

# The mounted volume carries the application's icon.
icon = defines["icon"]

# One TIFF holding the picture at 1x and at 2x.
background = defines["background"]

default_view = "icon-view"
show_toolbar = False
show_status_bar = False
show_tab_view = False
show_pathbar = False
show_sidebar = False
show_icon_preview = False
include_icon_view_settings = True
include_list_view_settings = False

window_rect = (
    (number("window_x"), number("window_y")),
    (number("window_width"), number("window_height")),
)

# Where the icons are is said here, not arranged by Finder.
arrange_by = None
label_pos = "bottom"
icon_size = number("icon_size")
text_size = number("text_size")
icon_locations = {
    name: (number("app_x"), number("app_y")),
    "Applications": (number("applications_x"), number("applications_y")),
}
