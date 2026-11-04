# UI style

How loom looks in the desktop app. The rules are written as **semantic tokens** and **components**; the app maps them to what its toolkit draws. A new view uses these components; a new component is added here first.

Tokens and components are normative in what they mean. The **Direction** columns and notes come from the [prototype](../prototypes/README.md) and are not normative.

## Desktop app target

The desktop app is as native to macOS as possible, following the design system of macOS 26 and 27 (Liquid Glass). The toolkit is still open, see [Architecture](architecture.md#desktop-app).

- **Glass** is only for the sidebar, the toolbar and floating controls. Rows, diffs and text sit on solid backgrounds.
- **Theme** follows the system, light or dark. The system's transparency settings apply.
- **Density** comes from the native controls. There is no density setting.
- **Context signal:** colour and shape on every row, plus a solid band on pages about one item, such as a review or a dialog.
- **Sizes:** one column in a narrow window, where `Enter` opens an item and `Esc` returns. List and task side by side on a 24 to 27 inch screen. The agent panel can also stay open on a 34 inch screen.

The sections below give the meaning of each element, whatever the toolkit. Keys and page flows are in [User workflow](user-workflow.md), which overrides any key named here.

## 1. Principles

1. **Meaning before decoration.** Color, weight and position say something (state, importance, focus). Nothing is colored only to look nice.
2. **Never color alone.** Every colored element also has a text label or a glyph, so the UI works without color and for color-blind users.
3. **Follow the user's theme.** The app follows the system light or dark preference.
4. **Columns line up.** Rows of a list are a table: every badge has its own column, and columns have the same width in every row.
5. **One focus.** At any time exactly one thing receives keys, and it is the only thing that looks focused.
6. **Transient things go away.** A confirmation disappears by itself; an error stays until it is resolved.

## 2. Color tokens

| Token | Use | Direction |
| --- | --- | --- |
| `text` | Normal text | `--fg` |
| `text-strong` | Titles, labels of the focused element, key names in hints | `--fg`, weight 600 |
| `muted` | Labels, secondary metadata, hints, separators, unfocused borders | `--fg-muted` |
| `danger` | Errors, read-only banner, `p0`, removed lines, flagged drafts, blocking review labels, request changes, a moved head | `--red` |
| `warning` | `p1`, `active` status, agent findings, a running step | `--amber` |
| `success` | `ready` status, success toasts, added lines, viewed files, approve | `--green` |
| `info` | `inbox` status, code, changes since the last review | `--blue` |
| `context-<name>` | Context badges | the context color, as a hue |

**Context color**: `[contexts.<name>] color` in `config.toml` when it is set. Otherwise, a distinct color from the default cycle (`cyan`, `green`, `yellow`, `blue`, `magenta`, `bright_cyan`, `bright_green`, `bright_yellow`, `bright_blue`, `bright_magenta`, `red`, `bright_red`) not already taken by an explicit one, in alphabetical order of context names. The `all` badge uses `white`, and `unassigned` uses `bright_black`.

Status and importance map to tokens:

| Value | Token |
| --- | --- |
| `p0` | `danger`, bold |
| `p1` | `warning`, bold |
| `p2` | `text` |
| `p3` | `muted` |
| `inbox` | `info` |
| `ready` | `success` |
| `active` | `warning`, bold |
| `waiting`, `done`, `cancelled`, unknown | `muted` |

A token is never used for a meaning other than its own: `danger` is not used for emphasis, `success` is not used for a context.

## 3. Spacing and type

The unit is 4 px (`1u`).

| Rule | Value |
| --- | --- |
| Gap between two badges or columns of a row | `2u` |
| Padding inside a pane or modal, left and right | `4u` |
| Gap between two sections of a pane | `4u` |
| Gap between two fields of a form | `3u` |
| Label column of a field grid | right-aligned, width of the longest label |

## 4. Components

### 4.1 Badge

A short value with a fixed meaning: importance, status, context, kind.

- Importance and status are colored text (their token), with no brackets.
- A context is `[name]` in its context color, bold.
- A kind is `muted` text.
- Badges do not have a background. A background is reserved for selection and focus. The diff is the one exception (section 4.11).

### 4.2 List row

A row of a queue, left to right:

| Column | Width |
| --- | --- |
| Tree indent (Today, grouped) | one step for each level under the parent |
| Importance | fixed (`p0` to `p3`) |
| Status | width of the longest status in the visible rows, with a maximum; a longer one is cut with `…` |
| Context | width of the longest `[context]` in the visible rows |
| Kind | width of the longest kind in the visible rows, with a maximum; a longer one is cut with `…` |
| Title | the rest |
| Suffixes (`until`, `on`, `parent`, `why`, `ro`) | after the title, `muted` (`ro` is `danger`) |

Columns are separated by the gap of section 3. A column never touches the next one, whatever the length of its value.

**Selection**: a background tint on the whole row and a left accent bar. The badges keep their own colors on the selected row, so the selection does not change their meaning.

### 4.3 Pane

A bordered region with a title.

- The title is in `text`. When two panes can take keys, the focused one gets a `text-strong` title and a stronger border, like a form field (section 4.6).
- The content has the padding of section 3.
- An empty pane says why, in `muted`: `No tasks in this queue.`, `No item selected.`

### 4.4 Field grid (task overview, preview)

The read view of an object:

1. The read-only banner, if any: `⚠ READ-ONLY: <reason>` in `danger`, bold, then a blank line.
2. The title, in `text-strong`.
3. The fields, one per line: a `muted` label column (section 3), then the value styled as its badge (section 4.1). Order: Status, Importance, Kind, Context, Due, Waiting, Rank, Parent, ID. A field with no value is not shown.
4. A blank line, then each body section (Notes).

### 4.5 Section header

A labeled rule that opens a section of a pane: a `muted` rule with the label in `text-strong`.

When a section shows Markdown and the Markdown starts with a heading equal to the section label (`## Notes` under the Notes header), that heading is not shown: the header already says it.

### 4.6 Modal

A dialog on top of the current view.

- Centered, and **always** with a border and the modal title (`New task`). Nothing of the view below shows inside it.
- The content has the padding of section 3.
- A form is a column of fields. Each field has a bordered box with its label as title.
  - The focused field has a stronger border and a `text-strong` label. The other fields have a plain border in `muted`. A field is never filled with a reverse color.
  - An empty text input shows a `muted` italic placeholder (italic, so it differs from a value without color).
  - A read-only value in a form (for example the context of a new task) is shown as its badge, with no input box.
- The last line is the hint line (section 4.7) or the modal's error in `danger`.

### 4.7 Hint line

Key hints: the key in `text-strong`, then the action in `muted` (`Enter create  Esc cancel`). The same format is used in the footer and in modals.

### 4.8 Footer and toasts

Feedback takes one of these forms, in order of priority:

1. **Error banner**: `[Error: <message>]` in `danger`. It stays until the error is resolved (for example the next successful refresh) or replaced.
2. **Toast**: the result of an action, for example `✓ task created in inbox`. A success starts with `✓` in `success`; information has no glyph and uses `text`. A toast disappears after **4 seconds** or at the next key press, whichever comes first.
3. **Hints** of the active view (section 4.7).

**Direction**: toasts in a corner stack, and errors in a banner on top of the view.

### 4.9 Review page

The object page of a Review task ([Code review](user-workflow.md#review), [MR review](reviews.md)). It uses the components 4.10 to 4.14.

- **Title**: `Review <MR key> <MR title>`, then the context badge, the head SHA (7 characters) and the totals, in `muted` except the title (`text-strong`) and the badge. In a wide window the author and the base branch follow. Fields that do not fit are dropped from the right; the title is cut with `…` before the badge is dropped.
- **Banners**: the "since last review" banner (section 4.13), then the reduced-view notice when there is no local clone (`No local clone: no context expansion, no worktree. Clone <remote URL>` in `muted`).
- **Panes**: the file tree (`Files`) and the diff (title: the file path and its counts), the file tree on the left. In a narrow window one pane fills the view.
- **Focus**: the file tree and the diff both take keys, so the focused pane has a stronger border and a `text-strong` title, and the other has a plain `muted` border (section 4.3). In the pane without focus, the selected row keeps a marker but not the selection color, so only one thing looks focused.
- **Unified or side by side**: side by side by default when the diff pane is wide enough, else unified. I can switch between the two; a diff pane too narrow for side by side always shows unified.
- **Hints**: the footer (section 4.8), with the hints of the focused pane and of the item under the cursor (for example `y accept  n dismiss  e edit` on a finding).
- **Direction**: the file tree in a sidebar, the diff in the main column; the sidebar is hidden in a narrow window.

### 4.10 File tree row

One row per changed file, in the order of the diff. A list row (section 4.2) with these columns, left to right:

| Column | Content |
| --- | --- |
| State | `✓` in `success`: viewed in this version. Else `●` in `info`: changed since the last review (section 4.13), so its viewed mark was reset. Else empty |
| Path | directory in `muted`, file name in `text`; a long path is cut on the left (`…/auth/jwt.rs`), never the file name. A renamed file shows `old → new` |
| Added | `+12` in `success`, right-aligned; empty when 0 |
| Removed | `-4` in `danger`, right-aligned; empty when 0 |
| Threads | `◆2` in `text`: unresolved forge threads. `◇1` in `muted` when all threads are resolved |
| Findings | `⚑1` in `warning` (agent findings) |
| Drafts | `✎1` in `text-strong`: my draft comments and replies |
| Flagged | `⚠1` in `danger`: flagged drafts |

- A column with no value in the visible rows is not drawn (the four counts columns of a review with no thread and no draft take no space).
- **Collapsed** (generated, vendored or too large, from the repo card patterns): the path and the counts are in `muted`, and the path ends with ` (collapsed)`. Its diff opens collapsed; the row stays in the tree.
- **Overview**: the first row, `Overview`, opens the MR description and the general comments (threads without an anchor) in the diff pane, as inline items (section 4.12) with no line.
- **Missing patch** (no local clone and the forge cut the patch): the counts are replaced by `no patch` in `muted`.
- **Direction**: a tree of folders with collapsible directories, one row per file with the same columns as right-aligned chips; viewed is a checkbox at the end of the row, and a viewed row has `--fg-muted` text. The selected row uses the row selection of section 4.2.

### 4.11 Diff hunk

A hunk header, then its lines.

- **Hunk header**: `@@ -40,7 +40,9 @@` in `muted`, the section text after it (the enclosing function) in `text`. A hunk that is new since the last review ends with `● new` in `info`, right-aligned. A gap separates two hunks.
- **Unified line**, left to right: the old line number, the new line number, the sign, the code. Line numbers are right-aligned to the width of the largest number of the file, in `muted`.

| Line | Old number | New number | Sign | Code |
| --- | --- | --- | --- | --- |
| Added | empty | shown | `+` in `success` | `success` |
| Removed | shown | empty | `-` in `danger` | `danger` |
| Context | shown | shown | space | `text` |
| Expanded context (outside the hunks, not commentable) | shown | shown | space | `muted`, between two separator lines |

- **Expanded context** opens with `expanded 30–39, not commentable` in a `muted` separator, and closes with a separator, so it differs from a hunk's own context without color.
- **Side by side**: the removed side on the left, the added side on the right, separated by a `muted` rule. Each side has its line number, its sign and its code. Removed and added lines of one change are paired in order; a side with no line left is empty. Context lines show on both sides.
- **Highlighting**: color says added, removed or context, so syntax is shown with attributes only: keywords in bold, comments in italic. Inside a changed line, the words that changed (intra-line diff) are underlined. No other color is added for syntax until a palette is chosen.
- **Long lines** wrap. A continuation line has empty line numbers and `↪` in `muted` in the sign column.
- **Tabs** show as 4 spaces. A whitespace-only change shows `·` for each changed space, in the color of the line.
- **Cursor and selection**: the cursor line has its line numbers highlighted. Side by side, the cursor is on one side: the new side by default, the old side for a row with only a removed line. It can move to the other side of a row that has both, and only the line number of that side is highlighted. A comment goes on the cursor's side; a suggestion is refused on the old side (suggestions are only on lines of the new file). The cursor also stops on inline items (section 4.12).
- **Collapsed file**: one line `Collapsed: generated file, 210 added, 198 removed.` in `muted`, with an action that expands it.
- **Direction**: added and removed lines get a background tint of `--green` and `--red` (about 15 % opacity) with a stronger tint on the changed words; this is the one exception to "a background is for selection and focus" (section 4.1), as diff tools do. Line numbers are in a gutter that is not selected with the text.

### 4.12 Inline items: thread, finding, draft, flagged draft

Items shown under the line they are anchored to (the last line of a range), in the column of the code; side by side, in the column of their side. Each item has a bar `╎` on every line, a header line and a body. The bar has the color of the item kind, and the header starts with a glyph, so the kind shows without color.

| Item | Glyph and header | Bar | Body |
| --- | --- | --- | --- |
| Forge thread, open | `◆ <author> · <age> · <n> replies` | `muted` | first comment, `text` |
| Forge thread, resolved | `◇ <author> · <age> · resolved` | `muted` | first comment, `muted` |
| Outdated thread (written on an older version) | `◆ … · outdated` | `muted` | `muted` |
| Agent finding (v0.7) | `⚑ agent · <label> (<decorations>) · <confidence>` | `warning` | finding body, then its suggestion |
| Draft comment | `✎ you · draft` (`· moved from L41` when re-anchored) | `text-strong` | my body, then its suggestion |
| Draft reply | `↳ ✎ you · draft` inside the forge thread, after its last reply | `muted` (the thread's) | my body |
| Pending resolve | `· resolve on publish` at the end of the thread header, in `text-strong` | `muted` (the thread's) | unchanged |
| Flagged draft | `⚠ you · draft · flagged: <reason>` in `danger` | `danger` | `muted` |

- **Header**: author in `text-strong`, the rest in `muted`, separated by ` · `. Ages are short (`3 h`, `2 d`).
- **Body**: Markdown, cut to 3 lines with `…` (a suggestion counts in the 3 lines) unless the item or its line is under the cursor; then the item shows all of it, with its replies indented (`↳ bob · 1 h`). A Conventional Comments label at the start of a body is styled by the table below.
- **Suggestion**: a `suggestion` line in `muted`, then the replaced lines with `-` in `danger` and the new lines with `+` in `success`. Long lines wrap with `↪` (section 4.11).
- **Flagged drafts** have no line to sit under: they are listed at the top of their file's diff, before the first hunk, with the line they had (`line 58 is not in the diff any more`). They are not published (section 4.14). `m` on a flagged draft, then `Enter` on a line, moves it there; `x` deletes it. File-level forge threads, and outdated threads whose line is no longer in the diff, are listed at the same place.
- **Keys on an item**: `e` edits a draft or a finding, `x` deletes a draft, `r` replies to a thread, `R` resolves it, `y` and `n` accept and dismiss a finding. The footer shows the keys of the item under the cursor.
- **Dismissed findings** are hidden; the view shows how many in the diff pane title (`2 dismissed`).
- **Agent findings** are a v0.7 feature; they are styled now so that the review page does not change when they arrive.

| Conventional Comments label | Token |
| --- | --- |
| any label with the `blocking` decoration | `danger`, bold |
| `issue`, `todo`, `suggestion`, `question`, `chore` | `text-strong` |
| `nitpick`, `thought`, `note` | `muted` |
| `praise` | `text` |

- **Direction**: a card under the line, full width of its side, with a left border of 3 px in the bar color and the glyph in the header. Replies are indented in the card. A flagged draft is a card at the top of the file with a `--red` border.

### 4.13 "Since last review" banner

On top of the diff of the review page, when the current head is not the last reviewed head and a last reviewed head exists (first review: no banner).

- Form: `● Since last review (<old head> → <new head>): <n> files changed, <n> viewed marks reset, <n> drafts moved, <n> drafts flagged`. `●` in `info`, the SHAs in `muted`, the counts in `text`, the flagged count in `danger`. A part with a count of 0 is not shown; when all are 0 (a rebase that changed no file), the banner says `no file changed`.
- When the banner does not fit, parts are dropped from the right, but the flagged part is kept: those drafts are not published until they are moved or deleted.
- The banner stays until the head is reviewed (a published verdict on it). The files it counts have the `●` of section 4.10 until I view them again, and their new hunks have the `● new` of section 4.11.
- **Direction**: an info banner on top of the diff, `--blue` left border, with a link that filters the file tree to the changed files.

### 4.14 Publish modal

A modal (section 4.6), titled `Publish review`.

1. **Summary**, a field grid (section 4.4): `MR` (context badge, key, title), `Head` (SHA and `current`, or `moved` in `danger`), `Comments` (new comments, replies and resolves), `Flagged` (`⚠ <n> drafts, stay local` in `danger`, shown only when there are some). The context badge is first: publishing to the wrong client must be obvious.
2. **Verdict** field: `(•) Comment   ( ) Approve   ( ) Request changes`; `←` and `→` choose. The chosen value has `(•)` and its token: `comment` in `text`, `approve` in `success`, `request changes` in `danger`. On my own MR, approve and request changes are `muted` with `(own MR)` and cannot be chosen. The default is `comment`.
3. **Review text** field: optional, with the placeholder `Optional text of the review`. In this field `Enter` adds a line.
4. **Warning**: `Everyone on the MR sees the review. loom cannot take it back.` in `text-strong`.
5. **Hint line**: `Enter publish  Tab next field  Esc cancel` on the Verdict field; `Tab next field  Esc cancel` on the Review text field.

- **States**: loomd runs the checks of [Publish](reviews.md#publish) after `Enter`.
  - **Ready**: as above. `Enter` goes to Checking.
  - **Checking**: the fields are read-only, the hint line says `Checking <host>…`.
  - **Changed**: the head moved, or an anchor is not in the forge's patch. The drafts are re-anchored or flagged, the modal is Ready again with the new summary, and a line in `danger` says what changed (`Head moved to e5f6a7b: 1 draft moved, 1 flagged.`). `Enter` asks again.
  - **Refused**: a pending review that loom did not make, or a verdict on my own MR. The hint line is the reason in `danger` (`Cannot publish: a pending review on github.com was not made by loom.`) and only `Esc` works.
  - **Publishing**: see Progress.
- **Progress**: the fields are read-only and the last two lines show the steps of this publish plan, without the steps that have nothing to do (GitLab adds `approve` for an approve verdict): `✓` done in `success`, `▸` running in `warning`, `·` waiting in `muted` (`✓ comments  ✓ replies  ▸ submit  · resolve`), then `Publishing to <host>…`. On success, the modal closes and a toast says `✓ review published on <MR key>`. On failure, the modal stays with the error in `danger` and `Enter retry  Esc close`; the publish resumes where it stopped.
- **Direction**: a dialog with the same summary, a segmented control for the verdict (with the verdict colors), a text area, and a primary button whose label is the verdict (`Approve`, `Request changes`, `Comment`). Progress replaces the buttons with a step list.

### 4.15 Review components without color

Without color, attributes (bold, italic, underline) stay and each meaning keeps a glyph or a word:

| Meaning | Without color |
| --- | --- |
| Viewed, changed since last review | `✓`, `●` |
| Collapsed file, no patch | `(collapsed)`, `no patch` |
| Added, removed lines and counts | `+`, `-` |
| Changed words | underline |
| Expanded context (not commentable) | `expanded …, not commentable` separators |
| New hunk | `● new` |
| Open, resolved, outdated thread | `◆`, `◇`, `outdated` |
| Finding, draft, flagged draft | `⚑`, `✎`, `⚠ … flagged` |
| Blocking label | the `(blocking)` decoration, bold |
| Chosen verdict | `(•)` |
| Publish steps | `✓`, `▸`, `·` |

## 5. Checklist for a new view

- [ ] Uses only the tokens of section 2.
- [ ] Columns of every list are aligned (section 4.2) and badges do not touch.
- [ ] Every modal has a border and a title (section 4.6).
- [ ] Works without color: every meaning has text or a glyph.
- [ ] A review view uses the components of sections 4.9 to 4.14 and their forms without color (section 4.15).
