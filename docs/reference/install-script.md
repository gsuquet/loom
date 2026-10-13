# Install script reference

> `install.sh`, the installer for `loom` and `loomd`, as shipped. Source: [`dist/install.sh`](../../dist/install.sh). Procedures: [Install loom](../how-to-guides/install.md).

## Synopsis

```text
install.sh [--channel stable|dev] [--version X.Y.Z] [--prefix DIR]
install.sh -h|--help
```

POSIX `sh` script, attached to every GitHub release as the `install.sh` asset. Values are given as `--opt value` or `--opt=value`. When an option is repeated, the last value wins.

## Options

| Option | Default | Behavior |
| --- | --- | --- |
| `--channel <CHANNEL>` | `stable` | `stable`: the release GitHub marks as latest. `dev`: among the 30 most recent releases, the non-draft release whose tag is `vX.Y.Z-dev.N` with the highest `N`. When given, the channel is also recorded for the auto-upgrade of `loomd` and for `loom upgrade` (see [Remembered channel](#remembered-channel)). |
| `--version <VERSION>` | none | Installs release `v<VERSION>`. `VERSION` is `X.Y.Z` or `X.Y.Z-dev.N`, without the leading `v`. Cannot be combined with `--channel`. |
| `--prefix <DIR>` | `$HOME/.local/bin` | Directory receiving `loom` and `loomd`. Created if missing. A relative path is resolved against the current directory. |
| `-h`, `--help` | | Prints the usage on stdout and exits with code 0. |

## Environment variables

| Variable | Effect |
| --- | --- |
| `LOOM_GITHUB_TOKEN` | GitHub token sent as `Authorization: Bearer <token>`. Optional: raises the API rate limit. |
| `GITHUB_TOKEN` | Used when `LOOM_GITHUB_TOKEN` is unset or empty. |
| `HOME` | Base of the default prefix. Required when `--prefix` is not given. |
| `LOOM_HOME` | Passed to `loom` when the channel is recorded: the loom home whose `state/update.json` receives it (default `~/.loom`). |
| `PATH` | Locates the required tools and `minisign`; checked for the PATH hint. |

When neither token variable is set and `gh` is installed, the script uses the output of `gh auth token`, only when `LOOM_RELEASES_URL` is unset. The token is written to a mode `600` file in the temporary directory and passed to `curl` with `-H @file`: it never appears in the process list or in the output.

Test hooks, for tests and mirrors:

| Variable | Effect |
| --- | --- |
| `LOOM_RELEASES_URL` | Replaces `https://api.github.com` as the base URL of every request. Allows `http://`. Disables the `gh auth token` fallback. |
| `LOOM_INSTALL_PUBKEY` | minisign public key (base64 key line) used instead of the embedded release key. |

## Platforms

| `uname -s` | `uname -m` | Target |
| --- | --- | --- |
| `Darwin` | `arm64`, `aarch64` | `aarch64-apple-darwin` |
| `Darwin` | `x86_64`, `amd64` | `x86_64-apple-darwin`, or `aarch64-apple-darwin` when the shell runs under Rosetta 2 (`sysctl.proc_translated` is `1`) |
| `Linux` | `arm64`, `aarch64` | `aarch64-unknown-linux-musl` |
| `Linux` | `x86_64`, `amd64` | `x86_64-unknown-linux-musl` |

Any other system or architecture is an error that lists the supported targets.

## Required tools

`curl`, `tar`, `mktemp`, `uname`, and `sha256sum` or `shasum` (`shasum -a 256`). The script exits with code 1 if one is missing. `minisign` is optional (see [Verification](#verification)).

## Requests

All requests go to the GitHub REST API with `Accept: application/vnd.github+json` (metadata) or `Accept: application/octet-stream` (assets), and `X-GitHub-Api-Version: 2022-11-28`. Without `LOOM_RELEASES_URL`, `curl` only accepts HTTPS (TLS 1.2 or later), redirects included.

| Selection | Request |
| --- | --- |
| stable channel | `GET /repos/gsuquet/loom/releases/latest` |
| dev channel | `GET /repos/gsuquet/loom/releases?per_page=30` |
| `--version V` | `GET /repos/gsuquet/loom/releases/tags/vV` |
| each asset | `GET /repos/gsuquet/loom/releases/assets/<asset id>` |

The JSON responses are parsed with `awk`; `jq` is not needed.

## Verification

Run in this order, before anything is written to the prefix. Any failure exits with code 1.

1. The selected release tag must be `vX.Y.Z` or `vX.Y.Z-dev.N`, and equal to `v<VERSION>` with `--version`.
2. `SHA256SUMS` is downloaded.
3. If `minisign` is in `PATH`: `SHA256SUMS.minisig` is downloaded and `SHA256SUMS` is verified with the embedded public key (`RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+`, key ID `0C6B3C0017776D53`). A missing signature asset or an invalid signature aborts. If `minisign` is not in `PATH`: a warning on stderr, and installation continues with checksum verification only.
4. `SHA256SUMS` must contain exactly one line for `loom-<version>-<target>.tar.gz`.
5. The archive is downloaded and its SHA-256 must match that line.
6. The archive must contain `loom` and `loomd` at its root.

## Files written

| Path | Content |
| --- | --- |
| `<prefix>/loom` | `loom` binary, mode `755`. |
| `<prefix>/loomd` | `loomd` binary, mode `755`. |
| `<prefix>/.loom.install.<pid>`, `<prefix>/.loomd.install.<pid>` | Copies of the new binaries, renamed to `loom` and `loomd` with `mv` (atomic per file on the same filesystem). Removed on exit if a step fails. |
| temporary directory from `mktemp -d` | Release metadata, `SHA256SUMS`, signature, archive. Removed on exit. |

With `--channel`, the installed `loom` writes `<loom home>/state/update.json` (see [Remembered channel](#remembered-channel)). No other file is written. Shell startup files (`.profile`, `.bashrc`, `.zshrc`, fish configuration) are never edited.

## Behavior

- **Already installed:** when `<prefix>/loom -V` prints `loom <version>` and `<prefix>/loomd -V` prints `loomd <version>` for the selected version, the script prints `loom <version> is already installed in <prefix>` and exits with code 0 without downloading any asset. With `--version`, this check runs before any request.
- **Other version installed:** both binaries are replaced.
- **Failure:** any failure before the renames leaves `<prefix>/loom` and `<prefix>/loomd` unchanged.
- **Remembered channel:** see [Remembered channel](#remembered-channel).
- **PATH hint:** when `<prefix>` is not an entry of `PATH`, the script prints the `export PATH=...` line to add to a shell profile.

## Remembered channel

Only when `--channel` is given: after a successful install, or when the selected version is already installed, the script runs the installed binary:

```text
<prefix>/loom upgrade --channel <CHANNEL> --check --quiet
```

stdin is closed and stdout discarded; the environment is passed through, `LOOM_HOME` included. `loom` records the channel in `channel` of `<loom home>/state/update.json` with its own code, so that `loomd` and `loom upgrade` keep following it after an upgrade to a build of another channel (a `dev` install moved to a stable release stays on `dev`). `[update] channel` in `config.toml` still takes precedence ([channel resolution](configuration.md#channel-resolution)). The check queries the releases API once more.

On success the script prints `loomd and loom upgrade will follow the <CHANNEL> channel`. When the command fails, the script prints `warning: could not record the <CHANNEL> channel; run: <prefix>/loom upgrade --channel <CHANNEL> --check` and still exits with code `0`: the binaries are installed.

Without `--channel` (the `stable` default) or with `--version`, nothing is recorded.

## Output

Messages go to stderr, prefixed with `loom-install:`; warnings with `loom-install: warning:`, errors with `loom-install: error:`. stdout only receives `--help` output.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Installed, already installed, or `--help`. |
| `1` | Error: unsupported platform, missing tool, HTTP or network failure, release or asset not found, verification failure, write failure. |
| `2` | Usage error: unknown argument, missing value, invalid channel or version, `--channel` combined with `--version`. |
| `130`, `143` | Interrupted by `SIGINT` or `SIGTERM`. |

## Tests

`mise run test-install` runs [`dist/test-install.sh`](../../dist/test-install.sh) against fake releases served by `python3 -m http.server` on `127.0.0.1`, through `LOOM_RELEASES_URL` and `LOOM_INSTALL_PUBKEY`. `mise run shellcheck` lints `dist/*.sh`. Both are part of `mise run check`.
