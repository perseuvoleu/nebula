import { useMemo, useState } from "react";
import { PlusIcon, SearchIcon } from "lucide-react";
import type { SessionRow } from "@/lib/types";
import { activityAge } from "@/lib/activity-age";
import { sessionKey } from "@/lib/session-list";
import { kindLabel, sessionDot, statusLabel } from "@/lib/status";
import { Pill, type PillTone } from "@/components/ui/pill";
import { StatusDot } from "@/components/ui/status-dot";
import { BranchTag, ProjectTag } from "@/components/ui/tags";
import { Button } from "@/components/ui/button";

import { DeleteSessionButton } from "./DeleteSessionButton";

const STATUS_TONE: Record<string, PillTone> = { running: "doing", needs_feedback: "needs", finished: "done", fresh: "next" };

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
          <SearchIcon aria-hidden className="pointer-events-none absolute top-1/2 left-3 size-3.5 -translate-y-1/2 text-dim" />
          <input value={filter} onChange={(e) => setFilter(e.target.value)} type="search" placeholder="Caută sesiuni"
            className="field h-10 w-full bg-bg1 pr-3 pl-8 text-sm" />
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
        <ul className="space-y-1">
          {visible.map((row) => {
            const active = sessionKey(row) === selected;
            const isUnseen = unseen.has(sessionKey(row));
            const age = activityAge(row.status_changed_at, now);
            return (
              <li key={sessionKey(row)} className="flex">
                <button type="button" onClick={() => onSelect(row)} aria-current={active ? "page" : undefined}
                  aria-description={`Ultima schimbare de stare: ${age}`}
                  aria-label={isUnseen ? `${showProject ? `${row.project}: ` : ""}${row.name}, finalizată și nevizualizată` : undefined}
                  className={`flex min-h-14 min-w-0 flex-1 flex-col gap-1 rounded-md border px-[9px] py-2 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${active ? "border-primary bg-sel2" : "border-transparent hover:border-edge hover:bg-bg2"} ${row.status === "needs_feedback" ? "border-l-[3px] border-l-err" : ""}`}>
                  <span className="flex items-center gap-1.5">
                    <StatusDot dot={sessionDot(row.status)} label={null} />
                    <span className={`min-w-0 flex-1 truncate text-[14px] font-semibold ${isUnseen ? "text-unseen" : row.status === "running" ? "session-working" : ""}`}>{row.name}</span>
                    <Pill tone={STATUS_TONE[row.status] ?? "neutral"}>{statusLabel(row.status)}</Pill>
                  </span>
                  <span className="flex min-w-0 items-center gap-1 text-[11px] text-muted-foreground">
                    {showProject && <ProjectTag project={row.project} />}
                    <span className="shrink-0">{kindLabel(row.kind)}</span>
                    <BranchTag branch={row.worktree} className="min-w-0 shrink" />
                    <span className="ml-auto shrink-0 pl-1 font-mono text-tag text-muted-foreground" title="Ultima schimbare de stare">{age}</span>
                  </span>
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
