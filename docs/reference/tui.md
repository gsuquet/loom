# TUI reference

> Terminal user interface of `loom`, as shipped.

## Invocation

`loom` with no subcommand starts the terminal user interface (TUI):

| Condition | Behavior |
| --- | --- |
| `stdin` and `stdout` are terminals | `loom` connects to `loomd` (and starts `loomd` on demand if necessary), enters the alternate screen in raw mode, and opens the TUI on the `Today` queue. |
| `stdin` or `stdout` is not a terminal | `loom` prints the CLI help (`loom --help`) on `stdout` and exits with code `0`. |

When `loom` starts the TUI, the initial context filter comes from `-c <CONTEXT>` (or `LOOM_CONTEXT`). Without `-c` or `LOOM_CONTEXT`, the initial context filter is `all` (every context).

When the TUI stops, or if `loom` panics while the TUI runs, `loom` leaves the alternate screen, disables raw mode, and shows the cursor again.

## Layout

The TUI has three regions:

| Region | Position | Content |
| --- | --- | --- |
| Top bar | Top line | The four queue tabs (`1 Today`, `2 Inbox`, `3 Waiting`, `4 Repos`) and the active context badge (`[all]` or `[<context>]`). |
| Main area | Middle | The active queue (with an optional right-hand preview pane) or the active object page. |
| Status bar | Bottom line | When a status message is present, the status message. Otherwise, the context-sensitive key hints for the active view. |

## Queues and preview pane

The TUI has four queues:

| Key | Queue | Source in `loomd` | Content |
| --- | --- | --- | --- |
| `1` | `Today` | `GET /v1/tasks` | Open tasks that are `ready` or `active`, in rank order (`p0` to `p3`, then `due`, then `rank`, then `id`). Each row shows the task rank, status, importance, context badge, kind, title, and due date. |
| `2` | `Inbox` | `GET /v1/tasks?status=inbox` | Open tasks with status `inbox`, in rank order. |
| `3` | `Waiting` | `GET /v1/tasks?status=waiting` | Open tasks with status `waiting`, in rank order. |
| `4` | `Repos` | `GET /v1/repos` | Indexed git repositories, sorted by context and name. Each row shows the context badge, repository name, branch, working-tree state (`clean` or `dirty`), and ahead/behind counts. |

A queue view shows a list on the left and a preview pane on the right when two conditions hold:

- The terminal width is at least `120` columns.
- The preview pane is enabled (`|` switches it on or off).

When the terminal width is below `120` columns, or when you switch the preview pane off with `|`, the list uses the full width of the main area.

## Object pages

When you press `Enter` on a selected row in a queue, on a search result, or on a related task link, the TUI pushes an object page onto the navigation stack. When you press `Esc`, the TUI pops the object page and returns to the previous view with its selection intact.

| Object | Sections |
| --- | --- |
| Task | Header (`id`, status, importance, context badge, kind, created and updated timestamps, schedule, repository slug, due date), `Body`, `Relations` (`parent`, `subtasks`, `blocks`, `blocked_by`, `related`), `Origin & Links`, `Notes`, and `Events`. In `Relations`, `Tab` and `Shift+Tab` move the focus across task links, and `Enter` opens the linked task page. |
| Repository | Header (`slug`, context badge, name, path), `Git state` (branch, default branch, working-tree state, ahead/behind counts, last commit, last fetch), and `Remotes` (remote name, URL, forge kind, role). |

## Modals

A modal sits on top of the active view and captures key presses until you close it with `Esc` or confirm a choice with `Enter`:

| Key | Modal | Behavior |
| --- | --- | --- |
| `?` | Help | Shows every key binding of the TUI, grouped by layer (`Global`, `Queue view`, `Object page`, `Modals`). `j`/`k` (or `Down`/`Up`) scrolls the list; `?` or `Esc` closes it. |
| `:` | Command palette | Fuzzy-filters views (`queue: Today`, `queue: Inbox`, `queue: Waiting`, `queue: Repos`), actions (`context: pick context`, `search: open search`, `help: key bindings`, `preview: toggle pane`, `refresh: reload from loomd`, `quit: exit loom`), and context switches (`context: all`, `context: <name>`). |
| `/` | Search | Queries `GET /v1/search?q=<query>` as you type (scoped to the active context when one is selected). `Enter` opens the selected task or repository page. |
| `c` | Context picker | Lists `all` and every known context. `Enter` switches the active context filter and reloads the queues. |

## Key bindings

Key resolution uses three layers, from innermost to outermost:

1. **Modal layer:** active when a modal (`?`, `:`, `/`, `c`) is open.
2. **View layer:** active for the current view (`Queue view` or `Object page`).
3. **Global layer:** active whenever no modal captures the key.

### Global

| Key | Action |
| --- | --- |
| `q`, `Ctrl+c` | Quit the TUI. |
| `?` | Open help. |
| `:` | Open the command palette. |
| `/` | Open search. |
| `c` | Switch context (`all` or one context). |
| `1` | Open the `Today` queue. |
| `2` | Open the `Inbox` queue. |
| `3` | Open the `Waiting` queue. |
| `4` | Open the `Repos` queue. |

### Queue view

| Key | Action |
| --- | --- |
| `j`, `Down` | Select the next item. |
| `k`, `Up` | Select the previous item. |
| `g`, `Home` | Select the first item. |
| `G`, `End` | Select the last item. |
| `Enter` | Open the selected item. |
| `\|` | Switch the preview pane on or off. |
| `r` | Reload the queues from `loomd`. |

### Object page

| Key | Action |
| --- | --- |
| `Esc` | Return to the previous view. |
| `j`, `Down` | Scroll down one line. |
| `k`, `Up` | Scroll up one line. |
| `g`, `Home` | Scroll to the top. |
| `G`, `End` | Scroll to the bottom. |
| `Tab` | Focus the next related item. |
| `Shift+Tab` | Focus the previous related item. |
| `Enter` | Open the focused related item. |
| `r` | Reload the object page from `loomd`. |

### Modals

| Key | Action |
| --- | --- |
| `Esc` | Close the modal. |
| `Enter` | Run or open the selected modal entry. |
| `Down`, `Ctrl+n` | Select the next modal entry. |
| `Up`, `Ctrl+p` | Select the previous modal entry. |
| `Backspace` | Delete the last query character (in `:` and `/`). |

## Live updates

While the TUI runs, `loom` subscribes to [`GET /v1/events`](api.md#change-events) over the `loomd` Unix socket:

- When `task.changed` or `repo.changed` arrives for the active context (or when the context filter is `all`), `loom` reloads the queues and refreshes the open object page if its object changed.
- When `resync` arrives, `loom` reloads the full state from `loomd`.
- If the event stream disconnects, `loom` reconnects with `Last-Event-ID` and reloads the state when necessary.

## Context colors

Each context badge uses one of the 16 standard ANSI colors so that the badge matches the terminal palette.

1. If `[contexts.<name>] color` is set in [`config.toml`](configuration.md#contexts-and-repositories), `loom` uses that color.
2. Otherwise, `loom` assigns a color from the default cycle (`cyan`, `yellow`, `green`, `magenta`, `blue`, `red`, `bright-cyan`, `bright-yellow`, `bright-green`, `bright-magenta`, `bright-blue`, `bright-red`) according to the context name.
3. The `all` badge uses `bright-white`, and `unassigned` uses `bright-black`.
4. When `--color never` is given, or when `NO_COLOR` is set and non-empty (unless `--color always` overrides it), `loom` emits no ANSI color attributes and distinguishes contexts by their `[<name>]` text label alone.
