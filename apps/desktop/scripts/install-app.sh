#!/usr/bin/env bash
# Builds the release app and installs it ad-hoc signed into ~/Applications. It never opens it.
set -euo pipefail
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo "macOS only" >&2; exit 1; }
npm ci
npm run build:web
npx tauri build --bundles app --ci
app="src-tauri/target/release/bundle/macos/Loom Spike.app"
# Tauri signed it with identity "-" (tauri.conf.json); sign again so that the check below is of what we copy.
codesign --force --deep --sign - "$app"
codesign --verify --deep --strict "$app"
mkdir -p "$HOME/Applications"
rm -rf "$HOME/Applications/Loom Spike.app"
cp -R "$app" "$HOME/Applications/"
echo "Installed: $HOME/Applications/Loom Spike.app"
echo "Start the sandbox with: mise run spike-sandbox"
