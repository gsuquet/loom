// A loomd with fake data only: fake HOME, fake Loom home, empty environment, update checks off.
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { generatePatch } from "../src/diff/generate-patch.ts";

export interface Sandbox {
  root: string;
  home: string;
  socket: string;
  stop: () => Promise<void>;
}

export async function startSandbox(): Promise<Sandbox> {
  // Under /tmp: a unix socket path is limited to about 104 bytes on macOS.
  const root = `/tmp/lsp-${process.pid}-${Date.now() % 100000}`;
  const home = join(root, "h");
  mkdirSync(join(home, "spike-fixtures"), { recursive: true });
  mkdirSync(join(root, "fake-home"));
  writeFileSync(
    join(home, "config.toml"),
    '[update]\npolicy = "off"\n\n[repos]\nroots = []\n\n[contexts.demo]\n',
  );
  writeFileSync(join(home, "machine.toml"), 'id = "spike"\n');
  writeFileSync(join(home, "spike-fixtures/mr.patch"), generatePatch(5 * 1024 * 1024));
  const bin = process.env.LOOMD_BIN ?? resolve(import.meta.dirname, "../../../target/debug/loomd");
  const child: ChildProcess = spawn(bin, ["--color", "never"], {
    env: { PATH: "/usr/bin:/bin", HOME: join(root, "fake-home"), LOOM_HOME: home },
    stdio: ["ignore", "ignore", process.env.E2E_VERBOSE ? "inherit" : "ignore"],
  });
  const socket = join(home, "run/loomd.sock");
  for (let i = 0; i < 300 && !existsSync(socket); i += 1)
    await new Promise((r) => setTimeout(r, 100));
  if (!existsSync(socket)) {
    child.kill();
    throw new Error("loomd did not create its socket");
  }
  return {
    root,
    home,
    socket,
    stop: async () => {
      const gone = new Promise((r) => child.once("exit", r));
      child.kill("SIGTERM");
      await Promise.race([gone, new Promise((r) => setTimeout(r, 5000))]);
      rmSync(root, { recursive: true, force: true });
    },
  };
}
