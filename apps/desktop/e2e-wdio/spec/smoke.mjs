import { browser, $, expect } from "@wdio/globals";
describe("wdio tauri service", () => {
  it("drives the real app", async () => {
    await browser.waitUntil(() => browser.execute(() => document.querySelectorAll(".row").length > 0));
    await browser.keys("j");
    const c = await browser.execute(() => document.querySelector("[data-testid=diff]").dataset.cursor);
    console.log("cursor", c);
    if (c !== "1") throw new Error("cursor " + c);
  });
});
