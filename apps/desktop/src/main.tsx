import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createFakeApi, type FakeApi } from "./api/fake-api";
import type { LoomApi } from "./api/loom-api";
import { tauriApi } from "./api/tauri-api";
import { App } from "./review/App";
import "./review/styles.css";

declare global {
  interface Window {
    __TAURI_INTERNALS__?: unknown;
    __spikeFake?: FakeApi;
    __cspViolations?: string[];
  }
}

// First thing in the page: every CSP violation of the session, for the real-app test.
window.__cspViolations = [];
document.addEventListener("securitypolicyviolation", (e) => {
  window.__cspViolations?.push(`${e.effectiveDirective} ${e.blockedURI}`);
});

function pickApi(): LoomApi {
  if (window.__TAURI_INTERNALS__) return tauriApi;
  const fake = createFakeApi();
  window.__spikeFake = fake;
  return fake;
}

const root = document.getElementById("root");
if (!root) throw new Error("no root element");
document.documentElement.classList.toggle(
  "glass",
  navigator.platform.startsWith("Mac") && Boolean(window.__TAURI_INTERNALS__),
);
createRoot(root).render(
  <StrictMode>
    <App api={pickApi()} />
  </StrictMode>,
);
