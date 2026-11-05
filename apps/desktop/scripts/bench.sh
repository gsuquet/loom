#!/usr/bin/env bash
# The frame figures of criterion 2, on this Mac, in the real webview. It uses a sandbox loomd.
set -euo pipefail
cd "$(dirname "$0")/.."
repo="$(cd ../.. && pwd)"
[ -d node_modules ] || npm ci
[ -x "$repo/target/debug/loomd" ] || cargo build --locked -p loomd --manifest-path "$repo/Cargo.toml"
npx vite build
npx tauri build --debug --no-bundle --features e2e --ci
mkdir -p .e2e
cp src-tauri/target/debug/loom-desktop .e2e/app-protected
TAURI_WEBDRIVER_PORT=14445 node e2e/real-app.ts .e2e/app-protected --label owner-bench
