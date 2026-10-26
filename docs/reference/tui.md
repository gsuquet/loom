# TUI reference

> Terminal user interface of `loom`, as shipped.

## Invocation

`loom` with no subcommand starts the terminal user interface (TUI):

| Condition | Behavior |
| --- | --- |
| `stdin` and `stdout` are terminals | `loom` connects to `loomd`, enters the alternate screen in raw mode, and opens the TUI on the `Today` queue. |
| `stdin` or `stdout` is not a terminal | `loom` prints the CLI help (`loom --help`) on `stdout` and exits with code `0`. |

When `loom` starts the TUI, the initial context filter comes from `-c <CONTEXT>` (or `LOOM_CONTEXT`). Without `-c` or `LOOM_CONTEXT`, the initial context filter is `all` (every context).

When the TUI stops, or if `loom` panics while the TUI runs, `loom` leaves the alternate screen, disables raw mode, and shows the cursor again.

## Layout

The TUI has three regions:

| Region | Position | Content |
| --- | --- | --- |
| Top bar | Top line | The three queue tabs (`1 Today`, `2 Inbox`, `3 Waiting`), the active context badge (`[all]` or `[<context>]`), and the live update status. |
| Main area | Middle | The active queue (with an optional right-hand preview pane) or the active object page. |
| Status bar | Bottom line | When an error occurs, the error banner. When a status message is present, the message. Otherwise, key shortcuts for the active view. |

## Queues and preview pane

The TUI has three queues:

| Key | Queue | Source in `loomd` | Content |
| --- | --- | --- | --- |
| `1` | `Today` | `GET /v1/tasks` | Open tasks that are `ready` or `active`, in priority order (pinned, overdue or due today, importance, due date, age, then `id`). Each row shows importance, status, context badge, kind, and title. `w` toggles priority reasons. `p` toggles grouping by parent task. |
| `2` | `Inbox` | `GET /v1/tasks?status=inbox` | Open tasks with status `inbox`, in rank order. |
| `3` | `Waiting` | `GET /v1/tasks?status=waiting` | Open tasks with status `waiting`, in rank order. Shows undated tasks and dated tasks, with `until` dates and `waiting on` notes. |

A queue view shows a list on the left and a preview pane on the right when two conditions hold:

- The terminal width is at least `120` columns.
- The preview pane is enabled (`|` switches it on or off).

When the terminal width is below `120` columns, or when you switch the preview pane off with `|`, the list uses the full width of the main area.

A task that is read-only shows `[ro: <reason>]` in the list row and a banner `⚠ READ-ONLY: <reason>` in the preview pane. `loom` refuses edits to read-only tasks and displays an error message.

## Object pages

When you press `Enter` on a selected row in a queue, or on a search result, the TUI pushes an object page onto the navigation stack. When you press `Esc`, the TUI pops the object page and returns to the previous view with its selection intact.

| Object | Content |
| --- | --- |
| Task | A tab bar and the selected tab. The tabs are `Overview`, `Relations` and `Events`. |

The task page opens on the `Overview` tab. `Overview` shows the `id`, the context badge, the status, the importance, the kind, the title, the due date, and the notes of the task. The TUI renders the notes as Markdown. It supports headings, bullet and numbered lists, quotes, rules, fenced code, and the inline marks `**bold**`, `*italic*` and `` `code` ``. It shows any other Markdown as written.

The `Relations` and `Events` tabs show the text `The <tab> tab has no content in this version.`

When `loomd` sends a change event for the task, the TUI reloads the page.

## Modals

A modal sits on top of the active view and captures key presses until you close it with `Esc` or confirm a choice with `Enter`:

| Key | Modal | Behavior |
| --- | --- | --- |
| `?` | Help | Shows every key binding of the TUI, grouped by layer (`Global`, `Queue`, `Object page`, `Modals`). `j`/`k` (or `Down`/`Up`) scrolls the list; `Esc`, `?`, or `q` closes it. |
| `:` | Command palette | Fuzzy-filters views (`queue: Today`, `queue: Inbox`, `queue: Waiting`), actions (`action: Search (/...)`, `action: Pick context filter (@)`, `action: Toggle preview pane (|)`, `action: Refresh from loomd (r)`, `action: Help (?)`, `action: Quit loom (q)`), and context switches (`context: all`, `context: <name>`). |
| `/` | Search | Queries `GET /v1/search?q=<query>` as you type after a 150 ms pause. `Enter` opens the selected task page. |
| `@` | Context picker | Lists `all` and every known context. `Enter` switches the active context filter and reloads the queues. |
| `a` | Accept | Triages an `inbox` task to `ready`. Configures `Kind`, `Importance`, and optional `Due` date. |
| `z` | Snooze | Moves a task to `waiting`. Configures `Until` date and optional `Waiting on` note. |
| `t`, `i`, `d` | Change field | On an object page only. Changes the kind (`t`), the importance (`i`) or the due date (`d`) of the task. `j`/`k` (or `Down`/`Up`) choose a value and `Enter` applies it. For the due date, type `YYYY-MM-DD`, `today`, `tomorrow`, `+1d` or `+1w`; an empty value removes the due date. A wrong date keeps the modal open with an error. The change is one `PATCH /v1/tasks/{id}` call and one `edited` event. The CLI equivalent is `loom set`. |
| `s` | Change status | On an object page only. Lists the statuses that the task can move to. `j`/`k` (or `Down`/`Up`) choose one and `Enter` applies it. The call is `POST /v1/tasks/{id}/actions/{status}`, as `loom set status=` does, and it writes one status event. The choice `waiting` opens the snooze modal for the page's task, because that status needs a date; `Esc` there returns to the page. The picker needs the status moves only, not the `edit` action. |
| `+` | New task | Creates a new task with status `inbox`. Resolves context from the active context filter, or the selected task's context, or the configured default context. Configures `Title`. |

## Key bindings

Key resolution uses three layers, from innermost to outermost:

1. **Modal layer:** active when a modal (`?`, `:`, `/`, `@`, `a`, `z`, `+`, `t`, `i`, `d`, `s`) is open.
2. **View layer:** active for the current view (`Queue` or `Object page`).
3. **Global layer:** active whenever no modal captures the key.

### Global

| Key | Action |
| --- | --- |
| `q`, `Ctrl+c` | Quit the TUI. |
| `?` | Open help. |
| `:` | Open the command palette. |
| `/` | Open search. |
| `@` | Switch context (`all` or one context). |
| `1` | Open the `Today` queue. |
| `2` | Open the `Inbox` queue. |
| `3` | Open the `Waiting` queue. |
| `Tab` | Select the next queue tab. |
| `Shift+Tab` | Select the previous queue tab. |

### Queue view

| Key | Action |
| --- | --- |
| `j`, `Down` | Select the next row. |
| `k`, `Up` | Select the previous row. |
| `Home` | Select the first row. |
| `G`, `End` | Select the last row. |
| `PgDn` | Scroll down one page. |
| `PgUp` | Scroll up one page. |
| `Enter` | Open the selected task. |
| `\|` | Switch the preview pane on or off. |
| `r` | Reload the queues from `loomd`. |
| `a` | Open the accept modal (triages `inbox` task to `ready`). |
| `z` | Open the snooze modal (moves task to `waiting`). |
| `c` | Mark the task as done. |
| `x` | Cancel the task. |
| `g` | Focus the task and quit the TUI (`loom shell-init` changes directory to the task repository). |
| `u` | Undo the last action performed (reverts the last change of the task). |
| `+` | Create a new task in the active context (or from the selected task or default context). |
| `w` | Toggle priority reasons (`--why`) in `Today` queue. |
| `p` | Toggle grouping by parent task in `Today` queue. |

The footer of a queue shows the keys of the actions for that queue. `?` shows all keys.

The TUI sends one task action at a time to `loomd`. These rules apply:

- If you start a second action while the first runs, the TUI keeps it and sends it when the first ends.
- If a third action arrives, the footer shows `action in progress` and the TUI drops it.
- If the first action fails, the TUI shows the error and drops the kept action.
- A kept `u` runs only if the task that it names is still the undo target. A second quick `u` after a successful undo shows `nothing to undo`.
- `u` stays ready until an undo succeeds.
- When you quit, the TUI waits up to 5 seconds for the running action to end. It does not send the kept action.

### Object page

| Key | Action |
| --- | --- |
| `Esc`, `Backspace` | Go back to the previous page. On the first page, return to the queue view. |
| `]`, `Right` | Select the next tab. The scroll position goes back to the top. |
| `[`, `Left` | Select the previous tab. The scroll position goes back to the top. |
| `j`, `Down` | Scroll down one line. |
| `k`, `Up` | Scroll up one line. |
| `PgDn` | Scroll down one page. |
| `PgUp` | Scroll up one page. |
| `r` | Reload the task from `loomd`. |
| `t` | Open the kind picker. |
| `i` | Open the importance picker. |
| `d` | Open the due date picker. |
| `s` | Open the status picker. |
| `u` | Undo the last change that the TUI made. |

The four pickers open on the `Overview` tab only. They show a status message when they refuse: `task is read-only: <reason>`, `the task is not loaded yet`, or `cannot edit task` when the task detail has no `edit` action (the kind, importance and due date pickers) or no status move (the status picker, which then shows `the task cannot change status`). For the kind, importance and due date pickers, a choice that does not change the task sends no call and closes the modal, and a change to a `content` task keeps its format. `u` on an object page undoes only the last change of that page's task, a status move included; otherwise it shows `nothing to undo`.

### Modals

| Key | Action |
| --- | --- |
| `Esc` | Close the modal. |
| `Enter` | Confirm the modal or open/execute the selected entry. |
| `Tab`, `Down` | Select the next field (in `Accept`, `Snooze`) or entry (in `Help`, `ContextPicker`). |
| `Shift+Tab`, `Up` | Select the previous field (in `Accept`, `Snooze`) or entry (in `Help`, `ContextPicker`). |
| `Ctrl+n` / `Ctrl+p` | Select next / previous command or result (in `Command palette`, `Search`). |
| `Left`, `h` | Cycle to the previous option in option fields (in `Accept`). |
| `Right`, `l` | Cycle to the next option in option fields (in `Accept`). |
| `Backspace` | Delete the last character in text inputs. |

## Live updates

While the TUI runs, `loom` subscribes to [`GET /v1/events`](api.md#change-events) over the `loomd` Unix socket:

- When `task.changed` or `task.archived` arrives for the active context (or when the context filter is `all`), `loom` queues a refresh in the background and updates the open object page if its task changed.
- When `resync` arrives, `loom` reloads the full state from `loomd`.
- If the event stream disconnects, `loom` reconnects with `Last-Event-ID` and resumes updates.

## Context colors

Each context badge uses one of the 16 standard ANSI colors so that the badge matches the terminal palette.

1. If `[contexts.<name>] color` is set in [`config.toml`](configuration.md#contexts-and-repositories), `loom` uses that color.
2. Otherwise, `loom` assigns a distinct color from the default cycle (`cyan`, `green`, `yellow`, `blue`, `magenta`, `bright_cyan`, `bright_green`, `bright_yellow`, `bright_blue`, `bright_magenta`, `red`, `bright_red`) not already taken by an explicit context color, in alphabetical order of context names.
3. The `all` badge uses `white`, and `unassigned` uses `bright_black`.
4. When `--color never` is given, or when `NO_COLOR` is set and non-empty (unless `--color always` overrides it), `loom` emits no ANSI color attributes and distinguishes contexts by their `[<name>]` text label alone.
