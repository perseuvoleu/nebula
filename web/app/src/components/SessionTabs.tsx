import { useEffect, useRef } from "react";
import { sessionKey } from "@/lib/session-list";
import type { SessionRow } from "@/lib/types";
import { sessionDot } from "@/lib/status";
import { StatusDot } from "@/components/ui/status-dot";

export function SessionTabs({ sessions, selected, unseen, onSelect, showProjects }: {
  /** Include source project names when navigating the aggregate view. */
  showProjects: boolean;
  sessions: SessionRow[]; selected: string; unseen: Set<string>; onSelect: (row: SessionRow) => void;
}) {
  const active = useRef<HTMLButtonElement>(null);
  useEffect(() => { active.current?.scrollIntoView({ block: "nearest", inline: "nearest" }); }, [selected]);
  return (
    <div role="tablist" aria-label="Schimbă sesiunea" className="flex min-h-11 min-w-0 shrink-0 items-center gap-1 overflow-x-auto border-b bg-bg0 px-2 py-1">
      {sessions.map((row, index) => <button key={sessionKey(row)} type="button" role="tab" ref={sessionKey(row) === selected ? active : undefined}
        aria-selected={sessionKey(row) === selected} aria-controls="conversation-panel" tabIndex={sessionKey(row) === selected ? 0 : -1}
        aria-label={unseen.has(sessionKey(row)) ? `${showProjects ? `${row.project}: ` : ""}${row.name}, finalizată și nevizualizată` : `${showProjects ? `${row.project}: ` : ""}${row.name}`}
        onClick={() => onSelect(row)} title={`${row.name} · ${row.worktree}`}
        onKeyDown={event => {
          if (!["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) return;
          event.preventDefault();
          const next = event.key === "Home" ? 0 : event.key === "End" ? sessions.length - 1
            : (index + (event.key === "ArrowRight" ? 1 : -1) + sessions.length) % sessions.length;
          onSelect(sessions[next]);
          requestAnimationFrame(() => document.querySelector<HTMLButtonElement>('[role="tab"][aria-selected="true"]')?.focus());
        }}
        className={`flex max-w-52 shrink-0 items-center gap-1.5 truncate rounded-sm border px-2.5 py-1.5 text-meta ${sessionKey(row) === selected ? "border-primary bg-sel2 font-semibold" : "bg-bg2 hover:border-edge-hover"} ${unseen.has(sessionKey(row)) ? "text-unseen" : "text-foreground"}`}>
        <StatusDot dot={sessionDot(row.status)} label={null} /><span className={`truncate ${!unseen.has(sessionKey(row)) && row.status === "running" ? "session-working" : ""}`}>{showProjects ? `${row.project}: ` : ""}{row.name}</span>
      </button>)}
    </div>
  );
}
