// Separate launches of the real-app test, without stopping at a failure. One JSON line per launch.
//
//   node e2e/loop.ts <app binary> --runs 20 --prefix raw
//   node e2e/loop.ts <app binary> --runs 20 --prefix policy --restart-once
//
// --restart-once: a hang before the first page check answered gets one restart, counted. A hang
// after it is a real failure and is never retried. A restart is a new app and a new sandbox loomd.
import { spawn } from "node:child_process";
import {
  appendFileSync,
  existsSync,
  mkdirSync,
  openSync,
  readFileSync,
  writeFileSync,
} from "node:fs";

const args = process.argv.slice(2);
const appBin = args[0];
if (!appBin) throw new Error("usage: loop <app binary> --runs n --prefix name [--restart-once]");
const option = (name: string, fallback: string) =>
  args.includes(name) ? (args[args.indexOf(name) + 1] as string) : fallback;
const runs = Number(option("--runs", "20"));
const prefix = option("--prefix", "loop");
const restartOnce = args.includes("--restart-once");
const launchLimitMs = Number(option("--launch-limit-s", "240")) * 1000;
const driverArgs = args.includes("--driver-args")
  ? option("--driver-args", "").split(" ").filter(Boolean)
  : ["--notify", "--no-budget"];

interface Launch {
  label: string;
  outcome: string;
  phase: string | null;
  driverReadyMs: number | null;
  firstPollMs: number | null;
  durationMs: number;
  exit: number | null;
  failed: string[];
  failure: string | null;
  leftovers: number | null;
  hang: unknown;
}

function launch(label: string): Promise<Launch> {
  mkdirSync(".e2e", { recursive: true });
  const log = openSync(`.e2e/${label}.log`, "w");
  const t = Date.now();
  // Its own process group: the outer limit must also stop the app of a stuck driver.
  const child = spawn(
    process.execPath,
    ["e2e/real-app.ts", appBin as string, ...driverArgs, "--label", label],
    {
      stdio: ["ignore", log, log],
      detached: true,
    },
  );
  return new Promise((resolve) => {
    let overLimit = false;
    const guard = setTimeout(() => {
      overLimit = true;
      try {
        process.kill(-(child.pid as number), "SIGKILL");
      } catch {}
    }, launchLimitMs);
    child.once("exit", (code) => {
      clearTimeout(guard);
      const path = `.e2e/${label}.json`;
      const r = existsSync(path) && !overLimit ? JSON.parse(readFileSync(path, "utf8")) : null;
      resolve({
        label,
        outcome: overLimit ? "over-limit" : (r?.outcome ?? "driver-crash"),
        phase: r?.phase ?? null,
        driverReadyMs: r?.numbers?.driverReadyMs ?? null,
        firstPollMs: r?.numbers?.firstPollMs ?? null,
        durationMs: Date.now() - t,
        exit: code,
        failed: r?.failed ?? [],
        failure: r?.failure ?? null,
        leftovers: r?.leftoversAtStart ?? null,
        hang: r?.hang ?? null,
      });
    });
  });
}

/** Wilson score interval, 95%. */
function wilson(k: number, n: number) {
  if (n === 0) return null;
  const z = 1.96;
  const p = k / n;
  const d = 1 + (z * z) / n;
  const c = (p + (z * z) / (2 * n)) / d;
  const h = (z * Math.sqrt((p * (1 - p)) / n + (z * z) / (4 * n * n))) / d;
  return { k, n, low: Math.max(0, c - h), high: Math.min(1, c + h) };
}

const all: Launch[] = [];
const logical: { run: number; result: string; restarts: number; labels: string[] }[] = [];
const record = (l: Launch) => {
  all.push(l);
  appendFileSync(`.e2e/${prefix}.jsonl`, `${JSON.stringify(l)}\n`);
  console.log(
    `${l.label} ${l.outcome} phase=${l.phase} driverReadyMs=${l.driverReadyMs} firstPollMs=${l.firstPollMs} durationMs=${l.durationMs} leftovers=${l.leftovers}${l.failed.length ? ` failed=${JSON.stringify(l.failed)}` : ""}`,
  );
};

for (let n = 1; n <= runs; n += 1) {
  const id = String(n).padStart(2, "0");
  const first = await launch(`${prefix}-${id}`);
  record(first);
  const labels = [first.label];
  let result = first.outcome;
  let restarts = 0;
  if (restartOnce && first.outcome === "startup-hang") {
    restarts = 1;
    const second = await launch(`${prefix}-${id}-restart`);
    record(second);
    labels.push(second.label);
    result = second.outcome;
  }
  logical.push({ run: n, result, restarts, labels });
}

const count = (list: { outcome?: string; result?: string }[], value: string) =>
  list.filter((x) => (x.outcome ?? x.result) === value).length;
const hangs = all.filter((l) => /hang|over-limit/.test(l.outcome));
const summary = {
  prefix,
  restartOnce,
  launches: all.length,
  outcomes: Object.fromEntries(
    [...new Set(all.map((l) => l.outcome))].map((o) => [o, count(all, o)]),
  ),
  startupHangs: wilson(count(all, "startup-hang"), all.length),
  anyHang: wilson(hangs.length, all.length),
  hangs: hangs.map((l) => ({
    label: l.label,
    outcome: l.outcome,
    phase: l.phase,
    durationMs: l.durationMs,
    driverReadyMs: l.driverReadyMs,
    hang: l.hang,
  })),
  logicalRuns: logical.length,
  logicalPass: count(logical, "pass"),
  logicalPassInterval: wilson(count(logical, "pass"), logical.length),
  restarts: logical.reduce((a, r) => a + r.restarts, 0),
  logical,
};
writeFileSync(`.e2e/${prefix}.summary.json`, JSON.stringify(summary, null, 1));
console.log(
  JSON.stringify(
    {
      ...summary,
      logical: undefined,
      hangs: summary.hangs.map((h) => ({ ...h, hang: undefined })),
    },
    null,
    1,
  ),
);
process.exit(summary.logicalPass === logical.length ? 0 : 1);
