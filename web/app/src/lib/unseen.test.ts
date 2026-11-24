import test from "node:test";
import assert from "node:assert/strict";
import { reconcileUnseen, type UnseenState } from "./unseen.ts";

const running = [{ id: "one", status: "running" }];

test("finished sessions stay blue until opened, including across reload state", () => {
  let state: UnseenState = { statuses: {}, unseen: [] };
  state = reconcileUnseen(state, "nebula", running, null);
  state = reconcileUnseen(state, "nebula", [{ id: "one", status: "finished" }], null);
  assert.deepEqual(state.unseen, ["nebula/one"]);

  state = reconcileUnseen(state, "nebula", [{ id: "one", status: "finished" }], "one");
  assert.deepEqual(state.unseen, []);
});

test("All list polls preserve read state per project, and a later observed completion becomes unread again", () => {
  let state: UnseenState = { statuses: {}, unseen: [] };
  for (const project of ["alpha", "beta"]) state = reconcileUnseen(state, project, running, null);
  for (const project of ["alpha", "beta"]) state = reconcileUnseen(state, project, [{ id: "one", status: "finished" }], null);
  state = reconcileUnseen(JSON.parse(JSON.stringify(state)), "alpha", [{ id: "one", status: "finished" }], "one");
  state = reconcileUnseen(state, "beta", [{ id: "one", status: "finished" }], null);
  assert.deepEqual(state.unseen, ["beta/one"]);
  state = reconcileUnseen(state, "alpha", running, null);
  state = reconcileUnseen(state, "alpha", [{ id: "one", status: "finished" }], null);
  assert.deepEqual(state.unseen, ["beta/one", "alpha/one"]);
});
