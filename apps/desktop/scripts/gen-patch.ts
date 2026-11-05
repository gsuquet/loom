// Writes the fake patch fixture: `node scripts/gen-patch.ts <out> [bytes]`. Never committed.
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { generatePatch } from "../src/diff/generate-patch.ts";

const [out, bytes] = process.argv.slice(2);
if (!out) throw new Error("usage: gen-patch <out> [bytes]");
mkdirSync(dirname(out), { recursive: true });
writeFileSync(out, generatePatch(Number(bytes ?? 5 * 1024 * 1024)));
