# Releases

> GitHub releases of `loom` and `loomd`, as published by [`.github/workflows/release.yml`](../../.github/workflows/release.yml). Decision: [ADR-0009](../adr/0009-release-and-update-tooling.md).

## Channels and tags

| Channel | Trigger | Git tag and release name | GitHub release | `LOOM_CHANNEL` |
| --- | --- | --- | --- | --- |
| `stable` | Push of a tag `vX.Y.Z` | `vX.Y.Z` | Release, marked latest | `stable` |
| `dev` | Every push to `main` that changes `Cargo.toml`, `Cargo.lock`, `crates/`, `dist/`, `.mise/`, the release workflow, `LICENSE` or `README.md` | `vX.Y.Z-dev.N` | Prerelease, never latest | `dev` |

- `X.Y.Z` is the workspace version (`[workspace.package] version` in `Cargo.toml`). On `main` it is the **next** release version, so semver orders every `X.Y.Z-dev.N` before `X.Y.Z`.
- `N` is the run number of the release workflow. It increases with each run, and pull request runs also increase it. Thus the dev numbers increase, but they are not consecutive.
- If the version of a stable tag is not the workspace version, the workflow fails and publishes nothing.
- The workflow creates a release as a draft, uploads its assets, then publishes it. A published release always has all its assets.
- The workflow publishes one release at a time (one concurrency group for `main` and tags).

## Dev retention

After each dev publish, the workflow keeps the 10 most recent prereleases whose tag matches `vX.Y.Z-dev.N` (by creation date). It deletes the older prereleases and their git tags. The workflow never deletes a stable release.

## Version scheme

| Build | Version reported by `loom version` |
| --- | --- |
| Stable release | `X.Y.Z` (the tag without `v`) |
| Dev prerelease | `X.Y.Z-dev.N` |
| Local build | the workspace version, channel `local` |

The workflow sets the build-time variables `LOOM_BUILD_VERSION` and `LOOM_CHANNEL` ([CLI conventions](cli/conventions.md#build-time-variables)).

## Targets

| Target triple | Platform | Build |
| --- | --- | --- |
| `aarch64-apple-darwin` | macOS, Apple silicon | `cargo build` on `macos-latest` |
| `x86_64-apple-darwin` | macOS, Intel | `cargo build` on `macos-latest` |
| `aarch64-unknown-linux-musl` | Linux, arm64, statically linked | `cargo zigbuild` on `ubuntu-latest` |
| `x86_64-unknown-linux-musl` | Linux, x86_64, statically linked | `cargo zigbuild` on `ubuntu-latest` |

The workflow builds the binaries with `cargo build --release --locked -p loom -p loomd`.

## Assets

Every release, stable or dev, has these assets:

| Asset | Content |
| --- | --- |
| `loom-<version>-<target>.tar.gz` | One per target. gzip-compressed tar archive. |
| `SHA256SUMS` | SHA-256 checksums of the other assets. |
| `SHA256SUMS.minisig` | minisign signature of `SHA256SUMS`. |
| `install.sh` | Installer (`dist/install.sh`), see [install script](install-script.md). |

`<version>` is the version without `v` (`0.1.0`, `0.1.0-dev.42`).

### Archive content

The files are at the root of the archive, in no directory:

| File | Mode |
| --- | --- |
| `loom` | `755` |
| `loomd` | `755` |
| `LICENSE` | `644` |
| `README.md` | `644` |

### `SHA256SUMS`

The file has one line for each asset (each archive and `install.sh`). The lines are sorted by file name (byte order). The format is that of `sha256sum` and `shasum -a 256`:

```text
<64 lowercase hex digits><two spaces><file name>
```

Example:

```text
61b6fca2aa9e48acd1a04b857da7e7e21c2a8331fcf54f1eec8895c27a17e9af  loom-0.1.0-dev.42-aarch64-apple-darwin.tar.gz
```

### Signature

- Algorithm: [minisign](https://jedisct1.github.io/minisign/), prehashed signature (minisign default) over `SHA256SUMS`.
- Public key: [`dist/minisign.pub`](../../dist/minisign.pub), key ID `0C6B3C0017776D53`:

  ```text
  RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+
  ```

- Trusted comment: minisign default, `timestamp:<unix time>`, `file:SHA256SUMS` and `hashed`, with tabs between them.
- Before it publishes the release, the workflow verifies the signature with `dist/minisign.pub`.

## Manual verification

Put the archive, `SHA256SUMS` and `SHA256SUMS.minisig` in the same directory. Then run these commands:

```bash
minisign -Vm SHA256SUMS -P RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+
sha256sum --ignore-missing -c SHA256SUMS      # Linux
shasum -a 256 --ignore-missing -c SHA256SUMS  # macOS
```

minisign prints `Signature and comment signature verified`. The checksum tool prints `<file>: OK` for each asset that you downloaded. If the output is different, do not use the download.

## Pull request dry run

The workflow does a dry run on each pull request that changes one of these paths: `Cargo.toml`, `Cargo.lock`, a `build.rs`, `dist/`, `.mise/` or the release workflow. In a dry run, the workflow:

1. Builds the four targets.
2. Packages the archives.
3. Writes `SHA256SUMS`.
4. Signs `SHA256SUMS` with a key pair that the job generates.
5. Verifies the signature and each checksum.

A dry run uses no secrets and publishes nothing.
