import { spawnSync } from "node:child_process";
import { startSandbox } from "../e2e/sandbox.ts";
const s = await startSandbox();
const r = spawnSync("npx", ["wdio", "run", "wdio.conf.mjs"], { stdio: "inherit", env: { ...process.env, LOOM_SPIKE_HOME: s.home } });
await s.stop();
console.log("wdio exit", r.status);
process.exit(r.status ?? 1);
