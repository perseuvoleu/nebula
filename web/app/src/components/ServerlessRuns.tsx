import { useEffect, useState } from "react";
import { ArrowLeftIcon, ChevronDownIcon } from "lucide-react";
import { api, ApiError, describeError } from "@/lib/api";
import { failedStep, formatCost, formatDuration, formatSeconds, formatStart, RUN_STATUS_LABEL, runDuration, shortRepo, startRunsPolling, type Run, type RunsResponse } from "@/lib/serverless";
import { Button } from "@/components/ui/button";

const COLUMNS = "md:grid md:grid-cols-[minmax(0,1.6fr)_minmax(0,0.8fr)_6rem_minmax(0,1.2fr)_7rem_10rem_6rem_7rem_1rem] md:items-center md:gap-3";

function Steps({ run }: { run: Run }) {
  if (run.steps.length === 0) return <p className="px-3 pb-3 text-xs text-muted-foreground">{run.status === "running" ? "Pașii apar după încheierea rulării." : "Niciun pas înregistrat."}</p>;
  const failed = failedStep(run.steps);
  return (
    <ol className="mx-3 mb-3 divide-y divide-border rounded-md border border-border text-xs">
      {run.steps.map((step, index) => (
        <li key={index} className={`flex items-center gap-3 px-2.5 py-1.5 ${index === failed ? "bg-tint-err font-medium text-err" : ""}`}>
          <span className="min-w-0 flex-1 truncate font-mono">{step.name}</span>
          <span className="tabular-nums">{formatSeconds(step.ms)}</span>
          <span className="w-16 text-right tabular-nums">{step.exitCode === null ? "cod —" : `cod ${step.exitCode}`}</span>
        </li>
      ))}
    </ol>
  );
}

function RunRow({ run, now, open, onToggle }: { run: Run; now: number; open: boolean; onToggle: () => void }) {
  const running = run.status === "running";
  const bad = run.status === "failed" || run.status === "error";
  return (
    <li className="rounded-lg border border-border md:rounded-none md:border-0 md:border-b">
      <button type="button" onClick={onToggle} aria-expanded={open}
        className={`flex w-full flex-col gap-1 px-3 py-2.5 text-left text-sm hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${COLUMNS}`}>
        <span className="flex items-center gap-2 md:contents">
          <span className={`min-w-0 flex-1 truncate font-medium ${running ? "text-working" : ""}`} title={run.repo ?? undefined}>{shortRepo(run.repo)}</span>
          <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{run.ref ?? "implicit"}</span>
          <span className={`shrink-0 text-[11px] md:text-xs ${bad ? "rounded border border-foreground px-1.5 py-px font-medium" : "text-muted-foreground md:text-foreground"}`}>
            {running && <span aria-hidden className="mr-1 inline-block size-1.5 animate-pulse rounded-full bg-warn align-middle" />}
            {RUN_STATUS_LABEL[run.status]}
          </span>
        </span>
        <span className="flex flex-wrap gap-x-1.5 text-xs text-muted-foreground md:contents">
          <span className="truncate">{run.cache ?? (run.cache_mode ? `cache ${run.cache_mode}` : "—")}</span>
          <span aria-hidden className="md:hidden">·</span>
          <span className="font-mono">{run.instance ?? "—"}</span>
          <span aria-hidden className="md:hidden">·</span>
          <span className="tabular-nums">{formatStart(run.started_at)}</span>
        </span>
        <span className="flex gap-x-1.5 text-xs md:contents">
          <span className="tabular-nums">{running ? `${formatDuration(runDuration(run, now))} până acum` : formatDuration(runDuration(run, now))}</span>
          <span aria-hidden className="text-muted-foreground md:hidden">·</span>
          <span className="tabular-nums text-muted-foreground md:text-foreground" title="Limită superioară: CPU presupus folosit integral">{formatCost(run.max_cost_usd)}</span>
          <ChevronDownIcon aria-hidden className={`ml-auto size-4 shrink-0 text-muted-foreground transition-transform md:ml-0 ${open ? "rotate-180" : ""}`} />
        </span>
      </button>
      {open && <Steps run={run} />}
    </li>
  );
}

/** Build/test runs on Cloudflare Containers, polled while this section is mounted and visible. */
export function ServerlessRuns({ onBack }: { onBack?: () => void }) {
  const [data, setData] = useState<RunsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<Set<string>>(() => new Set());
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => startRunsPolling(
    signal => api<RunsResponse>("serverless/runs", { limit: "100" }, undefined, { signal, timeoutMs: 15000 }),
    value => {
      setNow(Date.now());
      if (!(value instanceof Error)) { setData(value); setError(null); return; }
      // A rejected gateway token already logs out through api(); keep the last list otherwise.
      if (!(value instanceof ApiError && value.status === 401)) setError(describeError(value));
    }, document), []);
  const anyRunning = data?.runs.some(run => run.status === "running") ?? false;
  useEffect(() => {
    if (!anyRunning) return;
    const timer = setInterval(() => { if (!document.hidden) setNow(Date.now()); }, 1000);
    return () => clearInterval(timer);
  }, [anyRunning]);
  const toggle = (id: string) => setOpen(old => {
    const next = new Set(old);
    if (!next.delete(id)) next.add(id);
    return next;
  });

  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label="Rulări serverless">
      <header className="flex min-h-14 items-center gap-2 border-b border-border px-2 md:px-4">
        {onBack && <Button type="button" variant="ghost" size="icon-lg" aria-label="Înapoi la sesiuni" onClick={onBack} className="size-11"><ArrowLeftIcon aria-hidden className="size-5" /></Button>}
        <h1 className="text-[15px] font-semibold">Rulări serverless</h1>
        <span className="ml-auto text-xs text-muted-foreground">Cloudflare Containers</span>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-2 py-3 md:px-4" aria-busy={data === null && !error}>
        {error && <p role="alert" className="px-1 pb-3 text-sm text-muted-foreground">Nu s-au putut încărca rulările: {error}{data && " Lista afișată poate fi veche."}</p>}
        {data === null && !error && (
          <ul className="space-y-2" aria-hidden>{[0, 1, 2].map(i => <li key={i} className="h-20 animate-pulse rounded-lg bg-muted/60" />)}</ul>
        )}
        {data && !data.configured && (
          <div className="px-1 py-6 text-sm text-muted-foreground">
            <p className="font-medium text-foreground">Serverless neconfigurat</p>
            <p className="mt-1 max-w-md">Gateway-ul nu are <code className="font-mono">NEBULA_SERVERLESS_URL</code> și <code className="font-mono">NEBULA_SERVERLESS_TOKEN</code>. Tokenul Cloudflare rămâne pe server.</p>
          </div>
        )}
        {data?.configured && data.runs.length === 0 && <p className="px-1 py-6 text-sm text-muted-foreground">Nicio rulare înregistrată încă.</p>}
        {data?.configured && data.runs.length > 0 && (
          <>
            <div className={`hidden border-b border-border px-3 pb-2 text-xs text-muted-foreground ${COLUMNS}`} aria-hidden>
              <span>Repo</span><span>Ref</span><span>Stare</span><span>Cache</span><span>Instanță</span><span>Pornit</span><span>Durată</span><span>Cost maxim</span><span />
            </div>
            <ul className="space-y-2 md:space-y-0">
              {data.runs.map(run => <RunRow key={run.id} run={run} now={now} open={open.has(run.id)} onToggle={() => toggle(run.id)} />)}
            </ul>
            <p className="px-1 pt-3 text-[11px] text-muted-foreground">Costul este o limită superioară (CPU presupus folosit integral). Actualizare la 5 secunde cât timp pagina este vizibilă.</p>
          </>
        )}
      </div>
    </section>
  );
}
