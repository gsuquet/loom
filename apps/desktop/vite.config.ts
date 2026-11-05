import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// VITE_SPIKE_UNSAFE selects the renderer with its protections removed, for the negative test
// of criterion 6. It is a compile-time constant: the normal build drops that code.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  build: {
    target: "safari26",
    outDir: process.env.VITE_SPIKE_UNSAFE === "1" ? "dist-unsafe" : "dist",
  },
  server: { port: 5173, strictPort: true },
  test: { include: ["src/**/*.test.{ts,tsx}"], environment: "node" },
} as never);
