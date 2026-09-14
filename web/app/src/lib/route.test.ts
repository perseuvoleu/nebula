import test from "node:test";
import assert from "node:assert/strict";
import { parseSelection, selectionPath } from "./route.ts";

test("All is a virtual list route; opening preserves real project identity and aggregate context", () => {
  assert.equal(selectionPath({ project: null, session: null, all: true }), "/?view=all");
  assert.deepEqual(parseSelection("/?view=all"), { project: null, session: null, all: true });
  for (const project of ["alpha", "beta"]) {
    const selection = { project, session: "same", all: true };
    assert.equal(selectionPath(selection), `/p/${project}/s/same?view=all`);
    assert.deepEqual(parseSelection(selectionPath(selection)), selection);
  }
  assert.deepEqual(parseSelection("/p/alpha/s/same"), { project: "alpha", session: "same", all: false });
});
