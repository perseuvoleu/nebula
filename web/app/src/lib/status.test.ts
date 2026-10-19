import assert from "node:assert/strict";
import { test } from "node:test";
import { STATUS_LABEL, statusLabel } from "./status.ts";
import { AGENT_STATUSES } from "./types.ts";

test("every daemon status, queued included, has its own label", () => {
  assert.deepEqual(Object.keys(STATUS_LABEL).sort(), [...AGENT_STATUSES].sort());
  const labels = AGENT_STATUSES.map(statusLabel);
  assert.equal(new Set(labels).size, AGENT_STATUSES.length);
  for (const status of AGENT_STATUSES) assert.notEqual(statusLabel(status), status);
  assert.equal(statusLabel("queued"), "Așteaptă memorie");
});
