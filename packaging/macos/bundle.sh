#!/usr/bin/env bash
# Builds the macOS application bundle around a release binary.
#
#   packaging/macos/bundle.sh <binary> <version> <output directory>
#
# Writes "<output directory>/wuapi Inbox.app". The bundle is signed ad hoc
# here (a seal with no identity, which is what an Apple Silicon Mac needs to
# run it at all); the release workflow signs it again with the Developer ID
# when the certificate is there.
set -euo pipefail

binary="$1"
version="$2"
out="$3"
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
icons="$root/crates/app/assets/icons"

app="$out/wuapi Inbox.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/wuapi-inbox"
chmod 755 "$app/Contents/MacOS/wuapi-inbox"

# The icon, from the PNGs of crates/app/assets.
iconset="$(mktemp -d)/AppIcon.iconset"
mkdir -p "$iconset"
sips -z 16 16 "$icons/app-icon-32.png" --out "$iconset/icon_16x16.png" >/dev/null
cp "$icons/app-icon-32.png" "$iconset/icon_16x16@2x.png"
cp "$icons/app-icon-32.png" "$iconset/icon_32x32.png"
cp "$icons/app-icon-64.png" "$iconset/icon_32x32@2x.png"
cp "$icons/app-icon-128.png" "$iconset/icon_128x128.png"
cp "$icons/app-icon-256.png" "$iconset/icon_128x128@2x.png"
cp "$icons/app-icon-256.png" "$iconset/icon_256x256.png"
cp "$icons/app-icon-512.png" "$iconset/icon_256x256@2x.png"
cp "$icons/app-icon-512.png" "$iconset/icon_512x512.png"
iconutil --convert icns --output "$app/Contents/Resources/AppIcon.icns" "$iconset"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>wuapi Inbox</string>
    <key>CFBundleDisplayName</key>
    <string>wuapi Inbox</string>
    <key>CFBundleIdentifier</key>
    <string>dev.wuapi.inbox</string>
    <key>CFBundleExecutable</key>
    <string>wuapi-inbox</string>
    <key>CFBundleIconFile</key>
    <string>AppIcon</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleShortVersionString</key>
    <string>${version}</string>
    <key>CFBundleVersion</key>
    <string>${version}</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.social-networking</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>wuapi Inbox uses the microphone to record a voice note when you press the record button.</string>
</dict>
</plist>
PLIST

plutil -lint "$app/Contents/Info.plist" >/dev/null
codesign --force --sign - "$app"
echo "$app"
