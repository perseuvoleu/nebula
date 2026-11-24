import type { ReactNode } from "react";
import { GitBranchIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { statusLabel } from "@/lib/status";
import { type BoardCard, type Dot, type Stage, DOT_LABEL, STAGE_LABEL, cardDot, projectName, sessionDot } from "./model";

const DOT_CLASS: Record<Dot, string> = {
  needs: "bg-foreground ring-2 ring-foreground/30",
  running: "bg-working",
  done: "bg-muted-foreground",
  queued: "border border-working",
  off: "border border-muted-foreground/60",
};

export function StatusDot({ dot, label = DOT_LABEL[dot] }: { dot: Dot; label?: string }) {
  return <span role="img" aria-label={label} title={label} className={cn("inline-block size-2 shrink-0 rounded-full", DOT_CLASS[dot])} />;
}

export function CardRef({ cardRef }: { cardRef: string }) {
  const [number, host] = cardRef.split("@");
  return (
    <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">
      {number}{host && <span className="text-foreground/80">@{host}</span>}
    </span>
  );
}

export function StagePill({ stage }: { stage: Stage }) {
  return <span className="shrink-0 rounded-md bg-secondary px-1.5 py-0.5 text-xs font-medium">{STAGE_LABEL[stage]}</span>;
}

export function Tag({ children, className }: { children: ReactNode; className?: string }) {
  return <span className={cn("inline-flex max-w-40 shrink-0 items-center gap-1 truncate rounded-md border px-1.5 py-0.5 text-xs text-muted-foreground", className)}>{children}</span>;
}

export function CardTile({ card, selected = false, showProject = false, onOpen, actions }: {
  card: BoardCard;
  selected?: boolean;
  showProject?: boolean;
  onOpen?: (ref: string) => void;
  actions?: ReactNode;
}) {
  const blocked = card.blockers.length > 0;
  return (
    <article className={cn(
      "relative flex flex-col gap-1.5 rounded-lg border bg-card p-2.5 text-card-foreground transition-colors hover:bg-secondary/60 has-[button:focus-visible]:ring-2 has-[button:focus-visible]:ring-ring",
      selected && "border-ring bg-secondary",
      card.needsYou && "border-l-2 border-l-foreground",
      (blocked || card.proposed) && "border-dashed",
      blocked && "opacity-60",
    )}>
      <div className="flex min-w-0 items-center gap-1.5">
        <StatusDot dot={cardDot(card)} />
        <CardRef cardRef={card.ref} />
        {showProject && <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{projectName(card.project)}</span>}
        <span className="flex-1" />
        <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{card.age}</span>
      </div>
      <h3 className="text-sm leading-snug font-medium break-words">
        <button type="button" onClick={() => onOpen?.(card.ref)} aria-current={selected ? "true" : undefined}
          className="text-left outline-none after:absolute after:inset-0 after:rounded-lg">
          {card.title}
        </button>
      </h3>
      {(card.proposed || blocked || card.checkout || card.artifacts > 0) && (
        <div className="flex flex-wrap items-center gap-1">
          {card.proposed && <Tag className="border-dashed text-foreground">propus de {card.proposedBy ?? "agent"}</Tag>}
          {card.blockers.map((b) => <Tag key={b.ref} className="text-foreground"><span title={b.title}>blocat de {b.ref}</span></Tag>)}
          {card.checkout && <Tag className="font-mono"><GitBranchIcon aria-hidden className="size-3 shrink-0" /><span className="truncate">{card.checkout.branch}</span></Tag>}
          {card.artifacts > 0 && <Tag><span className="tabular-nums">{card.artifacts}</span> artefacte</Tag>}
        </div>
      )}
      <div className="flex flex-wrap items-center gap-1.5 border-t pt-1.5 text-xs text-muted-foreground">
        <span className="flex items-center gap-1">
          {card.sessions.length
            ? card.sessions.map((s) => <StatusDot key={s.id} dot={sessionDot(s.status)} label={`${s.name}: ${statusLabel(s.status)}`} />)
            : <span aria-label="Nicio sesiune">—</span>}
        </span>
        <span className="flex-1" />
        {actions && <span className="relative z-10 flex items-center gap-1">{actions}</span>}
      </div>
    </article>
  );
}
