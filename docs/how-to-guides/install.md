# Install loom

Install `loom` and `loomd` on macOS or Linux (arm64 or x86_64) with the install script. For the options and the exact behavior, see the [install script reference](../reference/install-script.md).

You need `curl`, `tar`, and `sha256sum` or `shasum`. If you want the script to verify the release signature, also install [minisign](https://jedisct1.github.io/minisign/). Without minisign, the script verifies only the checksums and prints a warning.

## Install the latest stable release

1. Run the install script:

   ```bash
   curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh
   ```

   The script puts the binaries in `~/.local/bin`. If that directory is not in your `PATH`, the script prints the line to add to your shell profile. The script never edits the profile.

2. Check the result:

   ```bash
   loom -V
   ```

If that version is already installed and you run the install command again, the script does nothing.

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

If you install a different version, the script replaces the two binaries. Do not use `--channel` and `--version` together. The script refuses this combination. The script records the channel that you give with `--channel`. `loomd` and `loom upgrade` then continue to use this channel, also after an upgrade to a stable release.

If GitHub returns HTTP 403 or 429 (API rate limit), set a token:

```bash
export LOOM_GITHUB_TOKEN="$(gh auth token)"
```

Then run the install command again.

## Verify a release by hand

1. Download the archive for your platform, `SHA256SUMS` and `SHA256SUMS.minisig` from the release page, `https://github.com/gsuquet/loom/releases/tag/v<version>`. The archive name is `loom-<version>-<target>.tar.gz`, for example `loom-0.1.0-aarch64-apple-darwin.tar.gz`.

2. Verify the signature of `SHA256SUMS`:

   ```bash
   minisign -V -m SHA256SUMS -P RWRTbXcXADxrDJF6OfZjfRu3ZKO17/ePCtMB3U0p8fMzAjx5gU+7Ie6+
   ```

   The output must contain `Signature and comment signature verified`.

3. Verify the archive against `SHA256SUMS`. In the command, use the name of your archive:

   ```bash
   # Linux
   grep ' loom-0.1.0-x86_64-unknown-linux-musl.tar.gz$' SHA256SUMS | sha256sum -c
   # macOS
   grep ' loom-0.1.0-aarch64-apple-darwin.tar.gz$' SHA256SUMS | shasum -a 256 -c
   ```

   The output must end with `OK`.

## Uninstall

1. Remove the two binaries from the install directory (`~/.local/bin` by default):

   ```bash
   rm ~/.local/bin/loom ~/.local/bin/loomd
   ```

2. If you added the directory to your `PATH` only for loom, remove that line from your shell profile.
