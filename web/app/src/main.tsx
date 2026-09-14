import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "./index.css";
import App from "./App.tsx";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

// Public app shell only; the worker never caches /api/ traffic or replays POSTs.
if ("serviceWorker" in navigator && !import.meta.env.DEV) {
  window.addEventListener("load", () => { navigator.serviceWorker.register("/sw.js").catch(() => undefined); });
}
const badgeNavigator = navigator as Navigator & { clearAppBadge?: () => Promise<void> };
if (badgeNavigator.clearAppBadge) void badgeNavigator.clearAppBadge();
