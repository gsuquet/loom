# Start your day with Loom

> Learn how to start your day in the Today view, triage the Inbox, and focus on your top task without leaving the TUI.

This tutorial guides you through your daily workflow in the terminal user interface (TUI). You will review your priority tasks, triage new inbox items, and select a task to begin work.

## Prerequisites

Before you start this tutorial:

1. Install `loom` and ensure `loomd` runs.
2. Initialize the shell wrapper in your shell configuration:
   ```bash
   eval "$(loom shell-init zsh)" # or bash
   ```

## Step 1: Start the TUI

To open the interface, run `loom` without arguments:

```bash
loom
```

The interface opens on the `Today` queue. The top bar shows your queue tabs (`1 Today`, `2 Inbox`, `3 Waiting`) and the active context filter (`[all]`).

## Step 2: Review the Today queue

The `Today` queue lists tasks that are ready or active, in priority order:

1. Use `j` and `k` (or the down and up arrow keys) to navigate through tasks.
2. Press `w` to toggle priority reasons. The reason why each task received its position appears in parentheses (such as `(first)` or `(urgent)`).
3. Press `p` to toggle grouping by parent project.
4. Press `|` to toggle the preview pane if your terminal has at least 100 columns. The pane displays the full task notes on the right side.

## Step 3: Triage the Inbox

New tasks from forges or quick captures enter the `Inbox` queue.

1. Press `2` to switch to the `Inbox` tab.
2. Select an incoming task with `j` or `k`.
3. Press `a` to open the accept modal:
   - Use `Left` and `Right` to change the task `Kind`.
   - Press `Tab` to move to `Importance`, and use `Left` and `Right` to adjust the level (`p0` to `p3`).
   - Press `Tab` to move to `Due date`. Type a date such as `tomorrow` or `+2d`.
   - Press `Enter` to confirm. The task moves to the `ready` state and joins `Today`.
4. If you made a mistake on a triaged task, press `u` to undo the change immediately.
5. Select a task that waits on external input, and press `z` to snooze it:
   - Type an until date (for example `tomorrow` or `+1w`).
   - Press `Tab` and type what you wait on (for example `design review`).
   - Press `Enter` to confirm. The task moves to `Waiting`.
6. Select an item you already finished and press `c` to mark it done.
7. Select an irrelevant item and press `x` to cancel it.

## Step 4: Check Waiting tasks

1. Press `3` to switch to the `Waiting` tab.
2. Review tasks that wait on people or dates. Tasks without a date show `(undated)`.
3. If a waiting condition clears, press `a` to accept the task back into `Today`.

## Step 5: Focus and begin work

1. Press `1` to return to the `Today` queue.
2. Select the top task you want to work on.
3. Press `g`.

When you press `g`, `loom` sets the task as your active focus and exits the interface. If the task connects to a local git repository, your shell changes directory to that repository automatically.

You are now ready to write code. When you complete your work, run `loom done` from your shell or return to the interface with `loom`.
