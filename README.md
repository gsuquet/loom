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
5. [How to use Loom](#how-to-use-loom)
6. [Guidelines for contributors](#guidelines-for-contributors)
7. [Additional documentation](#additional-documentation)
8. [How to get help](#how-to-get-help)
9. [Terms of use](#terms-of-use)

## Project description

Loom keeps all the work that I have to do in one prioritized list, for all my contexts (personal, company, clients). It records all the work that I or agents do.

* With _Loom_ you can _triage_ all your items in a single list, in the order of priority. The items are merge requests to review, tickets, incidents, alerts, support requests and your own ideas. The list shows one context or all of them.
* With _Loom_ you can _edit and create_ Jira, GitLab and GitHub issues locally and sync them upstream. You can also _review_ merge requests from the terminal.
* With _Loom_ you can _delegate_ a task, or a part of it, to specialist agents. Typed contracts, quality gates and approval rules control the agents, and you can watch the agents work live.
* Unlike _a tracker or an agent wrapper_, _Loom_ keeps an auditable ledger of all work on disk, with one ledger for each context. Learnings go from one context to the next, but client data stays in its context.

## Project status

[v0.2.0](https://github.com/gsuquet/loom/releases/tag/v0.2.0) has these functions:

* It installs, runs `loomd` as a user service and keeps itself up to date.
* It indexes the git repositories of the machine: contexts, repo cards, `loom search`, automatic fetch and fast-forward.

v0.3 (tasks and TUI) is in progress on the `dev` channel. The [changelog](CHANGELOG.md) gives the shipped changes. The design is in [docs/design/](docs/design/), and the plan is in the [roadmap](docs/roadmap.md).

## Who this project is for

Loom is for software engineers and consultants who:

* Work in several contexts (clients, employer, personal projects).
* Use many forges and trackers.
* Want to delegate a part of their work to coding agents, and keep control and traceability.

## Project dependencies

* **mise:** toolchain manager ([installation guide](https://mise.jdx.dev/getting-started.html)). It installs the pinned Rust toolchain (`1.99.0`) and the linters.
* **Git:** with worktree support.
* **Agent CLI (optional, from v0.7):** Claude Code, or another supported harness, to delegate tasks to agents.

## How to use Loom

Install `loom` and `loomd` into `~/.local/bin` (macOS and Linux, arm64 and x86_64). Then run `loomd` as a user service:

```bash
curl -fsSL https://github.com/gsuquet/loom/releases/latest/download/install.sh | sh
loom service install
loom status
```

1. Define your contexts in `~/.loom/config.toml` ([configure contexts](docs/how-to-guides/configure-contexts.md)).
2. Check them with `loom repo doctor`.
3. To register the repositories that the shell enters, use `eval "$(loom shell-init zsh)"` ([shell integration](docs/how-to-guides/shell-integration.md)).

To work on Loom, run these commands:

```bash
git clone https://github.com/gsuquet/loom.git
cd loom
mise install
mise run check
```

## Guidelines for contributors

Read the [Contributing Guide](CONTRIBUTING.md) and the [Code of Conduct](CODE_OF_CONDUCT.md). Coding agents also read [AGENTS.md](AGENTS.md).

## Additional documentation

The documentation uses the [Diátaxis](https://diataxis.fr/) framework:

* **Reference:** [CLI conventions](docs/reference/cli/conventions.md), [`loom`](docs/reference/cli/loom.md) and [`loomd`](docs/reference/cli/loomd.md) commands (generated), [install script](docs/reference/install-script.md), [releases](docs/reference/releases.md) (channels, assets, signature), [configuration](docs/reference/configuration.md), [loomd API](docs/reference/api.md), [task files](docs/reference/task-file.md) (`task.md`, `events.jsonl`), [style guide](docs/reference/style-guide.md). The reference gets new file formats when their features ship.
* **How-to guides:** [install loom](docs/how-to-guides/install.md), [run loomd as a service](docs/how-to-guides/run-loomd-as-a-service.md), [upgrade and roll back](docs/how-to-guides/upgrade-and-roll-back.md), [configure contexts](docs/how-to-guides/configure-contexts.md), [shell integration](docs/how-to-guides/shell-integration.md), [local traces](docs/how-to-guides/local-traces.md).
* **Explanation:** [auto-upgrade](docs/explanation/auto-upgrade.md), [repository updates](docs/explanation/repo-updates.md), [task store](docs/explanation/task-store.md), [search](docs/explanation/search.md).
* **Tutorials:** to come when features ship (from v0.3).

Design and plan, outside Diátaxis (target behavior, not shipped behavior):

* **Design:** [vision](docs/design/vision.md), [architecture](docs/design/architecture.md), [domain model](docs/design/domain-model.md), [storage](docs/design/storage.md), [repositories](docs/design/repos.md), [tracker sync](docs/design/tracker-sync.md), [capture and learnings](docs/design/capture-and-learnings.md), [agent orchestration](docs/design/agents.md), [user workflow](docs/design/user-workflow.md).
* **Decisions:** [architecture decision records](docs/adr/README.md).
* **Plan:** [roadmap](docs/roadmap.md).

## How to get help

* **Issue Tracker:** tell us about bugs or ask for features on [GitHub Issues](https://github.com/gsuquet/loom/issues).

## Terms of use

Project Loom is licensed under the [MIT License](LICENSE).
