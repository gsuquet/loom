#!/usr/bin/env bash
# End-to-end upgrade scenario (issue #14): install, manual upgrade, failed auto-upgrade
# rolled back by the watchdog, manual rollback, with real binaries against a local fake of
# the GitHub releases API.
#
# Usage:
#   bash dist/e2e-upgrade.sh             (or: mise run e2e)
#   bash dist/e2e-upgrade.sh --systemd   (CI only: Linux, loomd under a systemd user service)
#
# Three debug builds of loom and loomd are stamped A < B < C with LOOM_BUILD_VERSION (debug
# builds honour the test hooks), packaged with dist/package.sh, signed with a throwaway
# minisign key and served by `python3 -m http.server` at the GitHub API paths. Releases are
# published one at a time by writing files: A, then B, then C.
#
# Release C ships the real `loom` of C and a `loomd` wrapper that sets
# LOOM_TEST_FAIL_UNLESS_HOME_PREFIX=/tmp/loom-preflight- before `exec`-ing the real loomd of
# C: it passes the pre-flight check (throwaway home under /tmp/loom-preflight-) and fails in
# the real loom home, so the post-restart watchdog has to roll it back.
#
# Default mode: loomd runs without a service manager (`loom status` starts it, the watchdog
# restarts it). Everything lives in one directory under /tmp (short: socket path limit):
# loom home, install prefix, fake API. The real ~/.loom, ~/.local/bin, launchd and systemd
# are never touched; HOME is replaced for every loom and installer command.
#
# --systemd: same scenario up to the watchdog rollback, with loomd as a systemd user service
# (`loom service install`) and the test hooks in a drop-in of the unit. It writes to the
# user's systemd configuration (~/.config/systemd/user), so it refuses to run unless CI=true
# and no loomd unit exists yet. The manual rollback is not repeated there.
#
# Requires: cargo, python3, curl, tar, minisign (`mise install`). On failure, prints
# log/loomd.log, state/update.json, versions/ and the fake server log. LOOM_E2E_KEEP=1 keeps
# the work directory.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
repo_path="repos/gsuquet/loom/releases"

A=99.0.0
B=99.1.0
C=99.2.0
# Watchdog deadline (test hook); its grace period before starting loomd itself is a quarter.
health_deadline=8s
# Upper bound for one phase that waits on the daemon.
phase_timeout=120

mode=plain
case "${1:-}" in
"") ;;
--systemd) mode=systemd ;;
-h | --help)
    sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
*)
    printf 'e2e-upgrade: unknown argument: %s\n' "$1" >&2
    exit 2
    ;;
esac

# --- output and assertions -----------------------------------------------------------------

started=$SECONDS
step() { printf '\n[e2e %4ds] %s\n' "$((SECONDS - started))" "$*"; }
ok() { printf '  ok   %s\n' "$*"; }
fail() {
    printf '  FAIL %s\n' "$*" >&2
    exit 1
}

# expect DESCRIPTION ACTUAL EXPECTED
expect() {
    if [ "$2" = "$3" ]; then
        ok "$1: $2"
    else
        fail "$1: expected '$3', got '$2'"
    fi
}

# wait_until DESCRIPTION TIMEOUT_SECS COMMAND...: polls COMMAND every 200 ms.
wait_until() {
    local what=$1 timeout=$2
    shift 2
    local end=$((SECONDS + timeout)) from=$SECONDS
    until "$@"; do
        [ "$SECONDS" -lt "$end" ] || fail "timed out after ${timeout}s waiting for $what"
        sleep 0.2
    done
    ok "$what (after $((SECONDS - from))s)"
}

for tool in cargo python3 curl tar minisign; do
    command -v "$tool" >/dev/null 2>&1 || fail "'$tool' is required (run 'mise install')"
done

if [ "$mode" = systemd ]; then
    [ "$(uname -s)" = Linux ] || fail "--systemd runs on Linux only"
    [ "${CI:-}" = true ] || fail "--systemd changes the user's systemd configuration: CI only (CI=true)"
    systemctl --user show-environment >/dev/null ||
        fail "no systemd user instance (enable lingering and set XDG_RUNTIME_DIR)"
    unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    [ ! -e "$unit_dir/loomd.service" ] || fail "$unit_dir/loomd.service already exists: refusing to replace it"
fi

# --- work directory and cleanup ------------------------------------------------------------

# Short and not under /tmp/loom-preflight- (the prefix that lets loomd C start).
work="$(mktemp -d /tmp/loom-e2e.XXXXXX)"
home="$work/home"
prefix="$work/bin"
api="$work/api"
server_log="$work/server.log"
server_pid=""
user_home="$work/user"
mkdir -p "$home" "$prefix" "$api/$repo_path/tags" "$api/$repo_path/assets" "$user_home"
chmod 700 "$home"

# Journal of the user unit. The user journal may be missing or empty (volatile storage):
# the system journal too, through passwordless sudo (CI). Entries may appear twice.
journal() {
    journalctl --user -u loomd.service --no-pager 2>/dev/null || true
    sudo -n journalctl --no-pager "_SYSTEMD_USER_UNIT=loomd.service" 2>/dev/null || true
}

diagnostics() {
    printf '\n===== diagnostics (%s) =====\n' "$work" >&2
    printf '\n--- %s/log/loomd.log ---\n' "$home" >&2
    cat "$home/log/loomd.log" >&2 2>/dev/null || echo "(none)" >&2
    printf '\n--- %s/state/update.json ---\n' "$home" >&2
    cat "$home/state/update.json" >&2 2>/dev/null || echo "(none)" >&2
    printf '\n--- %s/versions ---\n' "$home" >&2
    (cd "$home" 2>/dev/null && find versions -print 2>/dev/null | sort) >&2 || echo "(none)" >&2
    printf '\n--- installed binaries ---\n' >&2
    "$prefix/loom" -V >&2 2>/dev/null || echo "(no loom)" >&2
    "$prefix/loomd" -V >&2 2>/dev/null || echo "(no loomd)" >&2
    printf '\n--- loom stderr ---\n' >&2
    cat "$work/loom.stderr" >&2 2>/dev/null || echo "(none)" >&2
    printf '\n--- fake server log ---\n' >&2
    cat "$server_log" >&2 2>/dev/null || echo "(none)" >&2
    if [ "$mode" = systemd ]; then
        printf '\n--- systemctl --user status loomd.service ---\n' >&2
        systemctl --user status --no-pager loomd.service >&2 || true
        printf '\n--- journal of loomd.service ---\n' >&2
        journal >&2
    fi
}

# Stops everything this run started: the service, the watchdogs and the loomd processes
# (every one has a path under $work on its command line), and the fake server.
teardown() {
    if [ "$mode" = systemd ]; then
        "$prefix/loom" --color=never service uninstall >/dev/null 2>&1 || true
        rm -rf "$unit_dir/loomd.service.d"
        systemctl --user daemon-reload || true
        systemctl --user reset-failed loomd.service 2>/dev/null || true
    fi
    pkill -TERM -f "$work/" 2>/dev/null || true
    local tries=0
    while pgrep -f "$work/" >/dev/null 2>&1 && [ "$tries" -lt 50 ]; do
        sleep 0.1
        tries=$((tries + 1))
    done
    pkill -KILL -f "$work/" 2>/dev/null || true
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
}

on_exit() {
    local code=$1
    trap - EXIT
    if [ "$code" -ne 0 ]; then
        diagnostics
    fi
    teardown
    if [ "${LOOM_E2E_KEEP:-}" = 1 ]; then
        printf 'e2e-upgrade: work directory kept: %s\n' "$work" >&2
    else
        rm -rf "$work"
    fi
    if [ "$code" -eq 0 ]; then
        printf '\ne2e-upgrade: PASSED in %ds (%s)\n' "$((SECONDS - started))" "$mode"
    else
        printf '\ne2e-upgrade: FAILED after %ds (%s)\n' "$((SECONDS - started))" "$mode" >&2
    fi
    exit "$code"
}
trap 'on_exit $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# --- environment ---------------------------------------------------------------------------

# Nothing from the caller's environment may change the scenario.
unset LOOM_TEST_BUILD_VERSION LOOM_TEST_FAIL_UNLESS_HOME_PREFIX LOOM_GITHUB_TOKEN GITHUB_TOKEN \
    LOOM_UPDATE_PUBKEY LOOM_INSTALL_PUBKEY NO_COLOR \
    OTEL_SDK_DISABLED OTEL_EXPORTER_OTLP_ENDPOINT OTEL_EXPORTER_OTLP_TRACES_ENDPOINT \
    OTEL_EXPORTER_OTLP_HEADERS OTEL_EXPORTER_OTLP_TRACES_HEADERS OTEL_SERVICE_NAME \
    OTEL_RESOURCE_ATTRIBUTES HTTP_PROXY HTTPS_PROXY ALL_PROXY http_proxy https_proxy all_proxy

# jget FILE DOTTED.PATH: prints a JSON value; lists comma-separated, null as "null".
jget() {
    python3 -I -c '
import json, sys
value = json.load(open(sys.argv[1]))
for key in filter(None, sys.argv[2].split(".")):
    value = value[int(key)] if isinstance(value, list) else (value or {}).get(key)
if value is None:
    print("null")
elif isinstance(value, list):
    print(",".join(str(item) for item in value))
elif isinstance(value, bool):
    print(str(value).lower())
else:
    print(value)
' "$1" "$2"
}

# --- builds --------------------------------------------------------------------------------

target_dir="$(cargo metadata --format-version 1 --no-deps --manifest-path "$repo_root/Cargo.toml" |
    python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"

# Sequential builds in one target directory: only the crates reading the stamp rebuild.
for v in "$A" "$B" "$C"; do
    step "build $v (debug, channel stable)"
    from=$SECONDS
    LOOM_BUILD_VERSION="$v" LOOM_CHANNEL=stable \
        cargo build --locked --manifest-path "$repo_root/Cargo.toml" -p loom -p loomd --bins
    mkdir -p "$work/build/$v"
    cp "$target_dir/debug/loom" "$target_dir/debug/loomd" "$work/build/$v/"
    expect "built loomd" "$("$work/build/$v/loomd" -V)" "loomd $v"
    printf '  build time %s: %ds\n' "$v" "$((SECONDS - from))"
done

"$work/build/$A/loom" version --json >"$work/version.json"
target="$(jget "$work/version.json" target)"
# A *-linux-gnu build installs the *-linux-musl archive (release naming).
target="${target/%-linux-gnu/-linux-musl}"

# --- releases ------------------------------------------------------------------------------

step "package and sign releases $A, $B, $C for $target"
minisign -G -W -p "$work/test.pub" -s "$work/test.key" >/dev/null
pubkey="$(sed -n 2p "$work/test.pub")"

next_id=100
# make_release VERSION BIN_DIR: archive, SHA256SUMS, signature and release JSON under
# $work/rel/VERSION, published later by `publish`.
make_release() {
    local v=$1 bins=$2 out="$work/rel/$1"
    mkdir -p "$out/assets"
    "$repo_root/dist/package.sh" archive "$v" "$target" "$bins" "$out" >/dev/null
    "$repo_root/dist/package.sh" checksums "$out" >/dev/null
    minisign -S -s "$work/test.key" -m "$out/SHA256SUMS" -x "$out/SHA256SUMS.minisig" >/dev/null
    local assets="" name
    for name in "loom-$v-$target.tar.gz" SHA256SUMS SHA256SUMS.minisig; do
        next_id=$((next_id + 1))
        cp "$out/$name" "$out/assets/$next_id"
        assets="$assets${assets:+,}{\"id\":$next_id,\"name\":\"$name\"}"
    done
    printf '{"tag_name":"v%s","name":"v%s","draft":false,"prerelease":false,"assets":[%s]}\n' \
        "$v" "$v" "$assets" >"$out/release.json"
}

make_release "$A" "$work/build/$A"
make_release "$B" "$work/build/$B"
# C: the real loom of C, and a loomd that only starts under the pre-flight's throwaway home.
mkdir -p "$work/stage-$C"
cp "$work/build/$C/loom" "$work/stage-$C/loom"
cat >"$work/stage-$C/loomd" <<EOF
#!/bin/sh
export LOOM_TEST_FAIL_UNLESS_HOME_PREFIX=/tmp/loom-preflight-
exec '$work/build/$C/loomd' "\$@"
EOF
chmod 755 "$work/stage-$C/loomd"
make_release "$C" "$work/stage-$C"

# publish VERSION: makes VERSION the latest stable release of the fake API.
publish() {
    local v=$1 rel="$work/rel/$1" releases="$api/$repo_path"
    cp "$rel"/assets/* "$releases/assets/"
    cp "$rel/release.json" "$releases/tags/v$v"
    cp "$rel/release.json" "$releases/latest.tmp"
    mv "$releases/latest.tmp" "$releases/latest"
    ok "published $v as the latest release"
}

asset_downloads() { grep -c "GET /$repo_path/assets/" "$server_log" || true; }

# --- fake API ------------------------------------------------------------------------------

step "start the fake releases API"
port="$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')"
python3 -I -u -m http.server "$port" --bind 127.0.0.1 --directory "$api" >"$server_log" 2>&1 &
server_pid=$!
export LOOM_RELEASES_URL="http://127.0.0.1:$port"
server_up() { curl -fs -o /dev/null "$LOOM_RELEASES_URL/"; }
wait_until "the fake API to answer" 10 server_up

# Debug-only test hooks of loom and loomd (docs/reference/cli/conventions.md).
export LOOM_HOME="$home"
export LOOM_INSTALL_PUBKEY="$pubkey"
export LOOM_UPDATE_PUBKEY="$pubkey"
export LOOM_UPDATE_CHECK_INTERVAL=1s
export LOOM_UPDATE_HEALTH_DEADLINE="$health_deadline"

# --- helpers on the installed loom ---------------------------------------------------------

# Every loom command gets a throwaway HOME, except under systemd where `loom service` needs
# the real one (the user manager only reads ~/.config/systemd/user).
if [ "$mode" = systemd ]; then
    loom_home_env="$HOME"
else
    loom_home_env="$user_home"
fi
loom() { HOME="$loom_home_env" "$prefix/loom" --color=never "$@" 2>>"$work/loom.stderr"; }

status_file="$work/status.json"
state_file="$home/state/update.json"

# Loads `loom status --no-start --json` into $status_file; fails when loomd does not answer.
read_status() { loom status --no-start --json >"$status_file"; }
daemon_is() { read_status && [ "$(jget "$status_file" daemon.version)" = "$1" ]; }
daemon_pid() { jget "$status_file" pid; }
result_is() { [ -f "$state_file" ] && [ "$(jget "$state_file" last_result.kind)" = "$1" ]; }
process_gone() { ! kill -0 "$1" 2>/dev/null; }
versions_saved() { (cd "$home" && find versions -mindepth 1 -maxdepth 1 -print 2>/dev/null | sed 's|^versions/||' | sort | tr '\n' ' ' | sed 's/ $//'); }

# loomd logs: log/loomd.log (loomd started by `loom status` or the watchdog, and the
# watchdog itself), plus the journal of the unit under systemd.
daemon_logs() {
    cat "$home/log/loomd.log" 2>/dev/null || true
    if [ "$mode" = systemd ]; then
        journal
    fi
}

expect_installed() {
    expect "loom -V" "$("$prefix/loom" -V)" "loom $1"
    expect "loomd -V" "$("$prefix/loomd" -V)" "loomd $1"
}

write_config() {
    printf '[update]\npolicy = "%s"\nchannel = "stable"\n' "$1" >"$home/config.toml"
}

# --- scenario ------------------------------------------------------------------------------

step "install.sh installs $A"
publish "$A"
HOME="$user_home" sh "$repo_root/dist/install.sh" --prefix "$prefix" 2>&1 | tee "$work/install.out"
grep -q "verified the SHA256SUMS signature" "$work/install.out" || fail "install.sh did not verify the signature"
ok "install.sh verified the signature"
expect_installed "$A"

# Off while A runs: B must arrive through `loom upgrade`, not through the auto-upgrade.
write_config off

if [ "$mode" = systemd ]; then
    step "loom service install starts loomd $A under systemd"
    mkdir -p "$unit_dir/loomd.service.d"
    # The user manager does not see this shell's environment: hooks go into a drop-in.
    cat >"$unit_dir/loomd.service.d/e2e.conf" <<EOF
[Service]
Environment="LOOM_RELEASES_URL=$LOOM_RELEASES_URL"
Environment="LOOM_UPDATE_PUBKEY=$pubkey"
Environment="LOOM_UPDATE_CHECK_INTERVAL=1s"
Environment="LOOM_UPDATE_HEALTH_DEADLINE=$health_deadline"
EOF
    loom service install
    grep -q '^KillMode=process$' "$unit_dir/loomd.service" || fail "the unit lacks KillMode=process"
    ok "unit has KillMode=process"
    wait_until "loomd $A to answer" 30 daemon_is "$A"
else
    step "loom status starts loomd $A"
    loom status >/dev/null
    wait_until "loomd $A to answer" 10 daemon_is "$A"
fi
pid_a="$(daemon_pid)"
ok "loomd $A PID $pid_a"
expect "versions/ after install" "$(versions_saved)" ""

supervised() {
    [ "$(systemctl --user show -p MainPID --value loomd.service)" = "$(daemon_pid)" ] &&
        systemctl --user is-active --quiet loomd.service
}

step "publish $B, loom upgrade to $B"
publish "$B"
# Auto from the next loomd start on: config.toml is read when loomd starts.
write_config auto
loom upgrade | tee "$work/upgrade.out"
grep -q "sent SIGTERM to loomd (PID $pid_a)" "$work/upgrade.out" || fail "loom upgrade did not signal loomd $pid_a"
expect_installed "$B"
expect "versions/ after upgrade" "$(versions_saved)" "$A"
wait_until "loomd $A (PID $pid_a) to exit" 15 process_gone "$pid_a"
if [ "$mode" = systemd ]; then
    wait_until "systemd to restart loomd on $B" 30 daemon_is "$B"
    wait_until "loomd $B to be the unit's main process" 10 supervised
else
    loom status >/dev/null
    wait_until "loomd $B to answer" 10 daemon_is "$B"
fi
pid_b="$(daemon_pid)"
[ "$pid_b" != "$pid_a" ] || fail "loomd PID did not change after the upgrade ($pid_b)"
ok "loomd $B PID $pid_b (was $pid_a)"
expect "policy" "$(jget "$status_file" update.policy)" auto
wait_until "a check of loomd $B to find it up to date" 30 result_is up_to_date

step "publish $C: loomd $B pre-flights it, swaps, the watchdog rolls it back"
downloads_before="$(asset_downloads)"
publish "$C"
wait_until "the watchdog to roll $C back" "$phase_timeout" result_is rolled_back
wait_until "loomd $B (PID $pid_b) to have exited" 15 process_gone "$pid_b"
expect "rolled back from" "$(jget "$state_file" last_result.from)" "$B"
expect "rolled back to" "$(jget "$state_file" last_result.to)" "$C"
expect "failed_versions" "$(jget "$state_file" failed_versions)" "$C"
expect "pending" "$(jget "$state_file" pending)" null
# Pre-flight passed, then loomd C started in the real home and exited: the watchdog path.
daemon_logs >"$work/daemon.log"
grep -q "pre-flight check passed" "$work/daemon.log" || fail "no 'pre-flight check passed' in the daemon logs"
ok "loomd $B logged: pre-flight check passed"
grep -q "LOOM_TEST_FAIL_UNLESS_HOME_PREFIX: the loom home" "$work/daemon.log" ||
    fail "loomd $C never failed in the real loom home"
ok "loomd $C started in the real loom home and failed"
expect_installed "$B"
expect "versions/ after the watchdog rollback" "$(versions_saved)" "$A"
[ "$(asset_downloads)" -gt "$downloads_before" ] || fail "loomd $B never downloaded $C"
ok "loomd $B downloaded $C"

wait_until "loomd $B to answer again" 60 daemon_is "$B"
pid_b2="$(daemon_pid)"
[ "$pid_b2" != "$pid_b" ] || fail "loomd PID did not change after the rollback ($pid_b2)"
ok "loomd $B PID $pid_b2 (was $pid_b)"
expect "status failed_versions" "$(jget "$status_file" update.failed_versions)" "$C"
loom status --no-start >"$work/status.txt"
grep -q "^loomd $B running$" "$work/status.txt" || fail "loom status does not report loomd $B running"
grep -q "^failed: *$C$" "$work/status.txt" || fail "loom status does not report $C as failed"
grep -q "^last_result: rolled back $C -> $B at " "$work/status.txt" || fail "loom status does not report the rollback"
ok "loom status reports $B running, $C failed and rolled back"

step "the next checks skip $C without downloading it"
downloads_after="$(asset_downloads)"
skipped_c() { result_is skipped_failed && [ "$(jget "$state_file" last_result.version)" = "$C" ]; }
wait_until "a check to skip the failed $C" 30 skipped_c
first_skip="$(jget "$state_file" last_check)"
later_check() { [ "$(jget "$state_file" last_check)" != "$first_skip" ]; }
wait_until "another check" 15 later_check
skipped_c || fail "the later check did not skip $C: $(jget "$state_file" last_result.kind)"
expect "asset downloads since the rollback" "$(asset_downloads)" "$downloads_after"
expect_installed "$B"

if [ "$mode" = systemd ]; then
    step "loomd $B is supervised by systemd again"
    printf '  unit: ActiveState=%s SubState=%s Result=%s MainPID=%s NRestarts=%s; loomd PID %s\n' \
        "$(systemctl --user show -p ActiveState --value loomd.service)" \
        "$(systemctl --user show -p SubState --value loomd.service)" \
        "$(systemctl --user show -p Result --value loomd.service)" \
        "$(systemctl --user show -p MainPID --value loomd.service)" \
        "$(systemctl --user show -p NRestarts --value loomd.service)" \
        "$pid_b2"
    supervised || fail "loomd $B (PID $pid_b2) is not the main process of an active loomd.service"
    ok "loomd.service active with loomd $B as main process"
    exit 0
fi

step "loom rollback to $A"
loom rollback | tee "$work/rollback.out"
grep -q "sent SIGTERM to loomd (PID $pid_b2)" "$work/rollback.out" || fail "loom rollback did not signal loomd $pid_b2"
expect_installed "$A"
expect "versions/ after rollback" "$(versions_saved)" "$B"
wait_until "loomd $B (PID $pid_b2) to exit" 15 process_gone "$pid_b2"
loom status >/dev/null
wait_until "loomd $A to answer" 10 daemon_is "$A"
pid_a2="$(daemon_pid)"
[ "$pid_a2" != "$pid_b2" ] || fail "loomd PID did not change after the rollback ($pid_a2)"
ok "loomd $A PID $pid_a2 (was $pid_b2)"
expect "failed_versions after rollback" "$(jget "$state_file" failed_versions)" "$C"
