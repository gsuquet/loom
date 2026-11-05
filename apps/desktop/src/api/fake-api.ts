import { generatePatch } from "../diff/generate-patch";
import type { NewTask, TaskDetail, TaskList, TaskSummary } from "../generated/api";
import type { ChangeMessage, LoomApi } from "./loom-api";

export interface FakeApi extends LoomApi {
  /** Test hook: what a task created elsewhere looks like. */
  emitTask(title: string): void;
}

let counter = 0;
const nextId = () => {
  counter += 1;
  return `fake${String(counter).padStart(4, "0")}`;
};

export function createFakeApi(opts?: { patchBytes?: number }): FakeApi {
  const tasks: TaskSummary[] = [];
  const listeners = new Set<(m: ChangeMessage) => void>();
  const add = (title: string, context: string, kind: string, status: string): TaskSummary => {
    const id = nextId();
    const summary = {
      id,
      prefix: id.slice(0, 5),
      context,
      task: { id, title, kind, status, importance: "p2", context },
    } as unknown as TaskSummary;
    tasks.push(summary);
    for (const on of listeners)
      on({
        type: "event",
        id: `1-${counter}`,
        event: "task.changed",
        data: JSON.stringify({ type: "task.changed", id, context }),
      });
    return summary;
  };
  add("Fake task one", "demo", "chore", "inbox");
  add("Fake task two", "demo", "bug", "ready");
  return {
    name: "fake",
    listTasks: async () => ({ tasks: [...tasks] }) as TaskList,
    createTask: async (t: NewTask) => {
      const s = add(t.title, t.context, t.kind, t.status);
      return { ...s } as unknown as TaskDetail;
    },
    subscribe(on) {
      listeners.add(on);
      on({ type: "connected" });
      return () => listeners.delete(on);
    },
    getPatch: async () => generatePatch(opts?.patchBytes ?? 5 * 1024 * 1024),
    notify: async () => "fake: no notification",
    emitTask(title) {
      add(title, "demo", "chore", "inbox");
    },
  };
}
