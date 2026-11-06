// Layer 4: the real app in the real webview, with no human. It speaks plain W3C WebDriver to the
// embedded server of tauri-plugin-wdio-webdriver, with `fetch` and no other dependency.
//
//   node e2e/real-app.ts <app binary> [--expect-attack] [--notify] [--label name]
//
// Exit code 0 when every check holds; with --expect-attack, when the attack did run. 3: a hang
// before the first page check answered. 4: a hang after it. 1: any other failure.
import { type ChildProcess, execFileSync, spawn } from "node:child_process";
import { mkdirSync, openSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { resolve } from "node:path";
import { captureHang, watchedProcesses } from "./hang-capture.ts";
import { startSandbox } from "./sandbox.ts";
import { socketCall } from "./socket-api.ts";

const args = process.argv.slice(2);
const appBin = args[0];
if (!appBin) throw new Error("usage: real-app <app binary>");
const expectAttack = args.includes("--expect-attack");
const notify = args.includes("--notify");
// A CI runner is a shared VM: the time budgets there are figures, not pass or fail.
const budget = !args.includes("--no-budget");
// Test of the driver only: stop after the first check, with no real page to check after it.
const firstCheckOnly = args.includes("--first-check-only");
const label = args.includes("--label") ? (args[args.indexOf("--label") + 1] as string) : "run";
const port = Number(process.env.TAURI_WEBDRIVER_PORT ?? 4445);
const base = `http://127.0.0.1:${port}`;

interface Check {
  name: string;
  ok: boolean;
  security: boolean;
  detail: string;
}
const checks: Check[] = [];
const numbers: Record<string, unknown> = {};
class FirstCheckDone extends Error {}
let lostFirstAnswers = 0;
let firstCheck: unknown = null;
// Which part of the test is running, for the record of a hang.
let phase = "sandbox";
// The first page check of a launch may lose its answer (macOS CI: 16 of 47 launches, with the page
// fine and a new script answered at once). So only this check is sent again on `script timeout`, a
// lost answer is counted, and a page that never answers within the limit is a startup hang. After
// the first answer nothing is ever sent again: any timeout is a real failure.
const FIRST_POLL_SCRIPT_MS = 3_000;
const FIRST_CHECK_LIMIT_MS = 30_000;
const LATER_SCRIPT_MS = 45_000;
let scriptMs = FIRST_POLL_SCRIPT_MS;
let firstPollAnswered = false;
let sessionId: string | null = null;
function check(name: string, ok: boolean, detail: unknown = "", security = false) {
  checks.push({
    name,
    ok,
    security,
    detail: typeof detail === "string" ? detail : JSON.stringify(detail),
  });
  console.log(
    `${ok ? "ok  " : "FAIL"} ${name} ${typeof detail === "string" ? detail : JSON.stringify(detail)}`,
  );
}

async function wd<T>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${base}${path}`, {
    method,
    signal: AbortSignal.timeout(scriptMs + 15_000),
    ...(body === undefined
      ? {}
      : { body: JSON.stringify(body), headers: { "content-type": "application/json" } }),
  });
  const json = (await res.json()) as { value: T };
  if (!res.ok) throw new Error(`${method} ${path}: ${JSON.stringify(json)}`);
  return json.value;
}

function appRssMb(pid: number | undefined): number | null {
  if (!pid) return null;
  const kb = Number(
    execFileSync("ps", ["-o", "rss=", "-p", String(pid)])
      .toString()
      .trim(),
  );
  return Math.round(kb / 1024);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const sandbox = await startSandbox();
const hits: string[] = [];
const sentinel = createServer((req, res) => {
  hits.push(req.url ?? "");
  res.setHeader("access-control-allow-origin", "*");
  res.end("ok");
});
await new Promise<void>((r) => sentinel.listen(47123, "127.0.0.1", r));

const newTask = (title: string) => ({ title, kind: "chore", context: "demo", status: "inbox" });
for (const n of [1, 2, 3])
  await socketCall(sandbox.socket, "POST", "/v1/tasks", newTask(`Fake task ${n}`));

mkdirSync(".e2e", { recursive: true });
const leftovers = watchedProcesses();
const appLog = openSync(`.e2e/${label}.app.log`, "w");
phase = "launch";
const t0 = Date.now();
const app: ChildProcess = spawn(resolve(appBin), [], {
  env: { ...process.env, LOOM_SPIKE_HOME: sandbox.home, TAURI_WEBDRIVER_PORT: String(port) },
  stdio: ["ignore", appLog, appLog],
});
let appExited: number | null = null;
app.once("exit", (c) => {
  appExited = c ?? -1;
});

let failure: unknown = null;
try {
  phase = "driver-wait";
  for (let i = 0; ; i += 1) {
    if (appExited !== null) throw new Error(`the app exited with ${appExited}`);
    if (i > 120) throw new Error("hang: the WebDriver server did not start in 60 s");
    try {
      await wd("GET", "/status");
      break;
    } catch {
      await sleep(500);
    }
  }
  numbers.driverReadyMs = Date.now() - t0;
  phase = "session";
  const session = await wd<{ sessionId: string }>("POST", "/session", {
    capabilities: { alwaysMatch: {} },
  });
  sessionId = session.sessionId;
  numbers.sessionMs = Date.now() - t0;
  const s = `/session/${session.sessionId}`;
  const js = <T>(script: string, ...a: unknown[]) =>
    wd<T>("POST", `${s}/execute/sync`, { script, args: a });
  const jsAsync = <T>(script: string, ...a: unknown[]) =>
    wd<T>("POST", `${s}/execute/async`, { script, args: a });
  await wd("POST", `${s}/timeouts`, { script: FIRST_POLL_SCRIPT_MS });
  phase = "first-poll";
  // Absolute times: the page's clock (performance.timeOrigin) can be compared with them.
  const sends: { sentAt: number; timedOutAt?: number }[] = [];
  const firstCheckStart = Date.now();
  let firstValue: unknown;
  for (;;) {
    const send: { sentAt: number; timedOutAt?: number } = { sentAt: Date.now() };
    sends.push(send);
    try {
      firstValue = await js("return window.__spike && window.__spike.committedMs !== null");
      break;
    } catch (e) {
      if (!/script timeout/i.test(String(e))) throw e;
      send.timedOutAt = Date.now();
      lostFirstAnswers += 1;
      firstCheck = { sendsSinceSpawnMs: sends.map((x) => x.sentAt - t0) };
      if (Date.now() - firstCheckStart >= FIRST_CHECK_LIMIT_MS)
        throw new Error(
          `hang: the first page check had no answer in ${FIRST_CHECK_LIMIT_MS} ms: ${e}`,
        );
    }
  }
  firstPollAnswered = true;
  phase = "after-first-check";
  numbers.firstPollMs = Date.now() - t0;
  scriptMs = LATER_SCRIPT_MS;
  await wd("POST", `${s}/timeouts`, { script: LATER_SCRIPT_MS });
  numbers.healthyProcesses = watchedProcesses();
  if (firstCheckOnly) throw new FirstCheckDone();
  // The likely cause, unproven: a script that arrives during navigation loses its answer. These
  // times place each first-check send against the navigation of the page that finally answered.
  const nav = await js<Record<string, number | string | null>>(
    "const n = performance.getEntriesByType('navigation')[0]; return { origin: performance.timeOrigin, committedMs: window.__spike ? window.__spike.committedMs : null, visibleMs: window.__spike ? window.__spike.visibleMs : null, type: n ? n.type : null, fetchStart: n ? n.fetchStart : null, responseEnd: n ? n.responseEnd : null, domInteractive: n ? n.domInteractive : null, domContentLoaded: n ? n.domContentLoadedEventEnd : null, loadEnd: n ? n.loadEventEnd : null }",
  );
  const origin = nav.origin as number;
  firstCheck = {
    firstValue,
    limitMs: FIRST_CHECK_LIMIT_MS,
    perScriptMs: FIRST_POLL_SCRIPT_MS,
    originSinceSpawnMs: Math.round(origin - t0),
    sessionSinceOriginMs: Math.round(t0 + (numbers.sessionMs as number) - origin),
    // Negative: sent before the page that answered began to navigate.
    sends: sends.map((x) => ({
      sentSinceOriginMs: Math.round(x.sentAt - origin),
      sentBeforeCommittedMs:
        nav.committedMs === null
          ? null
          : Math.round((nav.committedMs as number) - (x.sentAt - origin)),
      lost: x.timedOutAt !== undefined,
      timedOutSinceOriginMs: x.timedOutAt === undefined ? null : Math.round(x.timedOutAt - origin),
    })),
    nav,
  };
  const until = async (what: string, script: string, ms = 20000) => {
    const end = Date.now() + ms;
    for (;;) {
      if (await js<boolean>(script)) return true;
      if (Date.now() > end) {
        check(`wait: ${what}`, false, "timed out");
        return false;
      }
      await sleep(50);
    }
  };
  const key = async (value: string) => {
    await wd("POST", `${s}/actions`, {
      actions: [
        {
          type: "key",
          id: "kb",
          actions: [
            { type: "keyDown", value },
            { type: "keyUp", value },
          ],
        },
      ],
    });
    await sleep(40);
  };
  const chord = async (mod: string, value: string) => {
    await wd("POST", `${s}/actions`, {
      actions: [
        {
          type: "key",
          id: "kb",
          actions: [
            { type: "keyDown", value: mod },
            { type: "keyDown", value },
            { type: "keyUp", value },
            { type: "keyUp", value: mod },
          ],
        },
      ],
    });
    await sleep(40);
  };
  const cursor = () =>
    js<string>("return document.querySelector('[data-testid=diff]').dataset.cursor");

  // Criterion 1: visible within 1 s, and the bridge really delivered the patch.
  // WebKit fires no requestAnimationFrame in a hidden page (locked screen, occluded window): those
  // figures are then "could not measure", and the rest of the test still runs.
  await until("diff committed", "return window.__spike && window.__spike.committedMs !== null");
  phase = "open-checks";
  await sleep(300);
  const open = await js<Record<string, number | string | null>>(
    "const h = window.__spike; return { api: h.api, visibleMs: h.visibleMs, committedMs: h.committedMs, visibility: document.visibilityState, fetchMs: h.fetchMs, parseMs: h.parseMs, bytes: h.patchBytes, origin: performance.timeOrigin, rows: document.querySelectorAll('.row').length, text: document.querySelector('.row')?.textContent }",
  );
  const rendering = open.visibility === "visible";
  numbers.rendering = rendering;
  numbers.open = {
    ...open,
    committedSinceSpawnMs: Math.round((open.origin as number) + (open.committedMs as number) - t0),
    paintedSinceSpawnMs:
      open.visibleMs === null
        ? null
        : Math.round((open.origin as number) + (open.visibleMs as number) - t0),
  };
  check(
    "patch came through the Tauri bridge",
    open.api === "tauri" && (open.bytes as number) >= 5 * 1024 * 1024,
    open,
  );
  check(
    "rows in the DOM within 1 s of navigation",
    !budget || (open.committedMs as number) < 1000,
    `${open.committedMs} ms`,
  );
  if (rendering) {
    await until("first paint", "return window.__spike.visibleMs !== null");
    const painted = await js<number>("return window.__spike.visibleMs");
    check("painted within 1 s of navigation", !budget || painted < 1000, `${painted} ms`);
    const reopen = await jsAsync<number>(
      "const done = arguments[arguments.length - 1]; window.__spike.reopen().then(done);",
    );
    numbers.reopenMs = reopen;
    check("reopening the review within 1 s", !budget || reopen < 1000, `${reopen} ms`);
  } else {
    console.log(`SKIP paint and frame figures: page is ${open.visibility}`);
  }
  await until("diff back", "return document.querySelectorAll('.row').length > 0");

  phase = "list";
  // Criterion 5, first half: the socket and the event stream.
  await until(
    "live link",
    "return document.querySelector('[data-testid=link]').textContent === 'live'",
  );
  await until(
    "tasks from loomd",
    "return document.querySelectorAll('[data-testid=live-list] li').length === 3",
  );
  const firstTitles = await js<string[]>(
    "return [...document.querySelectorAll('[data-testid=live-list] li')].map(e => e.textContent)",
  );
  check(
    "the list shows the tasks of the real loomd",
    firstTitles.length === 3 && firstTitles.every((t) => t.includes("Fake task")),
    firstTitles,
  );

  phase = "keys";
  // Criterion 3 and 4, through WebDriver key actions.
  await key("j");
  await key("j");
  check("j moves the cursor", (await cursor()) === "2", await cursor());
  await key("k");
  check("k moves it back", (await cursor()) === "1", await cursor());
  await key("n");
  const fileStart = Number(await cursor());
  check("n jumps to the next file", fileStart > 100, fileStart);
  await key("p");
  check("p jumps back", (await cursor()) === "0");
  await key("m");
  check("m jumps to the first comment", (await cursor()) === "8", await cursor());
  await key("x");
  check(
    "x drops the comment",
    (await js<string>("return document.querySelector('[data-testid=kept]').textContent")) ===
      "2 will upload",
  );
  await key("x");
  await key("v");
  const split = await js<{ view: string; cells: number }>(
    "return { view: document.querySelector('[data-testid=diff]').dataset.view, cells: document.querySelector('.row.split')?.querySelectorAll('.cell').length }",
  );
  check("v shows side by side", split.view === "split" && split.cells === 2, split);
  if (rendering) {
    numbers.benchSplit = await jsAsync(
      "const done = arguments[arguments.length - 1]; window.__spike.scrollBench(240, 90).then(done);",
    );
  }
  await key("v");
  check(
    "v shows unified again",
    (await js<string>("return document.querySelector('[data-testid=diff]').dataset.view")) ===
      "unified",
  );
  await key("c");
  await until(
    "comment field",
    "return document.activeElement && document.activeElement.tagName === 'TEXTAREA'",
    3000,
  );
  await wd("POST", `${s}/actions`, {
    actions: [
      {
        type: "key",
        id: "kb",
        actions: [..."jk"].flatMap((value) => [
          { type: "keyDown", value },
          { type: "keyUp", value },
        ]),
      },
    ],
  });
  await sleep(100);
  check("letters in the field move nothing", (await cursor()) === "8", await cursor());
  await chord("", ""); // Meta+Enter
  await until(
    "comment saved",
    "return document.querySelector('[data-testid=kept]').textContent === '4 will upload'",
    3000,
  );
  check("c then Cmd+Enter keeps a draft comment", true);
  await key(""); // Esc
  await js("window.__testBefore = null; return 0;");

  phase = "live";
  // Criterion 5: a real event, created elsewhere, moves nothing.
  await key("h");
  await key("j");
  const snapshot = `const sel = document.querySelector('[data-testid=live-list] li.selected');
    const d = document.querySelector('[data-testid=diff]');
    return { selected: sel && sel.dataset.id, selectedTop: sel && sel.getBoundingClientRect().top, cursor: d.dataset.cursor, scrollTop: d.scrollTop, rows: [...document.querySelectorAll('[data-testid=live-list] li')].map(e => e.dataset.id), seen: window.__spike.eventsSeen, active: document.activeElement && document.activeElement.tagName, selection: String(getSelection()) };`;
  const before = await js<Record<string, unknown>>(snapshot);
  const created = await socketCall<{ id: string }>(
    sandbox.socket,
    "POST",
    "/v1/tasks",
    newTask("Fake arrival over SSE"),
  );
  const sseOk = await until(
    "the event reached the page",
    `return window.__spike.eventsSeen > ${before.seen} && document.querySelector('[data-testid=new-line]').textContent.startsWith('1 new')`,
    10000,
  );
  const after = await js<Record<string, unknown>>(snapshot);
  check("/v1/events reached the page over the Channel", sseOk, {
    created: created.id,
    seen: after.seen,
  });
  const same =
    ["selected", "selectedTop", "cursor", "scrollTop", "active", "selection"].every(
      (k) => after[k] === before[k],
    ) && JSON.stringify(after.rows) === JSON.stringify(before.rows);
  check("the event moved no cursor, selection or row", same, { before, after });
  await key("y");
  await until(
    "the new row is shown",
    "return document.querySelectorAll('[data-testid=live-list] li').length === 4",
    3000,
  );
  await key("");

  phase = "hostile";
  // Criterion 6.
  await sleep(500);
  const hostile = await js<Record<string, unknown>>(
    `const md = document.querySelector('[data-testid=mr-body] .md');
     return { pwned: window.__pwned ?? 0, elements: md.querySelectorAll('script,img,iframe,style,object,embed').length, jsLinks: [...md.querySelectorAll('a')].filter(a => /^javascript:/i.test(a.getAttribute('href') ?? '')).length, remoteAsLink: md.querySelectorAll('a[data-remote-image]').length, unsafeRenderer: md.dataset.unsafeRenderer ?? null, globalTauri: typeof window.__TAURI__, violationsSoFar: window.__cspViolations };`,
  );
  numbers.hostile = hostile;
  check("no script of the MR body ran", hostile.pwned === 0, hostile, true);
  const hitsBefore = [...hits];
  check("no request left the page for the MR body", hitsBefore.length === 0, hitsBefore, true);
  const tasks = await socketCall<{ tasks: { task?: { title?: string } }[] }>(
    sandbox.socket,
    "GET",
    "/v1/tasks",
  );
  const pwnedTasks = tasks.tasks.filter((t) => t.task?.title?.includes("PWNED")).length;
  check(
    "the MR body did not reach loomd through invoke",
    pwnedTasks === 0,
    `${pwnedTasks} PWNED tasks in the real loomd`,
    true,
  );
  check(
    "window.__TAURI__ is not a global",
    hostile.globalTauri === "undefined",
    String(hostile.globalTauri),
    true,
  );

  // The CSP of the built app, in the real webview: what a script that did get in could do.
  const probe = await jsAsync<Record<string, unknown>>(`
    const done = arguments[arguments.length - 1];
    const v = []; const on = (e) => v.push(e.effectiveDirective); document.addEventListener('securitypolicyviolation', on);
    const out = {};
    const s = document.createElement('script'); s.textContent = 'window.__cspInline = 1'; document.head.append(s);
    out.inlineScriptRan = window.__cspInline === 1;
    try { out.evalRan = eval('1 + 1') === 2; } catch (e) { out.evalRan = false; }
    const d = document.createElement('div'); d.innerHTML = '<img src="x" onerror="window.__cspHandler = 1">'; document.body.append(d);
    const st = document.createElement('div'); st.setAttribute('style', 'color: rgb(1, 2, 3)'); document.body.append(st);
    out.styleAttr = getComputedStyle(st).color === 'rgb(1, 2, 3)';
    const se = document.createElement('style'); se.textContent = '.csp-probe { color: rgb(4, 5, 6); }'; document.head.append(se);
    const sp = document.createElement('div'); sp.className = 'csp-probe'; document.body.append(sp);
    out.styleElement = getComputedStyle(sp).color === 'rgb(4, 5, 6)';
    const i = new Image(); const imgDone = new Promise(r => { i.onload = () => r('loaded'); i.onerror = () => r('blocked'); }); i.src = '${"http://127.0.0.1:47123"}/csp-probe.png';
    const f = fetch('http://127.0.0.1:47123/csp-probe-fetch').then(() => 'reached', () => 'blocked');
    const cssom = document.createElement('div'); cssom.style.color = 'rgb(7, 8, 9)'; document.body.append(cssom);
    out.cssomStyle = getComputedStyle(cssom).color === 'rgb(7, 8, 9)';
    out.appLayout = getComputedStyle(document.querySelector('.app')).display;
    Promise.all([imgDone, f]).then(async ([img, fetchResult]) => {
      out.image = img; out.fetch = fetchResult;
      await new Promise(r => setTimeout(r, 300));
      out.handlerRan = window.__cspHandler === 1;
      out.violations = v;
      out.metaCsp = document.querySelector('meta[http-equiv="Content-Security-Policy"]')?.content ?? null;
      out.allowedRoute = await window.__TAURI_INTERNALS__.invoke('loom_api', { method: 'GET', path: '/v1/status', body: null }).then(() => 'ok', e => JSON.stringify(e));
      out.refusedRoute = await window.__TAURI_INTERNALS__.invoke('loom_api', { method: 'POST', path: '/v1/repos/scan', body: null }).then(() => 'ok', e => JSON.stringify(e));
      out.unknownCommand = await window.__TAURI_INTERNALS__.invoke('plugin:notification|notify', { options: { title: 'x' } }).then(() => 'ok', e => String(e));
      done(out);
    });`);
  numbers.cspProbe = probe;
  check("CSP blocks an inline script in the built app", probe.inlineScriptRan === false, "", true);
  check("CSP blocks eval", probe.evalRan === false, "", true);
  check("CSP blocks an event handler attribute", probe.handlerRan === false, "", true);
  check(
    "CSP blocks an image and fetch to another origin",
    probe.image === "blocked" && probe.fetch === "blocked",
    {
      image: probe.image,
      fetch: probe.fetch,
      sentinel: hits.filter((h) => h.includes("csp-probe")),
    },
    true,
  );
  check(
    "CSP blocks a style attribute and a style element",
    probe.styleAttr === false && probe.styleElement === false,
    { attr: probe.styleAttr, element: probe.styleElement },
    true,
  );
  check(
    "CSSOM styles and the app stylesheet still work",
    probe.cssomStyle === true && probe.appLayout === "grid",
    { cssom: probe.cssomStyle, layout: probe.appLayout },
  );
  check(
    "the bridge still works under the CSP (ipc: allowed)",
    probe.allowedRoute === "ok",
    String(probe.allowedRoute),
  );
  check(
    "the allow-list refuses another /v1 route",
    String(probe.refusedRoute).includes("not_allowed"),
    String(probe.refusedRoute),
    true,
  );
  check(
    "the capability refuses a plugin command it does not grant",
    String(probe.unknownCommand).toLowerCase().includes("not allowed"),
    String(probe.unknownCommand),
    true,
  );

  phase = "bench";
  // Criterion 2: rAF inside the page. Numbers only: a shared machine or a CI runner cannot pass or fail it.
  numbers.workBench = await jsAsync(
    "const done = arguments[arguments.length - 1]; window.__spike.workBench(600, 90).then(done);",
  );
  numbers.workBenchJump = await jsAsync(
    "const done = arguments[arguments.length - 1]; window.__spike.workBench(600, 1200).then(done);",
  );
  if (rendering) {
    numbers.bench90 = await jsAsync(
      "const done = arguments[arguments.length - 1]; document.querySelector('[data-testid=diff]').scrollTop = 0; window.__spike.scrollBench(600, 90).then(done);",
    );
    numbers.bench1200 = await jsAsync(
      "const done = arguments[arguments.length - 1]; window.__spike.scrollBench(600, 1200).then(done);",
    );
  }
  numbers.viewport = await js(
    "return { w: innerWidth, h: innerHeight, dpr: devicePixelRatio, visibility: document.visibilityState, focused: document.hasFocus(), ua: navigator.userAgent, dark: matchMedia('(prefers-color-scheme: dark)').matches, }",
  );
  numbers.domRows = await js("return document.querySelectorAll('.row').length");

  phase = "notify";
  // Criterion 8: only on a CI runner, or when asked. It does not run on the owner's Mac by default.
  if (notify) {
    await key("t");
    await sleep(500);
    numbers.notify = await js(
      "return document.querySelector('[data-testid=toast]')?.textContent ?? null",
    );
  }

  phase = "end";
  numbers.appRssMb = appRssMb(app.pid);
  mkdirSync(".e2e", { recursive: true });
  const png = await wd<string>("GET", `${s}/screenshot`);
  writeFileSync(`.e2e/${label}.png`, Buffer.from(png, "base64"));
  await wd("DELETE", s).catch(() => {});
  phase = "done";
} catch (e) {
  if (e instanceof FirstCheckDone) phase = "done";
  else {
    failure = e;
    console.error(`ABORTED in phase ${phase}:`, e);
  }
}

// Any timeout, ours or the server's, is a hang. Other errors, such as the app exiting, are not.
const isHang = /script timeout|TimeoutError|aborted due to timeout|hang:/i.test(String(failure));
const startupHang = failure !== null && isHang && !firstPollAnswered;
const lateHang = failure !== null && isHang && firstPollAnswered;
// Before the kill: this is the only moment when the state of the hang can be read.
const hang =
  isHang && failure
    ? await captureHang(label, base, sessionId, app.pid).catch((e) => ({ captureError: String(e) }))
    : null;

app.kill("SIGTERM");
await sleep(300);
sentinel.close();
await sandbox.stop();
const failed = checks.filter((c) => !c.ok);
const securityFailed = failed.filter((c) => c.security);
const outcome = startupHang
  ? "startup-hang"
  : lateHang
    ? "late-hang"
    : failure
      ? "error"
      : failed.length > 0
        ? "fail"
        : "pass";
const report = {
  label,
  outcome,
  phase,
  firstPollAnswered,
  lostFirstAnswers,
  firstCheck,
  durationMs: Date.now() - t0,
  leftoversAtStart: leftovers.length,
  leftovers,
  hang,
  expectAttack,
  failure: failure ? String(failure) : null,
  checks: checks.length,
  failed: failed.map((c) => c.name),
  hits,
  numbers,
};
mkdirSync(".e2e", { recursive: true });
writeFileSync(`.e2e/${label}.json`, JSON.stringify(report, null, 1));
console.log(JSON.stringify(report.numbers, null, 1));
console.log(
  `${label}: ${outcome} in ${phase}, ${checks.length - failed.length}/${checks.length} checks passed`,
);
if (startupHang) process.exit(3);
if (lateHang) process.exit(4);
if (expectAttack) {
  console.log(
    `attack observed: ${securityFailed.length > 0} (${securityFailed.map((c) => c.name).join("; ")})`,
  );
  process.exit(securityFailed.length > 0 && !failure ? 0 : 1);
}
process.exit(failed.length === 0 && !failure ? 0 : 1);
