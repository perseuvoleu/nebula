import test from "node:test";
import assert from "node:assert/strict";
import { drop, patchOf, propose, reconcile, settle } from "./optimistic.ts";

test("a settled write keeps its patch through a read that began before it, and a later read releases it", () => {
  const pending = settle(propose([], "a", "#13", { stage: "review" }), "a", 1000);
  assert.deepEqual(patchOf(reconcile(pending, 999), "#13"), { stage: "review" });
  assert.deepEqual(reconcile(pending, 1001), []);
});

test("a settled write that the read still holds survives it, and the first read that no longer holds it releases it", () => {
  const pending = settle(propose([], "a", "s1@100", {}), "a", 1000);
  assert.equal(reconcile(pending, 2000, (key) => key === "s1@100").length, 1);
  assert.deepEqual(reconcile(pending, 3000, (key) => key === "s1@200"), []);
});

test("an unsettled write survives every read", () => {
  assert.equal(reconcile(propose([], "a", "#13", { stage: "review" }), 5000).length, 1);
});

test("a later write to the same card wins, and a failure drops only its own write", () => {
  const both = propose(propose([], "a", "#13", { stage: "review" }), "b", "#13", { stage: "done" });
  assert.deepEqual(patchOf(both, "#13"), { stage: "done" });
  assert.deepEqual(patchOf(drop(both, "a"), "#13"), { stage: "done" });
  assert.deepEqual(patchOf(drop(both, "b"), "#13"), { stage: "review" });
  assert.equal(patchOf(both, "#14"), undefined);
});

test("settling one write leaves a newer write to the same card pending", () => {
  const both = settle(propose(propose([], "a", "#13", { stage: "review" }), "b", "#13", { stage: "done" }), "a", 1000);
  assert.deepEqual(reconcile(both, 2000).map((p) => p.token), ["b"]);
});
