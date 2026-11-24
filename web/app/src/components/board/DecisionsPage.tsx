import { ArrowLeftIcon, SearchIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DecisionCard, type DecisionRow } from "./DecisionCard";

function byProject(decisions: DecisionRow[]): [string | null, DecisionRow[]][] {
  const groups = new Map<string | null, DecisionRow[]>();
  for (const d of decisions) groups.set(d.project, [...(groups.get(d.project) ?? []), d]);
  return [...groups];
}

export function DecisionsPage({ project, backHref, query, onQueryChange, decisions }: {
  project: string | null;
  backHref?: string;
  query: string;
  onQueryChange: (query: string) => void;
  decisions: DecisionRow[];
}) {
  const groups = byProject(decisions);
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <header className="flex flex-wrap items-center gap-2 border-b border-border px-4 py-2">
        {backHref && (
          <Button asChild variant="ghost" size="sm">
            <a href={backHref}><ArrowLeftIcon aria-hidden />{project ? <span className="font-mono">@{project}</span> : "Board"}</a>
          </Button>
        )}
        <h1 className="text-base font-medium">Decizii</h1>
        {project && !backHref && <span className="font-mono text-sm text-muted-foreground">@{project}</span>}
        <label className="relative ml-auto w-full sm:w-64">
          <span className="sr-only">Caută decizii</span>
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <input value={query} onChange={(e) => onQueryChange(e.target.value)} type="search" placeholder="Caută"
            className="h-9 w-full rounded-lg border border-input bg-card pr-3 pl-9 text-sm outline-none placeholder:text-muted-foreground focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60" />
        </label>
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
        <div className="mx-auto flex max-w-3xl flex-col gap-6">
          {groups.length === 0 && (
            <p className="py-8 text-center text-sm text-muted-foreground">
              {query.trim() ? "Nicio decizie nu se potrivește." : "Nicio decizie înregistrată încă."}
            </p>
          )}
          {groups.map(([name, rows]) => (
            <section key={name ?? ""} className="flex flex-col gap-2">
              {!project && (
                <h2 className="flex items-baseline gap-2 text-sm font-medium">
                  <span className="font-mono">{name ? `@${name}` : "fără proiect"}</span>
                  <span className="font-mono text-xs text-muted-foreground tabular-nums">{rows.length}</span>
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
