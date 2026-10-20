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
