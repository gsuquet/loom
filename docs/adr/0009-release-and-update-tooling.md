# ADR-0009: Release, install and update tooling

- Status: accepted
- Date: 2026-10-12

## Context

v0.1 ships `loom` and `loomd` for macOS and Linux (arm64, x86_64), with a stable channel (tags) and a `dev` channel (every merge to `main`), a verified installer, `loom upgrade` / `loom rollback` including local builds (`--from`), and an auto-upgrade in loomd with a health check and automatic rollback (#4, #5, #11, #12).

Constraints from the briefs: one archive per target containing both binaries, install into `~/.local/bin`, never edit shell rc files, verification fails closed, base URL overridable for tests. The repository is **public** (decided on 2026-10-12, after recreating it with a clean history).

Options evaluated:

1. **`dist`** (formerly cargo-dist, v0.33.0 on `main`) for build, release, installer, and its updater (`axoupdater` 0.10.2).
2. **Custom pipeline**: GitHub Actions + `cargo-zigbuild` + `gh release`, an `install.sh` and an updater written in `loom-update`.

## Decision

**Custom pipeline (option 2).** `dist` covers the build matrix well, but loom would have to override or replace most of what it does beyond that, and its updater cannot provide rollback or health checks, which loom needs anyway.

- **Build** (GitHub Actions):
  - Linux: one `ubuntu-latest` job building `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` with `cargo-zigbuild` (static binaries, no glibc dependency, no arm64 runner needed).
  - macOS: one `macos-latest` job building `aarch64-apple-darwin` and `x86_64-apple-darwin` with rustup targets.
- **Artifacts** per release: `loom-<version>-<target>.tar.gz` (contains `loom`, `loomd`, `LICENSE`, `README.md`), `SHA256SUMS` over all archives, `SHA256SUMS.minisig`, `install.sh`.
- **Signature**: minisign over `SHA256SUMS`. Secret key and password in Actions secrets (provisioned by the maintainer), public key committed at `dist/minisign.pub` and embedded in `loom-update` at build time. Verified in Rust with `minisign-verify` (zero dependencies).
- **Channels and versions**:
  - stable: tag `vX.Y.Z` -> GitHub release marked latest.
  - dev: every push to `main` -> prerelease `vX.Y.Z-dev.<run_number>`, where `X.Y.Z` is the workspace version, which on `main` is always the **next** release version (so semver orders dev builds before the release). The last 10 dev prereleases are kept, older ones and their tags deleted.
  - The build stamps the version with `LOOM_BUILD_VERSION` (falls back to `CARGO_PKG_VERSION`) and the channel with `LOOM_CHANNEL`.
- **Downloads**: every download goes through the GitHub REST API, `GET /repos/{owner}/{repo}/releases` and `GET /repos/{owner}/{repo}/releases/assets/{asset_id}` with `Accept: application/octet-stream`. Authentication is optional: a token (resolution order `LOOM_GITHUB_TOKEN`, `GITHUB_TOKEN`, then `gh auth token`) raises the API rate limit, and keeps the same code working if the repository ever becomes private again (fine-grained token, **Contents: read**).
- **Test hook**: `LOOM_RELEASES_URL` replaces `https://api.github.com` in the installer and the updater; tests serve an API-compatible fake.
- **Installer**: `install.sh` (POSIX sh), attached to each release. Bootstrap: `curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh` (latest stable release). Checksum verification is mandatory (`sha256sum` or `shasum -a 256`, abort if neither exists); minisign verification runs when `minisign` is installed, otherwise a warning. Never edits shell rc files.
- **Updater**: written in `loom-update` (#10 decision logic, #11 download/verify/swap/rollback, #12 auto-upgrade, health check, rollback). No `axoupdater`, no install receipts. On macOS and Linux, replacing a running binary is a same-filesystem `rename`; `self_replace` is not needed (it targets Windows).
- **Public repository.** No token is needed to install or update. GitHub artifact attestations can be added later as a second, independent proof of origin; minisign stays the primary check because it works offline, with a public key embedded in the binary.

## Consequences

- One workflow file, `install.sh` and the updater code are maintained by us. The updater had to be written anyway (rollback, health check, `--from`).
- No GitHub account or token is needed to install or update loom.
- Unauthenticated API calls are rate limited per IP; the 6 h check interval keeps loomd far below any limit, and a token raises it.
- Integrity (checksums) and authenticity (minisign) are verified by the updater; the installer guarantees authenticity only when `minisign` is installed.
- The minisign secret key is a long-lived secret: rotation means a new public key embedded in a new release, signed with the old key.
- Musl builds avoid glibc compatibility issues; allocation-heavy workloads may be slower with musl's allocator (not relevant for v0.1).

## Questions answered

| Question (brief #1) | Answer | Source |
| --- | --- | --- |
| Builds for macOS and Linux, arm64 and x86_64 on GitHub Actions | `dist`: yes, `targets` includes the four triples. Custom: `cargo-zigbuild` 0.23.4 supports Linux and macOS targets, `*-musl` for static binaries | [dist config](https://raw.githubusercontent.com/axodotdev/cargo-dist/main/book/src/reference/config.md), [cargo-zigbuild](https://docs.rs/crate/cargo-zigbuild/latest) (checked 2026-10-12) |
| Two binaries in one archive | `dist`: each package declares its own `binaries`; the docs do not say that binaries of two packages can share one archive (**unverified**, likely one archive per package). Custom: yes, we build the archive | dist config |
| GitHub Releases, checksums, signature or attestation | `dist`: `checksum` (default `sha256`, per-archive files), `github-attestations` (experimental), signed checksums listed as future work. Custom: `SHA256SUMS` + minisign | dist config, [minisign-verify 0.3.0](https://docs.rs/crate/minisign-verify/latest) |
| Attestations on a private repository | Private repositories use GitHub's own Sigstore instance. Plan availability for private repositories on a Free plan: **unverified** (my understanding: GitHub Enterprise Cloud required), not relied upon | [artifact attestations](https://docs.github.com/en/actions/concepts/security/artifact-attestations) |
| Stable channel on tags + dev prerelease on every merge | `dist` publishes semver prereleases as GitHub prereleases but runs on tags: dev builds would still need our own tagging workflow. Custom: done in the workflow | dist config (`publish-prereleases`, `force-latest`) |
| Shell installer, `~/.local/bin`, base URL override | `dist`: `install-path` accepts `~/subdir`, default `CARGO_HOME`. The generated installer reads overrides (download URL, install dir, no-modify-path, unmanaged dir, auth token) and **edits `.profile`, `.bashrc`, `.zshrc`, fish config unless told not to**. It verifies checksums only if the hashing tool exists, otherwise continues; the standalone updater is downloaded without checksum. Custom: our `install.sh` | [installer template](https://raw.githubusercontent.com/axodotdev/cargo-dist/main/cargo-dist/templates/installer/installer.sh.j2), [shell installer docs](https://raw.githubusercontent.com/axodotdev/cargo-dist/main/book/src/installers/shell.md) |
| Updater: channels, pinning, rollback | `axoupdater` 0.10.2: `UpdateRequest::{Latest, LatestMaybePrerelease, SpecificVersion, SpecificTag}`, `set_github_token` ("necessary to access private repositories", classic `repo` scope), `set_install_dir`, `set_release_source`. Requires install receipts from `dist` installers. No rollback or health check documented | [AxoUpdater](https://docs.rs/axoupdater/latest/axoupdater/struct.AxoUpdater.html), [UpdateRequest](https://docs.rs/axoupdater/latest/axoupdater/enum.UpdateRequest.html), [README](https://raw.githubusercontent.com/axodotdev/axoupdater/main/README.md) |
| Private repository: download auth | REST: `GET /repos/{owner}/{repo}/releases/assets/{asset_id}` with `Accept: application/octet-stream`, handles 200 or 302; fine-grained token permission **Contents: read** for both release endpoints. `curl \| sh` from the repository fails without a token | [release assets](https://docs.github.com/en/rest/releases/assets), [fine-grained permissions](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens) |

Version checked: `dist` workspace version `0.33.0` on `main` ([Cargo.toml](https://raw.githubusercontent.com/axodotdev/cargo-dist/main/Cargo.toml)). The release date could not be checked (the changelog request was blocked).

## Impact on tasks

- **#3 `loom version`**: build script honours `LOOM_BUILD_VERSION` (dev prerelease versions) before `CARGO_PKG_VERSION`.
- **#4 release pipeline**: workflow as decided above (zigbuild musl Linux job, macOS job for both darwin targets, version stamping, archives, `SHA256SUMS`, minisign signature, `install.sh` attached, dev prerelease pruning), `docs/reference/releases.md`. Maintainer: generate the minisign key pair, add `MINISIGN_SECRET_KEY` and `MINISIGN_PASSWORD` secrets, commit `dist/minisign.pub`.
- **#5 `install.sh`**: API-based download, optional token with the resolution order, `LOOM_RELEASES_URL`, mandatory checksum, optional minisign with warning, no rc file edits, `curl | sh` bootstrap from the latest release documented.
- **#11 `loom upgrade` / `rollback`**: GitHub REST client in `loom-update` (`reqwest` + rustls), token resolution, mandatory checksum and minisign verification with the embedded public key, rename-based swap, 2 previous versions kept, `--from`.
- **#12 auto-upgrade**: no tool support for health check or rollback; the mechanism stays to design in that task, as its brief requires.

## Unverified points

1. Whether `dist` can put binaries of two packages into one archive.
2. Artifact attestation availability for private repositories on the Free plan (no longer relevant: the repository is public).
3. `dist` latest release date.
4. Building `x86_64-apple-darwin` on an arm64 `macos-latest` runner with a rustup target only (expected to work; #4 confirms).
5. `releases/latest/download/install.sh` resolves to the latest stable release and skips prereleases (expected behaviour of GitHub's latest release; #5 confirms).
