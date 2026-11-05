import { defineConfig } from "vite";

export default defineConfig({
  test: { include: ["e2e/contract.spec.ts"], testTimeout: 30_000, environment: "node" },
} as never);
