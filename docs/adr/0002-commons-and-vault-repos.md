# 2. Two KB repositories (commons and vault); promotion only via `loom promote`

- Status: Accepted
- Date: 2026-11-09

## Context

Work happens in several contexts. Some contexts hold data that must stay isolated, while the
others should share one body of knowledge. A single repo would leak isolated data; one repo per
context would fragment knowledge that is not context-specific.

## Decision

Two repositories with an identical layout:

- **commons**: knowledge of the shared contexts (`context1`, `context2`), private.
- **vault**: knowledge of an isolated context (`context3`), in a separate private repo. Where it is hosted is configuration;
  isolation comes from the separate repo and `loom promote`, not from the host.

Each context names its KB in `contexts.toml` (`kb = "commons" | "vault"`). Content moves from
vault to commons only through `loom promote <note>`, which copies (never moves), asks for
confirmation and runs `loom lint` (including context-leak checks) on the result.

## Consequences

- Isolated data cannot reach commons by accident through normal commands.
- Cross-repo links are not resolvable; a daily note in commons links to the vault note by
  path/URL instead.
- Two repos to sync; `loom sync` handles each KB independently.
