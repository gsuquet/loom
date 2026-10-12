# Install loom

Install `loom` and `loomd` on macOS or Linux (arm64 or x86_64) with the install script. Options and exact behavior: [install script reference](../reference/install-script.md).

Requirements: `curl`, `tar`, and `sha256sum` or `shasum`. Install [minisign](https://jedisct1.github.io/minisign/) as well to have the script verify the release signature; without it, the script only verifies checksums and prints a warning.

## Install the latest stable release

```bash
curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh
```

The binaries go to `~/.local/bin`. If that directory is not in your `PATH`, the script prints the line to add to your shell profile; it never edits the profile itself.

Check the result:

```bash
loom -V
```

Running the same command again does nothing when that version is already installed.

## Choose a channel, a version or a directory

Pass options to the script after `sh -s --`:

```bash
# Latest development build
curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh -s -- --channel dev

# A specific version (stable or dev)
curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh -s -- --version 0.1.0

# Another directory
curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh -s -- --prefix /opt/loom/bin
```

Installing another version replaces both binaries. `--channel` and `--version` cannot be combined.

If GitHub answers with HTTP 403 or 429 (API rate limit), set a token and run the command again:

```bash
export LOOM_GITHUB_TOKEN="$(gh auth token)"
```

## Verify a release by hand

1. Download the archive for your platform, `SHA256SUMS` and `SHA256SUMS.minisig` from the release page, `https://github.com/gsuquet/loom/releases/tag/v<version>`. Archive names are `loom-<version>-<target>.tar.gz`, for example `loom-0.1.0-aarch64-apple-darwin.tar.gz`.

2. Verify the signature of `SHA256SUMS`:

   ```bash
   minisign -V -m SHA256SUMS -P RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+
   ```

   The output must contain `Signature and comment signature verified`.

3. Verify the archive against `SHA256SUMS`, replacing the archive name:

   ```bash
   # Linux
   grep ' loom-0.1.0-x86_64-unknown-linux-musl.tar.gz$' SHA256SUMS | sha256sum -c
   # macOS
   grep ' loom-0.1.0-aarch64-apple-darwin.tar.gz$' SHA256SUMS | shasum -a 256 -c
   ```

   The output must end with `OK`.

## Uninstall

Remove the two binaries from the install directory (`~/.local/bin` by default):

```bash
rm ~/.local/bin/loom ~/.local/bin/loomd
```

If you added the directory to your `PATH` only for loom, remove that line from your shell profile.
