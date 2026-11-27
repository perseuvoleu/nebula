import { ArrowLeftIcon, SearchIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DecisionCard, type DecisionRow } from "./DecisionCard";

function byProject(decisions: DecisionRow[]): [string | null, DecisionRow[]][] {
  const groups = new Map<string | null, DecisionRow[]>();
  for (const d of decisions) groups.set(d.project, [...(groups.get(d.project) ?? []), d]);
  return [...groups];
}

export function DecisionsPage({ project, backHref, query, onQueryChange, decisions, loading = false, error = null }: {
  project: string | null;
  backHref?: string;
  query: string;
  onQueryChange: (query: string) => void;
  decisions: DecisionRow[];
  loading?: boolean;
  error?: string | null;
}) {
  const groups = byProject(decisions);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex flex-wrap items-center gap-2 border-b bg-bg0 px-3.5 py-1.5">
        {backHref && (
          <Button asChild variant="ghost" size="sm">
            <a href={backHref}><ArrowLeftIcon aria-hidden />{project ? <span className="font-mono">@{project}</span> : "Board"}</a>
          </Button>
        )}
        <h1 className="flex items-center gap-1.5 text-sm font-semibold"><span aria-hidden className="size-2 rotate-45 rounded-[1px] bg-special" />Decizii</h1>
        {project && !backHref && <span className="font-mono text-sm text-primary">{project}</span>}
        <label className="relative ml-auto w-full sm:w-64">
          <span className="sr-only">Caută decizii</span>
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-dim" />
          <input value={query} onChange={(e) => onQueryChange(e.target.value)} type="search" placeholder="Caută" maxLength={200}
            className="field h-8 w-full bg-bg1 pr-3 pl-8 text-xs" />
        </label>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-[18px] py-3.5">
        <div className="mx-auto flex max-w-3xl flex-col gap-5">
          {error && <p role="alert" className="rounded-lg border border-dashed px-3 py-2 text-sm">{error}</p>}
          {loading && <p aria-busy="true" className="py-8 text-center text-sm text-muted-foreground">Se caută…</p>}
          {groups.length === 0 && !loading && !error && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {query.trim() ? "Nicio decizie nu se potrivește." : "Nicio decizie înregistrată încă."}
            </p>
          )}
          {groups.map(([name, rows]) => (
            <section key={name ?? ""} className="flex flex-col gap-2">
              {!project && (
                <h2 className="label-caps flex items-baseline gap-2">
                  <span className="font-mono normal-case">{name ?? "fără proiect"}</span>
                  <span className="font-mono tabular-nums">{rows.length}</span>
                </h2>
              )}
              {rows.map((d) => <DecisionCard key={d.id} decision={d} showProject={false} />)}
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
