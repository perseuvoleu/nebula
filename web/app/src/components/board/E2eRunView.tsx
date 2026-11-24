import { CheckIcon, ImageOffIcon, MinusIcon, XIcon } from "lucide-react";

export type E2eStatus = "passed" | "failed" | "skipped";

export interface E2eStep {
  name: string;
  status: E2eStatus;
  duration_ms: number;
  screenshot?: string;
  error?: string;
}

export interface E2eRun {
  title: string;
  status: E2eStatus;
  started_at: number;
  duration_ms: number;
  steps: E2eStep[];
}

const STATUS_LABEL: Record<E2eStatus, string> = { passed: "trecut", failed: "eșuat", skipped: "sărit" };
const STATUS_ICON = { passed: CheckIcon, failed: XIcon, skipped: MinusIcon };

function formatDuration(ms: number): string {
  if (ms < 1000) return `${Math.round(ms)} ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toLocaleString("ro-RO", { maximumFractionDigits: 1 })} s`;
  const whole = Math.round(seconds);
  return `${Math.floor(whole / 60)} min ${String(whole % 60).padStart(2, "0")} s`;
}

function summary(steps: E2eStep[]): string {
  const failed = steps.filter((s) => s.status === "failed").length;
  const passed = steps.filter((s) => s.status === "passed").length;
  const skipped = steps.length - failed - passed;
  if (failed === 0 && skipped === 0) return `Toți cei ${passed} pași au trecut`;
  return [`${passed} trecuți`, failed && `${failed} eșuați`, skipped && `${skipped} săriți`].filter(Boolean).join(" · ");
}

export function E2eRunView({ run, assetUrl }: { run: E2eRun; assetUrl: (path: string) => string }) {
  const started = new Date(run.started_at);
  return (
    <div className="flex flex-col gap-3">
      <p className="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
        <span className="font-medium">{summary(run.steps)}</span>
        <span className="font-mono text-xs text-muted-foreground tabular-nums">
          {formatDuration(run.duration_ms)}
          {!Number.isNaN(started.getTime()) && ` · ${started.toLocaleString("ro-RO", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })}`}
        </span>
      </p>
      <ol className="flex flex-col gap-2">
        {run.steps.map((step, i) => {
          const Icon = STATUS_ICON[step.status];
          const failed = step.status === "failed";
          return (
            <li key={i} className={`flex gap-3 rounded-lg border p-2 ${failed ? "border-foreground/40 bg-muted" : "border-border bg-card"}`}>
              {step.screenshot ? (
                <a href={assetUrl(step.screenshot)} target="_blank" rel="noopener noreferrer"
                  className="shrink-0 self-start overflow-hidden rounded-md border border-border focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
                  <img src={assetUrl(step.screenshot)} alt={`Captură: ${step.name}`} loading="lazy" decoding="async"
                    className="h-12 w-20 bg-background object-cover object-top sm:h-16 sm:w-28" />
                </a>
              ) : (
                <span className="flex h-12 w-20 shrink-0 self-start items-center justify-center rounded-md border border-dashed border-border text-muted-foreground sm:h-16 sm:w-28">
                  <ImageOffIcon aria-hidden className="size-4" />
                  <span className="sr-only">Fără captură</span>
                </span>
              )}
              <div className="flex min-w-0 flex-1 flex-col gap-1">
                <div className="flex min-w-0 items-start gap-2 text-sm">
                  <Icon aria-hidden className={`mt-0.5 size-4 shrink-0 ${step.status === "skipped" ? "text-muted-foreground" : ""}`} />
                  <span className="min-w-0 flex-1 font-medium break-words">
                    <span className="font-mono text-muted-foreground tabular-nums">{i + 1}.</span> {step.name}
                  </span>
                  <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{formatDuration(step.duration_ms)}</span>
                </div>
                <span className={`text-xs ${failed ? "font-medium" : "text-muted-foreground"}`}>{STATUS_LABEL[step.status]}</span>
                {step.error && <pre className="font-mono text-xs leading-relaxed whitespace-pre-wrap break-words">{step.error}</pre>}
              </div>
            </li>
          );
        })}
      </ol>
    </div>
  );
}
