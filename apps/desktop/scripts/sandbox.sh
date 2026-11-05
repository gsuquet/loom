#!/usr/bin/env bash
# Starts a loomd with fake data only: fake HOME, fake Loom home, no update checks, clean environment.
# Foreground; Ctrl-C stops it. `sandbox.sh --print-env` is for the tests.
set -euo pipefail
repo="$(cd "$(dirname "$0")/../../.." && pwd)"
root="${LOOM_SPIKE_ROOT:-$HOME/.cache/loom-spike}"
home="$root/home"
mkdir -p "$home" "$home/spike-fixtures" "$root/fake-home"
cat >"$home/config.toml" <<'T'
[update]
policy = "off"

[repos]
roots = []

[contexts.demo]
T
echo 'id = "spike"' >"$home/machine.toml"
node "$repo/apps/desktop/scripts/gen-patch.ts" "$home/spike-fixtures/mr.patch"
loomd="${LOOMD_BIN:-$repo/target/debug/loomd}"
if [ ! -x "$loomd" ]; then
  cargo build --locked -p loomd --manifest-path "$repo/Cargo.toml"
fi
sock="$home/run/loomd.sock"
add_tasks() {
  for _ in $(seq 1 100); do [ -S "$sock" ] && break; sleep 0.1; done
  for n in 1 2 3; do
    curl -s --unix-socket "$sock" -X POST http://loomd/v1/tasks -H 'content-type: application/json' \
      -d "{\"title\":\"Fake task $n\",\"kind\":\"chore\",\"context\":\"demo\",\"status\":\"inbox\"}" >/dev/null
  done
  echo "Sandbox ready. Add a row to the live list with:"
  echo "  curl --unix-socket '$sock' -X POST http://loomd/v1/tasks -H 'content-type: application/json' -d '{\"title\":\"Another fake task\",\"kind\":\"chore\",\"context\":\"demo\",\"status\":\"inbox\"}'"
}
add_tasks &
exec env -i PATH="/usr/bin:/bin" HOME="$root/fake-home" LOOM_HOME="$home" "$loomd" --color never
