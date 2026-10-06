#!/bin/sh
# wuapi Inbox on Linux: installs the app under your home, with no root, and
# keeps the signed self-updater working. The .deb and .rpm install under /usr
# instead, where the system's package manager owns the files and the app only
# tells you a new version is there.
#
#   curl -fsSL https://github.com/wuapidev/wuapi-inbox/releases/latest/download/install.sh | sh
#   sh install.sh --version 0.1.1     # a version instead of the latest
#   sh install.sh --uninstall
#
# Environment:
#   WUAPI_INBOX_VERSION   the same as --version
#   WUAPI_INBOX_BASE_URL  where the release's files are; the default is the
#                         GitHub release, and the same layout works on any
#                         static host (the manifest names the file)
#   WUAPI_INBOX_BIN_DIR   where the `wuapi-inbox` command goes
#                         (default: ~/.local/bin)
#
# The build is x86_64 Linux with glibc 2.35 or later; this says so before
# downloading anything. The download is checked against the SHA-256 in the
# release's manifest; the app's own updates go further, with the updater
# verifying the manifest's Ed25519 signature against a key in the binary.

set -eu

REPO="wuapidev/wuapi-inbox"
DEFAULT_BASE_URL="https://github.com/$REPO/releases/latest/download"
PINNED_BASE_URL="https://github.com/$REPO/releases/download"

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: %s\n' "$*" >&2; }
die() { warn "$*"; exit 1; }

usage() {
  cat <<'USAGE'
wuapi Inbox on Linux: install it under your home, or uninstall it.

  sh install.sh [--version <v>] [--base-url <url>] [--bin-dir <dir>]
  sh install.sh --uninstall

  --version <v>      install v<v> instead of the latest release
  --base-url <url>   where the release's files are (default: the GitHub release)
  --bin-dir <dir>    where the `wuapi-inbox` command goes (default: ~/.local/bin)
  --uninstall        remove what this script installed
  -h, --help         this text

Chats and settings live in ~/.local/share/wuapi-inbox; --uninstall leaves
them alone.
USAGE
}

version="${WUAPI_INBOX_VERSION:-}"
base_url="${WUAPI_INBOX_BASE_URL:-}"
bin_dir="${WUAPI_INBOX_BIN_DIR:-$HOME/.local/bin}"
uninstall=false

while [ $# -gt 0 ]; do
  case "$1" in
    --version) [ $# -ge 2 ] || die "--version needs a value"; version="$2"; shift 2 ;;
    --base-url) [ $# -ge 2 ] || die "--base-url needs a value"; base_url="$2"; shift 2 ;;
    --bin-dir) [ $# -ge 2 ] || die "--bin-dir needs a value"; bin_dir="$2"; shift 2 ;;
    --uninstall) uninstall=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1 (--help lists them)" ;;
  esac
done

data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
binary="$bin_dir/wuapi-inbox"
desktop_file="$data_home/applications/dev.wuapi.inbox.desktop"
icon_file="$data_home/icons/hicolor/256x256/apps/dev.wuapi.inbox.png"
license_file="$data_home/doc/wuapi-inbox/LICENSE"

if [ "$uninstall" = "true" ]; then
  found=false
  for file in "$binary" "$desktop_file" "$icon_file" "$license_file"; do
    if [ -e "$file" ]; then
      rm -f "$file"
      say "removed $file"
      found=true
    fi
  done
  rmdir "$(dirname "$license_file")" 2>/dev/null || true
  if [ "$found" = "false" ]; then
    say "nothing to remove"
  fi
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$data_home/applications" >/dev/null 2>&1 || true
  fi
  say "your chats and settings stay in $data_home/wuapi-inbox (delete it to remove them)"
  exit 0
fi

# The release is one binary. Say what it needs plainly, instead of letting the
# loader say it later.
[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux"
arch="$(uname -m)"
[ "$arch" = "x86_64" ] || die "there is no build for $arch yet (x86_64 only); see https://github.com/$REPO/releases"
if [ -e /lib/ld-musl-x86_64.so.1 ] || [ -e /lib/ld-musl-aarch64.so.1 ]; then
  die "this looks like a musl system (Alpine?): wuapi Inbox needs glibc"
fi
libc="$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}' || true)"
if [ -n "$libc" ] && ! awk -v have="$libc" 'BEGIN {
  split(have, parts, ".")
  exit !(parts[1] + 0 > 2 || (parts[1] + 0 == 2 && parts[2] + 0 >= 35))
}'; then
  die "glibc $libc is older than 2.35 and the build will not start here (Debian 12, Ubuntu 22.04, Fedora 35 and RHEL 9 have it)"
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/wuapi-inbox.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

download() { # <url> <file>
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1" -o "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$2" "$1"
  else
    die "curl or wget is needed"
  fi
}

if [ -n "$version" ]; then
  base="${base_url:-$PINNED_BASE_URL/v$version}"
else
  base="${base_url:-$DEFAULT_BASE_URL}"
fi

# The manifest names this platform's file and its SHA-256 (docs/RELEASING.md).
# A pre-release carries beta.json instead.
manifest=""
for name in latest.json beta.json; do
  if download "$base/$name" "$tmp/$name" 2>/dev/null; then
    manifest="$tmp/$name"
    break
  fi
done
[ -n "$manifest" ] || die "no manifest at $base (is there a release?)"

format="$(sed -n 's/.*"format"[[:space:]]*:[[:space:]]*\([0-9][0-9]*\).*/\1/p' "$manifest" | head -n 1)"
[ "$format" = "1" ] || die "the manifest is not the format this installer knows"

block="$(sed -n '/"linux-x86_64"/,/}/p' "$manifest")"
[ -n "$block" ] || die "the manifest has no linux-x86_64 build"
artifact="$(printf '%s\n' "$block" | sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
sha="$(printf '%s\n' "$block" | sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
[ -n "$artifact" ] || die "the manifest names no file for linux-x86_64"
[ -n "$sha" ] || die "the manifest has no SHA-256 for linux-x86_64"
asset="$(basename "$artifact")"

case "$artifact" in
  http://*|https://*) url="$artifact" ;;
  *) url="$base/$artifact" ;;
esac

say "downloading $asset"
download "$url" "$tmp/$asset" || die "could not download $url"

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
elif command -v openssl >/dev/null 2>&1; then
  actual="$(openssl dgst -sha256 "$tmp/$asset" | awk '{print $NF}')"
else
  die "no sha256sum, shasum or openssl to check the download with"
fi
[ "$(printf '%s' "$actual" | tr 'A-Z' 'a-z')" = "$(printf '%s' "$sha" | tr 'A-Z' 'a-z')" ] ||
  die "the SHA-256 does not match the manifest: the download is not the release's file"

mkdir "$tmp/unpack"
tar -xzf "$tmp/$asset" -C "$tmp/unpack" || die "could not unpack $asset"
source_bin="$(find "$tmp/unpack" -type f -name wuapi-inbox | head -n 1)"
source_desktop="$(find "$tmp/unpack" -type f -name '*.desktop' | head -n 1)"
source_icon="$(find "$tmp/unpack" -type f -name '*.png' | head -n 1)"
source_license="$(find "$tmp/unpack" -type f -name LICENSE | head -n 1)"
[ -n "$source_bin" ] || die "the archive has no wuapi-inbox"
[ -n "$source_desktop" ] || die "the archive has no .desktop entry"
[ -n "$source_icon" ] || die "the archive has no icon"
[ -n "$source_license" ] || die "the archive has no LICENSE"

missing="$(ldd "$source_bin" 2>/dev/null | awk '/not found/ {print $1}' | sort -u | tr '\n' ' ' || true)"
if [ -n "$missing" ]; then
  hint=""
  if command -v apt-get >/dev/null 2>&1; then
    hint="sudo apt-get install libasound2 libxkbcommon0 libxkbcommon-x11-0 libxcb1"
  elif command -v dnf >/dev/null 2>&1; then
    hint="sudo dnf install alsa-lib libxkbcommon libxkbcommon-x11 libxcb"
  elif command -v pacman >/dev/null 2>&1; then
    hint="sudo pacman -S alsa-lib libxkbcommon libxcb"
  elif command -v zypper >/dev/null 2>&1; then
    hint="sudo zypper install libasound2 libxkbcommon0 libxkbcommon-x11-0 libxcb1"
  fi
  warn "missing libraries: $missing"
  if [ -n "$hint" ]; then
    warn "install them with: $hint"
  fi
fi

mkdir -p "$bin_dir" "$(dirname "$desktop_file")" "$(dirname "$icon_file")" "$(dirname "$license_file")"
cp "$source_bin" "$binary"
chmod 755 "$binary"
cp "$source_icon" "$icon_file"
cp "$source_license" "$license_file"
# The menu's PATH does not always have ~/.local/bin: point the entry at the file.
sed "s|^Exec=.*|Exec=\"$binary\"|" "$source_desktop" >"$desktop_file"

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$(dirname "$desktop_file")" >/dev/null 2>&1 || true
fi

say "wuapi Inbox is installed:"
say "  $binary"
say "open it from your applications menu, or run it from a terminal"
case ":${PATH}:" in
  *":$bin_dir:"*) ;;
  *)
    say ""
    say "$bin_dir is not in your PATH; to run the command in a terminal:"
    say "  export PATH=\"$bin_dir:\$PATH\""
    ;;
esac
say ""
say "It updates itself from the signed manifest when a new version is out."
