export interface Step { name: string; ms: number | null; exitCode: number | null }
/** One build/test run on Cloudflare Containers, as normalized by the gateway. Times are epoch ms. */
export interface Run {
  id: string;
  repo: string | null;
  ref: string | null;
  cache_mode: string | null;
  instance: string | null;
  started_at: number;
  /** Null while the run is still going. */
  ended_at: number | null;
  status: "running" | "passed" | "failed" | "error";
  cache: string | null;
  /** Upper bound: CPU assumed fully used. */
  max_cost_usd: number | null;
  steps: Step[];
}
export interface RunsResponse { configured: boolean; runs: Run[] }

export const RUN_STATUS_LABEL: Record<Run["status"], string> = {
  running: "Rulează", passed: "Trecut", failed: "Eșuat", error: "Eroare",
};

export function shortRepo(repo: string | null): string {
  if (!repo) return "—";
  const match = /^https?:\/\/[^/]+\/([^/]+)\/([^/]+?)(?:\.git)?\/?$/.exec(repo);
  return match ? `${match[1]}/${match[2]}` : repo;
}

const decimal = new Intl.NumberFormat("ro-RO", { minimumFractionDigits: 1, maximumFractionDigits: 1 });
const pad = (value: number) => String(value).padStart(2, "0");

export function formatDuration(ms: number): string {
  const safe = Math.max(0, ms);
  if (safe < 60_000) return formatSeconds(safe);
  const seconds = Math.floor(safe / 1000);
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ${pad(seconds % 60)} s`;
  return `${Math.floor(seconds / 3600)} h ${pad(Math.floor(seconds / 60) % 60)} min`;
}

export const formatSeconds = (ms: number | null): string => ms === null ? "—" : `${decimal.format(Math.max(0, ms) / 1000)} s`;

export const runDuration = (run: Run, now: number): number => (run.ended_at ?? now) - run.started_at;

const cost = new Intl.NumberFormat("ro-RO", { maximumSignificantDigits: 2 });
export const formatCost = (usd: number | null): string => usd === null ? "—" : `≤ ${cost.format(usd)} $`;

export const formatStart = (ms: number): string =>
  new Date(ms).toLocaleString("ro-RO", { dateStyle: "short", timeStyle: "medium" });

export const failedStep = (steps: Step[]): number => steps.findIndex(step => step.exitCode !== null && step.exitCode !== 0);

/** Poll every 5 s while the page is visible; a hidden page cancels the timer and resumes on return. */
export function startRunsPolling<T>(read: (signal: AbortSignal) => Promise<T>, publish: (value: T | Error) => void,
  document: Pick<Document, "hidden" | "addEventListener" | "removeEventListener">): () => void {
  let disposed = false;
  let active: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const refresh = async () => {
    clearTimeout(timer);
    if (disposed || active || document.hidden) return;
    const abort = new AbortController();
    active = abort;
    try {
      const value = await read(abort.signal);
      if (!abort.signal.aborted) publish(value);
    } catch (error) {
      if (!abort.signal.aborted) publish(error instanceof Error ? error : new Error(String(error)));
    } finally {
      if (active === abort) {
        active = null;
        if (!disposed && !document.hidden) timer = setTimeout(() => void refresh(), 5000);
      }
    }
  };
  const wake = () => {
    if (document.hidden) { clearTimeout(timer); active?.abort(); active = null; return; }
    void refresh();
  };
  document.addEventListener("visibilitychange", wake);
  void refresh();
  return () => {
    disposed = true; clearTimeout(timer); active?.abort();
    document.removeEventListener("visibilitychange", wake);
  };
}
