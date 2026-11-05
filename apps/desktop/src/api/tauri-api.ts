import { Channel, invoke } from "@tauri-apps/api/core";
import type { NewTask, TaskDetail, TaskList } from "../generated/api";
import type { ChangeMessage, LoomApi } from "./loom-api";

const call = <T>(method: string, path: string, body?: unknown) =>
  invoke<T>("loom_api", { method, path, body: body ?? null });

export const tauriApi: LoomApi = {
  name: "tauri",
  listTasks: () => call<TaskList>("GET", "/v1/tasks"),
  createTask: (task: NewTask) => call<TaskDetail>("POST", "/v1/tasks", task),
  subscribe(on) {
    const channel = new Channel<ChangeMessage>();
    let stopped = false;
    channel.onmessage = (m) => {
      if (!stopped) on(m);
    };
    invoke("loom_events", { channel }).catch((e: unknown) =>
      on({ type: "disconnected", reason: String(e) }),
    );
    return () => {
      stopped = true;
    };
  },
  async getPatch() {
    // A raw IPC response: an ArrayBuffer, no JSON encoding of 5 MiB.
    const bytes = await invoke<ArrayBuffer>("spike_patch");
    return new TextDecoder().decode(bytes);
  },
  notify: (title, body) => invoke<string>("spike_notify", { title, body }),
};
