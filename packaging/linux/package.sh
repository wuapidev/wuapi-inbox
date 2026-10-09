#!/bin/sh
# Builds the .deb and .rpm of Wuapi from the Linux archive a release
# leaves in dist/ (docs/RELEASING.md), with nFPM:
# https://nfpm.goreleaser.com
#
#   bash packaging/linux/package.sh [--version <v>] [--dist <dir>] [--out <dir>]
#
# The archive wuapi-inbox-<version>-linux-x86_64.tar.gz must be in dist/
# (the release workflow's Linux job puts it there; `cargo xtask package` makes
# one locally). nFPM is needed in PATH:
#   go install github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.47.0
#
# The packages install under /usr, where the application does not replace
# itself: it tells you a new version is there. packaging/linux/install.sh
# installs under $HOME instead, where the self-updater keeps working.

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
      sed -n '2,15p' "$0"
      exit 0
      ;;
    *) echo "unknown option: $1 (--help)" >&2; exit 2 ;;
  esac
done

if [ -z "$version" ]; then
  version="$(sed -n '/^\[workspace.package\]/,/^\[/p' "$root/Cargo.toml" | sed -n 's/^version[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
fi
[ -n "$version" ] || { echo "no version in $root/Cargo.toml; pass --version" >&2; exit 2; }

nfpm=""
if command -v nfpm >/dev/null 2>&1; then
  nfpm="nfpm"
elif [ -x "$(go env GOPATH 2>/dev/null)/bin/nfpm" ]; then
  nfpm="$(go env GOPATH)/bin/nfpm"
else
  echo "nfpm is needed in PATH: go install github.com/goreleaser/nfpm/v2/cmd/nfpm@v2.47.0" >&2
  exit 2
fi

archive="$dist/wuapi-inbox-$version-linux-x86_64.tar.gz"
[ -f "$archive" ] || { echo "$archive is not there (the Linux job's dist/ goes to the publish job; docs/RELEASING.md)" >&2; exit 2; }

work="$(mktemp -d "${TMPDIR:-/tmp}/wuapi-inbox-packages.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM
tar -xzf "$archive" -C "$work"
linux_dir="$work/wuapi-inbox-$version-linux-x86_64"
for file in wuapi-inbox dev.wuapi.inbox.desktop dev.wuapi.inbox.png LICENSE; do
  [ -f "$linux_dir/$file" ] || { echo "$archive has no $file" >&2; exit 2; }
done

# nFPM does not expand variables in its config, so this fills the copy it reads.
sed -e "s|\${VERSION}|$version|g" -e "s|\${LINUX_DIR}|$linux_dir|g" \
  "$root/packaging/linux/nfpm.yaml" >"$work/nfpm.yaml"

mkdir -p "$out"
for packager in deb rpm; do
  "$nfpm" package --config "$work/nfpm.yaml" --packager "$packager" --target "$out/"
done

for package in "$out"/wuapi-inbox_"$version"_amd64.deb "$out"/wuapi-inbox-"$version"-*.x86_64.rpm; do
  [ -f "$package" ] && echo "$package"
done
