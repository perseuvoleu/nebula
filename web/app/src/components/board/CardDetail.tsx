import type { ReactNode } from "react";
import { PenLineIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { kindLabel, sessionDot, statusLabel } from "@/lib/status";
import type { BoardCard, LogEntry } from "@/lib/board";
import { StatusDot } from "@/components/ui/status-dot";
import { BranchTag, ProjectTag, StagePill } from "@/components/ui/tags";
import { Pill } from "@/components/ui/pill";
import { CardRef } from "./CardTile";

function Section({ title, count, children }: { title: string; count?: number; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-col gap-2">
      <h3 className="label-caps">
        {title}{count !== undefined && <span className="ml-1.5 font-mono tabular-nums">{count}</span>}
      </h3>
      {children}
    </section>
  );
}

export function CardDetail({ card, log, mode, actions, proposalActions, onOpenCard, children }: {
  card: BoardCard;
  log: LogEntry[];
  mode: "peek" | "page";
  actions?: ReactNode;
  proposalActions?: ReactNode;
  onOpenCard: (ref: string) => void;
  children?: ReactNode;
}) {
  const sessions = card.sessions ?? [];
  return (
    <div className={cn("flex flex-col gap-4", mode === "page" && "mx-auto w-full max-w-3xl")}>
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <CardRef cardRef={card.ref} />
          <StagePill stage={card.stage} />
          {card.needsYou && <Pill tone="needs"><StatusDot dot="needs" label={null} />Te așteaptă</Pill>}
          {card.proposed && <Pill tone="proposed">propus</Pill>}
          <span className="flex-1" />
          {actions}
        </div>
        <h2 className={cn("leading-tight font-bold text-balance", mode === "page" ? "text-xl" : "text-lg")}>{card.title}</h2>
      </div>

      {card.proposed && (
        <div className="flex flex-wrap items-center gap-2 rounded-md border border-dashed border-line-warn bg-tint-warn px-3 py-2 text-sm text-warn">
          <span className="flex-1">Propus de <span className="font-medium">{card.proposedBy ?? "agent"}</span></span>
          {proposalActions}
        </div>
      )}

      <dl className="grid grid-cols-[92px_1fr] items-center gap-x-2.5 gap-y-[5px] rounded-md border bg-bg0 px-[11px] py-[9px] text-xs">
        <dt className="text-meta text-muted-foreground">Etapă</dt>
        <dd><StagePill stage={card.stage} /></dd>
        <dt className="text-meta text-muted-foreground">Proiect</dt>
        <dd className="min-w-0"><ProjectTag project={card.project} /></dd>
        <dt className="text-meta text-muted-foreground">Checkout</dt>
        <dd className="flex min-w-0 flex-wrap items-center gap-1">
          {card.checkout
            ? <>
              <BranchTag branch={card.checkout.branch} />
              <Pill tone="remote">@{card.checkout.host}</Pill>
              {card.checkout.worktree && <Pill>worktree</Pill>}
              <span className="w-full truncate font-mono text-xs text-muted-foreground" title={card.checkout.path}>{card.checkout.path}</span>
            </>
            : <span className="text-muted-foreground">Niciunul</span>}
        </dd>
        <dt className="text-meta text-muted-foreground">Blocat de</dt>
        <dd className="flex min-w-0 flex-wrap items-center gap-1">
          {card.blockers.length
            ? card.blockers.map((b) => (
              <button key={b.ref} type="button" onClick={() => onOpenCard(b.ref)}
                className="inline-flex max-w-56 items-center gap-1 truncate rounded-full border border-line-err bg-tint-err px-[7px] py-[1.5px] text-tag text-err outline-none hover:border-err focus-visible:ring-2 focus-visible:ring-ring">
                <span className="font-mono">{b.ref}</span><span className="truncate">{b.title}</span>
              </button>
            ))
            : <span className="text-muted-foreground">Nimic</span>}
        </dd>
      </dl>

      {children}

      <div className={cn("flex flex-col gap-4", mode === "page" && "lg:grid lg:grid-cols-2")}>
        <Section title="Sesiuni" count={sessions.length}>
          {sessions.length
            ? (
              <ol aria-label="Sesiuni" className="flex flex-col rounded-md border bg-bg0">
                {sessions.map((s) => (
                  <li key={s.id} className="flex min-w-0 items-center gap-2 border-b border-hair px-3 py-2 last:border-b-0">
                    <StatusDot dot={sessionDot(s.status)} label={statusLabel(s.status)} />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate text-sm font-medium">{s.name}</span>
                        {s.writer && <PenLineIcon role="img" aria-label="Scrie în checkout" className="size-3 shrink-0 text-working" />}
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        {s.role} · {kindLabel(s.kind)} · <span className="font-mono text-remote">@{s.host}</span>
                      </span>
                    </span>
                    <span className="shrink-0 text-xs text-muted-foreground">{statusLabel(s.status)}</span>
                  </li>
                ))}
              </ol>
            )
            : <p className="rounded-md border bg-bg0 px-3 py-2.5 text-sm text-muted-foreground">Nicio sesiune încă.</p>}
        </Section>
        <Section title="Jurnal" count={log.length}>
          {log.length
            ? (
              <ul className="flex flex-col gap-1">
                {log.map((entry) => (
                  <li key={entry.id} className="rounded-r-sm border-l-2 border-l-edge bg-bg0 px-[9px] py-1.5 text-xs">
                    <span className="font-medium text-muted-foreground">{entry.author}:</span> {entry.text}
                    <span className="ml-1.5 font-mono text-tag text-muted-foreground tabular-nums">{entry.age}</span>
                  </li>
                ))}
              </ul>
            )
            : <p className="text-sm text-muted-foreground">Nicio notă.</p>}
        </Section>
      </div>
    </div>
  );
}
