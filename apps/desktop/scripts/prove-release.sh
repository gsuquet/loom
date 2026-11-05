#!/usr/bin/env bash
# Proves that a release build holds no WebDriver server. It inspects the graph, the binary, then
# runs the binary with the variable that would start the server and looks at the port.
set -uo pipefail
cd "$(dirname "$0")/.."
bin="src-tauri/target/release/loom-desktop"
status=0
[ -x "$bin" ] || { echo "no release binary at $bin"; exit 1; }

echo "== dependency graph without the e2e feature"
if cargo tree --manifest-path src-tauri/Cargo.toml -p loom-desktop -e normal,build --locked --prefix none | grep -iE 'wdio|webdriver'; then
  echo "FAIL: the graph holds a WebDriver crate"; status=1
else
  echo "ok: no wdio or webdriver crate"
fi
echo "== graph with the e2e feature (control: this must find it)"
cargo tree --manifest-path src-tauri/Cargo.toml -p loom-desktop -e normal --features e2e --locked --prefix none | grep -iE 'wdio' | sort -u | head -3

echo "== strings of the release binary"
count="$(strings -a "$bin" | grep -ciE 'wdio|webdriver|TAURI_WEBDRIVER_PORT|/session/\{|execute/sync')"
echo "matches: $count (binary: $(wc -c <"$bin") bytes)"
[ "$count" = 0 ] || { strings -a "$bin" | grep -iE 'wdio|webdriver|TAURI_WEBDRIVER_PORT' | head; status=1; }
if [ -x src-tauri/target/debug/loom-desktop ]; then
  echo "control, e2e binary: $(strings -a src-tauri/target/debug/loom-desktop | grep -ciE 'wdio|webdriver|TAURI_WEBDRIVER_PORT|/session/\{') matches"
fi

echo "== run it with TAURI_WEBDRIVER_PORT set: nothing may listen"
root="/tmp/lsr-$$"
mkdir -p "$root/run"
LOOM_SPIKE_HOME="$root" TAURI_WEBDRIVER_PORT=14446 ${RUN_PREFIX:-} "$bin" >/dev/null 2>&1 &
pid=$!
sleep 6
if kill -0 "$pid" 2>/dev/null; then
  if curl -s -m 2 http://127.0.0.1:14446/status >/dev/null; then
    echo "FAIL: a server answers on the port"; status=1
  else
    echo "ok: the app runs (pid $pid), and nothing answers on the port"
  fi
  kill "$pid"
else
  echo "could not check: the app did not stay up"; status=1
fi
rm -rf "$root"
exit "$status"
