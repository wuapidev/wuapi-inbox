#!/bin/bash
# Takes the measurements of docs/media/measurements.md again: how long the
# application takes to start, to open a chat, to search and to draw a frame
# while scrolling, how much memory and processor it uses, and how long the
# local store takes over a history of 100,000 messages.
#
#   docs/media/measure.sh [OUT_DIR]     # default: target/measure
#
# It builds the release binary twice: as it ships (target/release), for
# memory, processor and size, and with the `capture` feature
# (target/capture-build), which adds the timing steps of
# crates/app/src/capture.rs. Everything runs on the demo data, with no
# keychain and a scratch data directory. Windows open and close on the
# screen while it runs; it takes about eight minutes after the builds.
# Needs python3; the memory figures need macOS (`footprint`).
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-$root/target/measure}
cargo build --release -p wuapi-inbox --manifest-path "$root/Cargo.toml"
cargo build --release -p wuapi-inbox --features capture \
  --manifest-path "$root/Cargo.toml" --target-dir "$root/target/capture-build"
cargo build --release -p client-core --example store_bench \
  --manifest-path "$root/Cargo.toml" --target-dir "$root/target/capture-build"
mkdir -p "$out"
exec python3 "$root/docs/media/measure.py" "$root" "$out"
