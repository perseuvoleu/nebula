import fs from "node:fs";
import path from "node:path";
import type { FullConfig, FullResult, Reporter, TestCase, TestResult } from "@playwright/test/reporter";

type Status = "passed" | "failed" | "skipped";
type Step = { name: string; status: Status; duration_ms: number; screenshot?: string; error?: string };

const ANSI = /\u001b\[[0-9;]*m/g;

type Case = Pick<TestCase, "id" | "title">;
type Result = Pick<TestResult, "status" | "duration" | "attachments" | "errors">;
type Full = Pick<FullResult, "status" | "startTime" | "duration">;

function stepStatus(status: TestResult["status"]): Status {
  return status === "passed" || status === "skipped" ? status : "failed";
}

/** Writes Decision 5's e2e-run (result.json plus shots/*.png) for `nebula artifact add --kind e2e-run`. */
export default class E2eRunReporter implements Reporter {
  private readonly options: { outputDir: string; title?: string };
  private root = process.cwd();
  private readonly results = new Map<string, { test: Case; result: Result }>();

  constructor(options: { outputDir: string; title?: string }) {
    this.options = options;
  }

  onBegin(config: Pick<FullConfig, "configFile">) {
    if (config.configFile) this.root = path.dirname(config.configFile);
  }

  onTestEnd(test: Case, result: Result) {
    this.results.set(test.id, { test, result });
  }

  async onEnd(full: Full) {
    if (this.results.size === 0) return;
    const out = path.resolve(this.root, this.options.outputDir);
    const staging = `${out}.staging`;
    fs.rmSync(staging, { recursive: true, force: true });
    try {
      this.write(staging, full);
      fs.rmSync(out, { recursive: true, force: true });
      fs.renameSync(staging, out);
    } finally {
      fs.rmSync(staging, { recursive: true, force: true });
    }
  }

  private write(out: string, full: Full) {
    fs.mkdirSync(path.join(out, "shots"), { recursive: true });
    const steps = [...this.results.values()].map(({ test, result }, index): Step => {
      const step: Step = { name: test.title, status: stepStatus(result.status), duration_ms: Math.max(0, Math.round(result.duration)) };
      const shot = result.attachments.find((a) => a.contentType === "image/png" && a.path && fs.existsSync(a.path));
      if (shot?.path) {
        step.screenshot = `shots/${String(index + 1).padStart(3, "0")}.png`;
        fs.copyFileSync(shot.path, path.join(out, step.screenshot));
      }
      const error = result.errors.map((e) => e.message ?? e.value ?? "").join("\n").replace(ANSI, "").trim();
      if (step.status === "failed" && error) step.error = error;
      return step;
    });
    const run = { title: this.options.title ?? "e2e", status: full.status === "passed" ? "passed" : "failed", started_at: full.startTime.getTime(),
      duration_ms: Math.max(0, Math.round(full.duration)), steps };
    fs.writeFileSync(path.join(out, "result.json"), JSON.stringify(run, null, 1));
  }

  printsToStdio() {
    return false;
  }
}
