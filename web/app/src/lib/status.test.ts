import test from "node:test";
import assert from "node:assert/strict";
import { formatWhen } from "./status.ts";

test("formatWhen names the year only for a date outside this year", () => {
  const thisYear = new Date().getFullYear();
  assert.doesNotMatch(formatWhen(new Date(thisYear, 2, 5, 9, 30).getTime()), new RegExp(String(thisYear)));
  assert.match(formatWhen(new Date(thisYear - 1, 2, 5, 9, 30).toISOString()), new RegExp(String(thisYear - 1)));
});
