# Architecture decision records

> Decisions that are settled. Changing one means writing a new ADR that supersedes it.

| ADR | Decision |
| --- | --- |
| [0001](0001-rust.md) | Rust as the implementation language |
| [0002](0002-files-source-of-truth.md) | Files as source of truth, SQLite as rebuildable index |
| [0003](0003-one-store-per-context.md) | One store root per context |
| [0004](0004-agents-without-credentials.md) | Agents hold no credentials; effects are proposed actions |
| [0005](0005-mcp-loom-owned-tools.md) | MCP transport with loom-owned, shaped tools |
| [0006](0006-private-task-shared-mirror.md) | Private task file, shared issue mirrors with 3-way merge |
| [0007](0007-autonomy-policy.md) | Three-class autonomy policy for proposed actions |
| [0008](0008-git-cli-for-writes.md) | git CLI for writes, gix for reads |
| [0009](0009-release-and-update-tooling.md) | Release, install and update tooling |
| [0010](0010-cli-conventions.md) | CLI conventions and generated CLI reference |
| [0011](0011-auto-upgrade-rollback.md) | Auto-upgrade with pre-flight, watchdog and automatic rollback (proposed) |
