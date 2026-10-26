# UI style

How loom looks on every surface: the TUI now, the web UI (and a desktop shell of it) later. The rules are written as **semantic tokens** and **components**; each surface maps them to what it can draw. A new view uses these components; a new component is added here first.

The TUI mapping is normative for v0.3. The web mapping is a direction, settled when the web UI is designed (v0.11, see [vision](vision.md)).

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
| `danger` | Errors, read-only banner, `p0` | `bright_red` | `--red` |
| `warning` | `p1`, `active` status | `bright_yellow` / `yellow` | `--amber` |
| `success` | `ready` status, success toasts | `green` | `--green` |
| `info` | `inbox` status, code | `cyan` | `--blue` |
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
- Badges do not have a background. A background is reserved for selection and focus.

### 4.2 List row

A row of a queue, left to right:

| Column | Width |
| --- | --- |
| Selection marker | 2 cells: `▶ ` on the selected row, else 2 spaces |
| Tree indent (Today, grouped) | `└─ ` for a child, 2 more cells for each deeper level |
| Importance | 2 cells (`p0` to `p3`) |
| Status | width of the longest status in the visible rows |
| Context | width of the longest `[context]` in the visible rows |
| Kind | width of the longest kind in the visible rows, at most 10 cells; a longer kind is cut and ends with `…` |
| Title | the rest |
| Suffixes (`until`, `on`, `parent`, `why`, `ro`) | after the title, `muted` (`ro` is `danger`) |

Columns are separated by one cell (section 3). A column never touches the next one, whatever the length of its value.

**Selection**: the marker `▶` and the title in reverse video and bold. The badges keep their own colors on the selected row, so the selection does not change their meaning. The web UI uses a background tint on the whole row and a left accent bar instead.

### 4.3 Pane

A bordered region with a title.

- The title is ` Name ` (one space on each side) on the top border, in `text-strong` when the pane has the focus, else `text`.
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
  - A text input shows its value followed by a cursor `▏` when it has the focus, and a `muted` placeholder when it is empty.
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

## 5. Checklist for a new view

- [ ] Uses only the tokens of section 2.
- [ ] Columns of every list are aligned (section 4.2) and badges do not touch.
- [ ] Every modal has a border and a title (section 4.6).
- [ ] Works with `--color never`: every meaning has text or a glyph.
- [ ] Has a `TestBackend` snapshot test at 80 and 160 columns.
