# Configuration reference

> Loom home directory, configuration file, runtime files and log of `loomd`, as shipped.

## Loom home directory

`loomd` keeps its files in one directory, the loom home. It is resolved in this order:

| Order | Source | Condition |
| --- | --- | --- |
| 1 | `--loom-home <DIR>` | Given. An empty value is an error. |
| 2 | `LOOM_HOME` environment variable | Set and non-empty. |
| 3 | `~/.loom` | The user's home directory is known (`HOME`). |

When no source gives a directory, `loomd` exits with code 1.

`loom` resolves the loom home the same way, without step 1: it has no `--loom-home` flag.

A relative path is made absolute against the current directory. Symbolic links and `..` are kept as given, not resolved.

`loomd` creates the loom home if it does not exist (mode `0700`).

## `config.toml`

Path: `<loom home>/config.toml`. Format: [TOML](https://toml.io/en/v1.1.0).

| Case | Behavior |
| --- | --- |
| File missing | Defaults. |
| File empty or comments only | Defaults. |
| Unknown key or table, at any level | Rejected. |
| Value of the wrong type | Rejected. |
| TOML syntax error | Rejected. |

A rejected file stops `loomd` with exit code 1 before it creates anything, and this message on stderr:

```text
Error: invalid configuration <path>:<line>:<column>: <message>
```

`line` and `column` start at 1; `column` counts characters. For an unknown key, `message` names it:

```text
Error: invalid configuration /home/me/.loom/config.toml:3:1: unknown field `foo`, expected `daemon`
```

### Keys

| Key | Type | Default | Effect |
| --- | --- | --- | --- |
| `[daemon] service` | boolean | `true` | `false`: `loom service install` refuses with exit code 1 and a message naming this key and the file. `loom service uninstall` and `loom service status` still work. Does not stop or remove an installed service. |

```toml
[daemon]
service = false
```

## User service

`loom service install` installs `loomd` as a user service. `loom service` reads `config.toml` only for `[daemon] service`.

| Platform | Manager | File | Logs |
| --- | --- | --- | --- |
| macOS | launchd user agent, label `io.github.gsuquet.loomd`, domain `gui/<uid>` | `~/Library/LaunchAgents/io.github.gsuquet.loomd.plist` | `<loom home>/log/loomd.log` (stdout and stderr) |
| Linux | systemd user unit `loomd.service` | `$XDG_CONFIG_HOME/systemd/user/loomd.service` when `XDG_CONFIG_HOME` is an absolute path, otherwise `~/.config/systemd/user/loomd.service` | systemd journal: `journalctl --user -u loomd.service` |

The service runs the `loomd` found by `loom service install`: the `loomd` next to the `loom` executable (symbolic links to `loom` resolved), otherwise the first `loomd` file in a `PATH` directory. Its path is written canonicalized (absolute, symbolic links resolved). The resolved loom home is passed as `LOOM_HOME`.

Property list content (macOS): `ProgramArguments` = [`loomd` path], `EnvironmentVariables` = `LOOM_HOME`, `RunAtLoad` true, `KeepAlive` true, `ProcessType` `Background`, `StandardOutPath` and `StandardErrorPath` = `<loom home>/log/loomd.log`. Values are XML-escaped.

Unit content (Linux):

```ini
[Unit]
Description=Loom daemon

[Service]
ExecStart="/usr/local/bin/loomd"
Environment="LOOM_HOME=/home/me/.loom"
Restart=always

[Install]
WantedBy=default.target
```

Values are double-quoted; `\` and `"` are backslash-escaped, `%` is written `%%`, and `$` in `ExecStart` is written `$$`.

A `loomd` path or loom home that is not valid UTF-8 or contains a control character is refused with exit code 1. The file is rewritten by `install` whenever its content differs from the generated one: hand edits are overwritten.

## `run/`

Runtime files of `loomd`, in `<loom home>/run/`.

| Path | Mode | Content |
| --- | --- | --- |
| `run/` | `0700` | Created if missing; an existing directory is set to `0700` at start. |
| `run/loomd.lock` | `0600` | Single-instance lock. Holds the PID of the running `loomd`, followed by a newline. Kept after `loomd` stops. |
| `run/loomd.sock` | `0600` | Unix socket of the [API](api.md). Removed when `loomd` stops gracefully. |

### Single instance

At start, `loomd` takes an exclusive lock on `run/loomd.lock` (`flock`), held until the process exits. If another process holds it, `loomd` exits with code 1:

```text
Error: another loomd is running (PID 4242): /home/me/.loom/run/loomd.lock is locked
```

The `(PID …)` part is omitted when the lock file holds no PID.

### Stale socket

With the lock held, any existing file at `run/loomd.sock` was left by a daemon that did not stop gracefully (killed, crashed). `loomd` removes it before listening, and logs `removed stale socket` at info level.

### Socket path length

A unix socket path is limited to 107 bytes on Linux and 103 bytes on macOS. When `<loom home>/run/loomd.sock` is longer, `loomd` exits with code 1 and a message naming the path; use a shorter loom home. `loom status` checks the same limit before connecting, and fails the same way.

## `log/`

Log of a `loomd` started by `loom status`, in `<loom home>/log/`.

| Path | Mode | Content |
| --- | --- | --- |
| `log/` | `0700` | Created by `loom status` before it starts `loomd`; an existing directory is set to `0700`. |
| `log/loomd.log` | `0600` | stderr of every `loomd` that `loom status` started, appended across starts, never rotated or truncated. |

A `loomd` started another way (by hand, by a service manager) logs to its own stderr, not to this file.
