#!/bin/bash
# Records the clips of the README again: the application saves its own
# frames, twenty a second, while a script drives it on the demo data
# (crates/app/src/capture.rs), with no keychain and a scratch data
# directory. The clips play at the speed they were recorded.
#
#   docs/media/clips.sh [CLIP ...]     # default: start chats search palette
#
# Writes docs/media/clip-<name>.mp4 and clip-<name>.webp (the preview the
# README shows), and a sheet of frames to look at in target/clips. Look at
# it: the demo world plays live messages, so a fixed click can land
# differently from one run to the next. Needs python3, ffmpeg with libx264,
# img2webp (libwebp) and, for the timer of the start clip, swift on macOS.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cargo build --release -p wuapi-inbox --features capture \
  --manifest-path "$root/Cargo.toml" --target-dir "$root/target/capture-build"
exec python3 "$root/docs/media/clips.py" "$root" "$@"
