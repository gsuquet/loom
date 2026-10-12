# `loomd` CLI reference

> Generated from the `clap` definitions by `mise run docs`. Do not edit by hand.
> Conventions shared by every command: [conventions.md](conventions.md).

Loom daemon: API server, scheduler, updater

## Usage

```text
loomd [OPTIONS]
```

### Options

| Option | Values | Default | Description |
| --- | --- | --- | --- |
| `--loom-home <DIR>` |  |  | Loom home directory. Overrides `LOOM_HOME`; default `~/.loom` |
| `-q`, `--quiet` |  |  | Print errors only on stderr |
| `-v`, `--verbose` | repeatable |  | Print more diagnostics on stderr; repeat for more (`-vv`) |
| `--color <WHEN>` | `auto`, `always`, `never` | `auto` | When to use colors: `auto` colors a terminal unless `NO_COLOR` is set |
| `-h`, `--help` |  |  | Print help |
| `-V`, `--version` |  |  | Print version |
