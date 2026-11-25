import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { validateE2eRun } from "./e2e-run.ts";
import E2eRunReporter from "../../../e2e/e2e-run-reporter.ts";

const FIXTURES = path.resolve(import.meta.dirname, "../../../../crates/nebula-core/tests/fixtures/board");
const FIXTURE = path.join(FIXTURES, "e2e_run_result.json");
const REPORTED = path.join(FIXTURES, "e2e_run_reporter.json");

type Case = Parameters<E2eRunReporter["onTestEnd"]>[0];
type Result = Parameters<E2eRunReporter["onTestEnd"]>[1];
type Full = Parameters<E2eRunReporter["onEnd"]>[0];
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

test("the Playwright reporter writes the result.json board32's e2e_run_reporter_output_validates checks, with every screenshot it names", async () => {
  const work = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-run-"));
  const out = path.join(work, "run");
  fs.mkdirSync(out);
  fs.writeFileSync(path.join(out, "stale.png"), PNG);
  const shot = (name: string) => { const file = path.join(work, name); fs.writeFileSync(file, PNG); return file; };
  const testCase = (id: string, title: string): Case => ({ id, title });
  const result = (status: Result["status"], duration: number, attachments: Result["attachments"], error?: string): Result =>
    ({ status, duration, attachments, errors: error ? [{ message: error }] : [] });
  const reporter = new E2eRunReporter({ outputDir: out, title: "board.spec" });
  const startTime = new Date(1764000000000);
  reporter.onBegin({ configFile: path.join(work, "playwright.config.ts") });
  reporter.onTestEnd(testCase("a", "drag a card"), result("passed", 1200, [{ name: "screenshot", contentType: "image/png", path: shot("a.png") }]));
  reporter.onTestEnd(testCase("b", "run a worker"), result("failed", 900, [{ name: "screenshot", contentType: "image/png", path: shot("b1.png") }], "first try"));
  reporter.onTestEnd(testCase("b", "run a worker"), result("timedOut", 3000, [{ name: "screenshot", contentType: "image/png", path: shot("b2.png") }], "\u001b[31mtimed out\u001b[39m"));
  reporter.onTestEnd(testCase("c", "not on mobile"), result("skipped", 0, []));
  await reporter.onEnd({ status: "failed", startTime, duration: 5100 });

  const written = JSON.parse(fs.readFileSync(path.join(out, "result.json"), "utf8"));
  assert.deepEqual(written, JSON.parse(fs.readFileSync(REPORTED, "utf8")));
  const files = filesUnder(out);
  assert.deepEqual([...files].sort(), ["result.json", "shots/001.png", "shots/002.png"], "a previous run's files are gone");
  assert.deepEqual(fs.readFileSync(path.join(out, "shots/002.png")), PNG);
});

function reporterIn(work: string, out: string) {
  const reporter = new E2eRunReporter({ outputDir: out, title: "board.spec" });
  reporter.onBegin({ configFile: path.join(work, "playwright.config.ts") });
  return reporter;
}

const ended = (status: Result["status"], attachments: Result["attachments"] = []): Result =>
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
  await reporterIn(work, out).onEnd({ status: "passed", startTime: new Date(), duration: 5 });
  assert.deepEqual([...filesUnder(out)].sort(), ["result.json", "shots/001.png"]);
  assert.equal(fs.readFileSync(path.join(out, "result.json"), "utf8"), "{\"previous\":true}");
});

test("a run that fails while it is written leaves the previous run in place", async () => {
  const { work, out } = previousRun();
  const reporter = reporterIn(work, out);
  reporter.onTestEnd({ id: "a", title: "drag a card" }, ended("passed", [{ name: "screenshot", contentType: "image/png", path: work }]));
  await assert.rejects(Promise.resolve().then(() => reporter.onEnd({ status: "passed", startTime: new Date(), duration: 5 })));
  assert.equal(fs.readFileSync(path.join(out, "result.json"), "utf8"), "{\"previous\":true}");
  assert.deepEqual(fs.readdirSync(work).sort(), ["run"]);
});

test("the run's status is Playwright's final outcome, not a tally of its steps", async () => {
  const statusOf = async (full: Full["status"], step: Result["status"]) => {
    const work = fs.mkdtempSync(path.join(os.tmpdir(), "e2e-run-"));
    const out = path.join(work, "run");
    const reporter = reporterIn(work, out);
    reporter.onTestEnd({ id: "a", title: "a step" }, ended(step));
    await reporter.onEnd({ status: full, startTime: new Date(), duration: 5 });
    return JSON.parse(fs.readFileSync(path.join(out, "result.json"), "utf8")).status;
  };
  assert.equal(await statusOf("passed", "failed"), "passed", "an expected failure (test.fail) passes the run");
  assert.equal(await statusOf("failed", "passed"), "failed", "a failing global teardown fails the run");
  assert.equal(await statusOf("interrupted", "passed"), "failed");
});
