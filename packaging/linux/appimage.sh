#!/bin/sh
# Builds the AppImage of wuapi Inbox from the Linux archive a release leaves
# in dist/ (docs/RELEASING.md), with appimagetool:
# https://github.com/AppImage/appimagetool
#
#   APPIMAGETOOL=/path/to/appimagetool-x86_64.AppImage \
#     sh packaging/linux/appimage.sh [--version <v>] [--dist <dir>] [--out <dir>]
#
# The archive wuapi-inbox-<version>-linux-x86_64.tar.gz must be in dist/ (the
# release workflow's Linux job puts it there). The AppImage holds the same
# files as the archive, in an AppDir: the binary, the desktop entry and the
# icon. It is named after the archive, with .AppImage in place of .tar.gz,
# which is the name the website links to (lib/desktop.ts in wuapi).
#
# An AppImage is not replaced by the self-updater: the application sees
# $APPIMAGE and says a new version is there, as it does for the .deb and
# .rpm. The tarball stays the updater's archive.

set -eu

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"

version=""
dist="$root/dist"
out="$root/dist"

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || { echo "--version needs a value" >&2; exit 2; }; version="$2"; shift 2 ;;
    --dist) [ $# -ge 2 ] || { echo "--dist needs a value" >&2; exit 2; }; dist="$2"; shift 2 ;;
    --out) [ $# -ge 2 ] || { echo "--out needs a value" >&2; exit 2; }; out="$2"; shift 2 ;;
    -h|--help)
      sed -n '2,16p' "$0"
      exit 0
      ;;
    *) echo "unknown option: $1 (--help)" >&2; exit 2 ;;
  esac
done

if [ -z "$version" ]; then
  version="$(sed -n '/^\[workspace.package\]/,/^\[/p' "$root/Cargo.toml" | sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
fi
[ -n "$version" ] || { echo "no version in $root/Cargo.toml; pass --version" >&2; exit 2; }

tool="${APPIMAGETOOL:-}"
if [ -z "$tool" ]; then
  if command -v appimagetool >/dev/null 2>&1; then
    tool="appimagetool"
  else
    echo "appimagetool is needed: set APPIMAGETOOL to its path (https://github.com/AppImage/appimagetool/releases)" >&2
    exit 2
  fi
fi

archive="$dist/wuapi-inbox-$version-linux-x86_64.tar.gz"
[ -f "$archive" ] || { echo "$archive is not there (the Linux job's dist/ goes to the publish job; docs/RELEASING.md)" >&2; exit 2; }

work="$(mktemp -d "${TMPDIR:-/tmp}/wuapi-inbox-appimage.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
tar -xzf "$archive" -C "$work"
linux_dir="$work/wuapi-inbox-$version-linux-x86_64"
for file in wuapi-inbox dev.wuapi.inbox.desktop dev.wuapi.inbox.png LICENSE; do
  [ -f "$linux_dir/$file" ] || { echo "$archive has no $file" >&2; exit 2; }
done

# appimagetool reads the AppDir: AppRun starts the binary, the desktop entry
# and the icon sit at its root, and .DirIcon is the icon file managers show.
appdir="$work/AppDir"
mkdir -p "$appdir/usr/bin" "$appdir/usr/share/doc/wuapi-inbox"
cp "$linux_dir/wuapi-inbox" "$appdir/usr/bin/wuapi-inbox"
cp "$linux_dir/dev.wuapi.inbox.desktop" "$appdir/dev.wuapi.inbox.desktop"
cp "$linux_dir/dev.wuapi.inbox.png" "$appdir/dev.wuapi.inbox.png"
cp "$linux_dir/LICENSE" "$appdir/usr/share/doc/wuapi-inbox/LICENSE"
ln -s dev.wuapi.inbox.png "$appdir/.DirIcon"
cat >"$appdir/AppRun" <<'APPRUN'
#!/bin/sh
here="$(dirname -- "$(readlink -f -- "$0")")"
exec "$here/usr/bin/wuapi-inbox" "$@"
APPRUN
chmod 755 "$appdir/AppRun" "$appdir/usr/bin/wuapi-inbox"

mkdir -p "$out"
target="$out/wuapi-inbox-$version-linux-x86_64.AppImage"
# The tool itself is an AppImage: without FUSE it unpacks itself first.
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$tool" "$appdir" "$target"
chmod 755 "$target"
echo "$target"
