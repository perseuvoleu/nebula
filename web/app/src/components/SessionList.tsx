import { useMemo, useState } from "react";
import { PlusIcon, SearchIcon } from "lucide-react";
import type { SessionRow } from "@/lib/types";
import { activityAge } from "@/lib/activity-age";
import { sessionKey } from "@/lib/session-list";
import { kindLabel, statusLabel } from "@/lib/status";
import { Button } from "@/components/ui/button";

import { DeleteSessionButton } from "./DeleteSessionButton";

export type ListPhase = "loading" | "ready" | "error" | "offline";

export function SessionList({ sessions, phase, error, selected, unseen, onSelect, onNew, project, showProject, onDeleted }: {
  sessions: SessionRow[];
  phase: ListPhase;
  error: string | null;
  selected: string | null;
  unseen: Set<string>;
  onSelect: (row: SessionRow) => void;
  onNew: () => void;
  project: string;
  /** Show the real project for rows in the aggregate view. */
  showProject: boolean;
  onDeleted: (row: SessionRow) => void;
}) {
  const [filter, setFilter] = useState("");
  // Existing list refreshes advance relative labels without additional requests or per-row timers.
  const now = Date.now();
  const visible = useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const rows = needle ? sessions.filter((r) => `${r.project} ${r.name} ${r.kind} ${r.worktree} ${statusLabel(r.status)}`.toLowerCase().includes(needle)) : sessions;
    return rows.slice(0, 200);
  }, [sessions, filter]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 px-3 pt-3 pb-2">
        <label className="relative flex-1">
          <span className="sr-only">Caută sesiuni</span>
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <input value={filter} onChange={(e) => setFilter(e.target.value)} type="search" placeholder="Caută sesiuni"
            className="h-10 w-full field pl-9 pr-3 text-sm" />
        </label>
        <Button type="button" variant="outline" size="icon-lg" aria-label="Agent nou" onClick={onNew} className="size-10">
          <PlusIcon aria-hidden />
        </Button>
      </div>
      <nav aria-label={`Sesiuni în ${project}`} className="min-h-0 flex-1 overflow-y-auto px-2 pb-3" aria-busy={phase === "loading"}>
        {phase === "loading" && sessions.length === 0 && (
          <ul className="space-y-1 px-1" aria-hidden>
            {[0, 1, 2].map((i) => <li key={i} className="h-14 animate-pulse rounded-lg bg-muted/60" />)}
          </ul>
        )}
        {error && phase !== "offline" && <p role="alert" className="px-2 py-3 text-sm text-muted-foreground">Nu s-au putut încărca sesiunile: {error}</p>}
        {phase === "error" && sessions.length > 0 && <p role="status" className="px-2 py-3 text-sm text-muted-foreground">Lista poate fi veche; se reîncearcă.</p>}
        {phase === "offline" && <p role="status" className="px-2 py-3 text-sm text-muted-foreground">Fără conexiune. Lista poate fi veche; agenții continuă pe server.</p>}
        {phase === "ready" && !error && sessions.length === 0 && (
          <div className="px-2 py-6 text-sm text-muted-foreground">
            <p>Nicio sesiune activă în acest proiect.</p>
            <Button type="button" variant="outline" size="sm" className="mt-3" onClick={onNew}>Pornește un agent</Button>
          </div>
        )}
        {phase === "ready" && sessions.length > 0 && visible.length === 0 && <p className="px-2 py-3 text-sm text-muted-foreground">Nicio sesiune nu se potrivește.</p>}
        <ul className="space-y-0.5">
          {visible.map((row) => {
            const active = sessionKey(row) === selected;
            const isUnseen = unseen.has(sessionKey(row));
            const age = activityAge(row.status_changed_at, now);
            return (
              <li key={sessionKey(row)} className="flex">
                <button type="button" onClick={() => onSelect(row)} aria-current={active ? "page" : undefined}
                  aria-description={`Ultima schimbare de stare: ${age}`}
                  aria-label={isUnseen ? `${showProject ? `${row.project}: ` : ""}${row.name}, finalizată și nevizualizată` : undefined}
                  className={`flex min-h-14 min-w-0 flex-1 flex-col gap-0.5 rounded-lg px-3 py-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${active ? "bg-secondary" : "hover:bg-muted/60"}`}>
                  <span className="flex items-center gap-2">
                    <span className={`min-w-0 flex-1 truncate text-[15px] font-medium ${isUnseen ? "text-unseen" : row.status === "running" ? "session-working" : ""}`}>{row.name}</span>
                    <span className={`shrink-0 text-[11px] ${row.status === "needs_feedback" ? "rounded border border-foreground px-1.5 py-px font-medium" : "text-muted-foreground"}`}>
                      {row.status === "running" && <span aria-hidden className="mr-1 inline-block size-1.5 animate-pulse rounded-full bg-foreground align-middle" />}
                      {statusLabel(row.status)}
                    </span>
                  </span>
                  <span className="flex min-w-0 items-center gap-1.5 text-xs text-muted-foreground">
                    {showProject && <><span className="truncate">{row.project}</span><span aria-hidden>·</span></>}
                    <span>{kindLabel(row.kind)}</span>
                    <span aria-hidden>·</span>
                    <span className="min-w-0 truncate font-mono">{row.worktree}</span>
                  </span>
                  <span className="text-xs text-muted-foreground" title="Ultima schimbare de stare">{age}</span>
                </button>
                <DeleteSessionButton project={row.project} row={row} onDeleted={() => onDeleted(row)} />
              </li>
            );
          })}
        </ul>
      </nav>
    </div>
  );
}
