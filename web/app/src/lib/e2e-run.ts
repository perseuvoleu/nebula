import type { E2eRun, E2eStatus, E2eStep } from "../components/board/E2eRunView.tsx";

const STATUSES: readonly unknown[] = ["passed", "failed", "skipped"] satisfies E2eStatus[];

const isObject = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null && !Array.isArray(value);
const isCount = (value: unknown): value is number => Number.isSafeInteger(value) && (value as number) >= 0;

function field<T>(row: Record<string, unknown>, name: string, ok: (value: unknown) => boolean, where: string): T {
  if (!ok(row[name])) throw new Error(`result.json: ${where}${name} is missing or has the wrong type`);
  return row[name] as T;
}

function optional(row: Record<string, unknown>, name: string, where: string): string | undefined {
  const value = row[name];
  if (value === undefined || value === null) return undefined;
  if (typeof value !== "string") throw new Error(`result.json: ${where}${name} must be a string`);
  return value;
}

/** Decision 5's result.json, checked as nebula-core's validate_e2e_run does; `files` holds the run's relative paths. */
export function validateE2eRun(value: unknown, files: ReadonlySet<string>): E2eRun {
  if (!isObject(value) || !Array.isArray(value.steps) || !value.steps.every(isObject)) {
    throw new Error("result.json: the run and each step must be JSON objects");
  }
  const steps = (value.steps as Record<string, unknown>[]).map((row, index): E2eStep => {
    const where = `steps[${index}].`;
    const step: E2eStep = {
      name: field(row, "name", (v) => typeof v === "string", where),
      status: field(row, "status", (v) => STATUSES.includes(v), where),
      duration_ms: field(row, "duration_ms", isCount, where),
    };
    const screenshot = optional(row, "screenshot", where);
    const error = optional(row, "error", where);
    if (screenshot !== undefined) step.screenshot = screenshot;
    if (error !== undefined) step.error = error;
    return step;
  });
  const run: E2eRun = {
    title: field(value, "title", (v) => typeof v === "string", ""),
    status: field(value, "status", (v) => STATUSES.includes(v), ""),
    started_at: field(value, "started_at", Number.isSafeInteger, ""),
    duration_ms: field(value, "duration_ms", isCount, ""),
    steps,
  };
  const missing = steps.find((s) => s.screenshot !== undefined && !files.has(s.screenshot));
  if (missing) throw new Error(`result.json names ${missing.screenshot}, which is not in the run`);
  return run;
}
