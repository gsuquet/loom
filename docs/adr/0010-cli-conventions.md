# ADR-0010: CLI conventions and generated CLI reference

- Status: proposed
- Date: 2026-10-12

## Context

The CLI must be predictable: someone who knows common Unix tools should guess how `loom` behaves without reading its help, and scripts and agents must be able to rely on its output and exit codes. There is no single list of "standard Unix flags"; the conventions below follow the POSIX utility syntax guidelines, the GNU long option conventions and common practice in modern CLIs.

The CLI reference (`docs/reference/`) describes shipped behavior. Written by hand, it drifts from the code.

## Decision

### Syntax

Every binary (`loom`, `loomd`) parses its arguments with `clap` (derive), which gives:

- `-h`/`--help` and `-V`/`--version` on every binary, `--help` on every subcommand.
- Grouped short flags (`-qv`), `--opt=value` and `--opt value`.
- `--` ends option parsing.

Where a command reads or writes a file, `-` means stdin or stdout.

### Global flags of `loom`

| Flag | Meaning |
| --- | --- |
| `-c`, `--context <name>` | Context to work in. First step of the context resolution (`docs/design/domain-model.md`). Takes a context name, never a path or a `key=value` pair. |
| `-q`, `--quiet` | Errors only on stderr. |
| `-v`, `--verbose` | More diagnostics on stderr; repeatable (`-vv`). Conflicts with `-q`. |
| `--color <auto\|always\|never>` | Default `auto`: color only when the stream is a terminal and `NO_COLOR` is unset or empty. |
| `--json` | Machine output on stdout, validated by the JSON Schema of the command's output type. Available on every command that outputs data. |
| `-n`, `--dry-run` | On every command that changes state: print what would change, change nothing. |
| `-y`, `--yes` | Answer yes to every confirmation. |

`-c` has other meanings elsewhere (`git -c name=value`, `sh -c <command>`); loom has neither feature, and the argument is validated as a context name, so a `key=value` or a path fails with a clear error.

`loomd` takes `-q`, `-v`, `--color` and its own flags only.

### Confirmations

- A command that asks for confirmation asks on the terminal.
- `-y` is the **only** way to skip a confirmation. There is no environment variable or configuration key for it.
- When stdin is not a terminal and `-y` is absent, the command fails with exit code 1 and a message naming `-y`. It never waits for input.

### Streams

- stdout carries the command's data only (human or `--json`).
- stderr carries logs, progress, warnings and errors.
- A closed stdout (`loom ... | head`) ends the command with exit code 0 and no error message. Rust ignores `SIGPIPE`, so writes go through `writeln!` and `ErrorKind::BrokenPipe` is handled explicitly (`unsafe` is forbidden in the workspace, so the signal handler is not reset).

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Success. |
| 1 | Error. |
| 2 | Usage error (unknown flag, missing or invalid argument), as `clap` reports it. |
| 130 | Interrupted by `SIGINT`. |

Other codes are added only through this ADR's successor.

### Environment

`NO_COLOR`, `PAGER`, `EDITOR` and the XDG base directories (`XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME`, `XDG_CACHE_HOME`) are honoured. Loom-specific variables start with `LOOM_`.

### Generated CLI reference

- The reference of each binary's commands, arguments, flags and help texts is **generated from the `clap` definitions**: `docs/reference/cli/loom.md` and `docs/reference/cli/loomd.md`. Help texts are written once, in the code (`///` doc comments on the clap types).
- Generator: `clap-markdown`, called from an integration test in each binary crate. The test renders the markdown and compares it with the committed file; it fails when they differ. `LOOM_UPDATE_DOCS=1` rewrites the files, and `mise run docs` runs that. CI therefore fails on a stale reference.
- To reach the `clap` definitions from a test, each binary crate exposes its `Cli` type through a `lib.rs`; `main.rs` keeps the I/O.
- What `clap` cannot describe stays hand-written in `docs/reference/cli/conventions.md`: these conventions, exit codes, environment variables, build-time variables, and the `--json` output of each command (generated from `schemars` in a later task).

## Consequences

- Help output and reference cannot disagree.
- `clap-markdown` (0.1.x, single maintainer) fixes the markdown layout. If it blocks us, it is replaced by a renderer of our own over `clap::Command`, behind the same test; nothing else changes.
- Every new command must declare whether it changes state (then `--dry-run`, and `-y` if it confirms) and whether it outputs data (then `--json`).
- Shell completions (`clap_complete`) and man pages (`clap_mangen`) come from the same definitions, in a later task.
- `docs/design/domain-model.md` uses `-c` instead of `-C` for the context.
