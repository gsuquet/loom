export interface LiveRow {
  id: string;
  title: string;
  status: string;
  removed?: boolean;
}

export interface LiveState {
  rows: LiveRow[];
  /** Rows that wait behind the "N new" line. */
  pending: LiveRow[];
  cursorId: string | null;
}

export const EMPTY_LIVE: LiveState = { rows: [], pending: [], cursorId: null };

/**
 * Merges a fresh task list. Pure. Rows on screen keep their place and only change content; new
 * rows wait in `pending`, so nothing moves under the cursor (ux-challenge M22).
 * The first load has nothing to protect, so it shows everything.
 */
export function mergeLive(s: LiveState, fresh: LiveRow[]): LiveState {
  if (s.rows.length === 0 && s.pending.length === 0) {
    return { rows: fresh, pending: [], cursorId: fresh[0]?.id ?? null };
  }
  const byId = new Map(fresh.map((r) => [r.id, r]));
  const shown = new Set(s.rows.map((r) => r.id));
  const rows = s.rows.map((r) => byId.get(r.id) ?? { ...r, removed: true });
  const pending = fresh.filter((r) => !shown.has(r.id));
  return { ...s, rows, pending };
}

/** Shows the waiting rows at the end, and drops removed rows other than the one under the cursor. */
export function applyPending(s: LiveState): LiveState {
  const rows = s.rows.filter((r) => !r.removed || r.id === s.cursorId);
  return { ...s, rows: [...rows, ...s.pending], pending: [] };
}

export function moveCursor(s: LiveState, delta: number): LiveState {
  const i = s.rows.findIndex((r) => r.id === s.cursorId);
  const next = s.rows[Math.max(0, Math.min(s.rows.length - 1, i + delta))];
  return next ? { ...s, cursorId: next.id } : s;
}
