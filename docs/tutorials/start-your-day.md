# Start your day with Loom

> Learn how to read your priority list, triage the inbox, and focus on your top task with the `loom` commands.

This tutorial guides you through your daily workflow in the shell. You review your priority tasks, triage new inbox tasks, and select a task to work on.

## Prerequisites

Before you start this tutorial:

1. Install `loom` and make sure that `loomd` runs.
2. Add the shell integration to your shell configuration:
   ```bash
   eval "$(loom shell-init zsh)" # or bash
   ```

The IDs in this tutorial are examples. Use the IDs that `loom` shows you. A unique prefix of an ID is sufficient.

## Step 1: Add some tasks

If your lists are empty, add tasks to use in the next steps. A new task goes to the inbox. With `--ready`, it goes directly to the priority list:

```bash
loom add "Fix the login redirect" --kind bug
loom add "Ask Alice about the budget"
loom add "Update the old dashboard"
loom add "Review the design proposal"
loom add "Reply to the support ticket" --ready --importance p1 --due today
loom add "Write the release notes" --ready
```

## Step 2: Review the priority list

The priority list shows the tasks that are ready or active, in priority order.

1. Show the list:

   ```bash
   loom ls
   ```

   ```text
   #  ID  CONTEXT   STATUS  P   KIND   DUE         TITLE
   1  2s  personal  ready   p1  chore  2026-11-04  Reply to the support ticket
   2  z   personal  ready   p3  chore              Write the release notes
   ```

2. Add the `--why` option to see why each task comes after the previous task:

   ```bash
   loom ls --why
   ```

3. To see a task with its notes, use its ID:

   ```bash
   loom show 2s
   ```

## Step 3: Triage the inbox

New tasks from forges and from `loom add` go to the inbox.

1. List the inbox:

   ```bash
   loom ls --status inbox
   ```

   ```text
   ID  CONTEXT   STATUS  P   KIND   DUE  TITLE
   27  personal  inbox   p3  chore       Ask Alice about the budget
   2b  personal  inbox   p3  chore       Review the design proposal
   f   personal  inbox   p3  chore       Update the old dashboard
   v   personal  inbox   p1  bug         Fix the login redirect
   ```

2. Accept a task. The task moves to the `ready` status and joins the priority list. You can also change its kind, its importance and its due date:

   ```bash
   loom accept v --importance p1 --due +2d
   ```

3. If you make a mistake, revert the last change of the task. The accepted task moves back to the inbox:

   ```bash
   loom undo v
   ```

   **Note:** If a tracker sync or another client changed the task after your command, `loom undo` reverts that change instead.

4. Accept the task again:

   ```bash
   loom accept v --importance p1 --due +2d
   ```

5. Snooze a task that waits on someone. Give a date, and say what the task waits on. The task moves to the `waiting` status:

   ```bash
   loom snooze 27 +1w --on "Alice"
   ```

6. Mark a task done if it is already finished:

   ```bash
   loom done f
   ```

7. Cancel a task that is not necessary:

   ```bash
   loom cancel 2b
   ```

## Step 4: Check the waiting tasks

1. List the tasks that wait on people or dates:

   ```bash
   loom ls --status waiting
   ```

   ```text
   ID  CONTEXT   STATUS                             P   KIND   DUE  TITLE
   27  personal  waiting on Alice until 2026-11-11  p3  chore       Ask Alice about the budget
   ```

2. If a waiting task can continue, make it the focus with `loom go`, as step 5 shows. When its date comes, the task also joins the priority list.

## Step 5: Focus and begin work

1. Show the priority list again, and select the task that you want to work on:

   ```bash
   loom ls
   ```

2. Make the task your focus:

   ```bash
   loom go 2s
   ```

   The status of the task changes to `active`. If the task has a linked git repository, your shell changes to the directory of that repository.

3. Check your focus:

   ```bash
   loom focus
   ```

You can now start your work. To record a note on the focus task while you work, run `loom log`. When you complete the work, run `loom done` with the ID of the task:

```bash
loom log "Sent the first answer"
loom done 2s
```
