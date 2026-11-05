export type RowKind = "file" | "meta" | "hunk" | "add" | "del" | "ctx";

/** One line of the patch, in unified order. Text stays in the shared array. */
export interface ParsedPatch {
  lines: string[];
  kinds: Uint8Array;
  oldNo: Int32Array;
  newNo: Int32Array;
  files: { path: string; start: number; end: number }[];
}

export const KIND: RowKind[] = ["file", "meta", "hunk", "add", "del", "ctx"];
const K_FILE = 0;
const K_META = 1;
const K_HUNK = 2;
const K_ADD = 3;
const K_DEL = 4;
const K_CTX = 5;

/** Parses a unified diff. Pure, one pass, no regular expression per line (it runs on 5 MiB). */
export function parsePatch(text: string): ParsedPatch {
  const lines = text.split("\n");
  if (lines.at(-1) === "") lines.pop();
  const n = lines.length;
  const kinds = new Uint8Array(n);
  const oldNo = new Int32Array(n);
  const newNo = new Int32Array(n);
  const files: ParsedPatch["files"] = [];
  let o = 0;
  let w = 0;
  let inHunk = false;
  for (let i = 0; i < n; i += 1) {
    const l = lines[i] as string;
    const c = l.charCodeAt(0);
    if (c === 100 && l.startsWith("diff --git ")) {
      kinds[i] = K_FILE;
      inHunk = false;
      const last = files.at(-1);
      if (last) last.end = i;
      const b = l.lastIndexOf(" b/");
      files.push({ path: b >= 0 ? l.slice(b + 3) : l, start: i, end: n });
    } else if (c === 64 && l.startsWith("@@")) {
      kinds[i] = K_HUNK;
      inHunk = true;
      const m = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(l);
      o = m ? Number(m[1]) : 0;
      w = m ? Number(m[2]) : 0;
    } else if (!inHunk) {
      kinds[i] = K_META;
    } else if (c === 43) {
      kinds[i] = K_ADD;
      newNo[i] = w;
      w += 1;
    } else if (c === 45) {
      kinds[i] = K_DEL;
      oldNo[i] = o;
      o += 1;
    } else if (c === 92) {
      kinds[i] = K_META; // "\ No newline at end of file"
    } else {
      kinds[i] = K_CTX;
      oldNo[i] = o;
      newNo[i] = w;
      o += 1;
      w += 1;
    }
  }
  return { lines, kinds, oldNo, newNo, files };
}

/** A row of the side-by-side view: indexes into the patch lines, -1 for an empty side. */
export interface SplitRows {
  left: Int32Array;
  right: Int32Array;
  /** For each patch line, its row in this view. */
  rowOf: Int32Array;
}

/** Pairs each run of removed lines with the run of added lines that follows it. Pure. */
export function buildSplit(p: ParsedPatch): SplitRows {
  const n = p.lines.length;
  const left = new Int32Array(n);
  const right = new Int32Array(n);
  const rowOf = new Int32Array(n);
  let row = 0;
  let i = 0;
  while (i < n) {
    const k = p.kinds[i];
    if (k === K_DEL || k === K_ADD) {
      let d = i;
      while (d < n && p.kinds[d] === K_DEL) d += 1;
      let a = d;
      while (a < n && p.kinds[a] === K_ADD) a += 1;
      const dels = d - i;
      const adds = a - d;
      for (let j = 0; j < Math.max(dels, adds); j += 1) {
        left[row] = j < dels ? i + j : -1;
        right[row] = j < adds ? d + j : -1;
        if (j < dels) rowOf[i + j] = row;
        if (j < adds) rowOf[d + j] = row;
        row += 1;
      }
      i = a;
    } else {
      left[row] = i;
      right[row] = i;
      rowOf[i] = row;
      row += 1;
      i += 1;
    }
  }
  return { left: left.subarray(0, row), right: right.subarray(0, row), rowOf };
}
