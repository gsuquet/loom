#!/usr/bin/env bash
# Packages release archives and writes or verifies SHA256SUMS.
# Used by .github/workflows/release.yml, for both the pull request dry run and the publish path.
#
# Usage:
#   dist/package.sh archive <version> <target> <bin-dir> <out-dir>
#       Writes <out-dir>/loom-<version>-<target>.tar.gz with loom, loomd, LICENSE
#       and README.md at the archive root. <bin-dir> holds the built binaries.
#   dist/package.sh checksums <dir>
#       Writes <dir>/SHA256SUMS over every *.tar.gz in <dir> and <dir>/install.sh
#       (when present: the release workflow always copies it), one "<hex>  <file>" line per file, sorted by file name.
#   dist/package.sh verify <dir>
#       Checks every file listed in <dir>/SHA256SUMS.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

die() {
    printf 'package.sh: %s\n' "$*" >&2
    exit 1
}

usage() {
    sed -n '4,13p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

# Prints the SHA-256 tool available on this machine.
sha256_tool() {
    if command -v sha256sum >/dev/null 2>&1; then
        echo sha256sum
    elif command -v shasum >/dev/null 2>&1; then
        echo "shasum -a 256"
    else
        die "neither sha256sum nor shasum is installed"
    fi
}

cmd_archive() {
    [ "$#" -eq 4 ] || usage
    local version="$1" target="$2" bin_dir="$3" out_dir="$4"

    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "invalid version: $version"
    [[ "$target" =~ ^[a-z0-9_]+(-[a-z0-9_]+)+$ ]] || die "invalid target: $target"
    local file
    for file in "$bin_dir/loom" "$bin_dir/loomd"; do
        [ -f "$file" ] && [ -x "$file" ] || die "missing executable: $file"
    done
    for file in "$repo_root/LICENSE" "$repo_root/README.md"; do
        [ -f "$file" ] || die "missing file: $file"
    done

    mkdir -p "$out_dir"
    local stage
    stage="$(mktemp -d)"
    # shellcheck disable=SC2064 # expand now: $stage is local to this function
    trap "rm -rf '$stage'" EXIT
    install -m 755 "$bin_dir/loom" "$bin_dir/loomd" "$stage/"
    install -m 644 "$repo_root/LICENSE" "$repo_root/README.md" "$stage/"

    local archive
    archive="$(cd "$out_dir" && pwd)/loom-$version-$target.tar.gz"
    # COPYFILE_DISABLE keeps macOS tar from adding AppleDouble (._*) entries.
    COPYFILE_DISABLE=1 tar -czf "$archive" -C "$stage" loom loomd LICENSE README.md
    printf '%s\n' "$archive"
}

cmd_checksums() {
    [ "$#" -eq 1 ] || usage
    local dir="$1" tool
    [ -d "$dir" ] || die "not a directory: $dir"
    tool="$(sha256_tool)"

    local files=() file
    for file in "$dir"/*.tar.gz "$dir/install.sh"; do
        [ -f "$file" ] && files+=("$(basename "$file")")
    done
    [ "${#files[@]}" -gt 0 ] || die "no archive in $dir"

    (
        cd "$dir"
        printf '%s\n' "${files[@]}" | LC_ALL=C sort | while IFS= read -r file; do
            # Both tools print "<hex>  <file>"; word splitting of $tool is intended.
            # shellcheck disable=SC2086
            $tool -- "$file"
        done >SHA256SUMS
    )
    printf '%s\n' "$dir/SHA256SUMS"
}

cmd_verify() {
    [ "$#" -eq 1 ] || usage
    local dir="$1" tool
    [ -f "$dir/SHA256SUMS" ] || die "missing file: $dir/SHA256SUMS"
    tool="$(sha256_tool)"
    # shellcheck disable=SC2086
    (cd "$dir" && $tool -c SHA256SUMS)
}

[ "$#" -ge 1 ] || usage
command="$1"
shift
case "$command" in
archive) cmd_archive "$@" ;;
checksums) cmd_checksums "$@" ;;
verify) cmd_verify "$@" ;;
*) usage ;;
esac
