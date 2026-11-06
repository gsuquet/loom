/** Deterministic unified diff of about `bytes` bytes, with fake content. */
export function generatePatch(bytes: number): string {
  const out: string[] = [];
  let size = 0;
  let file = 0;
  const push = (line: string) => {
    out.push(line);
    size += line.length + 1;
  };
  while (size < bytes) {
    const name = `src/module_${String(file).padStart(3, "0")}/handler_${file % 17}.rs`;
    push(`diff --git a/${name} b/${name}`);
    push(
      `index ${(file * 7919).toString(16).padStart(7, "0")}..${(file * 104729).toString(16).padStart(7, "0")} 100644`,
    );
    push(`--- a/${name}`);
    push(`+++ b/${name}`);
    let oldLine = 1;
    for (let hunk = 0; hunk < 12; hunk += 1) {
      push(`@@ -${oldLine},24 +${oldLine},30 @@ fn fake_function_${hunk}()`);
      for (let i = 0; i < 30; i += 1) {
        const body = `let value_${i} = compute_fake_${(file * 31 + hunk * 7 + i) % 97}(input, ${i}); // fake`;
        if (i % 5 === 1) push(`+    ${body} added`);
        else if (i % 5 === 3) {
          push(`-    ${body} removed`);
          push(`+    ${body} replaced`);
        } else push(`     ${body}`);
      }
      oldLine += 60;
    }
    // One very long line in the third file: layout cost of a minified line.
    if (file === 2) push(`+${"x".repeat(64 * 1024)}`);
    file += 1;
  }
  return `${out.join("\n")}\n`;
}
