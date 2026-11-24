import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { validateE2eRun } from "./e2e-run.ts";
import E2eRunReporter from "../../../e2e/e2e-run-reporter.ts";

const FIXTURE = path.resolve(import.meta.dirname, "../../../../crates/nebula-core/tests/fixtures/board/e2e_run_result.json");
const PNG = Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=", "base64");

function filesUnder(root: string): Set<string> {
  return new Set((fs.readdirSync(root, { recursive: true }) as string[])
    .filter((name) => fs.statSync(path.join(root, name)).isFile()).map((name) => name.split(path.sep).join("/")));
}

test("the golden e2e_run_result.json passes, and a run naming a missing screenshot or an unknown status does not", () => {
  const fixture = JSON.parse(fs.readFileSync(FIXTURE, "utf8"));
  const shots = new Set(["result.json", "shots/drag.png", "shots/run.png"]);
  assert.equal(validateE2eRun(fixture, shots).steps.length, 2);
  assert.throws(() => validateE2eRun(fixture, new Set(["result.json", "shots/drag.png"])), /shots\/run\.png/);
  assert.throws(() => validateE2eRun({ ...fixture, steps: [{ ...fixture.steps[0], status: "timedOut" }] }, shots));
  assert.throws(() => validateE2eRun({ ...fixture, started_at: "2026-11-24" }, shots));
});

test("the Playwright reporter writes a result.json in Decision 5's format with every screenshot it names", async () => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-run-"));
  const out = path.join(work, "run");
  fs.mkdirSync(out);
  fs.writeFileSync(path.join(out, "stale.png"), PNG);
  const shot = (name: string) => { const file = path.join(work, name); fs.writeFileSync(file, PNG); return file; };
  const testCase = (id: string, title: string) => ({ id, title });
  const result = (status: string, duration: number, attachments: object[], error?: string) =>
    ({ status, duration, attachments, errors: error ? [{ message: error }] : [], error: error ? { message: error } : undefined });
  const reporter = new E2eRunReporter({ outputDir: out, title: "board.spec" });
  const startTime = new Date(1764000000000);
  reporter.onBegin({ configFile: path.join(work, "playwright.config.ts") } as never, {} as never);
  reporter.onTestEnd(testCase("a", "drag a card") as never, result("passed", 1200, [{ name: "screenshot", contentType: "image/png", path: shot("a.png") }]) as never);
  reporter.onTestEnd(testCase("b", "run a worker") as never, result("failed", 900, [{ name: "screenshot", contentType: "image/png", path: shot("b1.png") }], "first try") as never);
  reporter.onTestEnd(testCase("b", "run a worker") as never, result("timedOut", 3000, [{ name: "screenshot", contentType: "image/png", path: shot("b2.png") }], "\u001b[31mtimed out\u001b[39m") as never);
  reporter.onTestEnd(testCase("c", "not on mobile") as never, result("skipped", 0, []) as never);
  await reporter.onEnd({ status: "failed", startTime, duration: 5100 } as never);

  const written = JSON.parse(fs.readFileSync(path.join(out, "result.json"), "utf8"));
  const files = filesUnder(out);
  const run = validateE2eRun(written, files);
  assert.deepEqual(Object.keys(written).sort(), Object.keys(JSON.parse(fs.readFileSync(FIXTURE, "utf8"))).sort());
  assert.deepEqual({ title: run.title, status: run.status, started_at: run.started_at, duration_ms: run.duration_ms },
    { title: "board.spec", status: "failed", started_at: 1764000000000, duration_ms: 5100 });
  assert.deepEqual(run.steps.map((s) => [s.name, s.status, s.duration_ms, !!s.screenshot, s.error]), [
    ["drag a card", "passed", 1200, true, undefined],
    ["run a worker", "failed", 3000, true, "timed out"],
    ["not on mobile", "skipped", 0, false, undefined],
  ]);
  assert.ok(!files.has("stale.png"), "a previous run's files are gone");
  assert.equal(new Set(run.steps.flatMap((s) => s.screenshot ?? [])).size, 2);
});

function reporterIn(work: string, out: string) {
  const reporter = new E2eRunReporter({ outputDir: out, title: "board.spec" });
  reporter.onBegin({ configFile: path.join(work, "playwright.config.ts") } as never, {} as never);
  return reporter;
}

const ended = (status: string, attachments: object[] = []) =>
  ({ status, duration: 10, attachments, errors: status === "failed" ? [{ message: "boom" }] : [] });

function previousRun() {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-run-"));
  const out = path.join(work, "run");
  fs.mkdirSync(path.join(out, "shots"), { recursive: true });
  fs.writeFileSync(path.join(out, "result.json"), "{\"previous\":true}");
  fs.writeFileSync(path.join(out, "shots", "001.png"), PNG);
  return { work, out };
}

test("a run that executed no test, such as --list, leaves the previous run in place", async () => {
  const { work, out } = previousRun();
  await reporterIn(work, out).onEnd({ status: "passed", startTime: new Date(), duration: 5 } as never);
  assert.deepEqual([...filesUnder(out)].sort(), ["result.json", "shots/001.png"]);
  assert.equal(fs.readFileSync(path.join(out, "result.json"), "utf8"), "{\"previous\":true}");
});

test("a run that fails while it is written leaves the previous run in place", async () => {
  const { work, out } = previousRun();
  const reporter = reporterIn(work, out);
  reporter.onTestEnd({ id: "a", title: "drag a card" } as never, ended("passed", [{ name: "screenshot", contentType: "image/png", path: work }]) as never);
  await assert.rejects(Promise.resolve().then(() => reporter.onEnd({ status: "passed", startTime: new Date(), duration: 5 } as never)));
  assert.equal(fs.readFileSync(path.join(out, "result.json"), "utf8"), "{\"previous\":true}");
  assert.deepEqual(fs.readdirSync(work).sort(), ["run"]);
});

test("the run's status is Playwright's final outcome, not a tally of its steps", async () => {
  const statusOf = async (full: string, step: string) => {
    const work = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-run-"));
    const out = path.join(work, "run");
    const reporter = reporterIn(work, out);
    reporter.onTestEnd({ id: "a", title: "a step" } as never, ended(step) as never);
    await reporter.onEnd({ status: full, startTime: new Date(), duration: 5 } as never);
    return JSON.parse(fs.readFileSync(path.join(out, "result.json"), "utf8")).status;
  };
  assert.equal(await statusOf("passed", "failed"), "passed", "an expected failure (test.fail) passes the run");
  assert.equal(await statusOf("failed", "passed"), "failed", "a failing global teardown fails the run");
  assert.equal(await statusOf("interrupted", "passed"), "failed");
});
