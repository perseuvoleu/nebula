import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";

function worker() {
  const handlers: Record<string, (event: any) => void> = {};
  const shown: any[] = [];
  const opened: string[] = [];
  runInNewContext(readFileSync(new URL("../../public/sw.js", import.meta.url), "utf8"), {
    URL,
    self: {
      location: { origin: "https://example.com" },
      addEventListener: (name: string, handler: (event: any) => void) => { handlers[name] = handler; },
      registration: { showNotification: async (...args: any[]) => { shown.push(args); } },
      navigator: { clearAppBadge: async () => { throw new Error("Badging unavailable"); } },
      clients: { matchAll: async () => [], openWindow: async (url: string) => { opened.push(url); } },
    },
  });
  return { handlers, shown, opened };
}

test("notification clicks stay in the app even when badging fails", async () => {
  for (const url of ["//evil.example/x", "https://evil.example/x", "/p/project/s/one"]) {
    const { handlers, opened } = worker();
    let pending: Promise<unknown> | undefined;
    handlers.notificationclick({ notification: { close() {}, data: { url } }, waitUntil: (p: Promise<unknown>) => { pending = p; } });
    await pending;
    assert.deepEqual(opened, [url === "/p/project/s/one" ? url : "/"]);
  }
});
