#!/bin/bash
# Turns the frames of docs/media/capture.sh into the screenshots the README
# shows: WebP at the window's Retina size (2480 x 1600), untouched otherwise.
#
#   docs/media/process.sh [RAW_DIR]     # default: target/capture/raw
#
# Needs cwebp (libwebp).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
raw=${1:-$root/target/capture/raw}
for screen in chats search group media polls palette status settings welcome; do
  for theme in light dark; do
    cwebp -quiet -q 86 -m 6 -sharp_yuv "$raw/$screen-$theme.png" \
      -o "$root/docs/media/screenshot-$screen-$theme.webp"
  done
done
ls -l "$root"/docs/media/screenshot-*.webp
