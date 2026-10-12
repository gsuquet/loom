# CLI reference

> Commands and flags of `loom` and `loomd`, as shipped.

## `loom version`

Prints the build information of the `loom` binary.

```
loom version [--json]
```

| Flag | Effect |
| --- | --- |
| `--json` | Prints the build information as JSON (`VersionInfo` schema). |

Human output: the version on the first line, then one field per line.

```
loom 0.1.0-dev.42
commit:      0123456789abcdef0123456789abcdef01234567
commit_date: 2026-10-12T10:00:00+02:00
target:      aarch64-apple-darwin
channel:     dev
profile:     release
```

JSON output fields:

| Field | Type | Content |
| --- | --- | --- |
| `version` | string | `LOOM_BUILD_VERSION` at build time when set, otherwise the crate version. |
| `commit` | string | Full git commit hash of the build, `unknown` when built outside a git checkout. |
| `commit_date` | string | Committer date of `commit` (strict ISO 8601), `unknown` outside a git checkout. |
| `target` | string | Target triple the binary was compiled for. |
| `channel` | `stable` \| `dev` \| `local` | `LOOM_CHANNEL` at build time; `local` when unset. |
| `profile` | `debug` \| `release` | Cargo profile family of the build. |

`loom --version` prints the same text as `loom version`; `loom -V` prints only `loom <version>`.

Exit codes: `0` on success, `2` on invalid arguments.

## `loomd --version`

```
loomd --version
loomd -V
```

`--version` prints the same build information as `loom version`, prefixed with `loomd`. `-V` prints only `loomd <version>`. Both binaries built together report identical build information.

## Build-time variables

| Variable | Values | Effect |
| --- | --- | --- |
| `LOOM_BUILD_VERSION` | semver string | Overrides the reported version (dev prereleases such as `0.1.0-dev.42`). |
| `LOOM_CHANNEL` | `stable`, `dev`, `local` | Sets the reported channel. Any other value fails the build. |
