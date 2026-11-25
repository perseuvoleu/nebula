import test from "node:test";
import assert from "node:assert/strict";
import { orderSessions } from "./session-list.ts";
import type { SessionRow } from "./types.ts";

const row = (id: string, status: string, stamp?: number, project = "alpha"): SessionRow => ({
  id, status, status_changed_at: stamp, project, name: "Same name", kind: "pi", worktree: "main", path: `/srv/${project}`, archived: false,
});

test("attention states precede unread completions, then remaining sessions, newest status activity first", () => {
  const rows = [row("idle", "finished", 900), row("blue", "finished", 100), row("working", "running", 200),
    row("blocked", "needs_feedback", 300), row("fresh", "fresh", 800), row("old-blue", "finished", 50)];
  assert.deepEqual(orderSessions(rows, new Set(["alpha/blue", "alpha/old-blue"])).map(r => r.id),
    ["blocked", "working", "blue", "old-blue", "idle", "fresh"]);
});

test("unknown timestamps sort last with deterministic project/id ties, independent of input order", () => {
  const rows = [row("z", "running", undefined), row("same", "running", 20, "beta"),
    row("same", "running", 20), row("invalid", "running", NaN), row("negative", "running", -1)];
  const expected = ["alpha/same", "beta/same", "alpha/invalid", "alpha/negative", "alpha/z"];
  for (const input of [rows, [...rows].reverse()]) {
    assert.deepEqual(orderSessions(input, new Set()).map(r => `${r.project}/${r.id}`), expected);
  }
});
