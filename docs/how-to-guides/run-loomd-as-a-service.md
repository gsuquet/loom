# Run loomd as a service

> Keep `loomd` running in the background: started at login, restarted when it stops. macOS uses a launchd user agent, Linux a systemd user unit.

Before you start: `loom` and `loomd` are installed, either in the same directory or with `loomd` on your `PATH`. On Linux, your session has a systemd user instance (`systemctl --user` works).

## Install the service

1. Check what would change:

   ```bash
   loom service install --dry-run
   ```

   It prints the service file and the commands, and changes nothing.

2. Install and start the service:

   ```bash
   loom service install
   ```

   Running it again is safe: when the file is unchanged and the service is loaded, it prints `loomd service already installed` and does nothing. After you move `loomd` or change `LOOM_HOME`, run it again to rewrite the file and reload the service.

The service uses the loom home of the shell you run `install` from: `LOOM_HOME` if set, otherwise `~/.loom`.

## Check the service

```bash
loom service status
```

```text
platform:  macos
unit:      /Users/me/Library/LaunchAgents/io.github.gsuquet.loomd.plist
installed: yes
loaded:    yes
running:   yes
```

`running: no` with `loaded: yes` means the service manager knows the service but `loomd` is not running: read the logs. For scripts, `loom service status --json` prints the same fields as JSON.

## Read the logs

- macOS: `<loom home>/log/loomd.log`, for example:

  ```bash
  tail -f ~/.loom/log/loomd.log
  ```

- Linux: the systemd journal:

  ```bash
  journalctl --user -u loomd.service -f
  ```

## Uninstall the service

```bash
loom service uninstall
```

It stops `loomd`, unloads the service and removes its file. `--dry-run` shows the steps first. With nothing installed, it prints `loomd service not installed` and does nothing.

## Keep loomd from being installed as a service

1. Uninstall the service if it is installed (see above): the setting below does not remove it.
2. Add to `<loom home>/config.toml`:

   ```toml
   [daemon]
   service = false
   ```

`loom service install` then refuses with exit code 1 and names this key. Remove the key, or set it to `true`, to allow the service again.

## Reference

- Commands and flags: [loom CLI reference](../reference/cli/loom.md).
- Output, commands run and exit codes: [CLI conventions](../reference/cli/conventions.md#loom-service-output).
- File paths, file content, `[daemon] service`: [configuration](../reference/configuration.md#user-service).
