// Runs under `node --test` (Node ≥ 22.18 strips types). Pure reconciliation/merge/route logic only;
// browser flows live in web/e2e and run where a browser exists.
import assert from "node:assert/strict";
import { test } from "node:test";
import { mergeToolResults, reconcileAttempts, type Attempt } from "./conversation.ts";
import { parseSelection, selectionPath } from "./route.ts";
import type { Message } from "./types.ts";

const user = (id: string, text: string, timestamp = "2026-09-13T10:00:05.000Z"): Message =>
  ({ id, role: "user", timestamp, parts: [{ type: "text", text }] });
const attempt = (text: string, knownIds: string[], state: Attempt["state"] = "accepted"): Attempt =>
  ({ id: "att-" + text, text, createdAt: "2026-09-13T10:00:00.000Z", state, knownIds });

test("an accepted attempt is resolved by a new authoritative user message, once", () => {
  const messages = [user("u1", "same text", "2026-09-13T09:00:00.000Z"), user("u2", "same text")];
  const left = reconcileAttempts([attempt("same text", ["u1"])], messages);
  assert.equal(left.length, 0);
});

test("repeated identical instructions need one new message each; known history never claims an attempt", () => {
  const two = [attempt("go", ["u1"]), { ...attempt("go", ["u1"]), id: "att-2" }];
  assert.equal(reconcileAttempts(two, [user("u1", "go"), user("u2", "go")]).length, 1);
  assert.equal(reconcileAttempts(two, [user("u1", "go"), user("u2", "go"), user("u3", "go")]).length, 0);
  // Timestamps play no part (phone/server clocks may differ); identity does.
  assert.equal(reconcileAttempts([attempt("go", ["old"])], [user("old", "go", "2026-09-13T11:59:00.000Z")]).length, 1);
});

test("uncertain deliveries resolve when observed; sending and failed attempts are kept", () => {
  const messages = [user("u9", "x")];
  assert.equal(reconcileAttempts([attempt("x", [], "uncertain")], messages).length, 0);
  assert.equal(reconcileAttempts([attempt("x", [], "sending"), attempt("x", [], "failed")], messages).length, 2);
});

test("tool results update the matching call without duplicating the message", () => {
  const messages: Message[] = [{ id: "a1", role: "assistant", timestamp: null, parts: [
    { type: "tool", callId: "c1", name: "bash", args: "{}", argsTruncated: false, state: "pending", output: "", outputLength: 0, truncated: false },
  ] }];
  const merged = mergeToolResults(messages, [{ callId: "c1", name: "bash", state: "error", output: "boom", outputLength: 4, truncated: false }]);
  assert.equal(merged.length, 1);
  const part = merged[0].parts[0];
  assert.equal(part.type === "tool" && part.state, "error");
  assert.equal(part.type === "tool" && part.output, "boom");
  assert.equal(messages[0].parts[0].type === "tool" && messages[0].parts[0].state, "pending", "input is not mutated");
});

test("routes carry project and session identity only", () => {
  assert.deepEqual(parseSelection("/p/nebula/s/01ABC"), { project: "nebula", session: "01ABC" });
  assert.deepEqual(parseSelection("/p/nebula"), { project: "nebula", session: null });
  assert.deepEqual(parseSelection("/nope/../x"), { project: null, session: null });
  assert.equal(selectionPath({ project: "vela-hub-fork", session: "s1" }), "/p/vela-hub-fork/s/s1");
});
