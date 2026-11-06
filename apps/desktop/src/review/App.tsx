import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import type { LoomApi } from "../api/loom-api";
import { type ParsedPatch, parsePatch } from "../diff/patch";
import type { TaskSummary } from "../generated/api";
import { DiffPane } from "./DiffPane";
import { type DraftComment, INITIAL_COMMENTS, MR } from "./fixtures";
import { type Action, resolveKey } from "./keys";
import {
  applyPending,
  EMPTY_LIVE,
  type LiveRow,
  type LiveState,
  mergeLive,
  moveCursor,
} from "./live-list";
import { BodyRenderer } from "./Markdown";
import { type BenchResult, scrollBench, workBench } from "./perf";

type LiveAction = { t: "merge"; rows: LiveRow[] } | { t: "apply" } | { t: "move"; delta: number };

function liveReducer(s: LiveState, a: LiveAction): LiveState {
  if (a.t === "merge") return mergeLive(s, a.rows);
  if (a.t === "apply") return applyPending(s);
  return moveCursor(s, a.delta);
}

const toRow = (t: TaskSummary): LiveRow => ({
  id: t.id,
  title: t.task?.title ?? "(invalid task)",
  status: t.task?.status ?? "unknown",
});

declare global {
  interface Window {
    __spike?: SpikeHook;
  }
}
/** Read by the tests and the bench. It holds numbers and a scroll bench, nothing that writes. */
export interface SpikeHook {
  api: string;
  visibleMs: number | null;
  committedMs: number | null;
  patchBytes: number;
  parseMs: number | null;
  fetchMs: number | null;
  eventsSeen: number;
  scrollBench: (frames: number, px: number) => Promise<BenchResult>;
  workBench: (steps: number, px: number) => Promise<unknown>;
  reopen: () => Promise<number>;
}

export function App({ api }: { api: LoomApi }) {
  const [patch, setPatch] = useState<ParsedPatch | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [view, setView] = useState<"unified" | "split">("unified");
  const [cursor, setCursor] = useState(0);
  const [comments, setComments] = useState<DraftComment[]>(INITIAL_COMMENTS);
  const [editing, setEditing] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [focus, setFocus] = useState<"diff" | "list">("diff");
  const [viewed, setViewed] = useState<Set<string>>(new Set());
  const [toast, setToast] = useState("");
  const [link, setLink] = useState("connecting");
  const [live, dispatch] = useReducer(liveReducer, EMPTY_LIVE);
  const scroller = useRef<HTMLDivElement | null>(null);
  const hook = useRef<SpikeHook | null>(null);
  const editor = useRef<HTMLTextAreaElement | null>(null);
  const [draftText, setDraftText] = useState("");

  const load = useCallback(async () => {
    const t0 = performance.now();
    const text = await api.getPatch();
    const t1 = performance.now();
    const parsed = parsePatch(text);
    const t2 = performance.now();
    setPatch(parsed);
    if (hook.current) {
      hook.current.patchBytes = text.length;
      hook.current.fetchMs = t1 - t0;
      hook.current.parseMs = t2 - t1;
    }
  }, [api]);

  useEffect(() => {
    hook.current = {
      api: api.name,
      visibleMs: null,
      committedMs: null,
      patchBytes: 0,
      parseMs: null,
      fetchMs: null,
      eventsSeen: 0,
      scrollBench: (frames, px) => {
        if (!scroller.current) throw new Error("no diff yet");
        return scrollBench(scroller.current, frames, px);
      },
      workBench: (steps, px) => {
        if (!scroller.current) throw new Error("no diff yet");
        return workBench(scroller.current, steps, px);
      },
      reopen: async () => {
        setPatch(null);
        await new Promise(requestAnimationFrame);
        const t = performance.now();
        await load();
        await new Promise(requestAnimationFrame);
        await new Promise(requestAnimationFrame);
        return Math.round(performance.now() - t);
      },
    };
    window.__spike = hook.current;
    load().catch((e: unknown) => setError(String(e)));
  }, [api, load]);

  // First paint with diff rows: after two frames, the rows have been through layout and paint.
  useEffect(() => {
    if (!patch || !hook.current || hook.current.visibleMs !== null) return;
    hook.current.committedMs = Math.round(performance.now());
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        if (hook.current) hook.current.visibleMs = Math.round(performance.now());
      }),
    );
  }, [patch]);

  // Live list: one fetch at a time, and one more if events came during it.
  useEffect(() => {
    let busy = false;
    let again = false;
    const refresh = async () => {
      if (busy) {
        again = true;
        return;
      }
      busy = true;
      try {
        const list = await api.listTasks();
        dispatch({ t: "merge", rows: list.tasks.map(toRow) });
      } catch (e) {
        setLink(`error: ${String(e)}`);
      }
      busy = false;
      if (again) {
        again = false;
        void refresh();
      }
    };
    const stop = api.subscribe((m) => {
      if (m.type === "connected") {
        setLink("live");
        void refresh();
      } else if (m.type === "disconnected") setLink(`reconnecting: ${m.reason}`);
      else {
        if (hook.current) hook.current.eventsSeen += 1;
        void refresh();
      }
    });
    return stop;
  }, [api]);

  const fileIndex = useCallback(
    (line: number) => (patch ? patch.files.findIndex((f) => line >= f.start && line < f.end) : -1),
    [patch],
  );
  const currentFile = patch?.files[fileIndex(cursor)];

  const act = useCallback(
    (a: Action) => {
      if (!patch) return;
      const n = patch.lines.length;
      const sorted = [...comments].sort((x, y) => x.line - y.line);
      switch (a) {
        case "lineDown":
          if (focus === "list") dispatch({ t: "move", delta: 1 });
          else setCursor((c) => Math.min(n - 1, c + 1));
          break;
        case "lineUp":
          if (focus === "list") dispatch({ t: "move", delta: -1 });
          else setCursor((c) => Math.max(0, c - 1));
          break;
        case "fileNext": {
          const next = patch.files[fileIndex(cursor) + 1];
          if (next) setCursor(next.start);
          break;
        }
        case "filePrev": {
          const prev = patch.files[Math.max(0, fileIndex(cursor) - 1)];
          if (prev) setCursor(prev.start);
          break;
        }
        case "commentNext":
        case "commentPrev": {
          const list = a === "commentNext" ? sorted : [...sorted].reverse();
          const hit =
            list.find((c) => (a === "commentNext" ? c.line > cursor : c.line < cursor)) ?? list[0];
          if (hit) {
            setCursor(hit.line);
            setSelected(hit.id);
          }
          break;
        }
        case "comment": {
          const id = `c${Date.now()}`;
          setComments((cs) => [...cs, { id, line: cursor, text: "", dropped: false, by: "You" }]);
          setSelected(id);
          setDraftText("");
          setEditing(id);
          break;
        }
        case "edit": {
          const c = comments.find((x) => x.id === selected);
          if (c) {
            setDraftText(c.text);
            setEditing(c.id);
          } else setToast("Move to a comment first.");
          break;
        }
        case "toggleKeep":
          setComments((cs) =>
            cs.map((c) => (c.id === selected ? { ...c, dropped: !c.dropped } : c)),
          );
          break;
        case "viewed":
          if (currentFile)
            setViewed((v) => {
              const next = new Set(v);
              if (next.has(currentFile.path)) next.delete(currentFile.path);
              else next.add(currentFile.path);
              return next;
            });
          break;
        case "toggleView":
          setView((v) => (v === "unified" ? "split" : "unified"));
          break;
        case "focusList":
          setFocus((f) => (f === "list" ? "diff" : "list"));
          break;
        case "listApplyNew":
          dispatch({ t: "apply" });
          break;
        case "notify":
          api
            .notify("Loom spike", "Fake notification")
            .then(setToast, (e: unknown) => setToast(`Notification failed: ${String(e)}`));
          break;
        case "openSource":
          setToast("The spike has no open at source.");
          break;
        case "upload":
          if (editing) {
            const id = editing;
            const text = draftText;
            setComments((cs) =>
              text.trim()
                ? cs.map((c) => (c.id === id ? { ...c, text } : c))
                : cs.filter((c) => c.id !== id),
            );
            setEditing(null);
          } else
            setToast(
              `Upload ${comments.filter((c) => !c.dropped).length} comments: not in the spike.`,
            );
          break;
        case "back":
          if (editing) {
            const id = editing;
            setComments((cs) => cs.filter((c) => c.id !== id || c.text));
            setEditing(null);
          } else if (focus === "list") setFocus("diff");
          else setToast("");
          break;
        default: {
          const exhaustive: never = a;
          throw new Error(`unknown action ${String(exhaustive)}`);
        }
      }
    },
    [patch, comments, cursor, focus, selected, editing, draftText, api, fileIndex, currentFile],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = resolveKey(e, editing !== null);
      if (!a) return;
      // preventDefault also stops the system beep of a key nothing handled in WKWebView.
      e.preventDefault();
      act(a);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [act, editing]);

  useEffect(() => {
    if (editing) editor.current?.focus();
  }, [editing]);

  const kept = useMemo(() => comments.filter((c) => !c.dropped).length, [comments]);

  return (
    <div className="app">
      <aside className={`sidebar${focus === "list" ? " focused" : ""}`} data-testid="live-list">
        <div className="titlebar-space" data-tauri-drag-region />
        <h2>
          Tasks <small data-testid="link">{link}</small>
        </h2>
        {/* The line always takes its place, so its arrival moves no row. */}
        <div className="new-line" data-testid="new-line">
          {live.pending.length > 0 ? `${live.pending.length} new, press y` : ""}
        </div>
        <ul>
          {live.rows.map((r) => (
            <li
              key={r.id}
              data-id={r.id}
              className={`${r.id === live.cursorId ? "selected" : ""}${r.removed ? " removed" : ""}`}
            >
              <span className="status">{r.status}</span> {r.title}
            </li>
          ))}
        </ul>
      </aside>
      <main className="main">
        <header className="toolbar" data-tauri-drag-region>
          <strong>{MR.title}</strong> <span className="muted">{MR.key}</span>
          <span className="spacer" />
          <span className="muted" data-testid="file">
            {currentFile?.path ?? ""}{" "}
            {currentFile && viewed.has(currentFile.path) ? "✓ viewed" : ""}
          </span>
          <span className="muted" data-testid="view">
            {view === "unified" ? "Unified" : "Side by side"}
          </span>
        </header>
        {error ? <div className="error">{error}</div> : null}
        {patch ? (
          <DiffPane
            patch={patch}
            view={view}
            cursor={cursor}
            comments={comments}
            focused={focus === "diff"}
            scrollerRef={(el) => {
              scroller.current = el;
            }}
          />
        ) : (
          <div className="diff" />
        )}
        <footer className="legend">
          j k line · n p file · m M comment · c comment · e edit · x keep · f viewed · v view · h
          list · y new rows · t notify
          {toast ? <span data-testid="toast"> · {toast}</span> : null}
        </footer>
      </main>
      <aside className="side">
        <section className="overview" data-testid="mr-body">
          <h2>Description</h2>
          <BodyRenderer source={MR.body} />
        </section>
        <section className="comments" data-testid="comments">
          <h2>
            Draft review <small data-testid="kept">{kept} will upload</small>
          </h2>
          {[...comments]
            .sort((a, b) => a.line - b.line)
            .map((c) => (
              <div
                key={c.id}
                className={`comment${c.id === selected ? " selected" : ""}${c.dropped ? " dropped" : ""}`}
                data-comment={c.id}
              >
                <small>
                  {c.by} · line {c.line + 1}
                </small>
                {editing === c.id ? (
                  <textarea
                    ref={editor}
                    aria-label="Comment"
                    value={draftText}
                    onChange={(e) => setDraftText(e.target.value)}
                  />
                ) : (
                  <p>{c.text}</p>
                )}
              </div>
            ))}
        </section>
      </aside>
    </div>
  );
}
