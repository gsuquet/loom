# Vision

> Why loom exists, what it is for, and what it deliberately does not do (yet).

Loom is a personal task and knowledge management system, connected directly to the right sources, in which agents are first-class tools to refine, review, improve and implement.

It succeeds when, after a month, I have somewhere to store references, no longer spawn agents by hand, know what to do next, and no longer open the forge to see MRs waiting.

Anything I do shall be written, traced and exploitable. Continuous improvement and learning.

Anything delegated to and accomplished by agents shall be observed and persisted in the same manner.

I have multiple contexts (personal, company-a, company-b, ...). I want to switch cleanly between them, and still exploit learnings from all of them.

Once a task is done, it's archived. It stays on disk, queryable for audit and insight, but no longer on the forefront. Avoid clutter.

When searching, active tasks and knowledge rank higher.

My goal is a list of things I need to do, ordered by priority, for all contexts or a single one. I can delegate any of them, fully or partially, to specialist agents, under control.

Loom replaces the git inventory scripts in my dotfiles. The previous loom implementation is a stopgap and constrains nothing.

## Non-goals (before v1)

- No write-back beyond the [write levels](connectors.md#write-levels) of each connector.
- No capture of shell, keystrokes or screen. Loom does not record meetings.
- No team or multi-user features. Each user runs a completely separate install.
- No automatic git conflict resolution.
- Nothing tightly coupled to one user's setup: tools, accounts and rules are configuration.
- No legal or provider-approval feature. Users are responsible for their own usage; loom offers multiple contexts.
- The desktop app is macOS only for now, and will likely need to install on Linux at some point. The CLI and `loomd` keep their current platforms.
