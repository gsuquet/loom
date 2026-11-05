// The API over the unix socket, from Node. It is the third LoomApi, used by the contract test
// and to act on loomd from the real-app test.
import { request } from "node:http";
import type { ChangeMessage, LoomApi } from "../src/api/loom-api.ts";
import type { NewTask, TaskDetail, TaskList } from "../src/generated/api.ts";

export function socketCall<T>(
  socketPath: string,
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const data = body === undefined ? undefined : JSON.stringify(body);
    const req = request(
      {
        socketPath,
        method,
        path,
        headers: { host: "loomd", ...(data ? { "content-type": "application/json" } : {}) },
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on("data", (c: Buffer) => chunks.push(c));
        res.on("end", () => {
          const text = Buffer.concat(chunks).toString();
          if ((res.statusCode ?? 500) < 300) resolve(JSON.parse(text) as T);
          else reject(new Error(`HTTP ${res.statusCode}: ${text}`));
        });
      },
    );
    req.on("error", reject);
    if (data) req.write(data);
    req.end();
  });
}

export function socketApi(socketPath: string): LoomApi {
  return {
    name: "socket",
    listTasks: () => socketCall<TaskList>(socketPath, "GET", "/v1/tasks"),
    createTask: (t: NewTask) => socketCall<TaskDetail>(socketPath, "POST", "/v1/tasks", t),
    subscribe(on: (m: ChangeMessage) => void) {
      const req = request(
        { socketPath, method: "GET", path: "/v1/events", headers: { host: "loomd" } },
        (res) => {
          on({ type: "connected" });
          let buffer = "";
          res.on("data", (c: Buffer) => {
            buffer += c.toString();
            for (;;) {
              const end = buffer.indexOf("\n\n");
              if (end < 0) break;
              const block = buffer.slice(0, end);
              buffer = buffer.slice(end + 2);
              const field = (n: string) =>
                block
                  .split("\n")
                  .find((l) => l.startsWith(`${n}: `))
                  ?.slice(n.length + 2);
              const data = field("data");
              if (data)
                on({ type: "event", id: field("id") ?? null, event: field("event") ?? "", data });
            }
          });
        },
      );
      req.on("error", () => {});
      req.end();
      return () => req.destroy();
    },
    getPatch: () => Promise.reject(new Error("loomd has no diff route yet")),
    notify: () => Promise.reject(new Error("no notification from Node")),
  };
}
