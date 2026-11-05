// Layer 2: WebKit on the fake API. The built frontend, with the CSP of tauri.conf.json as a header.
// The hostile test runs on four builds, so that each protection is shown to matter.
import { existsSync, readFileSync } from "node:fs";
import { extname, join } from "node:path";
import { expect, type Page, test } from "@playwright/test";

const conf = JSON.parse(
  readFileSync(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
);
const CSP: string = conf.app.security.csp;
const TYPES: Record<string, string> = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
};

async function open(page: Page, dist: "dist" | "dist-unsafe", csp: boolean) {
  const hits: string[] = [];
  const root = new URL(`../${dist}/`, import.meta.url).pathname;
  await page.route("**/*", async (route) => {
    const url = new URL(route.request().url());
    if (url.hostname !== "app.test") {
      hits.push(url.href);
      return route.fulfill({
        status: 200,
        body: "ok",
        headers: { "access-control-allow-origin": "*" },
      });
    }
    const file = join(root, url.pathname === "/" ? "index.html" : url.pathname);
    if (!existsSync(file)) return route.fulfill({ status: 404, body: "" });
    return route.fulfill({
      body: readFileSync(file),
      headers: {
        "content-type": TYPES[extname(file)] ?? "application/octet-stream",
        ...(csp ? { "content-security-policy": CSP } : {}),
      },
    });
  });
  await page.goto("http://app.test/");
  await page.waitForFunction(() => window.__spike?.visibleMs != null);
  return hits;
}

const key = (page: Page, k: string) => page.keyboard.press(k);
const cursor = (page: Page) => page.getByTestId("diff").getAttribute("data-cursor");

test("5 MiB patch is visible, and text only", async ({ page }) => {
  await open(page, "dist", true);
  const { ms, bytes } = await page.evaluate(() => ({
    ms: window.__spike?.visibleMs,
    bytes: window.__spike?.patchBytes,
  }));
  console.log(`webkit-fake visibleMs=${ms} bytes=${bytes}`);
  expect(bytes).toBeGreaterThanOrEqual(5 * 1024 * 1024);
  expect(await page.locator(".row").count()).toBeLessThan(200);
});

test("unified and side by side, with the comments column", async ({ page }) => {
  await open(page, "dist", true);
  await expect(page.getByTestId("comments")).toContainText("3 will upload");
  await expect(page.getByTestId("view")).toHaveText("Unified");
  await key(page, "v");
  await expect(page.getByTestId("view")).toHaveText("Side by side");
  expect(await page.locator(".row.split").first().locator(".cell").count()).toBe(2);
  await key(page, "v");
  await expect(page.getByTestId("view")).toHaveText("Unified");
});

test("keyboard only: lines, files, comments, edit, keep", async ({ page }) => {
  await open(page, "dist", true);
  await key(page, "j");
  await key(page, "j");
  expect(await cursor(page)).toBe("2");
  await key(page, "k");
  expect(await cursor(page)).toBe("1");
  await key(page, "n");
  expect(Number(await cursor(page))).toBeGreaterThan(100);
  await key(page, "p");
  expect(await cursor(page)).toBe("0");
  await key(page, "m");
  expect(await cursor(page)).toBe("8");
  await key(page, "x");
  await expect(page.getByTestId("comments")).toContainText("2 will upload");
  await key(page, "x");
  await key(page, "c");
  await page.keyboard.type("jk fake");
  expect(await cursor(page)).toBe("8"); // Letters in the field move nothing.
  await key(page, "Meta+Enter");
  await expect(page.getByTestId("comments")).toContainText("jk fake");
  await expect(page.getByTestId("comments")).toContainText("4 will upload");
});

test("a task event moves neither the cursor nor the list selection", async ({ page }) => {
  await open(page, "dist", true);
  await key(page, "j");
  const list = page.getByTestId("live-list");
  await expect(list.locator("li")).toHaveCount(2);
  const before = await list
    .locator("li.selected")
    .evaluate((e) => ({ id: e.getAttribute("data-id"), top: e.getBoundingClientRect().top }));
  await page.evaluate(() => window.__spikeFake?.emitTask("Fake arrival"));
  await expect(page.getByTestId("new-line")).toHaveText("1 new, press y");
  const after = await list
    .locator("li.selected")
    .evaluate((e) => ({ id: e.getAttribute("data-id"), top: e.getBoundingClientRect().top }));
  expect(after).toEqual(before);
  expect(await cursor(page)).toBe("1");
  await key(page, "y");
  await expect(list.locator("li")).toHaveCount(3);
});

const VARIANTS = [
  { name: "renderer and CSP on", dist: "dist", csp: true, attackRuns: false },
  { name: "CSP only", dist: "dist-unsafe", csp: true, attackRuns: false },
  { name: "renderer only", dist: "dist", csp: false, attackRuns: false },
  { name: "both removed", dist: "dist-unsafe", csp: false, attackRuns: true },
] as const;

for (const v of VARIANTS) {
  test(`hostile MR body, ${v.name}: attack ${v.attackRuns ? "runs" : "is stopped"}`, async ({
    page,
  }) => {
    const hits = await open(page, v.dist, v.csp);
    await page.waitForTimeout(500);
    const pwned = await page.evaluate(
      () => (window as unknown as { __pwned?: number }).__pwned ?? 0,
    );
    console.log(`${v.name}: pwned=${pwned} hits=${JSON.stringify(hits)}`);
    if (v.attackRuns) {
      expect(pwned).toBeGreaterThan(0);
      expect(hits.length).toBeGreaterThan(0);
    } else {
      expect(pwned).toBe(0);
      expect(hits).toEqual([]);
    }
  });
}

test("the normal bundle has no unsafe renderer", () => {
  const dir = new URL("../dist/assets/", import.meta.url).pathname;
  const js = readFileSync(join(dir, readdirJs(dir)), "utf8");
  expect(js).not.toContain("SPIKE_UNSAFE_RENDERER");
});

function readdirJs(dir: string): string {
  const { readdirSync } = process.getBuiltinModule("node:fs");
  const name = readdirSync(dir).find((f: string) => f.endsWith(".js"));
  if (!name) throw new Error("no bundle");
  return name;
}
