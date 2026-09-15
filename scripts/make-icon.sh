#!/usr/bin/env bash
# Regenerate assets/deepseek.icns from assets/deepseek.svg.
#
# The .icns is committed, so this is only needed when the source SVG changes.
# macOS-only: it uses sips and iconutil.
set -euo pipefail

cd "$(dirname "$0")/.."

SVG="assets/deepseek.svg"
ICONSET="assets/icon.iconset"
ICNS="assets/deepseek.icns"

if [[ "$(uname)" != "Darwin" ]]; then
  echo "error: icon generation requires macOS (sips, iconutil)" >&2
  exit 1
fi

rm -rf "$ICONSET"
mkdir -p "$ICONSET"

# iconutil requires these exact names, including the @2x variants.
render() { sips -s format png -z "$1" "$1" "$SVG" --out "$ICONSET/$2" >/dev/null; }

render 16   icon_16x16.png
render 32   icon_16x16@2x.png
render 32   icon_32x32.png
render 64   icon_32x32@2x.png
render 128  icon_128x128.png
render 256  icon_128x128@2x.png
render 256  icon_256x256.png
render 512  icon_256x256@2x.png
render 512  icon_512x512.png
render 1024 icon_512x512@2x.png

iconutil -c icns "$ICONSET" -o "$ICNS"
rm -rf "$ICONSET"
echo "wrote $ICNS"
