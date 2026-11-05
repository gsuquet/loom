import { memo, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { flushSync } from "react-dom";
import { buildSplit, KIND, type ParsedPatch } from "../diff/patch";
import type { DraftComment } from "./fixtures";
import { workSamples } from "./perf";

export const ROW_HEIGHT = 18;
const OVERSCAN = 12;

interface Props {
  patch: ParsedPatch;
  view: "unified" | "split";
  cursor: number;
  comments: DraftComment[];
  focused: boolean;
  scrollerRef: (el: HTMLDivElement | null) => void;
}

// Text nodes only: `lines[i]` goes through React as a string child, never as HTML.
const Cell = memo(function Cell({
  patch,
  i,
  active,
}: {
  patch: ParsedPatch;
  i: number;
  active: boolean;
}) {
  if (i < 0) return <div className="cell empty" />;
  const kind = KIND[patch.kinds[i] as number];
  const line = patch.lines[i] as string;
  const text = kind === "add" || kind === "del" || kind === "ctx" ? line.slice(1) : line;
  const marker = kind === "add" ? "+" : kind === "del" ? "−" : " ";
  return (
    <div className={`cell ${kind}${active ? " cursor" : ""}`}>
      <span className="no">{patch.oldNo[i] || ""}</span>
      <span className="no">{patch.newNo[i] || ""}</span>
      <span className="sign">{marker}</span>
      <span className="text">{text}</span>
    </div>
  );
});

export function DiffPane({ patch, view, cursor, comments, focused, scrollerRef }: Props) {
  const split = useMemo(() => (view === "split" ? buildSplit(patch) : null), [patch, view]);
  const rowCount = split ? split.left.length : patch.lines.length;
  const cursorRow = split ? (split.rowOf[cursor] ?? 0) : cursor;
  const scroller = useRef<HTMLDivElement | null>(null);
  const [first, setFirst] = useState(0);
  const [height, setHeight] = useState(800);

  const attach = useCallback(
    (el: HTMLDivElement | null) => {
      scroller.current = el;
      scrollerRef(el);
    },
    [scrollerRef],
  );

  useEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    setHeight(el.clientHeight);
    return () => ro.disconnect();
  }, []);

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    const next = Math.floor(el.scrollTop / ROW_HEIGHT);
    if (workSamples.on) {
      const t = performance.now();
      flushSync(() => setFirst(next));
      void el.offsetHeight; // Forces layout, so the sample includes it.
      workSamples.values.push(performance.now() - t);
    } else {
      setFirst(next);
    }
  }, []);

  // Keeps the cursor in view when keys move it. It never scrolls on other updates.
  // biome-ignore lint/correctness/useExhaustiveDependencies: only the cursor and the view move it.
  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el) return;
    const top = cursorRow * ROW_HEIGHT;
    if (top < el.scrollTop) el.scrollTop = top;
    else if (top + ROW_HEIGHT > el.scrollTop + el.clientHeight)
      el.scrollTop = top + ROW_HEIGHT - el.clientHeight;
  }, [cursorRow, view]);

  const visible = Math.ceil(height / ROW_HEIGHT);
  const start = Math.max(0, first - OVERSCAN);
  const end = Math.min(rowCount, first + visible + OVERSCAN);
  const commented = useMemo(
    () => new Set(comments.filter((c) => !c.dropped).map((c) => c.line)),
    [comments],
  );

  const rows = [];
  for (let r = start; r < end; r += 1) {
    if (split) {
      const l = split.left[r] as number;
      const w = split.right[r] as number;
      const isCursor = r === cursorRow;
      rows.push(
        <div className="row split" key={r} data-row={r}>
          <Cell patch={patch} i={l} active={isCursor && l === cursor} />
          <Cell patch={patch} i={w} active={isCursor && w === cursor} />
          {commented.has(l) || commented.has(w) ? <span className="mark">●</span> : null}
        </div>,
      );
    } else {
      rows.push(
        <div className="row" key={r} data-row={r}>
          <Cell patch={patch} i={r} active={r === cursorRow} />
          {commented.has(r) ? <span className="mark">●</span> : null}
        </div>,
      );
    }
  }

  return (
    <div
      className={`diff${focused ? " focused" : ""}`}
      data-testid="diff"
      data-view={view}
      data-cursor={cursor}
      ref={attach}
      onScroll={onScroll}
    >
      <div className="sizer" style={{ height: rowCount * ROW_HEIGHT }}>
        <div className="window" style={{ transform: `translateY(${start * ROW_HEIGHT}px)` }}>
          {rows}
        </div>
      </div>
    </div>
  );
}
