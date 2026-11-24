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
    Request: class { url: string; constructor(url: string) { this.url = url; } },
    fetch: async () => new Response("shell"),
    caches: { open: async () => ({ put() {}, match: async () => undefined }) },
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

test("artifact navigations reach the gateway instead of the app shell", () => {
  const { handlers } = worker();
  const answered = (path: string) => {
    let responded = false;
    handlers.fetch({ request: { method: "GET", mode: "navigate", url: "https://example.com" + path }, respondWith: () => { responded = true; } });
    return responded;
  };
  assert.equal(answered("/a/1764003600.mac/01JG0000000000000000000001/index.html"), false);
  assert.equal(answered("/board"), true);
});
