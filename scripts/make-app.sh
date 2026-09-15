#!/usr/bin/env bash
# Assemble and install a macOS .app bundle.
#
# Usage:
#   scripts/make-app.sh              # build for this machine, install to /Applications
#   scripts/make-app.sh --no-install # assemble in ./dist only
set -euo pipefail

cd "$(dirname "$0")/.."

INSTALL=1
[[ "${1:-}" == "--no-install" ]] && INSTALL=0

APP_NAME="DSH Shell"
DIST="dist/${APP_NAME}.app"

echo "==> building release binary"
cargo build --release

echo "==> assembling bundle"
rm -rf "$DIST"
mkdir -p "$DIST/Contents/MacOS" "$DIST/Contents/Resources"

cp target/release/dsh-shell "$DIST/Contents/MacOS/"
cp theme.json "$DIST/Contents/Resources/"
cp assets/deepseek.icns "$DIST/Contents/Resources/"

cat > "$DIST/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>DSH Shell</string>
<key>CFBundleDisplayName</key><string>DSH Shell</string>
<key>CFBundleIdentifier</key><string>ai.deepseek.dsh.shell</string>
<key>CFBundleExecutable</key><string>dsh-shell</string>
<key>CFBundleIconFile</key><string>deepseek</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>11.0</string>
<key>NSHighResolutionCapable</key><true/>
<key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict></plist>
PLIST

plutil -lint "$DIST/Contents/Info.plist"
echo "==> $DIST ($(du -sh "$DIST" | cut -f1))"

# Ad-hoc sign so the bundle launches locally. Distribution needs a Developer ID
# and notarization; see the README.
codesign --force --deep --sign - "$DIST" 2>/dev/null || \
  echo "note: ad-hoc signing skipped"

if [[ "$INSTALL" == "1" ]]; then
  echo "==> installing to /Applications"
  rm -rf "/Applications/${APP_NAME}.app"
  cp -R "$DIST" "/Applications/${APP_NAME}.app"
  echo "installed. launch with: open -a \"${APP_NAME}\""
fi
