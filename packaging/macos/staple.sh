#!/usr/bin/env bash
# Staples the notarization ticket to a bundle or a disk image.
#
#   packaging/macos/staple.sh work/Wuapi.app
#
# Apple accepts a submission a little before its ticket can be fetched:
# stapled at once, the lookup sometimes fails ("The staple and validate
# action failed! Error 68", a dropped connection or a ticket not there
# yet). So it is tried a few times, further apart each time, and fails
# only when the last try does.
set -euo pipefail

target="${1:?usage: staple.sh <bundle or disk image>}"
tries=6
wait=15

for try in $(seq 1 "$tries"); do
  if xcrun stapler staple "$target"; then
    exit 0
  fi
  if [ "$try" -lt "$tries" ]; then
    echo "The ticket could not be stapled (try $try of $tries); again in ${wait}s." >&2
    sleep "$wait"
    wait=$((wait * 2))
  fi
done

echo "error: the ticket could not be stapled to $target after $tries tries." >&2
exit 1
