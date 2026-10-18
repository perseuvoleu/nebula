import test from "node:test";
import assert from "node:assert/strict";
import { failedStep, formatCost, formatDuration, runDuration, shortRepo, startRunsPolling, type Run } from "./serverless.ts";

const run: Run = {
  id: "e564497e", repo: "https://github.com/unjs/h3", ref: null, cache_mode: "modules", instance: "standard-2",
  started_at: 1_792_323_464_686, ended_at: 1_792_323_466_574, status: "passed", cache: "modules hit (331 MB)",
  max_cost_usd: 0.0000377, steps: [],
};

test("repositories show as owner/name and unknown sources stay readable", () => {
  assert.equal(shortRepo("https://github.com/unjs/h3"), "unjs/h3");
  assert.equal(shortRepo("https://github.com/vitejs/vite.git/"), "vitejs/vite");
  assert.equal(shortRepo("git@example.com:team/app"), "git@example.com:team/app");
  assert.equal(shortRepo(null), "—");
});

test("durations read in seconds, minutes and hours", () => {
  assert.equal(formatDuration(1888), "1,9 s");
  assert.equal(formatDuration(184), "0,2 s");
  assert.equal(formatDuration(125_000), "2 min 05 s");
  assert.equal(formatDuration(3_720_000), "1 h 02 min");
  assert.equal(formatDuration(-5), "0,0 s");
});

test("a finished run lasts until its end; a running one keeps ticking with the clock", () => {
  assert.equal(runDuration(run, run.started_at + 99_000), 1888);
  const running: Run = { ...run, status: "running", ended_at: null };
  assert.equal(runDuration(running, run.started_at + 4000), 4000);
  assert.equal(runDuration(running, run.started_at + 9000), 9000);
});

test("cost is an upper bound and missing cost is explicit", () => {
  assert.equal(formatCost(0.0000377), "≤ 0,000038 $");
  assert.equal(formatCost(0.0123), "≤ 0,012 $");
  assert.equal(formatCost(null), "—");
});

test("the first step with a non-zero exit code is the failed one", () => {
  assert.equal(failedStep([{ name: "boot", ms: 1, exitCode: 0 }, { name: "test", ms: 2, exitCode: 1 }, { name: "x", ms: 3, exitCode: 2 }]), 1);
  assert.equal(failedStep([{ name: "boot", ms: 1, exitCode: 0 }, { name: "clone", ms: null, exitCode: null }]), -1);
});

test("runs poll every 5 s only while the page is visible, and stop when disposed", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const document = Object.assign(new EventTarget(), { hidden: false });
  let reads = 0;
  const results: unknown[] = [];
  const stop = startRunsPolling(async () => { reads++; return reads; }, value => results.push(value), document);
  assert.equal(reads, 1);
  await Promise.resolve(); await Promise.resolve();
  t.mock.timers.tick(5000);
  assert.equal(reads, 2);
  await Promise.resolve(); await Promise.resolve();
  document.hidden = true;
  document.dispatchEvent(new Event("visibilitychange"));
  t.mock.timers.tick(20000);
  assert.equal(reads, 2);
  document.hidden = false;
  document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(reads, 3);
  await Promise.resolve(); await Promise.resolve();
  stop();
  t.mock.timers.tick(20000);
  document.dispatchEvent(new Event("visibilitychange"));
  assert.equal(reads, 3);
  assert.deepEqual(results, [1, 2, 3]);
});

test("a failed read is published and polling continues", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout"] });
  const document = Object.assign(new EventTarget(), { hidden: false });
  let reads = 0;
  const results: unknown[] = [];
  const stop = startRunsPolling(async () => { reads++; throw new Error("down"); }, value => results.push(value), document);
  await Promise.resolve(); await Promise.resolve();
  t.mock.timers.tick(5000);
  assert.equal(reads, 2);
  stop();
  assert.ok(results[0] instanceof Error);
});
