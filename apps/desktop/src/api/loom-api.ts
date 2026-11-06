import type { NewTask, TaskDetail, TaskList } from "../generated/api";

export type ChangeMessage =
  | { type: "connected" }
  | { type: "disconnected"; reason: string }
  | { type: "event"; id: string | null; event: string; data: string };

/** The one way the screen reaches loomd. It has a real and a fake implementation. */
export interface LoomApi {
  readonly name: "tauri" | "fake" | "socket";
  listTasks(): Promise<TaskList>;
  createTask(task: NewTask): Promise<TaskDetail>;
  /** Calls `on` for each change. Returns the function that stops the stream. */
  subscribe(on: (message: ChangeMessage) => void): () => void;
  /** The patch of the MR. loomd has no diff route yet, so this reads a fixture. */
  getPatch(): Promise<string>;
  /** Shows an OS notification; resolves with what the platform layer reports. */
  notify(title: string, body: string): Promise<string>;
}
