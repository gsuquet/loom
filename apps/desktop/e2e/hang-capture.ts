// What a hung app looks like, taken before it is killed: enough to tell apart a lost script, a
// stuck page, a stuck main thread and a starved or crashed WebKit process.
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, writeFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";

const WATCHED = /loom-desktop|WebKit|WebContent|Networking|\.GPU/;

function run(cmd: string, args: string[], timeoutMs = 20_000): string {
  const r = spawnSync(cmd, args, {
    timeout: timeoutMs,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  return `${r.stdout ?? ""}${r.stderr ?? ""}${r.error ? `\n[${String(r.error)}]` : ""}`;
}

/** The app and WebKit lines of `ps`. Used on every launch too, to compare with a healthy one. */
export function watchedProcesses(): string[] {
  return run("/bin/ps", ["-axo", "pid,ppid,pcpu,pmem,rss,state,etime,comm"])
    .split("\n")
    .filter((l) => WATCHED.test(l));
}

interface Probe {
  name: string;
  ms: number;
  status?: number;
  value?: unknown;
  error?: string;
}

async function probe(
  base: string,
  name: string,
  method: string,
  path: string,
  body?: unknown,
  timeoutMs = 6000,
): Promise<Probe> {
  const t = Date.now();
  try {
    const res = await fetch(`${base}${path}`, {
      method,
      signal: AbortSignal.timeout(timeoutMs),
      ...(body === undefined
        ? {}
        : { body: JSON.stringify(body), headers: { "content-type": "application/json" } }),
    });
    const json = (await res.json()) as { value: unknown };
    const value =
      typeof json.value === "string" && json.value.length > 300
        ? `(${json.value.length} chars)`
        : json.value;
    return { name, ms: Date.now() - t, status: res.status, value };
  } catch (e) {
    return { name, ms: Date.now() - t, error: String(e) };
  }
}

const PAGE =
  "return { ready: document.readyState, visibility: document.visibilityState, focus: document.hasFocus(), url: location.href, spike: !!window.__spike, committedMs: window.__spike ? window.__spike.committedMs : null, rows: document.querySelectorAll('.row').length }";

/** Short-timeout questions, in order from least to most intrusive. */
async function probes(base: string, session: string | null): Promise<Probe[]> {
  const out: Probe[] = [await probe(base, "server /status", "GET", "/status")];
  if (session) {
    const s = `/session/${session}`;
    out.push(
      await probe(base, "same session: script timeout 3 s", "POST", `${s}/timeouts`, {
        script: 3000,
      }),
    );
    out.push(
      await probe(base, "same session: return 1", "POST", `${s}/execute/sync`, {
        script: "return 1",
        args: [],
      }),
    );
    out.push(
      await probe(base, "same session: page state", "POST", `${s}/execute/sync`, {
        script: PAGE,
        args: [],
      }),
    );
    out.push(await probe(base, "same session: url", "GET", `${s}/url`));
    out.push(await probe(base, "same session: window rect", "GET", `${s}/window/rect`));
  }
  const fresh = await probe(
    base,
    "fresh session",
    "POST",
    "/session",
    { capabilities: { alwaysMatch: {} } },
    8000,
  );
  out.push(fresh);
  const id = (fresh.value as { sessionId?: string } | undefined)?.sessionId;
  if (id) {
    const s = `/session/${id}`;
    out.push(
      await probe(base, "fresh session: script timeout 3 s", "POST", `${s}/timeouts`, {
        script: 3000,
      }),
    );
    out.push(
      await probe(base, "fresh session: return 1", "POST", `${s}/execute/sync`, {
        script: "return 1",
        args: [],
      }),
    );
    out.push(
      await probe(base, "fresh session: page state", "POST", `${s}/execute/sync`, {
        script: PAGE,
        args: [],
      }),
    );
    out.push(await probe(base, "fresh session: delete", "DELETE", s));
  }
  return out;
}

const answered = (list: Probe[], name: string) => list.find((p) => p.name === name)?.status === 200;

/** Writes `.e2e/diag/<label>/` and returns the short summary kept in the report. */
export async function captureHang(
  label: string,
  base: string,
  session: string | null,
  appPid: number | undefined,
) {
  const dir = join(".e2e/diag", label);
  mkdirSync(dir, { recursive: true });
  const write = (name: string, text: string) => writeFileSync(join(dir, name), text);
  const started = Date.now();

  const first = await probes(base, session);
  write("probes-1.json", JSON.stringify(first, null, 1));
  const ps = run("/bin/ps", ["-axo", "pid,ppid,pcpu,pmem,rss,state,etime,comm"]);
  write("ps.txt", ps);
  write("uptime.txt", run("/usr/bin/uptime", []));
  const watched = watchedProcesses();
  const windowTool = ".e2e/windows-tool";
  const windows = existsSync(windowTool) && appPid ? run(windowTool, [String(appPid)]) : "no tool";
  write("windows.json", windows);

  // On a CI runner only: elsewhere these would read the WebKit processes of unrelated apps.
  if (process.platform === "darwin" && process.env.CI) {
    write(
      "top.txt",
      run("/usr/bin/top", [
        "-l",
        "2",
        "-n",
        "15",
        "-o",
        "cpu",
        "-stats",
        "pid,command,cpu,mem,state,threads",
      ]),
    );
    // `sample` needs only the same user. WebKit services are children of launchd, not of the app.
    const pids = watched
      .map((l) => l.trim().split(/\s+/)[0] as string)
      .filter(Boolean)
      .slice(0, 8);
    await Promise.all(
      pids.map(
        (pid) =>
          new Promise<void>((done) => {
            const line = watched.find((l) => l.trim().startsWith(`${pid} `)) ?? pid;
            const name = /WebContent/.test(line)
              ? "webcontent"
              : /Networking/.test(line)
                ? "networking"
                : /GPU/.test(line)
                  ? "gpu"
                  : "app";
            write(
              `sample-${name}-${pid}.txt`,
              run("/usr/bin/sample", [pid, "3", "-mayDie"], 30_000),
            );
            done();
          }),
      ),
    );
    write(
      "system-log.txt",
      run(
        "/usr/bin/log",
        [
          "show",
          "--last",
          "3m",
          "--style",
          "compact",
          "--predicate",
          'process == "loom-desktop" OR process CONTAINS "WebContent" OR process CONTAINS "com.apple.WebKit"',
        ],
        60_000,
      )
        .split("\n")
        .slice(-1500)
        .join("\n"),
    );
    const reports = join(homedir(), "Library/Logs/DiagnosticReports");
    write("crash-reports.txt", existsSync(reports) ? readdirSync(reports).join("\n") : "none");
  }

  // The same questions after the samples: did it recover by itself, about 10 s later?
  const second = await probes(base, session);
  write("probes-2.json", JSON.stringify(second, null, 1));
  return {
    dir,
    captureMs: Date.now() - started,
    serverAnswers: answered(first, "server /status"),
    sameSessionReturn1: answered(first, "same session: return 1"),
    sameSessionPage: first.find((p) => p.name === "same session: page state")?.value ?? null,
    freshSessionReturn1: answered(first, "fresh session: return 1"),
    freshSessionPage: first.find((p) => p.name === "fresh session: page state")?.value ?? null,
    laterSameSessionReturn1: answered(second, "same session: return 1"),
    laterFreshSessionReturn1: answered(second, "fresh session: return 1"),
    laterFreshSessionPage:
      second.find((p) => p.name === "fresh session: page state")?.value ?? null,
    watched,
  };
}
