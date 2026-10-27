# UI style

How loom looks on every surface: the TUI now, the web UI (and a desktop shell of it) later. The rules are written as **semantic tokens** and **components**; each surface maps them to what it can draw. A new view uses these components; a new component is added here first.

The TUI mapping is normative from v0.3; the review components (sections 4.9 to 4.16) are normative from v0.4. The web mapping is a direction, settled when the web UI is designed (v0.11, see [vision](vision.md)).

## 1. Principles

1. **Meaning before decoration.** Color, weight and position say something (state, importance, focus). Nothing is colored only to look nice.
2. **Never color alone.** Every colored element also has a text label or a glyph, so the UI works with `--color never`, `NO_COLOR`, and for color-blind users.
3. **Follow the user's theme.** The TUI uses the terminal's 16 ANSI colors only ([ADR-0014](../adr/0014-tui-architecture.md), decision 6); the web UI follows the system light or dark preference.
4. **Columns line up.** Rows of a list are a table: every badge has its own column, and columns have the same width in every row.
5. **One focus.** At any time exactly one thing receives keys, and it is the only thing that looks focused.
6. **Transient things go away.** A confirmation disappears by itself; an error stays until it is resolved.

## 2. Color tokens

| Token | Use | TUI (ANSI) | Web (direction) |
| --- | --- | --- | --- |
| `text` | Normal text | terminal default (`Reset`) | `--fg` |
| `text-strong` | Titles, labels of the focused element, key names in hints | default + bold | `--fg`, weight 600 |
| `muted` | Labels, secondary metadata, hints, separators, unfocused borders | `bright_black` (`DarkGray`) | `--fg-muted` |
| `danger` | Errors, read-only banner, `p0`, removed lines, flagged drafts, blocking review labels, request changes, a moved head | `bright_red` | `--red` |
| `warning` | `p1`, `active` status, agent findings, a running step | `bright_yellow` / `yellow` | `--amber` |
| `success` | `ready` status, success toasts, added lines, viewed files, approve | `green` | `--green` |
| `info` | `inbox` status, code, changes since the last review | `cyan` | `--blue` |
| `context-<name>` | Context badges | configured color, else the default cycle ([reference](../reference/tui.md#context-colors)) | same palette, as a hue |

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

The TUI unit is one terminal cell; the web unit is 4 px (`1u`).

| Rule | TUI | Web |
| --- | --- | --- |
| Gap between two badges or columns of a row | 1 cell | `2u` |
| Padding inside a pane or modal, left and right | 1 cell | `4u` |
| Gap between two sections of a pane | 1 blank line | `4u` |
| Gap between two fields of a form | 0 (the field borders separate them) | `3u` |
| Label column of a field grid | right-aligned, width of the longest label | same, as a grid column |

Only bold, italic, underline and reverse are used as attributes in the TUI. Dim (`Modifier::DIM`) is not used: some terminals ignore it.

## 4. Components

### 4.1 Badge

A short value with a fixed meaning: importance, status, context, kind.

- Importance and status are colored text (their token), with no brackets.
- A context is `[name]` in its context color, bold.
- A kind is `muted` text.
- Badges do not have a background. A background is reserved for selection and focus. The web diff is the one exception (section 4.11).

### 4.2 List row

A row of a queue, left to right:

| Column | Width |
| --- | --- |
| Selection marker | 2 cells: `▶ ` on the selected row, else 2 spaces |
| Tree indent (Today, grouped) | `└─ ` for a child, 2 more cells for each deeper level |
| Importance | 2 cells (`p0` to `p3`) |
| Status | width of the longest status in the visible rows, at most 10 cells, cut with `…` like the kind |
| Context | width of the longest `[context]` in the visible rows |
| Kind | width of the longest kind in the visible rows, at most 10 cells; a longer kind is cut and ends with `…` |
| Title | the rest |
| Suffixes (`until`, `on`, `parent`, `why`, `ro`) | after the title, `muted` (`ro` is `danger`) |

Columns are separated by one cell (section 3). A column never touches the next one, whatever the length of its value.

**Selection**: the marker `▶` and the title in reverse video and bold. The badges keep their own colors on the selected row, so the selection does not change their meaning. The web UI uses a background tint on the whole row and a left accent bar instead.

### 4.3 Pane

A bordered region with a title.

- The title is ` Name ` (one space on each side) on the top border, in `text`. While a screen has only one pane that takes keys (v0.3), focus is not marked on panes; when two panes can take keys, the focused one gets a `text-strong` title and a thick border, like a form field (section 4.6).
- The content has the padding of section 3.
- An empty pane says why, in `muted`: `No tasks in this queue.`, `No item selected.`

### 4.4 Field grid (task overview, preview)

The read view of an object:

1. The read-only banner, if any: `⚠ READ-ONLY: <reason>` in `danger`, bold, then a blank line.
2. The title, in `text-strong`.
3. The fields, one per line: a `muted` label column (section 3), one space, the value styled as its badge (section 4.1). Order: Status, Importance, Kind, Context, Due, Waiting, Rank, Parent, ID. A field with no value is not shown.
4. A blank line, then each body section (Notes).

### 4.5 Section header

A labeled rule that opens a section of a pane: `── Notes ` followed by `─` to the right edge of the content, in `muted`, with the label in `text-strong`.

When a section shows Markdown and the Markdown starts with a heading equal to the section label (`## Notes` under the Notes header), that heading is not shown: the header already says it.

### 4.6 Modal

A dialog on top of the current view.

- Centered, with a cleared background, and **always** a border with the modal title (` New task `). Nothing of the view below shows inside it.
- The content has the padding of section 3.
- A form is a column of fields. Each field has a bordered box with its label as title.
  - The focused field has a thick border (`┏━┓`) and a `text-strong` label. The other fields have a plain border in `muted`. A field is never filled with reverse video.
  - A text input shows its value followed by a cursor `▏` when it has the focus, and a `muted` italic placeholder when it is empty (italic, so it differs from a value without color).
  - A read-only value in a form (for example the context of a new task) is shown as its badge, with no input box.
- The last line is the hint line (section 4.7) or the modal's error in `danger`.

### 4.7 Hint line

Key hints: the key in `text-strong`, one space, the action in `muted`; two spaces between two hints (`Enter create  Esc cancel`). The same format is used in the footer and in modals.

### 4.8 Footer and toasts

The bottom line of the screen shows one of these, in order of priority:

1. **Error banner**: `[Error: <message>]` in `danger`. It stays until the error is resolved (for example the next successful refresh) or replaced.
2. **Toast**: the result of an action, for example `✓ task created in inbox`. A success starts with `✓` in `success`; information has no glyph and uses `text`. A toast disappears after **4 seconds** or at the next key press, whichever comes first.
3. **Hints** of the active view (section 4.7).

The web UI shows toasts in a corner stack with the same lifetime rules, and errors in a banner on top of the view.

### 4.9 Review page

The object page of a Review task (v0.4, [Code review](user-workflow.md#code-review), [MR review](reviews.md)). It uses the components 4.10 to 4.14.

- **Line 1, title**: `Review <MR key> <MR title>`, then the context badge, the head SHA (7 characters) and the totals, in `muted` except the title (`text-strong`) and the badge. At 160 columns the author and the base branch follow. Fields that do not fit are dropped from the right; the title is cut with `…` before the badge is dropped.
- **Banner lines**: the "since last review" banner (section 4.13), then the reduced-view notice when there is no local clone (`No local clone: no context expansion, no worktree. Clone <remote URL>` in `muted`). Each takes one line when it applies; with none, the panes start on line 2.
- **Panes**: the file tree (`Files`) and the diff (title: the file path and its counts). At 120 columns or more both are shown, the file tree on the left, as wide as its longest row, at most one third of the screen and at least 28 cells. Under 120 columns one pane fills the screen and `|` shows the other one.
- **Focus**: the file tree and the diff both take keys, so the focused pane has a thick border and a `text-strong` title, and the other has a plain `muted` border (section 4.3). `Tab` moves the focus to the other pane when both are shown. In the pane without focus, the selected row keeps its `▶` marker but not the reverse video, so only one thing looks focused.
- **Unified or side by side**: side by side by default when the content of the diff pane is at least 100 cells wide, else unified. `S` switches between the two; a diff pane too narrow for side by side always shows unified.
- **Last line**: the footer (section 4.8), with the hints of the focused pane and of the item under the cursor (for example `y accept  n dismiss  e edit` on a finding).

Sketch R1, 80 columns, file tree focused:

```text
Review acme/shop#412 auth refactor  [company-b]  head c3d4e5f  5 files +355 -235
● Since last review (a1b2c3d → c3d4e5f): 2 files changed, 1 draft flagged
┏ Files ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ ▶ ● src/auth/jwt.rs         +88  -31 ◆2 ⚑1 ✎1 ⚠1                             ┃
┃   ✓ src/auth/mod.rs         +12   -4 ◇1                                      ┃
┃   ● src/api/login.rs         +5   -2                                         ┃
┃     docs/auth.md            +40                                              ┃
┃   ✓ Cargo.lock (collapsed) +210 -198                                         ┃
┃                                                                              ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
enter open  v viewed  }/{ file  | diff  P publish  ? help
```

Sketch R2, 80 columns, diff focused (unified):

```text
Review acme/shop#412 auth refactor  [company-b]  head c3d4e5f  5 files +355 -235
● Since last review (a1b2c3d → c3d4e5f): 2 files changed, 1 draft flagged
┏ src/auth/jwt.rs +88 -31 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ ╎ ⚠ you · draft · flagged: line 58 is not in the diff any more               ┃
┃ ╎ nitpick: rename `tmp` to `raw`.                                            ┃
┃                                                                              ┃
┃ @@ -40,5 +40,7 @@ fn verify(token: &str) -> Result<Claims>             ● new ┃
┃    40  40   let claims = decode(token)?;                                     ┃
┃    41     - if claims.exp < now() {                                          ┃
┃        41 + if claims.exp + LEEWAY <= now() {                                ┃
┃             ╎ ◆ alice · 3 h · 1 reply                                        ┃
┃             ╎ why not a configurable leeway?                                 ┃
┃    42  42       return Err(Error::Expired);                                  ┃
┃    43  43   }                                                                ┃
┃        44 + // LEEWAY covers clock skew between hosts.                       ┃
┃ ▶      45 + check_issuer(&claims)?;                                          ┃
┃             ╎ ⚑ agent · issue (blocking) · 0.82                              ┃
┃             ╎ `nbf` is not checked: same clock skew.                         ┃
┃             ╎ ✎ you · draft                                                  ┃
┃             ╎ issue: return the issuer in the error.                         ┃
┃             ╎ suggestion                                                     ┃
┃             ╎ - check_issuer(&claims)?;                                      ┃
┃             ╎ + check_issuer(&claims).context("iss")?;                       ┃
┃    44  46   Ok(claims)                                                       ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
]/[ hunk  }/{ file  c comment  r reply  y/n finding  | files  P publish
```

Sketch R3, 160 columns, diff focused (side by side):

```text
Review acme/shop#412 auth refactor  [company-b]  head c3d4e5f  5 files +355 -235  author dana  base main
● Since last review (a1b2c3d → c3d4e5f): 2 files changed, 2 viewed marks reset, 1 draft moved, 1 draft flagged
┌ Files ───────────────────────────────────────────┐┏ src/auth/jwt.rs +88 -31 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
│ ▶ ● src/auth/jwt.rs         +88  -31 ◆2 ⚑1 ✎1 ⚠1 │┃ ╎ ⚠ you · draft · flagged: line 58 is not in the diff any more                                           ┃
│   ✓ src/auth/mod.rs         +12   -4 ◇1          │┃ ╎ nitpick: rename `tmp` to `raw`.                                                                        ┃
│   ● src/api/login.rs         +5   -2             │┃                                                                                                          ┃
│     docs/auth.md            +40                  │┃ @@ -40,5 +40,7 @@ fn verify(token: &str) -> Result<Claims>                                         ● new ┃
│   ✓ Cargo.lock (collapsed) +210 -198             │┃    40   let claims = decode(token)?;                │  40   let claims = decode(token)?;                 ┃
│                                                  │┃    41 - if claims.exp < now() {                     │  41 + if claims.exp + LEEWAY <= now() {            ┃
│                                                  │┃                                                             ╎ ◆ alice · 3 h · 1 reply                    ┃
│                                                  │┃                                                             ╎ why not a configurable leeway?             ┃
│                                                  │┃    42       return Err(Error::Expired);             │  42       return Err(Error::Expired);              ┃
│                                                  │┃    43   }                                           │  43   }                                            ┃
│                                                  │┃                                                     │  44 + // LEEWAY covers clock skew between hosts.   ┃
│                                                  │┃ ▶                                                   │  45 + check_issuer(&claims)?;                      ┃
│                                                  │┃                                                             ╎ ⚑ agent · issue (blocking) · 0.82          ┃
│                                                  │┃                                                             ╎ `nbf` is not checked: same clock skew.     ┃
│                                                  │┃                                                             ╎ ✎ you · draft                              ┃
│                                                  │┃                                                             ╎ issue: return the issuer in the error.     ┃
│                                                  │┃                                                             ╎ suggestion                                 ┃
│                                                  │┃                                                             ╎ - check_issuer(&claims)?;                  ┃
│                                                  │┃                                                             ╎ + check_issuer(&claims).context("iss")?;   ┃
│                                                  │┃    44   Ok(claims)                                  │  46   Ok(claims)                                   ┃
└──────────────────────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
]/[ hunk  }/{ file  v viewed  c comment  V select  s suggest  r reply  R resolve  y/n finding  o editor  w worktree  Tab files  P publish  ? help
```

### 4.10 File tree row

One row per changed file, in the order of the diff. A list row (section 4.2) with these columns, left to right:

| Column | Width | Content |
| --- | --- | --- |
| Selection marker | 2 cells | `▶ ` on the selected row (marker and file name in reverse video and bold, section 4.2), else 2 spaces |
| State | 2 cells | `✓` in `success`: viewed in this version. Else `●` in `info`: changed since the last review (section 4.13), so its viewed mark was reset. Else a space |
| Path | width of the longest path in the visible rows | directory in `muted`, file name in `text`; a long path is cut on the left (`…/auth/jwt.rs`), never the file name. A renamed file shows `old → new` |
| Added | width of the largest count, right-aligned | `+12` in `success`; empty when 0 |
| Removed | same | `-4` in `danger`; empty when 0 |
| Threads | 2 cells or more | `◆2` in `text`: unresolved forge threads. `◇1` in `muted` when all threads are resolved |
| Findings | same | `⚑1` in `warning` (agent findings, v0.7) |
| Drafts | same | `✎1` in `text-strong`: my draft comments and replies |
| Flagged | same | `⚠1` in `danger`: flagged drafts |

- A column with no value in the visible rows is not drawn (the four counts columns of a review with no thread and no draft take no space).
- **Collapsed** (generated, vendored or too large, from the repo card patterns): the path and the counts are in `muted`, and the path ends with ` (collapsed)`. Its diff opens collapsed; the row stays in the tree.
- **Overview**: the first row, `Overview`, opens the MR description and the general comments (threads without an anchor) in the diff pane, as inline items (section 4.12) with no line.
- **Missing patch** (no local clone and the forge cut the patch): the counts are replaced by `no patch` in `muted`.
- Sketches R1 (80 columns) and R3 (160 columns).
- **Web**: a tree of folders with collapsible directories, one row per file with the same columns as right-aligned chips; viewed is a checkbox at the end of the row, and a viewed row has `--fg-muted` text. The selected row uses the row selection of section 4.2.

### 4.11 Diff hunk

A hunk header, then its lines.

- **Hunk header**: `@@ -40,7 +40,9 @@` in `muted`, the section text after it (the enclosing function) in `text`. A hunk that is new since the last review ends with `● new` in `info`, right-aligned. One blank line separates two hunks.
- **Unified line**, left to right: the cursor marker (2 cells, `▶ ` on the cursor line), the old line number, one space, the new line number, one space, the sign, one space, the code. Line numbers are right-aligned to the width of the largest number of the file, in `muted`.

| Line | Old number | New number | Sign | Code |
| --- | --- | --- | --- | --- |
| Added | empty | shown | `+` in `success` | `success` |
| Removed | shown | empty | `-` in `danger` | `danger` |
| Context | shown | shown | space | `text` |
| Expanded context (outside the hunks, not commentable) | shown | shown | space | `muted`, between two separator lines |

- **Expanded context** opens with `┄ expanded 30–39, not commentable ┄` and closes with `┄` to the right edge, both in `muted`, so it differs from a hunk's own context without color.
- **Side by side**: the removed side on the left, the added side on the right, separated by ` │ ` in `muted`. Each side has its line number, its sign and its code. Removed and added lines of one change are paired in order; a side with no line left is empty. Context lines show on both sides.
- **Highlighting**: color says added, removed or context, so syntax is shown with attributes only: keywords in bold, comments in italic. Inside a changed line, the words that changed (intra-line diff) are underlined. No other color is added for syntax; the 16 ANSI colors cannot hold both meanings.
- **Long lines** wrap. A continuation line has empty line numbers and `↪` in `muted` in the sign column.
- **Tabs** show as 4 spaces. A whitespace-only change shows `·` for each changed space, in the color of the line.
- **Cursor and selection**: the cursor line has `▶ ` and its line numbers in reverse video. Side by side, the cursor is on one side: the new side by default, the old side for a row with only a removed line. `←` and `→` move it to the other side of a row that has both, and only the line number of that side is in reverse video. `c` comments on the cursor's side; `s` is refused on the old side (suggestions are only on lines of the new file). A `V` selection shows `┃` in `text-strong` in the marker column of each selected line. The cursor also stops on inline items (section 4.12).
- **Collapsed file**: one line `Collapsed: generated file, 210 added, 198 removed. Enter expands.` in `muted`.
- Sketches R2 (unified, 80 columns) and R3 (side by side, 160 columns).
- **Web**: a monospace table. Added and removed lines get a background tint of `--green` and `--red` (about 15 % opacity) with a stronger tint on the changed words; this is the one web exception to "a background is for selection and focus" (section 4.1), as diff tools do. Syntax colors come from a palette chosen with the web UI (v0.11); until then, weight and italic as in the TUI. Line numbers are in a gutter that is not selected with the text.

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
- **Body**: Markdown, cut to 3 lines with `…` (a suggestion counts in the 3 lines) unless the item or its line is under the cursor; then the item shows all of it, with its replies indented by 2 cells (`↳ bob · 1 h`). A Conventional Comments label at the start of a body is styled by the table below.
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

- Sketches R2 (80 columns) and R3 (160 columns).
- **Web**: a card under the line, full width of its side, with a left border of 3 px in the bar color and the glyph in the header. Replies are indented in the card. A flagged draft is a card at the top of the file with a `--red` border.

### 4.13 "Since last review" banner

Line 2 of the review page when the current head is not the last reviewed head and a last reviewed head exists (first review: no banner).

- Form: `● Since last review (<old head> → <new head>): <n> files changed, <n> viewed marks reset, <n> drafts moved, <n> drafts flagged`. `●` in `info`, the SHAs in `muted`, the counts in `text`, the flagged count in `danger`. A part with a count of 0 is not shown; when all are 0 (a rebase that changed no file), the banner says `no file changed`.
- When the banner does not fit, parts are dropped from the right, but the flagged part is kept: those drafts are not published until they are moved or deleted.
- The banner stays until the head is reviewed (a published verdict on it). The files it counts have the `●` of section 4.10 until I view them again, and their new hunks have the `● new` of section 4.11.
- Sketches R1, R2 (80 columns) and R3 (160 columns).
- **Web**: an info banner on top of the diff, `--blue` left border, with a link that filters the file tree to the changed files.

### 4.14 Publish modal

`P` opens it. A modal (section 4.6), 72 cells wide or the screen width minus 4, whichever is smaller, titled ` Publish review `.

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
- Sketch R4 (80 columns, choosing the verdict) and R5 (160 columns, publishing).
- **Web**: a dialog with the same summary, a segmented control for the verdict (with the verdict colors), a text area, and a primary button whose label is the verdict (`Approve`, `Request changes`, `Comment`). Progress replaces the buttons with a step list.

Sketch R4, 80 columns:

```text
Review acme/shop#412 auth refactor  [company-b]  head c3d4e5f  5 files +355 -235
● Since last review (a1b2c3d → c3d4e5f): 2 files changed, 1 draft flagged
┏ src/auth/jwt.rs +88 -31 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
┃ ╎ ⚠ you · draft · flagged: line 58 is not in the diff any more               ┃
┃ ╎ nitpick: rename `tmp` to `raw`.                                            ┃
┃   ┌ Publish review ──────────────────────────────────────────────────────┐   ┃
┃ @@│       MR [company-b] acme/shop#412 auth refactor                     │ew ┃
┃   │     Head c3d4e5f, current                                            │   ┃
┃   │ Comments 3 new, 1 reply, 1 resolve                                   │   ┃
┃   │  Flagged ⚠ 1 draft, stays local                                      │   ┃
┃   │                                                                      │   ┃
┃   │ ┏ Verdict ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓ │   ┃
┃   │ ┃ (•) Comment   ( ) Approve   ( ) Request changes                  ┃ │   ┃
┃   │ ┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛ │   ┃
┃   │ ┌ Review text ─────────────────────────────────────────────────────┐ │   ┃
┃ ▶ │ │ Optional text of the review                                      │ │   ┃
┃   │ └──────────────────────────────────────────────────────────────────┘ │   ┃
┃   │                                                                      │   ┃
┃   │ Everyone on the MR sees the review. loom cannot take it back.        │   ┃
┃   │ Enter publish  Tab next field  Esc cancel                            │   ┃
┃   └──────────────────────────────────────────────────────────────────────┘   ┃
┃             ╎ - check_issuer(&claims)?;                                      ┃
┃             ╎ + check_issuer(&claims).context("iss")?;                       ┃
┃    44  46   Ok(claims)                                                       ┃
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
]/[ hunk  }/{ file  c comment  r reply  y/n finding  | files  P publish
```

Sketch R5, 160 columns:

```text
Review acme/shop#412 auth refactor  [company-b]  head c3d4e5f  5 files +355 -235  author dana  base main
● Since last review (a1b2c3d → c3d4e5f): 2 files changed, 2 viewed marks reset, 1 draft moved, 1 draft flagged
┌ Files ───────────────────────────────────────────┐┏ src/auth/jwt.rs +88 -31 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┓
│ ▶ ● src/auth/jwt.rs         +88  -31 ◆2 ⚑1 ✎1 ⚠1 │┃ ╎ ⚠ you · draft · flagged: line 58 is not in the diff any more                                           ┃
│   ✓ src/auth/mod.rs         +12   -4 ◇1   ┌ Publish review ──────────────────────────────────────────────────────┐                                           ┃
│   ● src/api/login.rs         +5   -2      │       MR [company-b] acme/shop#412 auth refactor                     │                                           ┃
│     docs/auth.md            +40           │     Head c3d4e5f, current                                            │                                     ● new ┃
│   ✓ Cargo.lock (collapsed) +210 -198      │ Comments 3 new, 1 reply, 1 resolve                                   │t claims = decode(token)?;                 ┃
│                                           │  Flagged ⚠ 1 draft, stays local                                      │ claims.exp + LEEWAY <= now() {            ┃
│                                           │                                                                      │◆ alice · 3 h · 1 reply                    ┃
│                                           │ ┌ Verdict ─────────────────────────────────────────────────────────┐ │why not a configurable leeway?             ┃
│                                           │ │ (•) Comment   ( ) Approve   ( ) Request changes                  │ │  return Err(Error::Expired);              ┃
│                                           │ └──────────────────────────────────────────────────────────────────┘ │                                           ┃
│                                           │ ┌ Review text ─────────────────────────────────────────────────────┐ │ LEEWAY covers clock skew between hosts.   ┃
│                                           │ │ Thanks, two points before merge.                                 │ │eck_issuer(&claims)?;                      ┃
│                                           │ └──────────────────────────────────────────────────────────────────┘ │⚑ agent · issue (blocking) · 0.82          ┃
│                                           │                                                                      │`nbf` is not checked: same clock skew.     ┃
│                                           │ ✓ comments  ✓ replies  ▸ submit  · resolve                           │✎ you · draft                              ┃
│                                           │ Publishing to github.com…                                            │issue: return the issuer in the error.     ┃
│                                           └──────────────────────────────────────────────────────────────────────┘suggestion                                 ┃
│                                                  │┃                                                             ╎ - check_issuer(&claims)?;                  ┃
│                                                  │┃                                                             ╎ + check_issuer(&claims).context("iss")?;   ┃
│                                                  │┃    44   Ok(claims)                                  │  46   Ok(claims)                                   ┃
└──────────────────────────────────────────────────┘┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛
]/[ hunk  }/{ file  v viewed  c comment  V select  s suggest  r reply  R resolve  y/n finding  o editor  w worktree  Tab files  P publish  ? help
```

### 4.15 Review components without color

With `--color never` or `NO_COLOR`, attributes (bold, italic, underline, reverse) stay and each meaning keeps a glyph or a word:

| Meaning | Without color |
| --- | --- |
| Viewed, changed since last review | `✓`, `●` |
| Collapsed file, no patch | `(collapsed)`, `no patch` |
| Added, removed lines and counts | `+`, `-` |
| Changed words | underline |
| Expanded context (not commentable) | `┄ expanded …, not commentable ┄` separator lines |
| New hunk | `● new` |
| Open, resolved, outdated thread | `◆`, `◇`, `outdated` |
| Finding, draft, flagged draft | `⚑`, `✎`, `⚠ … flagged` |
| Blocking label | the `(blocking)` decoration, bold |
| Chosen verdict | `(•)` |
| Publish steps | `✓`, `▸`, `·` |

### 4.16 Web mapping of the review components

| Component | TUI | Web |
| --- | --- | --- |
| Review page | two panes and `Tab`; one pane and `\|` under 120 columns | file tree in a sidebar, diff in the main column; sidebar hidden under 960 px |
| File tree row | list row with glyph and count columns | tree with chips and a viewed checkbox |
| Diff hunk | colored signs and code, attributes for syntax | tinted line backgrounds, syntax palette at v0.11 |
| Inline item | `╎` bar in the kind color, glyph in the header | card with a 3 px colored left border |
| Since last review banner | line 2, `●` in `info` | info banner with a filter link |
| Publish modal | modal, radio verdict, step line | dialog, segmented verdict, step list |

## 5. Checklist for a new view

- [ ] Uses only the tokens of section 2.
- [ ] Columns of every list are aligned (section 4.2) and badges do not touch.
- [ ] Every modal has a border and a title (section 4.6).
- [ ] Works with `--color never`: every meaning has text or a glyph.
- [ ] Has a `TestBackend` snapshot test at 80 and 160 columns.
- [ ] A review view uses the components of sections 4.9 to 4.14 and their forms without color (section 4.15).
