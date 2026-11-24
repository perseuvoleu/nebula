import type { ReactNode } from "react";
import { GitBranchIcon, PenLineIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { kindLabel, statusLabel } from "@/lib/status";
import { type BoardCard, type LogEntry, STAGE_LABEL, projectName, sessionDot } from "@/lib/board";
import { CardRef, StagePill, StatusDot, Tag } from "./CardTile";

function Section({ title, count, children }: { title: string; count?: number; children: ReactNode }) {
  return (
    <section className="flex min-w-0 flex-col gap-2">
      <h3 className="text-xs font-medium text-muted-foreground">
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
  onOpenCard?: (ref: string) => void;
  children?: ReactNode;
}) {
  return (
    <div className={cn("flex flex-col gap-4", mode === "page" && "mx-auto w-full max-w-3xl")}>
      <div className="flex flex-col gap-2">
        <div className="flex flex-wrap items-center gap-1.5">
          <CardRef cardRef={card.ref} />
          <StagePill stage={card.stage} />
          {card.needsYou && <Tag className="border-foreground font-medium text-foreground"><StatusDot dot="needs" label="" />Te așteaptă</Tag>}
          {card.proposed && <Tag className="border-dashed text-foreground">propus</Tag>}
          <span className="flex-1" />
          {actions}
        </div>
        <h2 className={cn("font-medium leading-tight text-balance", mode === "page" ? "text-xl" : "text-lg")}>{card.title}</h2>
      </div>

      {card.proposed && (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-dashed px-3 py-2 text-sm">
          <span className="flex-1">Propus de <span className="font-medium">{card.proposedBy ?? "agent"}</span></span>
          {proposalActions}
        </div>
      )}

      <dl className="grid grid-cols-[6rem_1fr] items-center gap-x-3 gap-y-1.5 rounded-lg border bg-background px-3 py-2.5 text-sm">
        <dt className="text-xs text-muted-foreground">Etapă</dt>
        <dd>{STAGE_LABEL[card.stage]}</dd>
        <dt className="text-xs text-muted-foreground">Proiect</dt>
        <dd className="truncate font-mono text-xs">{projectName(card.project)}</dd>
        <dt className="text-xs text-muted-foreground">Checkout</dt>
        <dd className="flex min-w-0 flex-wrap items-center gap-1">
          {card.checkout
            ? <><Tag className="font-mono"><GitBranchIcon aria-hidden className="size-3 shrink-0" /><span className="truncate">{card.checkout.branch}</span></Tag><span className="font-mono text-xs text-muted-foreground">@{card.checkout.host}</span></>
            : <span className="text-muted-foreground">Niciunul</span>}
        </dd>
        <dt className="text-xs text-muted-foreground">Blocat de</dt>
        <dd className="flex min-w-0 flex-wrap items-center gap-1">
          {card.blockers.length
            ? card.blockers.map((b) => (
              <button key={b.ref} type="button" onClick={() => onOpenCard?.(b.ref)}
                className="inline-flex max-w-56 items-center gap-1 truncate rounded-md border border-dashed px-1.5 py-0.5 text-xs outline-none hover:bg-secondary focus-visible:ring-2 focus-visible:ring-ring">
                <span className="font-mono">{b.ref}</span><span className="truncate text-muted-foreground">{b.title}</span>
              </button>
            ))
            : <span className="text-muted-foreground">Nimic</span>}
        </dd>
      </dl>

      {children}

      <div className={cn("flex flex-col gap-4", mode === "page" && "lg:grid lg:grid-cols-2")}>
        <Section title="Sesiuni" count={card.sessions.length}>
          {card.sessions.length
            ? (
              <ol aria-label="Sesiuni" className="flex flex-col rounded-lg border bg-background">
                {card.sessions.map((s) => (
                  <li key={s.id} className="flex min-w-0 items-center gap-2 border-b px-3 py-2 last:border-b-0">
                    <StatusDot dot={sessionDot(s.status)} label={statusLabel(s.status)} />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center gap-1.5">
                        <span className="truncate text-sm font-medium">{s.name}</span>
                        {s.writer && <PenLineIcon role="img" aria-label="Scrie în checkout" className="size-3 shrink-0 text-working" />}
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        {s.role} · {kindLabel(s.kind)} · <span className="font-mono">@{s.host}</span>
                      </span>
                    </span>
                    <span className="shrink-0 text-xs text-muted-foreground">{statusLabel(s.status)}</span>
                  </li>
                ))}
              </ol>
            )
            : <p className="rounded-lg border bg-background px-3 py-2.5 text-sm text-muted-foreground">Nicio sesiune încă.</p>}
        </Section>
        <Section title="Jurnal" count={log.length}>
          {log.length
            ? (
              <ul className="flex flex-col gap-1">
                {log.map((entry) => (
                  <li key={entry.id} className="rounded-r-md border-l-2 bg-background px-2.5 py-1.5 text-sm">
                    <span className="font-medium text-muted-foreground">{entry.author}:</span> {entry.text}
                    <span className="ml-1.5 font-mono text-xs text-muted-foreground tabular-nums">{entry.age}</span>
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
