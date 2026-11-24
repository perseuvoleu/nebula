import fs from "node:fs";
import path from "node:path";
import type { FullConfig, FullResult, Reporter, Suite, TestCase, TestResult } from "@playwright/test/reporter";

type Status = "passed" | "failed" | "skipped";
type Step = { name: string; status: Status; duration_ms: number; screenshot?: string; error?: string };

const ANSI = /\u001b\[[0-9;]*m/g;

function stepStatus(status: TestResult["status"]): Status {
  return status === "passed" || status === "skipped" ? status : "failed";
}

/** Writes Decision 5's e2e-run (result.json plus shots/*.png) for `nebula artifact add --kind e2e-run`. */
export default class E2eRunReporter implements Reporter {
  private readonly options: { outputDir: string; title?: string };
  private root = process.cwd();
  private readonly results = new Map<string, { test: TestCase; result: TestResult }>();

  constructor(options: { outputDir: string; title?: string }) {
    this.options = options;
  }

  onBegin(config: FullConfig, _suite: Suite) {
    if (config.configFile) this.root = path.dirname(config.configFile);
  }

  onTestEnd(test: TestCase, result: TestResult) {
    this.results.set(test.id, { test, result });
  }

  async onEnd(full: FullResult) {
    const out = path.resolve(this.root, this.options.outputDir);
    fs.rmSync(out, { recursive: true, force: true });
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
    const status: Status = full.status === "passed" && !steps.some((s) => s.status === "failed") ? "passed" : "failed";
    const run = { title: this.options.title ?? "e2e", status, started_at: full.startTime.getTime(),
      duration_ms: Math.max(0, Math.round(full.duration)), steps };
    fs.writeFileSync(path.join(out, "result.json"), JSON.stringify(run, null, 1));
  }

  printsToStdio() {
    return false;
  }
}
