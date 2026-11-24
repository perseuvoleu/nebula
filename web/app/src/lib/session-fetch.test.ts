import test from "node:test";
import assert from "node:assert/strict";
import { fetchSessionList } from "./session-fetch.ts";
import type { SessionRow } from "./types.ts";

const row = (project: string): SessionRow => ({ id: "same", name: "Same name", project, status: "running", kind: "pi", path: `/srv/${project}`, worktree: "main", archived: false });

test("All requests only accessible scopes, preserving mixed-project identity and excluding archives", async () => {
  const requested: string[] = [];
  const result = await fetchSessionList(["alpha", "beta"], {
    async sessions(project) {
      requested.push(project);
      return [row(project), { ...row(project), id: "archived", archived: true }];
    },
  }, new AbortController().signal);
  assert.deepEqual({ requested, identities: result.rows.map(r => `${r.project}/${r.id}`), errors: result.errors },
    { requested: ["alpha", "beta"], identities: ["alpha/same", "beta/same"], errors: [] });
});

test("partial failure explicitly identifies unavailable projects without stale or duplicate rows", async () => {
  const result = await fetchSessionList(["alpha", "beta", "alpha"], {
    async sessions(project) {
      if (project === "beta") throw new Error("unavailable");
      return [row(project), row(project)];
    },
  }, new AbortController().signal);
  assert.deepEqual({ rows: result.rows.map(r => `${r.project}/${r.id}`), loaded: result.loaded,
    failed: result.errors.map(e => e.project) }, { rows: ["alpha/same"], loaded: ["alpha"], failed: ["beta"] });
});
