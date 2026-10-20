import test from "node:test";
import assert from "node:assert/strict";
import { applicationServerKey } from "./push.ts";

test("VAPID application server keys decode from unpadded base64url", () => {
  assert.deepEqual([...applicationServerKey("_-4")], [255, 238]);
});
