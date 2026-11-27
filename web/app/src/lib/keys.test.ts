import test from "node:test";
import assert from "node:assert/strict";
import { pageKey } from "./keys.ts";

function fakeEvent(overrides: {
  key?: string;
  repeat?: boolean;
  metaKey?: boolean;
  ctrlKey?: boolean;
  altKey?: boolean;
  defaultPrevented?: boolean;
  matches?: string[];
} = {}): KeyboardEvent {
  const matched = new Set(overrides.matches ?? []);
  return {
    key: overrides.key ?? "j",
    repeat: overrides.repeat ?? false,
    metaKey: overrides.metaKey ?? false,
    ctrlKey: overrides.ctrlKey ?? false,
    altKey: overrides.altKey ?? false,
    defaultPrevented: overrides.defaultPrevented ?? false,
    target: {
      closest: (selector: string) => selector.split(",").some((s) => matched.has(s.trim())) ? {} : null,
    },
  } as unknown as KeyboardEvent;
}

test("pageKey ignores key repeats so holding a does not double-approve", () => {
  assert.equal(pageKey(fakeEvent({ key: "a", repeat: false })), true);
  assert.equal(pageKey(fakeEvent({ key: "a", repeat: true })), false);
});

test("pageKey does not intercept Enter or Space on focused buttons or links", () => {
  for (const selector of ["button", "a", '[role="button"]', '[role="link"]']) {
    assert.equal(pageKey(fakeEvent({ key: "Enter", matches: [selector] })), false, `Enter on ${selector}`);
    assert.equal(pageKey(fakeEvent({ key: " ", matches: [selector] })), false, `Space on ${selector}`);
    assert.equal(pageKey(fakeEvent({ key: "j", matches: [selector] })), true, `j on ${selector}`);
  }
  assert.equal(pageKey(fakeEvent({ key: "Enter" })), true);
});

test("pageKey ignores typing targets and dialogs", () => {
  for (const selector of ["input", "textarea", "select", "[contenteditable=true]", "[role=dialog]"]) {
    assert.equal(pageKey(fakeEvent({ key: "j", matches: [selector] })), false, selector);
    assert.equal(pageKey(fakeEvent({ key: "i", matches: [selector] })), false, `i on ${selector}`);
    assert.equal(pageKey(fakeEvent({ key: "0", matches: [selector] })), false, `0 on ${selector}`);
  }
});

test("pageKey allows i and 0 on page targets and ignores repeats", () => {
  for (const key of ["i", "0"]) {
    assert.equal(pageKey(fakeEvent({ key })), true, key);
    assert.equal(pageKey(fakeEvent({ key, matches: ["button"] })), true, `${key} on button`);
    assert.equal(pageKey(fakeEvent({ key, repeat: true })), false, `${key} repeat`);
  }
});
