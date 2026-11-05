import { describe, expect, it } from "vitest";
import { applyPending, EMPTY_LIVE, mergeLive, moveCursor } from "./live-list";

const r = (id: string, title = id) => ({ id, title, status: "inbox" });

describe("live list", () => {
  it("does not move rows or the cursor when a row arrives", () => {
    let s = mergeLive(EMPTY_LIVE, [r("b"), r("c")]);
    s = moveCursor(s, 1);
    const before = s.rows.map((x) => x.id);
    // "a" sorts before everything in the ID order of the API.
    s = mergeLive(s, [r("a"), r("b"), r("c")]);
    expect(s.rows.map((x) => x.id)).toEqual(before);
    expect(s.cursorId).toBe("c");
    expect(s.pending.map((x) => x.id)).toEqual(["a"]);
  });
  it("updates a row in place", () => {
    let s = mergeLive(EMPTY_LIVE, [r("a"), r("b")]);
    s = mergeLive(s, [r("a", "new title"), r("b")]);
    expect(s.rows[0]?.title).toBe("new title");
  });
  it("keeps a removed row until new rows are applied", () => {
    let s = mergeLive(EMPTY_LIVE, [r("a"), r("b")]);
    s = mergeLive(s, [r("a")]);
    expect(s.rows[1]?.removed).toBe(true);
    expect(applyPending(s).rows.map((x) => x.id)).toEqual(["a"]);
  });
  it("applies new rows at the end, keeping the cursor", () => {
    let s = mergeLive(EMPTY_LIVE, [r("b")]);
    s = mergeLive(s, [r("a"), r("b")]);
    s = applyPending(s);
    expect(s.rows.map((x) => x.id)).toEqual(["b", "a"]);
    expect(s.cursorId).toBe("b");
  });
});
