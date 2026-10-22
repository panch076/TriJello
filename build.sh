#!/bin/bash
# Builds TriJello.app next to this script.
set -euo pipefail
cd "$(dirname "$0")"

APP=TriJello.app
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

swiftc -O -swift-version 5 -target arm64-apple-macos14.0 \
    -framework AppKit -framework Metal -framework QuartzCore \
    -framework ScreenCaptureKit -framework CoreMedia -framework CoreVideo \
    Sources/*.swift -o "$APP/Contents/MacOS/TriJello"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>TriJello</string>
    <key>CFBundleIdentifier</key><string>local.trijello</string>
    <key>CFBundleName</key><string>TriJello</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>1.0</string>
    <key>CFBundleVersion</key><string>1</string>
    <key>LSMinimumSystemVersion</key><string>14.0</string>
    <key>LSUIElement</key><true/>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$APP"
echo "Built $(pwd)/$APP"
