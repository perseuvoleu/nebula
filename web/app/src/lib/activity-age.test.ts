import test from "node:test";
import assert from "node:assert/strict";
import { activityAge, compactAge } from "./activity-age.ts";

const now = 1_800_000_000_000;

test("session activity labels show elapsed minutes, hours and days, including time passing", () => {
  assert.equal(activityAge(now - 10_000, now), "acum câteva secunde");
  assert.equal(activityAge(now - 60_000, now), "acum 1 min");
  assert.equal(activityAge(now - 120_000, now), "acum 2 min");
  assert.equal(activityAge(now - 3_600_000, now), "acum 1 h");
  assert.equal(activityAge(now - 86_400_000, now), "acum 1 zi");
  assert.equal(activityAge(now - 172_800_000, now), "acum 2 zile");
  assert.equal(activityAge(now - 60_000, now + 60_000), "acum 2 min");
});

test("unknown activity stays explicit and clock skew never displays negative elapsed time", () => {
  for (const stamp of [undefined, 0, -1, NaN, Infinity]) {
    assert.equal(activityAge(stamp, now), "Activitate necunoscută");
  }
  assert.equal(activityAge(now + 60_000, now), "acum câteva secunde");
});

test("tile ages fit a few characters: minutes, hours, then days, and nothing for an unknown time", () => {
  assert.equal(compactAge(now - 10_000, now), "0m");
  assert.equal(compactAge(now - 9 * 60_000, now), "9m");
  assert.equal(compactAge(now - 3 * 3_600_000, now), "3h");
  assert.equal(compactAge(now - 12 * 86_400_000, now), "12z");
  assert.equal(compactAge(undefined, now), "");
});
