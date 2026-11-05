import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "e2e",
  testMatch: "web.spec.ts",
  timeout: 60_000,
  reporter: [["list"], ["json", { outputFile: ".e2e/playwright.json" }]],
  projects: [{ name: "webkit", use: { ...devices["Desktop Safari"] } }],
});
