import { describe, expect, it } from "vitest";
import { generatePatch } from "./generate-patch";
import { buildSplit, KIND, parsePatch } from "./patch";

const SMALL = `diff --git a/x.rs b/x.rs
index 1..2 100644
--- a/x.rs
+++ b/x.rs
@@ -10,3 +10,3 @@ fn f()
 keep
-old
+new
 end
`;

describe("parsePatch", () => {
  it("classifies lines and numbers them", () => {
    const p = parsePatch(SMALL);
    expect([...p.kinds].map((k) => KIND[k])).toEqual([
      "file",
      "meta",
      "meta",
      "meta",
      "hunk",
      "ctx",
      "del",
      "add",
      "ctx",
    ]);
    expect(p.oldNo[6]).toBe(11);
    expect(p.newNo[7]).toBe(11);
    expect(p.files).toEqual([{ path: "x.rs", start: 0, end: 9 }]);
  });

  it("treats --- and +++ in a hunk as content, not headers", () => {
    const p = parsePatch("diff --git a/a b/a\n@@ -1 +1 @@\n--- x\n+++ y\n");
    expect(KIND[p.kinds[2] as number]).toBe("del");
    expect(KIND[p.kinds[3] as number]).toBe("add");
  });

  it("parses 5 MiB quickly enough for the 1 s budget", () => {
    const text = generatePatch(5 * 1024 * 1024);
    expect(text.length).toBeGreaterThanOrEqual(5 * 1024 * 1024);
    const t = performance.now();
    const p = parsePatch(text);
    expect(performance.now() - t).toBeLessThan(300);
    expect(p.files.length).toBeGreaterThan(50);
  });
});

describe("buildSplit", () => {
  it("pairs a removed run with the added run after it", () => {
    const p = parsePatch(SMALL);
    const s = buildSplit(p);
    const r = s.rowOf[6] as number;
    expect(s.left[r]).toBe(6);
    expect(s.right[r]).toBe(7);
    expect(s.rowOf[7]).toBe(r);
    expect(s.left.length).toBe(8);
  });

  it("leaves a side empty when the runs differ", () => {
    const p = parsePatch("diff --git a/a b/a\n@@ -1 +1,2 @@\n-a\n+b\n+c\n");
    const s = buildSplit(p);
    expect([...s.left]).toEqual([0, 1, 2, -1]);
    expect([...s.right]).toEqual([0, 1, 3, 4]);
  });
});
