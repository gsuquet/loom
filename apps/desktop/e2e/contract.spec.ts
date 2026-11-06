// The same promises, checked on the fake and on a real loomd, so that the fake cannot drift.
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { createFakeApi } from "../src/api/fake-api.ts";
import type { ChangeMessage, LoomApi } from "../src/api/loom-api.ts";
import { type Sandbox, startSandbox } from "./sandbox.ts";
import { socketApi } from "./socket-api.ts";

const newTask = (title: string) =>
  ({ title, kind: "chore", context: "demo", status: "inbox" }) as never;

function suite(name: string, make: () => LoomApi) {
  describe(name, () => {
    it("lists a created task, with the fields the screen reads", async () => {
      const api = make();
      const created = await api.createTask(newTask("Fake contract task"));
      const { tasks } = await api.listTasks();
      const row = tasks.find((t) => t.id === (created as { id: string }).id);
      expect(row?.task?.title).toBe("Fake contract task");
      expect(row?.task?.status).toBe("inbox");
      expect(typeof row?.prefix).toBe("string");
    });

    it("sends a task.changed event for a new task", async () => {
      const api = make();
      const seen: ChangeMessage[] = [];
      const stop = api.subscribe((m) => seen.push(m));
      await new Promise((r) => setTimeout(r, 200));
      const created = (await api.createTask(newTask("Fake event task"))) as { id: string };
      await expect
        .poll(
          () =>
            seen.some(
              (m) =>
                m.type === "event" &&
                m.event === "task.changed" &&
                JSON.parse(m.data).id === created.id,
            ),
          { timeout: 5000 },
        )
        .toBe(true);
      stop();
    });
  });
}

suite("fake API", () => createFakeApi({ patchBytes: 1024 }));

describe("real loomd", () => {
  let sandbox: Sandbox;
  beforeAll(async () => {
    sandbox = await startSandbox();
  });
  afterAll(async () => {
    await sandbox.stop();
  });
  suite("over the socket", () => socketApi(sandbox.socket));
});
