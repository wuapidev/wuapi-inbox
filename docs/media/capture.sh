#!/bin/bash
# Takes the screenshots of docs/media again: the application draws its own
# window into PNG files (crates/app/src/capture.rs), on the demo data, with
# no keychain and a scratch data directory. Nothing else on the screen can
# be in a frame, and no screen-recording permission is needed.
#
#   docs/media/capture.sh [OUT_DIR]     # default: target/capture/raw
#
# Then docs/media/process.sh turns the frames into the files of docs/media.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/capture/raw}
scripts=$root/docs/media/capture
cargo build --release -p wuapi-inbox --features capture --manifest-path "$root/Cargo.toml"
scratch=$root/target/capture/tmp
mkdir -p "$out" "$scratch"
for theme in light dark; do
  for script in screens welcome; do
    data=$(mktemp -d "$scratch/XXXXXX")
    flags=(--provider mock --no-keychain --data-dir "$data" --theme "$theme" --app-id dev.wuapi.inbox.capture)
    if [ "$script" = welcome ]; then flags+=(--welcome); else flags+=(--open-chat 1); fi
    frames=$(mktemp -d "$scratch/XXXXXX")
    WUAPI_INBOX_CAPTURE_SCRIPT=$scripts/$script.txt WUAPI_INBOX_CAPTURE_DIR=$frames \
      "$root/target/release/wuapi-inbox" "${flags[@]}" 2>&1 | grep '^capture' || true
    for frame in "$frames"/*.png; do
      mv "$frame" "$out/$(basename "$frame" .png)-$theme.png"
    done
    rm -rf "$data" "$frames"
  done
done
ls -l "$out"
