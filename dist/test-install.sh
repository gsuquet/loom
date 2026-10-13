#!/bin/sh
# Tests for dist/install.sh against fake releases served by a local fake of the
# GitHub REST API (python3 -m http.server on 127.0.0.1).
#
# Usage: sh dist/test-install.sh   (or: mise run test-install)
#
# Signature cases need `minisign`; without it they are skipped with a message.

set -eu

here=$(cd "$(dirname "$0")" && pwd)
installer="$here/install.sh"
repo_path="repos/gsuquet/loom/releases"
targets="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl"

work=$(mktemp -d 2>/dev/null || mktemp -d -t loom-install-test)
server_pid=""
cleanup() {
    if [ -n "$server_pid" ]; then
        kill "$server_pid" 2>/dev/null || true
        wait "$server_pid" 2>/dev/null || true
    fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for tool in python3 curl tar; do
    command -v "$tool" >/dev/null 2>&1 || {
        echo "test-install: '$tool' is required" >&2
        exit 1
    }
done

if command -v sha256sum >/dev/null 2>&1; then
    sha256_files() { sha256sum "$@"; }
else
    sha256_files() { shasum -a 256 "$@"; }
fi

have_minisign=false
if command -v minisign >/dev/null 2>&1; then
    have_minisign=true
else
    echo "test-install: minisign not installed: signature cases are SKIPPED (run 'mise install')"
fi

# The installer must never pick up the caller's credentials or key override,
# and the stub loom must never write into the caller's loom home.
unset LOOM_GITHUB_TOKEN GITHUB_TOKEN LOOM_INSTALL_PUBKEY LOOM_HOME LOOM_TEST_STUB_FAIL

# --- results -----------------------------------------------------------------

passed=0
failed=0
out="$work/out"

ok() {
    passed=$((passed + 1))
    printf 'ok   %s\n' "$1"
}

ko() {
    failed=$((failed + 1))
    printf 'FAIL %s\n' "$1"
    if [ -f "$out" ]; then
        sed 's/^/     | /' "$out"
    fi
}

check() {
    # check DESCRIPTION COMMAND...: passes when COMMAND succeeds.
    desc=$1
    shift
    if "$@"; then ok "$desc"; else ko "$desc"; fi
}

# --- fake releases -----------------------------------------------------------

api="$work/api/$repo_path"
mkdir -p "$api/tags" "$api/assets" "$work/build" "$work/json"
next_id=1000

if $have_minisign; then
    minisign -G -W -p "$work/test.pub" -s "$work/test.key" >/dev/null 2>&1
    LOOM_INSTALL_PUBKEY=$(sed -n 2p "$work/test.pub")
    export LOOM_INSTALL_PUBKEY
fi

cat >"$work/release_json.py" <<'EOF'
# release_json.py TAG PRERELEASE STYLE ID=NAME... : prints a GitHub-like release.
# Key order and nesting vary between assets, as the installer must not depend on them.
import json, sys

tag, prerelease, style = sys.argv[1], sys.argv[2] == "true", sys.argv[3]
assets = []
for n, pair in enumerate(sys.argv[4:]):
    asset_id, name = pair.split("=", 1)
    uploader = {"login": "gsuquet", "id": 7, "name": "decoy-" + name}
    if n % 2 == 0:
        asset = {"url": "https://example.invalid/assets/" + asset_id, "id": int(asset_id),
                 "name": name, "uploader": uploader, "content_type": "application/gzip"}
    else:
        asset = {"uploader": uploader, "label": "", "name": name, "state": "uploaded",
                 "id": int(asset_id)}
    assets.append(asset)
release = {
    "url": "https://example.invalid/releases/1",
    "id": 1,
    "author": {"login": "gsuquet", "id": 7, "tag_name": "v0.0.0-decoy"},
    "tag_name": tag,
    "name": tag,
    "draft": False,
    "prerelease": prerelease,
    "assets": assets,
    "body": "Notes with \"quotes\", commas, {braces}, [brackets] and \\backslashes\\.",
}
if style == "pretty":
    print(json.dumps(release, indent=2))
else:
    print(json.dumps(release, separators=(",", ":")))
EOF

cat >"$work/release_list.py" <<'EOF'
# release_list.py FILE... : prints a pretty JSON array of the given releases.
import json, sys

print(json.dumps([json.load(open(f)) for f in sys.argv[1:]], indent=2))
EOF

# make_release VERSION MODE STYLE
#   MODE: ok | badsum (wrong checksum) | nosum (archive missing from SHA256SUMS)
#         | badsig (SHA256SUMS changed after signing) | nosig (no .minisig asset)
#   STYLE: pretty | compact JSON for releases/tags/vVERSION
make_release() {
    v=$1
    mode=$2
    style=$3
    d="$work/build/$v"
    mkdir -p "$d"
    for t in $targets; do
        s="$d/stage-$t"
        mkdir -p "$s"
        for bin in loom loomd; do
            {
                # shellcheck disable=SC2016 # $1 belongs to the generated stub.
                printf '#!/bin/sh\nif [ "${1:-}" = -V ]; then echo "%s %s"; exit 0; fi\n' "$bin" "$v"
                if [ "$bin" = loom ]; then
                    # Any other call is logged to $LOOM_HOME/loom-args, one line per
                    # call, and fails when LOOM_TEST_STUB_FAIL is set.
                    # shellcheck disable=SC2016 # Expanded by the generated stub.
                    printf '%s\n' \
                        'if [ -n "${LOOM_HOME:-}" ]; then printf "%s\\n" "$*" >>"$LOOM_HOME/loom-args"; fi' \
                        '[ -z "${LOOM_TEST_STUB_FAIL:-}" ] || exit 3'
                fi
            } >"$s/$bin"
            chmod 755 "$s/$bin"
        done
        echo "license" >"$s/LICENSE"
        echo "readme" >"$s/README.md"
        tar -czf "$d/loom-$v-$t.tar.gz" -C "$s" loom loomd LICENSE README.md
    done
    (cd "$d" && sha256_files loom-*.tar.gz | sort -k 2 >SHA256SUMS)
    case $mode in
        badsum)
            awk '{ print "0000000000000000000000000000000000000000000000000000000000000000  " $2 }' \
                "$d/SHA256SUMS" >"$d/SHA256SUMS.new"
            mv "$d/SHA256SUMS.new" "$d/SHA256SUMS"
            ;;
        nosum) sed "s/loom-$v-/loom-0.0.1-/" "$d/SHA256SUMS" >"$d/SHA256SUMS.new" && mv "$d/SHA256SUMS.new" "$d/SHA256SUMS" ;;
    esac
    if $have_minisign && [ "$mode" != nosig ]; then
        minisign -S -s "$work/test.key" -m "$d/SHA256SUMS" -x "$d/SHA256SUMS.minisig" >/dev/null 2>&1
    fi
    if [ "$mode" = badsig ]; then
        echo "0000000000000000000000000000000000000000000000000000000000000000  extra.tar.gz" >>"$d/SHA256SUMS"
    fi
    set --
    for f in "$d"/loom-*.tar.gz "$d/SHA256SUMS" "$d/SHA256SUMS.minisig"; do
        [ -f "$f" ] || continue
        next_id=$((next_id + 1))
        cp "$f" "$api/assets/$next_id"
        set -- "$@" "$next_id=$(basename "$f")"
    done
    case $v in *-dev.*) pre=true ;; *) pre=false ;; esac
    python3 -I "$work/release_json.py" "v$v" "$pre" "$style" "$@" >"$api/tags/v$v"
    python3 -I "$work/release_json.py" "v$v" "$pre" pretty "$@" >"$work/json/v$v"
}

make_release 1.0.0 ok pretty
make_release 1.1.0 ok compact
make_release 1.2.0-dev.3 ok compact
make_release 1.2.0-dev.12 ok pretty
make_release 1.2.0-dev.7 ok pretty
make_release 9.0.0 badsum pretty
make_release 9.1.0 nosum compact
make_release 9.2.0 badsig pretty
make_release 9.3.0 nosig pretty

cp "$api/tags/v1.1.0" "$api/latest"
# http.server serves the directory index for /releases?per_page=30 (after a
# redirect to /releases/?per_page=30). The list order is deliberately not the
# dev-number order: dev.12 must win over dev.7 and dev.3.
(cd "$work/json" && python3 -I "$work/release_list.py" v1.2.0-dev.3 v1.1.0 v1.2.0-dev.12 v1.2.0-dev.7 v1.0.0) \
    >"$api/index.html"

# --- fake API server ---------------------------------------------------------

port=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
log="$work/server.log"
python3 -I -m http.server "$port" --bind 127.0.0.1 --directory "$work/api" >"$log" 2>&1 &
server_pid=$!
LOOM_RELEASES_URL="http://127.0.0.1:$port"
export LOOM_RELEASES_URL

tries=0
until curl -fs -o /dev/null "$LOOM_RELEASES_URL/$repo_path/latest"; do
    tries=$((tries + 1))
    if [ "$tries" -ge 100 ]; then
        echo "test-install: the fake API server did not start" >&2
        cat "$log" >&2
        exit 1
    fi
    sleep 0.1
done

# --- helpers -----------------------------------------------------------------

# inst ARGS...: runs the installer, output in $out, exit code in $code.
inst() {
    code=0
    sh "$installer" "$@" >"$out" 2>&1 || code=$?
}

asset_downloads() {
    grep -c "GET /$repo_path/assets/" "$log" || true
}

reports() {
    # reports PREFIX VERSION: both binaries report VERSION.
    [ "$("$1/loom" -V)" = "loom $2" ] && [ "$("$1/loomd" -V)" = "loomd $2" ]
}

no_leftovers() {
    [ -z "$(find "$1" -name '.loom*.install.*' 2>/dev/null)" ]
}

exit_is() { [ "$code" -eq "$1" ]; }
says() { grep -q -- "$1" "$out"; }
silent_about() { ! grep -q -- "$1" "$out"; }

# Restricted PATH: the installer's tools, without minisign or gh.
restricted="$work/restricted-bin"
mkdir -p "$restricted"
for tool in sh curl tar gzip uname mktemp sha256sum shasum awk grep sed tr cat mkdir cp mv rm chmod sysctl dirname env sort; do
    p=$(command -v "$tool" 2>/dev/null || true)
    case $p in /*) ln -s "$p" "$restricted/$tool" ;; esac
done

# --- usage -------------------------------------------------------------------

inst --help
if exit_is 0 && says "Usage: install.sh"; then ok "--help exits 0 and prints usage"; else ko "--help exits 0 and prints usage"; fi

inst -h
check "-h exits 0" exit_is 0

inst --bogus
if exit_is 2 && says "unknown argument"; then ok "unknown option exits 2"; else ko "unknown option exits 2"; fi

inst --channel
check "--channel without a value exits 2" exit_is 2

inst --channel nightly
if exit_is 2 && says "invalid channel"; then ok "invalid channel exits 2"; else ko "invalid channel exits 2"; fi

inst --version 1.0
if exit_is 2 && says "invalid version"; then ok "invalid version exits 2"; else ko "invalid version exits 2"; fi

inst --channel dev --version 1.0.0
if exit_is 2 && says "cannot be combined"; then ok "--channel with --version exits 2"; else ko "--channel with --version exits 2"; fi

inst --version=1.0.0 --channel=stable
check "--channel=stable with --version= exits 2" exit_is 2

# --- fresh install -----------------------------------------------------------

p1="$work/p1/bin"
inst --prefix "$p1"
if exit_is 0 && reports "$p1" 1.1.0; then ok "fresh stable install installs the latest release"; else ko "fresh stable install installs the latest release"; fi
check "fresh install leaves no temporary file" no_leftovers "$p1"
check "PATH hint printed when the prefix is not in PATH" says "$p1 is not in your PATH"
if $have_minisign; then
    check "signature verified when minisign is installed" says "verified the SHA256SUMS signature"
fi

before=$(asset_downloads)
inst --prefix "$p1"
after=$(asset_downloads)
if exit_is 0 && says "already installed" && [ "$before" -eq "$after" ]; then ok "re-run with the same version is a no-op"; else ko "re-run with the same version is a no-op"; fi

inst --prefix "$p1" --version 1.1.0
after2=$(asset_downloads)
if exit_is 0 && says "already installed" && [ "$after" -eq "$after2" ]; then ok "re-run with an explicit installed version is a no-op"; else ko "re-run with an explicit installed version is a no-op"; fi

inst --prefix "$p1" --version 1.0.0
if exit_is 0 && reports "$p1" 1.0.0 && no_leftovers "$p1"; then ok "version switch replaces both binaries"; else ko "version switch replaces both binaries"; fi

p2="$work/p2"
inst --channel dev --prefix "$p2"
if exit_is 0 && reports "$p2" 1.2.0-dev.12; then ok "dev channel picks the highest dev number"; else ko "dev channel picks the highest dev number"; fi

inst --prefix "$p2" --version 1.2.0-dev.3
if exit_is 0 && reports "$p2" 1.2.0-dev.3; then ok "explicit dev version installs it"; else ko "explicit dev version installs it"; fi

inst --prefix "$p2" --version=1.1.0
if exit_is 0 && reports "$p2" 1.1.0; then ok "--version=X.Y.Z form installs it"; else ko "--version=X.Y.Z form installs it"; fi

p3="$work/p3"
PATH="$p3:$PATH" inst --prefix "$p3/"
if exit_is 0 && silent_about "not in your PATH"; then ok "no PATH hint when the prefix is in PATH"; else ko "no PATH hint when the prefix is in PATH"; fi

# --- failures leave the previous install intact ------------------------------

inst --prefix "$p1" --version 9.0.0
if exit_is 1 && says "checksum mismatch"; then ok "wrong checksum aborts"; else ko "wrong checksum aborts"; fi
if reports "$p1" 1.0.0 && no_leftovers "$p1"; then ok "wrong checksum leaves the previous install intact"; else ko "wrong checksum leaves the previous install intact"; fi

inst --prefix "$p1" --version 9.1.0
if exit_is 1 && says "is not listed in SHA256SUMS"; then ok "archive missing from SHA256SUMS aborts"; else ko "archive missing from SHA256SUMS aborts"; fi
check "missing checksum leaves the previous install intact" reports "$p1" 1.0.0

inst --prefix "$p1" --version 8.8.8
if exit_is 1 && says "release v8.8.8 not found"; then ok "unknown version aborts"; else ko "unknown version aborts"; fi

if $have_minisign; then
    inst --prefix "$p1" --version 9.2.0
    if exit_is 1 && says "invalid signature"; then ok "bad signature aborts"; else ko "bad signature aborts"; fi
    check "bad signature leaves the previous install intact" reports "$p1" 1.0.0

    inst --prefix "$p1" --version 9.3.0
    if exit_is 1 && says "has no asset .SHA256SUMS.minisig."; then ok "missing signature asset aborts when minisign is installed"; else ko "missing signature asset aborts when minisign is installed"; fi

    saved_key=$LOOM_INSTALL_PUBKEY
    unset LOOM_INSTALL_PUBKEY
    inst --prefix "$p1" --version 1.1.0
    if exit_is 1 && says "invalid signature" && reports "$p1" 1.0.0; then ok "a release not signed with the embedded key aborts"; else ko "a release not signed with the embedded key aborts"; fi
    LOOM_INSTALL_PUBKEY=$saved_key
    export LOOM_INSTALL_PUBKEY
else
    echo "skip bad signature, missing signature, embedded key (minisign not installed)"
fi

# --- remembered channel ------------------------------------------------------

# inst_env ASSIGNMENT... : runs the installer under `env ASSIGNMENT...`, so
# that no variable leaks into the next cases; arguments after `--`.
inst_env() {
    code=0
    set_env=""
    while [ $# -gt 0 ] && [ "$1" != -- ]; do
        set_env="$set_env $1"
        shift
    done
    [ $# -gt 0 ] && shift
    # shellcheck disable=SC2086 # Assignments without spaces, split on purpose.
    env $set_env sh "$installer" "$@" >"$out" 2>&1 || code=$?
}

# logged HOME LINES...: the stub loom was called exactly with LINES.
logged() {
    log_file="$1/loom-args"
    shift
    [ "$(cat "$log_file" 2>/dev/null || true)" = "$(printf '%s\n' "$@")" ]
}

h1="$work/h1"
mkdir -p "$h1"
inst_env LOOM_HOME="$h1" -- --channel dev --prefix "$work/pc1"
if exit_is 0 && reports "$work/pc1" 1.2.0-dev.12 && logged "$h1" "upgrade --channel dev --check --quiet" && says "follow the dev channel"; then ok "--channel dev records the channel with the installed loom"; else ko "--channel dev records the channel with the installed loom"; fi

inst_env LOOM_HOME="$h1" -- --channel=dev --prefix "$work/pc1"
if exit_is 0 && says "already installed" && logged "$h1" "upgrade --channel dev --check --quiet" "upgrade --channel dev --check --quiet"; then ok "an install already done still records the channel"; else ko "an install already done still records the channel"; fi

h2="$work/h2"
mkdir -p "$h2"
inst_env LOOM_HOME="$h2" -- --prefix "$work/pc2"
if exit_is 0 && reports "$work/pc2" 1.1.0 && [ ! -e "$h2/loom-args" ] && silent_about "follow the"; then ok "no --channel records nothing"; else ko "no --channel records nothing"; fi

inst_env LOOM_HOME="$h2" -- --version 1.2.0-dev.3 --prefix "$work/pc2"
if exit_is 0 && reports "$work/pc2" 1.2.0-dev.3 && [ ! -e "$h2/loom-args" ]; then ok "--version records no channel"; else ko "--version records no channel"; fi

h3="$work/h3"
mkdir -p "$h3"
inst_env LOOM_HOME="$h3" LOOM_TEST_STUB_FAIL=1 -- --channel stable --prefix "$work/pc3"
if exit_is 0 && reports "$work/pc3" 1.1.0 && logged "$h3" "upgrade --channel stable --check --quiet" && says "warning: could not record the stable channel"; then ok "a failure to record the channel is a warning"; else ko "a failure to record the channel is a warning"; fi

# --- environment -------------------------------------------------------------

p4="$work/p4"
PATH="$restricted" inst --prefix "$p4"
if exit_is 0 && says "minisign not found" && reports "$p4" 1.1.0; then ok "missing minisign warns and still installs"; else ko "missing minisign warns and still installs"; fi

LOOM_GITHUB_TOKEN="loom-test-token-0123" inst --prefix "$work/p5"
if exit_is 0 && silent_about "loom-test-token-0123"; then ok "a token is accepted and never printed"; else ko "a token is accepted and never printed"; fi

fake_os="$work/fake-os"
mkdir -p "$fake_os"
cp -P "$restricted"/* "$fake_os"/
rm "$fake_os/uname"
# shellcheck disable=SC2016 # $1 belongs to the generated script.
printf '#!/bin/sh\ncase "$1" in -s) echo FreeBSD ;; -m) echo amd64 ;; esac\n' >"$fake_os/uname"
chmod 755 "$fake_os/uname"
PATH="$fake_os" inst --prefix "$work/p6"
if exit_is 1 && says "unsupported operating system" && says "x86_64-unknown-linux-musl"; then ok "unsupported OS fails and lists the supported targets"; else ko "unsupported OS fails and lists the supported targets"; fi

# --- embedded key ------------------------------------------------------------

if [ -f "$here/minisign.pub" ]; then
    committed=$(sed -n 2p "$here/minisign.pub")
    check "embedded public key matches dist/minisign.pub" grep -q "^EMBEDDED_PUBKEY=\"$committed\"$" "$installer"
else
    echo "skip embedded key vs dist/minisign.pub (file not present)"
fi

# --- summary -----------------------------------------------------------------

printf '\n%d passed, %d failed\n' "$passed" "$failed"
[ "$failed" -eq 0 ]
