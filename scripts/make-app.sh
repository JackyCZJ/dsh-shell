#!/usr/bin/env bash
# Assemble and install a macOS .app bundle.
#
# Usage:
#   scripts/make-app.sh                    # shell only: uses a DSH already installed
#   scripts/make-app.sh --bundle-runtime   # self-contained: ships bun + the dsh tree
#   scripts/make-app.sh --no-install       # assemble in ./dist and stop
#
# Options:
#   --bundle-runtime         Ship a self-contained runtime (bun + node_modules)
#   --dsh-version <version>  @deepseek-ai/dsh version to bundle (default: the
#                            version already installed on this machine)
#   --no-install             Assemble into ./dist and stop
#   -h, --help               This message
#
# Without --bundle-runtime the app launches whatever `dsh` it can find, which is
# fine for a machine that already has one. With it the app carries its own
# runtime, so a clean machine needs nothing installed first.
set -euo pipefail

cd "$(dirname "$0")/.."

INSTALL=1
BUNDLE_RUNTIME=0
DSH_VERSION=""

# Print the comment header, so the usage text and the file cannot drift apart.
usage() { sed -n '2,/^[^#]/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-install)     INSTALL=0; shift ;;
    --bundle-runtime) BUNDLE_RUNTIME=1; shift ;;
    --dsh-version)    DSH_VERSION="${2:?--dsh-version needs a version}"; shift 2 ;;
    -h|--help)        usage; exit 0 ;;
    *) echo "make-app.sh: unknown argument: $1" >&2; usage >&2; exit 2 ;;
  esac
done

APP_NAME="DSH Shell"
DIST="dist/${APP_NAME}.app"

echo "==> building release binary"
cargo build --release

echo "==> assembling bundle"
rm -rf "$DIST"
mkdir -p "$DIST/Contents/MacOS" "$DIST/Contents/Resources"

cp target/release/dsh-shell "$DIST/Contents/MacOS/"
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

# --- Bundled runtime --------------------------------------------------------

# Delete artifacts nothing reads at runtime.
#
# npm packages ship TypeScript declarations, source maps, and — from packages
# with native builds — Windows debug symbols. On this tree that is ~93 MB of
# ~210 MB. TypeScript *sources* are deliberately kept: it is only another 7 MB,
# and a package whose runtime entry is `.ts` would break without them.
prune_runtime() {
  local modules="$1/node_modules"
  [[ -d "$modules" ]] || return 0
  find "$modules" -type f \
    \( -name '*.d.ts' -o -name '*.d.mts' -o -name '*.d.cts' \
       -o -name '*.map' -o -name '*.pdb' \) -delete
}

bundle_runtime() {
  local resources="$DIST/Contents/Resources"
  local runtime="$resources/runtime"

  local bun_bin="${BUN_BIN:-}"
  if [[ -z "$bun_bin" ]]; then
    bun_bin="$(command -v bun || true)"
  fi
  if [[ -z "$bun_bin" || ! -x "$bun_bin" ]]; then
    echo "error: bun not found. Install it or set BUN_BIN=/path/to/bun" >&2
    exit 1
  fi

  # Default to the version already on this machine, so a rebuild bundles what
  # the developer is actually running rather than drifting to latest.
  local version="$DSH_VERSION"
  if [[ -z "$version" ]]; then
    version="$(dsh --version 2>/dev/null | tail -1 | tr -d '[:space:]')" || true
  fi
  if [[ -z "$version" ]]; then
    echo "error: could not determine the dsh version; pass --dsh-version" >&2
    exit 1
  fi

  echo "==> bundling runtime: bun $( "$bun_bin" --version ) + @deepseek-ai/dsh@$version"
  mkdir -p "$runtime"
  cp "$bun_bin" "$runtime/bun"

  # The tree is installed next to the launcher rather than taken from the
  # machine's global install: the app has to be self-contained, and this is the
  # directory DSH treats as its installation anchor when it links the profile.
  cat > "$runtime/package.json" <<JSON
{
  "name": "dsh-shell-runtime",
  "private": true,
  "dependencies": {
    "@deepseek-ai/dsh": "$version"
  }
}
JSON

  ( cd "$runtime" && bun install --production )

  # Keep the size visible: this step is the whole point of the flag.
  local before after
  before=$(du -sk "$runtime/node_modules" | cut -f1)
  prune_runtime "$runtime"
  after=$(du -sk "$runtime/node_modules" | cut -f1)
  echo "==> pruned $(( (before - after) / 1024 )) MB of build-time artifacts"

  # The shell launches this instead of a system `dsh`. `resolve_launcher`
  # prefers $BUNDLE/Contents/Resources/bin/dsh for exactly this reason.
  mkdir -p "$resources/bin"
  cat > "$resources/bin/dsh" <<'SHIM'
#!/bin/sh
# Runs the DSH bundled inside this app, so nothing has to be installed first.
#
# `exec` matters twice over: the shell supervises this process and kills this
# pid on quit, so no extra shell may sit in between; and bun runs the CLI in
# this process, so the pid stays the one the shell is watching.
#
# The `cd` is what makes `bun run` resolve the CLI from the bundled
# node_modules. It does not change which directory an agent works in — DSH
# takes that from the workspace the UI selected, not from the process cwd.
set -e
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$here/runtime"
exec ./bun run dsh "$@"
SHIM
  chmod +x "$resources/bin/dsh"

  # Prove the bundled launcher is the one that will run, before signing a
  # bundle that could not start.
  local found
  found="$("$resources/bin/dsh" --version 2>/dev/null | tail -1 | tr -d '[:space:]')" || true
  if [[ "$found" != "$version" ]]; then
    echo "error: bundled launcher answered '$found', expected '$version'" >&2
    exit 1
  fi
  echo "==> bundled launcher answers: $found"
}

if [[ "$BUNDLE_RUNTIME" == "1" ]]; then
  bundle_runtime
fi

echo "==> $DIST ($(du -sh "$DIST" | cut -f1))"
if [[ "$BUNDLE_RUNTIME" == "1" ]]; then
  echo "    runtime: $(du -sh "$DIST/Contents/Resources/runtime" | cut -f1)"
fi

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
