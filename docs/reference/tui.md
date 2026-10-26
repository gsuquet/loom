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

The error banner `[Error: <message>]` stays until the next successful reload from `loomd`. `loom` removes a status message after 4 seconds or at the next key press. A status message for a successful action starts with `✓` (for example `✓ task created in inbox`).

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

Each row has these columns, from left to right: the selection marker `▶`, the tree indent (`Today` grouped by parent), the importance, the status, the context badge, the kind, the title, and then the notes of the row (`until`, `on`, `parent`, priority reason, `ro`). Each column has the width of its longest value in the queue. A status or a kind longer than 10 characters is cut and ends with `…`. On the selected row, only the marker and the title are in reverse video.

A task that is read-only shows `[ro: <reason>]` in the list row and a banner `⚠ READ-ONLY: <reason>` in the preview pane. `loom` refuses edits to read-only tasks and displays an error message.

## Object pages

When you press `Enter` on a selected row in a queue, or on a search result, the TUI pushes an object page onto the navigation stack. When you press `Esc`, the TUI pops the object page and returns to the previous view with its selection intact.

| Object | Content |
| --- | --- |
| Task | A tab bar and the selected tab. The tabs are `Overview`, `Relations` and `Events`. |

The task page opens on the `Overview` tab. `Overview` and the preview pane show the title of the task, then one line for each field (`Status`, `Importance`, `Kind`, `Context`, `Due`, `Waiting`, `Rank`, `Parent`, `ID`), then the `Notes` section. A field with no value is not shown. When the notes start with a `Notes` heading, the TUI does not show that heading again. When the notes are empty, the section shows `No notes.` The TUI renders the notes as Markdown. It supports headings, bullet and numbered lists, quotes, rules, fenced code, and the inline marks `**bold**`, `*italic*` and `` `code` ``. It shows any other Markdown as written.

### Relations tab

The `Relations` tab lists the tasks that the task links to, in five sections:

| Section | Content |
| --- | --- |
| `Parent` | The parent task. |
| `Children` | The tasks whose parent is this task, as a tree: each child shows its own children, one level deeper. The tree stops at 4 levels or 100 tasks and then shows `the tree is cut here`. Archived children are included. |
| `Spawned from` | The tasks in `spawned_from`. |
| `Blocks` | The tasks in `blocks`. |
| `Repository` | The repository slugs in the `repo` links of the task. |

A section with no entry shows `none`. A task row shows `<prefix>  <status>  <importance>  <title>`. A relation to a task that does not exist shows `<id>  (no such task)`. The TUI cuts a long row at the edge of the pane and shows `…`. Until the relations arrive, the tab shows `Loading relations...`. When the read fails, the tab shows `Cannot load the relations: <reason>`.

The TUI reads the relations each time that you select the tab, and again each time the page reloads while the tab is open. The previous relations stay on screen until the new ones arrive. The reads are `GET /v1/tasks/{id}` for the task and for each task that it names, and `GET /v1/tasks?parent={id}` (with and without `archived=true`) for the children.

`j` and `k` select a task row that exists. The pane scrolls to keep the selected row in view. `Enter` opens the selected task on a new page. `Esc` or `Backspace` goes back to the previous page, with its tab and selection as they were. The pages form a stack with no fixed depth. Archived and read-only tasks have a `Relations` tab like any other task.

When `loomd` sends a change event for the task, the TUI reloads the page.

### Events tab

The `Events` tab shows the event log of the task, from [`GET /v1/tasks/{id}/events`](api.md#taskevents), with the newest event first. The TUI reads the log each time that you select the tab, and again each time the page reloads while the tab is open. The previous log stays on screen until the new one arrives. Until the log arrives, the tab shows `Loading events...`. When the read fails, the tab shows `Cannot load the events: <reason>`. A task with no events shows `No events.` When the log has lines that this version of `loom` does not replay, the tab ends with `<n> lines not shown`.

Each event is one line with four columns:

| Column | Content |
| --- | --- |
| Time | `YYYY-MM-DD HH:MM`, in the local time zone of the system. |
| Actor | `user`, `file` (a hand edit), `loomd`, or `agent <name>`. |
| Type | `created`, `edited`, `status_changed`, `note_logged` or `undone`. |
| Summary | What the event changed, for example `status: ready -> active`. The TUI cuts a long summary at the edge of the pane and shows `…`. |

The TUI cuts an actor name longer than 14 characters and shows `…`. The width of the columns comes from the whole log, so a filter does not move them.

The footer of the tab shows the two filters, `f type: <value>` and `a actor: <value>`. The value is `all` when the filter is off. The two filters work together. When no event passes them, the tab shows `No event matches the filters.` Archived and read-only tasks have an `Events` tab like any other task.

## Modals

A modal sits on top of the active view and captures key presses until you close it with `Esc` or confirm a choice with `Enter`. Each modal has a border with its title. In a form (`Accept`, `Snooze`, `New task`), the field that has the focus has a thick border, and the last line shows the keys of the form or its error:

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
| `j`, `Down` | Scroll down one line. On the `Events` tab, the scroll stops at the last line. On the `Relations` tab, select the next task. |
| `k`, `Up` | Scroll up one line. On the `Relations` tab, select the previous task. |
| `PgDn` | Scroll down one page. |
| `PgUp` | Scroll up one page. |
| `Enter` | On the `Relations` tab, open the selected task. On the other tabs, it does nothing. |
| `r` | Reload the task from `loomd`. |
| `Home` | Scroll to the top. |
| `End` | On the `Events` tab, scroll to the last line, the oldest event. On the other tabs, it does nothing. |
| `f` | On the `Events` tab, select the next event type in the log, then no type. |
| `a` | On the `Events` tab, select the next actor in the log, then no actor. |
| `e` | Open the task file in the editor (see [Edit the task file](#edit-the-task-file)). |
| `t` | Open the kind picker. |
| `i` | Open the importance picker. |
| `d` | Open the due date picker. |
| `s` | Open the status picker. |
| `u` | Undo the last change that the TUI made. |

The four pickers open on the `Overview` tab only. They show a status message when they refuse: `task is read-only: <reason>`, `the task is not loaded yet`, or `cannot edit task` when the task detail has no `edit` action (the kind, importance and due date pickers) or no status move (the status picker, which then shows `the task cannot change status`). For the kind, importance and due date pickers, a choice that does not change the task sends no call and closes the modal, and a change to a `content` task keeps its format. `u` on an object page undoes only the last change of that page's task, a status move included; otherwise it shows `nothing to undo`.

#### Edit the task file

`e` on the `Overview` tab opens `task.md` of the task in the editor, as `loom edit` does. The TUI leaves the alternate screen and raw mode, runs the editor, then takes the terminal back and reloads the task. The editor is `$VISUAL`, then `$EDITOR`, then `vi`; it can have arguments, such as `code --wait`.

- `Ctrl-C` belongs to the editor. A `SIGINT` that reaches `loom` while the editor runs is dropped when the editor ends, so the exit status of the editor decides what happens. A `SIGTERM` is kept: the TUI stops when the editor ends.
- Change events wait while the editor runs. When the editor ends, the TUI reloads the queues and the task, so no change is lost.
- If the terminal cannot be given to the editor, the editor does not run and the status bar shows the error. If the terminal cannot be taken back, the TUI stops.
- A task that is not loaded, or on another tab, does not open. An archived task is read-only and does not open. A task with an invalid file opens, so that the edit can fix it.
- If the editor ends with an error, the file stays as it is and the status bar shows the error.
- If the saved file is not a valid task, the status bar shows the error and the file stays as saved. `loomd` shows the task read-only until the file is valid.
- If the saved file is valid, the status bar shows the task after `loomd` records the save. If the task changed while the editor was open, a warning names who changed it; `u` reverts the edit.

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
