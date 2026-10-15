# Install script reference

> `install.sh`, the installer for `loom` and `loomd`, as shipped. Source: [`dist/install.sh`](../../dist/install.sh). Procedures: [Install loom](../how-to-guides/install.md).

## Synopsis

```text
install.sh [--channel stable|dev] [--version X.Y.Z] [--prefix DIR]
install.sh -h|--help
```

A POSIX `sh` script. Each GitHub release has it as the `install.sh` asset. Give a value as `--opt value` or `--opt=value`. If an option occurs more than once, the script uses the last value.

## Options

| Option | Default | Behavior |
| --- | --- | --- |
| `--channel <CHANNEL>` | `stable` | `stable`: the release GitHub marks as latest. `dev`: among the 30 most recent releases, the non-draft release whose tag is `vX.Y.Z-dev.N` with the highest `N`. If you give this option, the script also records the channel for the auto-upgrade of `loomd` and for `loom upgrade` (see [Remembered channel](#remembered-channel)). |
| `--version <VERSION>` | none | Installs release `v<VERSION>`. `VERSION` is `X.Y.Z` or `X.Y.Z-dev.N`, without the leading `v`. Cannot be combined with `--channel`. |
| `--prefix <DIR>` | `$HOME/.local/bin` | The directory that receives `loom` and `loomd`. If the directory does not exist, the script creates it. A relative path is relative to the current directory. |
| `-h`, `--help` | | Prints the usage on stdout and exits with code 0. |

## Environment variables

| Variable | Effect |
| --- | --- |
| `LOOM_GITHUB_TOKEN` | A GitHub token, sent as `Authorization: Bearer <token>`. Optional. It increases the API rate limit. |
| `GITHUB_TOKEN` | The token if `LOOM_GITHUB_TOKEN` is not set or is empty. |
| `HOME` | The base of the default prefix. Necessary if you do not give `--prefix`. |
| `LOOM_HOME` | Passed to `loom` when the script records the channel. It is the loom home whose `state/update.json` receives the channel (default `~/.loom`). |
| `PATH` | Gives the location of the required tools and of `minisign`. The script also uses it for the PATH hint. |

If no token variable is set and `gh` is installed, the script uses the output of `gh auth token`. It does this only if `LOOM_RELEASES_URL` is not set. The script writes the token to a mode `600` file in the temporary directory. It gives this file to `curl` with `-H @file`. Thus the token never shows in the process list or in the output.

Test hooks, for tests and mirrors:

| Variable | Effect |
| --- | --- |
| `LOOM_RELEASES_URL` | Replaces `https://api.github.com` as the base URL of each request. Allows `http://`. Disables the `gh auth token` fallback. |
| `LOOM_INSTALL_PUBKEY` | A minisign public key (base64 key line). The script uses it instead of the embedded release key. |

## Platforms

| `uname -s` | `uname -m` | Target |
| --- | --- | --- |
| `Darwin` | `arm64`, `aarch64` | `aarch64-apple-darwin` |
| `Darwin` | `x86_64`, `amd64` | `x86_64-apple-darwin`, or `aarch64-apple-darwin` when the shell runs under Rosetta 2 (`sysctl.proc_translated` is `1`) |
| `Linux` | `arm64`, `aarch64` | `aarch64-unknown-linux-musl` |
| `Linux` | `x86_64`, `amd64` | `x86_64-unknown-linux-musl` |

For a different system or architecture, the script stops with an error that lists the supported targets.

## Required tools

`curl`, `tar`, `mktemp`, `uname`, and `sha256sum` or `shasum` (`shasum -a 256`). If one of these tools is not there, the script exits with code 1. `minisign` is optional (see [Verification](#verification)).

## Requests

All requests go to the GitHub REST API with `X-GitHub-Api-Version: 2022-11-28`. They use `Accept: application/vnd.github+json` (metadata) or `Accept: application/octet-stream` (assets). Without `LOOM_RELEASES_URL`, `curl` accepts only HTTPS (TLS 1.2 or later), also for redirects.

| Selection | Request |
| --- | --- |
| stable channel | `GET /repos/gsuquet/loom/releases/latest` |
| dev channel | `GET /repos/gsuquet/loom/releases?per_page=30` |
| `--version V` | `GET /repos/gsuquet/loom/releases/tags/vV` |
| each asset | `GET /repos/gsuquet/loom/releases/assets/<asset id>` |

The script parses the JSON responses with `awk`. It does not need `jq`.

## Verification

The script does these checks in this order, before it writes to the prefix. If a check fails, the script exits with code 1.

1. The selected release tag must be `vX.Y.Z` or `vX.Y.Z-dev.N`, and equal to `v<VERSION>` with `--version`.
2. The script downloads `SHA256SUMS`.
3. If `minisign` is in `PATH`, the script downloads `SHA256SUMS.minisig`. It verifies `SHA256SUMS` with the embedded public key (`RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+`, key ID `0C6B3C0017776D53`). If the signature asset is not there or the signature is not valid, the script stops. If `minisign` is not in `PATH`, the script writes a warning on stderr. Then it continues with only the checksum verification.
4. `SHA256SUMS` must contain exactly one line for `loom-<version>-<target>.tar.gz`.
5. The script downloads the archive. Its SHA-256 must match that line.
6. The archive must contain `loom` and `loomd` at its root.

## Files written

| Path | Content |
| --- | --- |
| `<prefix>/loom` | `loom` binary, mode `755`. |
| `<prefix>/loomd` | `loomd` binary, mode `755`. |
| `<prefix>/.loom.install.<pid>`, `<prefix>/.loomd.install.<pid>` | Copies of the new binaries. The script renames them to `loom` and `loomd` with `mv` (atomic for each file on the same filesystem). If a step fails, the script removes them on exit. |
| temporary directory from `mktemp -d` | Release metadata, `SHA256SUMS`, signature, archive. The script removes it on exit. |

With `--channel`, the installed `loom` writes `<loom home>/state/update.json` (see [Remembered channel](#remembered-channel)). The script writes no other file. It never changes the shell startup files (`.profile`, `.bashrc`, `.zshrc`, fish configuration).

## Behavior

- **Already installed:** the script compares the selected version with the output of `<prefix>/loom -V` (`loom <version>`) and `<prefix>/loomd -V` (`loomd <version>`). If both are the same version, the script prints `loom <version> is already installed in <prefix>`. Then it exits with code 0 and downloads no asset. With `--version`, this check occurs before the first request.
- **Other version installed:** the script replaces the two binaries.
- **Failure:** if a step fails before the renames, `<prefix>/loom` and `<prefix>/loomd` do not change.
- **Remembered channel:** see [Remembered channel](#remembered-channel).
- **PATH hint:** if `<prefix>` is not an entry of `PATH`, the script prints an `export PATH=...` line. Add this line to a shell profile.

## Remembered channel

This occurs only with `--channel`. After a successful install, or if the selected version is already installed, the script runs the installed binary:

```text
<prefix>/loom upgrade --channel <CHANNEL> --check --quiet
```

The script closes stdin and discards stdout. It passes the environment to the command, with `LOOM_HOME`. `loom` records the channel in `channel` of `<loom home>/state/update.json` with its own code.

Thus `loomd` and `loom upgrade` continue to follow this channel after an upgrade to a build of a different channel. For example, a `dev` install that moves to a stable release stays on `dev`. `[update] channel` in `config.toml` still has precedence ([channel resolution](configuration.md#channel-resolution)). The check sends one more request to the releases API.

If the command succeeds, the script prints `loomd and loom upgrade will follow the <CHANNEL> channel`. If the command fails, the script prints `warning: could not record the <CHANNEL> channel; run: <prefix>/loom upgrade --channel <CHANNEL> --check`. It still exits with code `0`, because the binaries are installed.

Without `--channel` (the `stable` default) or with `--version`, the script records no channel.

## Output

Messages go to stderr with the prefix `loom-install:`. Warnings have the prefix `loom-install: warning:` and errors have the prefix `loom-install: error:`. stdout receives only the `--help` output.

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Installed, already installed, or `--help`. |
| `1` | Error: unsupported platform, missing tool, HTTP or network failure, release or asset not found, verification failure, write failure. |
| `2` | Usage error: unknown argument, missing value, invalid channel or version, `--channel` combined with `--version`. |
| `130`, `143` | Interrupted by `SIGINT` or `SIGTERM`. |

## Tests

`mise run test-install` runs [`dist/test-install.sh`](../../dist/test-install.sh) against fake releases, through `LOOM_RELEASES_URL` and `LOOM_INSTALL_PUBKEY`. `python3 -m http.server` serves these releases on `127.0.0.1`. `mise run shellcheck` lints `dist/*.sh`. The two tasks are part of `mise run check`.
