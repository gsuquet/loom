# Project Loom

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust 1.99.0](https://img.shields.io/badge/rust-1.99.0-orange.svg)](.mise/config.toml)

* **Project Repository:** [https://github.com/gsuquet/loom](https://github.com/gsuquet/loom)
* **Project Maintainer:** Gabriel Suquet ([@gsuquet](https://github.com/gsuquet))

## Table of contents

1. [Project description](#project-description)
2. [Project status](#project-status)
3. [Who this project is for](#who-this-project-is-for)
4. [Project dependencies](#project-dependencies)
5. [Instructions for using Loom](#instructions-for-using-loom)
6. [Contributing guidelines](#contributing-guidelines)
7. [Additional documentation](#additional-documentation)
8. [How to get help](#how-to-get-help)
9. [Terms of use](#terms-of-use)

## Project description

Loom keeps everything I have to do in one prioritized list, across all my contexts (personal, company, clients), and records everything that gets done, by me or by agents.

* With _Loom_ you can _triage_ merge requests to review, tickets, incidents, alerts, support requests and your own ideas in a single list, ordered by priority, for one context or all of them.
* _Loom_ helps you _edit and create_ Jira, GitLab and GitHub issues locally and sync them upstream, and _review_ merge requests from the terminal.
* _Loom_ lets you _delegate_ a task, or part of it, to specialist agents with typed contracts, quality gates and approval rules, and watch them work live.
* Unlike _a tracker or an agent wrapper_, _Loom_ keeps an auditable ledger of all work on disk, separated per context, so learnings carry over without client data leaking between contexts.

## Project status

Rewrite from scratch, before v0.1. Nothing is installable yet. The design lives in [docs/design/](docs/design/) and the plan in the [roadmap](docs/roadmap.md).

## Who this project is for

Software engineers and consultants who juggle several contexts (clients, employer, personal projects), work across forges and trackers, and want to delegate part of their work to coding agents without losing control or traceability.

## Project dependencies

* **mise:** toolchain manager ([installation guide](https://mise.jdx.dev/getting-started.html)). It installs the pinned Rust toolchain (`1.99.0`) and the linters.
* **Git:** with worktree support.
* **Agent CLI (optional, from v0.7):** Claude Code, or another supported harness, to delegate tasks to agents.

## Instructions for using Loom

Loom is not released yet. From v0.1.0, `loom` and `loomd` install on macOS and Linux with the install script: see [Install loom](docs/how-to-guides/install.md).

To work on Loom:

```bash
git clone https://github.com/gsuquet/loom.git
cd loom
mise install
mise run check
```

Configuration and usage instructions will be added as features ship.

## Contributing guidelines

Read the [Contributing Guide](CONTRIBUTING.md), the [Code of Conduct](CODE_OF_CONDUCT.md) and, for coding agents, [AGENTS.md](AGENTS.md).

## Additional documentation

Organized with the [Diátaxis](https://diataxis.fr/) framework:

* **Reference:** [CLI conventions](docs/reference/cli/conventions.md), [`loom`](docs/reference/cli/loom.md) and [`loomd`](docs/reference/cli/loomd.md) commands (generated), [install script](docs/reference/install-script.md), [releases](docs/reference/releases.md) (channels, assets, signature), [configuration](docs/reference/configuration.md), [loomd API](docs/reference/api.md), [style guide](docs/reference/style-guide.md). File formats are added as features ship.
* **How-to guides:** [install loom](docs/how-to-guides/install.md).
* **Explanation, tutorials:** written as features ship.

Design and planning, outside Diátaxis (target behavior, not shipped behavior):

* **Design:** [vision](docs/design/vision.md), [architecture](docs/design/architecture.md), [domain model](docs/design/domain-model.md), [storage](docs/design/storage.md), [repositories](docs/design/repos.md), [tracker sync](docs/design/tracker-sync.md), [capture and learnings](docs/design/capture-and-learnings.md), [agent orchestration](docs/design/agents.md), [user workflow](docs/design/user-workflow.md).
* **Decisions:** [architecture decision records](docs/adr/README.md).
* **Plan:** [roadmap](docs/roadmap.md).

## How to get help

* **Issue Tracker:** report bugs or request features on [GitHub Issues](https://github.com/gsuquet/loom/issues).

## Terms of use

Project Loom is licensed under the [MIT License](LICENSE).
