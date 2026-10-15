# Run loomd as a service

> Make `loomd` run in the background at all times. The service starts `loomd` at login and starts it again when it stops. macOS uses a launchd user agent. Linux uses a systemd user unit.

Before you start, make sure that:

- `loom` and `loomd` are installed. They are in the same directory, or `loomd` is on your `PATH`.
- On Linux, your session has a systemd user instance (`systemctl --user` works).

## Install the service

1. Check the changes before you make them:

   ```bash
   loom service install --dry-run
   ```

   The command prints the service file and the commands. It changes nothing.

2. Install and start the service:

   ```bash
   loom service install
   ```

   You can run this command again safely. If the file has no change and the service is loaded, it prints `loomd service already installed`. It then does nothing. If you move `loomd` or change `LOOM_HOME`, run the command again. It writes the file again and reloads the service.

The service uses the loom home of the shell where you run `install`. This is `LOOM_HOME` if it is set, otherwise `~/.loom`.

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

If the output shows `running: no` with `loaded: yes`, the service manager knows the service but `loomd` does not run. Then read the logs. For scripts, `loom service status --json` prints the same fields as JSON.

## Restart loomd

After a change to `config.toml`, run this command:

```bash
loom service restart
```

```text
loomd restarted (pid 812 -> 9431, 0.3.0, via launchd) in 640 ms
```

The command first checks the configuration. If the configuration is not valid, loomd continues to run. The service manager restarts loomd (`launchctl kickstart -k` on macOS, `systemctl --user restart` on Linux). The command returns when the new loomd answers. If the new loomd does not answer within 15 s, the command fails and gives the step and the log.

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

The command stops `loomd`, unloads the service and removes its file. To see the steps first, add `--dry-run`. If no service is installed, it prints `loomd service not installed` and does nothing.

## Keep loomd from being installed as a service

1. If the service is installed, uninstall it (see above). The key below does not remove the service.
2. Add these lines to `<loom home>/config.toml`:

   ```toml
   [daemon]
   service = false
   ```

After this change, `loom service install` refuses with exit code 1 and gives the name of this key. To allow the service again, remove the key or set it to `true`.

## Reference

- Commands and flags: [loom CLI reference](../reference/cli/loom.md).
- Output, commands run and exit codes: [CLI conventions](../reference/cli/conventions.md#loom-service-output).
- File paths, file content, `[daemon] service`: [configuration](../reference/configuration.md#user-service).
