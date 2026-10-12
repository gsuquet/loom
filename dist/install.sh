#!/bin/sh
# loom installer: downloads, verifies and installs `loom` and `loomd`.
#
#   curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh
#   sh install.sh [--channel stable|dev] [--version X.Y.Z] [--prefix DIR]
#
# Reference: docs/reference/install-script.md

set -eu

REPO="gsuquet/loom"
# minisign public key of the release signing key (key ID 0C6B3C0017776D53).
EMBEDDED_PUBKEY="RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+"
SUPPORTED_TARGETS="aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl"

tmp_dir=""
tmp_loom=""
tmp_loomd=""

say() {
    printf 'loom-install: %s\n' "$*" >&2
}

warn() {
    printf 'loom-install: warning: %s\n' "$*" >&2
}

die() {
    printf 'loom-install: error: %s\n' "$*" >&2
    exit 1
}

usage_error() {
    printf 'loom-install: error: %s\n' "$*" >&2
    printf "Run 'install.sh --help' for usage.\n" >&2
    exit 2
}

usage() {
    cat <<'EOF'
Install loom and loomd from a GitHub release.

Usage: install.sh [--channel stable|dev] [--version X.Y.Z] [--prefix DIR]

Options:
  --channel <CHANNEL>  stable (default): latest stable release.
                       dev: latest development prerelease.
  --version <VERSION>  Exact version to install: X.Y.Z or X.Y.Z-dev.N.
                       Cannot be combined with --channel.
  --prefix <DIR>       Directory for the binaries (default: $HOME/.local/bin).
  -h, --help           Print this help.

Environment:
  LOOM_GITHUB_TOKEN, GITHUB_TOKEN  Optional GitHub token (raises the API rate limit).
EOF
}

cleanup() {
    [ -n "$tmp_loom" ] && rm -f "$tmp_loom"
    [ -n "$tmp_loomd" ] && rm -f "$tmp_loomd"
    [ -n "$tmp_dir" ] && rm -rf "$tmp_dir"
    return 0
}

# --- arguments ---------------------------------------------------------------

channel=""
version=""
prefix=""

need_value() {
    # $1: option name, $2: number of remaining arguments
    [ "$2" -ge 2 ] || usage_error "$1 requires a value"
}

while [ $# -gt 0 ]; do
    case $1 in
        -h | --help)
            usage
            exit 0
            ;;
        --channel)
            need_value "$1" $#
            channel=$2
            shift 2
            ;;
        --channel=*)
            channel=${1#--channel=}
            shift
            ;;
        --version)
            need_value "$1" $#
            version=$2
            shift 2
            ;;
        --version=*)
            version=${1#--version=}
            shift
            ;;
        --prefix)
            need_value "$1" $#
            prefix=$2
            shift 2
            ;;
        --prefix=*)
            prefix=${1#--prefix=}
            shift
            ;;
        *)
            usage_error "unknown argument '$1'"
            ;;
    esac
done

if [ -n "$channel" ] && [ -n "$version" ]; then
    usage_error "--channel and --version cannot be combined"
fi

case $channel in
    "" | stable | dev) ;;
    *) usage_error "invalid channel '$channel' (expected stable or dev)" ;;
esac

is_version() {
    printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-dev\.[0-9]+)?$'
}

if [ -n "$version" ] && ! is_version "$version"; then
    usage_error "invalid version '$version' (expected X.Y.Z or X.Y.Z-dev.N)"
fi

if [ -z "$prefix" ]; then
    [ -n "${HOME:-}" ] || die "HOME is not set; pass --prefix"
    prefix="$HOME/.local/bin"
fi
case $prefix in
    /*) ;;
    *) prefix="$(pwd)/$prefix" ;;
esac
# Strip trailing slashes so the PATH check compares like with like.
while [ "${#prefix}" -gt 1 ] && [ "${prefix%/}" != "$prefix" ]; do
    prefix=${prefix%/}
done

# --- platform and tools ------------------------------------------------------

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case $arch in
        arm64 | aarch64) arch=aarch64 ;;
        x86_64 | amd64) arch=x86_64 ;;
        *) die "unsupported architecture '$arch'; supported targets: $SUPPORTED_TARGETS" ;;
    esac
    case $os in
        Darwin)
            # An x86_64 shell under Rosetta on Apple silicon still gets the native build.
            if [ "$arch" = x86_64 ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = 1 ]; then
                arch=aarch64
            fi
            printf '%s-apple-darwin\n' "$arch"
            ;;
        Linux)
            printf '%s-unknown-linux-musl\n' "$arch"
            ;;
        *) die "unsupported operating system '$os'; supported targets: $SUPPORTED_TARGETS" ;;
    esac
}

for tool in curl tar mktemp; do
    command -v "$tool" >/dev/null 2>&1 || die "'$tool' is required but was not found in PATH"
done

if command -v sha256sum >/dev/null 2>&1; then
    sha256() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
    sha256() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
    die "neither 'sha256sum' nor 'shasum' was found; one is required to verify the download"
fi

target=$(detect_target)

api_url=${LOOM_RELEASES_URL:-https://api.github.com}
api_url=${api_url%/}
pubkey=${LOOM_INSTALL_PUBKEY:-$EMBEDDED_PUBKEY}

# --- installed version -------------------------------------------------------

# True when both binaries in the prefix report version $1.
installed_is() {
    [ -x "$prefix/loom" ] && [ -x "$prefix/loomd" ] || return 1
    [ "$("$prefix/loom" -V 2>/dev/null || true)" = "loom $1" ] || return 1
    [ "$("$prefix/loomd" -V 2>/dev/null || true)" = "loomd $1" ] || return 1
}

already_installed() {
    say "loom $1 is already installed in $prefix"
    exit 0
}

if [ -n "$version" ] && installed_is "$version"; then
    already_installed "$version"
fi

# --- HTTP --------------------------------------------------------------------

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
tmp_dir=$(mktemp -d 2>/dev/null || mktemp -d -t loom-install)

# The token goes into a header file (mode 600) so it never shows up in the
# process list or in any message.
auth_header=""
token=${LOOM_GITHUB_TOKEN:-${GITHUB_TOKEN:-}}
# `gh auth token` is a github.com credential: only send it to github.com.
if [ -z "$token" ] && [ -z "${LOOM_RELEASES_URL:-}" ] && command -v gh >/dev/null 2>&1; then
    token=$(gh auth token 2>/dev/null || true)
fi
if [ -n "$token" ]; then
    auth_header="$tmp_dir/auth-header"
    (umask 077 && printf 'Authorization: Bearer %s\n' "$token" >"$auth_header")
fi
token=""

# http_get URL ACCEPT OUTPUT: prints the HTTP status code.
http_get() {
    url=$1
    set -- -sSL --retry 2 -H "Accept: $2" -H "X-GitHub-Api-Version: 2022-11-28" -o "$3" -w '%{http_code}'
    if [ -n "${LOOM_RELEASES_URL:-}" ]; then
        set -- "$@" --proto '=http,https'
    else
        set -- "$@" --proto '=https' --proto-redir '=https' --tlsv1.2
    fi
    if [ -n "$auth_header" ]; then
        set -- "$@" -H "@$auth_header"
    fi
    curl "$@" "$url"
}

# fetch URL ACCEPT OUTPUT WHAT: downloads or exits with a clear error.
fetch() {
    if ! code=$(http_get "$1" "$2" "$3"); then
        die "cannot download $4 from $1"
    fi
    case $code in
        2??) ;;
        404) die "$4 not found ($1)" ;;
        401 | 403 | 429) die "HTTP $code while downloading $4 ($1): access denied or API rate limit exceeded; set LOOM_GITHUB_TOKEN or GITHUB_TOKEN" ;;
        *) die "HTTP $code while downloading $4 ($1)" ;;
    esac
}

# --- release metadata --------------------------------------------------------

# Flattens JSON into "path<TAB>value" lines, one per scalar value, e.g.
# "assets/2/name<TAB>SHA256SUMS". Strings keep their JSON escapes. Strings never
# span lines in valid JSON, so the parser state carries over between lines.
json_flatten() {
    LC_ALL=C awk '
        BEGIN { depth = 0; want_key = 0 }
        function emit(v,   p, l) {
            p = ""
            for (l = 1; l <= depth; l++) {
                if (l > 1) p = p "/"
                p = p (type[l] == "o" ? key[l] : idx[l])
            }
            print p "\t" v
        }
        {
            line = $0
            n = length(line)
            i = 1
            while (i <= n) {
                c = substr(line, i, 1)
                if (c == "\"") {
                    j = i + 1
                    while (j <= n) {
                        d = substr(line, j, 1)
                        if (d == "\\") { j += 2; continue }
                        if (d == "\"") break
                        j++
                    }
                    s = substr(line, i + 1, j - i - 1)
                    i = j + 1
                    if (depth > 0 && type[depth] == "o" && want_key) { key[depth] = s; want_key = 0 }
                    else emit(s)
                } else if (c == "{") {
                    depth++; type[depth] = "o"; key[depth] = ""; want_key = 1; i++
                } else if (c == "[") {
                    depth++; type[depth] = "a"; idx[depth] = 0; want_key = 0; i++
                } else if (c == "}" || c == "]") {
                    depth--; want_key = 0; i++
                } else if (c == ",") {
                    if (type[depth] == "o") want_key = 1; else idx[depth]++
                    i++
                } else if (c == ":" || c == " " || c == "\t" || c == "\r") {
                    i++
                } else {
                    j = i
                    while (j <= n) {
                        d = substr(line, j, 1)
                        if (d == "," || d == "}" || d == "]" || d == " " || d == "\t" || d == "\r") break
                        j++
                    }
                    emit(substr(line, i, j - i))
                    i = j
                }
            }
        }
    ' "$1"
}

# field FLAT KEY: value of a top-level key of a flattened release.
field() {
    awk -F '\t' -v k="$2" '$1 == k { print $2; exit }' "$1"
}

# asset_id FLAT NAME: id of the asset named NAME, empty when absent.
asset_id() {
    awk -F '\t' -v want="$2" '
        $1 ~ /^assets\/[0-9]+\/name$/ && $2 == want { split($1, a, "/"); hit[a[2]] = 1 }
        $1 ~ /^assets\/[0-9]+\/id$/ { split($1, a, "/"); id[a[2]] = $2 }
        END { for (k in hit) if (k in id) print id[k] }
    ' "$1"
}

release_flat="$tmp_dir/release.flat"
releases_url="$api_url/repos/$REPO/releases"

if [ -n "$version" ]; then
    say "looking up release v$version"
    fetch "$releases_url/tags/v$version" "application/vnd.github+json" "$tmp_dir/release.json" "release v$version"
    json_flatten "$tmp_dir/release.json" >"$release_flat"
elif [ "$channel" = dev ]; then
    say "looking up the latest dev prerelease"
    fetch "$releases_url?per_page=30" "application/vnd.github+json" "$tmp_dir/releases.json" "the release list"
    json_flatten "$tmp_dir/releases.json" >"$tmp_dir/releases.flat"
    # Dev builds are ordered by their run number, not by list position.
    best=$(awk -F '\t' '
        $1 ~ /^[0-9]+\/tag_name$/ { split($1, a, "/"); tag[a[1]] = $2 }
        $1 ~ /^[0-9]+\/draft$/ { split($1, a, "/"); draft[a[1]] = $2 }
        END {
            top = -1
            for (k in tag) {
                if (draft[k] == "true") continue
                t = tag[k]
                if (t !~ /^v[0-9]+\.[0-9]+\.[0-9]+-dev\.[0-9]+$/) continue
                n = t; sub(/.*-dev\./, "", n); n += 0
                if (n > top) { top = n; best = k }
            }
            if (top >= 0) print best
        }
    ' "$tmp_dir/releases.flat")
    [ -n "$best" ] || die "no dev prerelease found"
    awk -F '\t' -v p="$best/" 'index($1, p) == 1 { print substr($1, length(p) + 1) "\t" $2 }' \
        "$tmp_dir/releases.flat" >"$release_flat"
else
    say "looking up the latest stable release"
    fetch "$releases_url/latest" "application/vnd.github+json" "$tmp_dir/release.json" "the latest release"
    json_flatten "$tmp_dir/release.json" >"$release_flat"
fi

tag=$(field "$release_flat" tag_name)
release_version=${tag#v}
if [ "$tag" = "$release_version" ] || ! is_version "$release_version"; then
    die "unexpected release tag '$tag'"
fi
if [ -n "$version" ] && [ "$release_version" != "$version" ]; then
    die "asked for v$version, the API returned '$tag'"
fi

if installed_is "$release_version"; then
    already_installed "$release_version"
fi

# --- download and verify -----------------------------------------------------

# download_asset NAME OUTPUT
download_asset() {
    id=$(asset_id "$release_flat" "$1")
    case $id in
        "") die "release $tag has no asset '$1'" ;;
        *[!0-9]*) die "release $tag lists asset '$1' more than once or with an invalid id" ;;
    esac
    fetch "$releases_url/assets/$id" "application/octet-stream" "$2" "$1"
}

archive="loom-$release_version-$target.tar.gz"
say "installing loom $release_version ($target) into $prefix"

download_asset SHA256SUMS "$tmp_dir/SHA256SUMS"

if command -v minisign >/dev/null 2>&1; then
    download_asset SHA256SUMS.minisig "$tmp_dir/SHA256SUMS.minisig"
    if ! minisign -V -q -P "$pubkey" -m "$tmp_dir/SHA256SUMS" -x "$tmp_dir/SHA256SUMS.minisig" >/dev/null 2>&1; then
        die "invalid signature on SHA256SUMS of release $tag; nothing was installed"
    fi
    say "verified the SHA256SUMS signature"
else
    warn "minisign not found: the SHA256SUMS signature is not verified (checksums still are); install minisign to verify authenticity"
fi

expected=$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp_dir/SHA256SUMS")
case $expected in
    "") die "$archive is not listed in SHA256SUMS; nothing was installed" ;;
    *[!0-9a-fA-F]*) die "SHA256SUMS lists $archive more than once or with an invalid checksum; nothing was installed" ;;
esac

download_asset "$archive" "$tmp_dir/$archive"
actual=$(sha256 "$tmp_dir/$archive")
expected=$(printf '%s' "$expected" | tr 'A-F' 'a-f')
if [ "$actual" != "$expected" ]; then
    die "checksum mismatch for $archive (expected $expected, got $actual); nothing was installed"
fi
say "verified the checksum of $archive"

# --- install -----------------------------------------------------------------

extract="$tmp_dir/extract"
mkdir "$extract"
tar -xzf "$tmp_dir/$archive" -C "$extract" || die "cannot extract $archive"
for bin in loom loomd; do
    [ -f "$extract/$bin" ] || die "$archive does not contain '$bin'"
done

mkdir -p "$prefix" || die "cannot create $prefix"

# Copy next to the targets first, then rename: each rename is atomic on the
# same filesystem, and a failure before it leaves the previous install intact.
tmp_loom="$prefix/.loom.install.$$"
tmp_loomd="$prefix/.loomd.install.$$"
cp "$extract/loom" "$tmp_loom" || die "cannot write to $prefix"
cp "$extract/loomd" "$tmp_loomd" || die "cannot write to $prefix"
chmod 755 "$tmp_loom" "$tmp_loomd" || die "cannot set permissions in $prefix"
mv -f "$tmp_loom" "$prefix/loom" || die "cannot replace $prefix/loom"
tmp_loom=""
mv -f "$tmp_loomd" "$prefix/loomd" || die "cannot replace $prefix/loomd"
tmp_loomd=""

say "installed loom and loomd $release_version into $prefix"

case ":${PATH:-}:" in
    *":$prefix:"* | *":$prefix/:"*) ;;
    *)
        say "$prefix is not in your PATH; add it to your shell profile, for example:"
        # shellcheck disable=SC2016 # $PATH is printed literally on purpose.
        printf '    export PATH="%s:$PATH"\n' "$prefix" >&2
        ;;
esac
