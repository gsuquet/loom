# loom

A small Go CLI that manages a plain-markdown knowledge base, records the work of AI agents
into it, builds daily/weekly notes from real activity, and hosts git workflow commands
(`loom git inventory|pr|review`). Everything is markdown in git; there is no daemon and no
database.

**Status: pre-alpha.** Only `loom context` exists so far. Interfaces will change.

## Install

Once a release is published:

```sh
mise use -g github:gsuquet/loom
```

Until then, build from source with [mise](https://mise.jdx.dev):

```sh
mise install && mise run build   # → bin/loom
```

## Usage

```sh
loom context                 # which context applies here, and why
loom context --json
loom context gitconfig       # git includeIf blocks generated from contexts.toml
```

Contexts are configured in `~/.config/loom/contexts.toml` (or `$LOOM_CONTEXTS`); see the
example in [docs/design.md §2](docs/design.md#2-contexts).

## Docs

- [Design](docs/design.md)
- [Architecture decision records](docs/adr/)
- [Contributing (humans and agents)](AGENTS.md)

## License

[MIT](LICENSE)
