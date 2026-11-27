import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import { statusLabel } from "@/lib/status";
import { type BoardCard, type Stage, STAGE_LABEL, cardDot, projectName, sessionDot } from "@/lib/board";
import { Pill } from "@/components/ui/pill";
import { StatusDot } from "@/components/ui/status-dot";

export { StatusDot };

export function CardRef({ cardRef }: { cardRef: string }) {
  const [number, host] = cardRef.split("@");
  return (
    <span className="shrink-0 font-mono text-meta text-dim tabular-nums">
      {number}{host && <span className="text-remote">@{host}</span>}
    </span>
  );
}

export function StagePill({ stage }: { stage: Stage }) {
  return <Pill tone={stage}>{STAGE_LABEL[stage]}</Pill>;
}

export function ProjectTag({ project }: { project: string | null }) {
  return <Pill tone="project" className="max-w-[86px] truncate">{projectName(project)}</Pill>;
}

export function BranchTag({ branch }: { branch: string }) {
  return <Pill tone="branch" className="max-w-[124px] truncate" title={branch}>{branch}</Pill>;
}

export function CardTile({ card, selected = false, showProject = false, onOpen, actions }: {
  card: BoardCard;
  selected?: boolean;
  showProject?: boolean;
  onOpen?: (ref: string) => void;
  actions?: ReactNode;
}) {
  const blocked = card.blockers.length > 0;
  const dot = card.needsYou ? "needs" : cardDot(card);
  return (
    <article className={cn(
      "relative flex flex-col gap-[5px] rounded-md border bg-bg2 px-[9px] py-2 text-card-foreground transition-[border-color,background-color,opacity] hover:border-edge-hover hover:bg-bg2-hover has-[button:focus-visible]:ring-2 has-[button:focus-visible]:ring-ring",
      selected && "border-primary bg-sel2 hover:border-primary hover:bg-sel2",
      card.needsYou && "border-l-[3px] border-l-err hover:border-l-err",
      blocked && "border-dashed opacity-66",
      card.proposed && "border-dashed border-line-warn",
    )}>
      <div className="flex min-w-0 items-center gap-1">
        <StatusDot dot={dot ?? "off"} label={dot ? undefined : null} />
        <CardRef cardRef={card.ref} />
        {showProject && <ProjectTag project={card.project} />}
        <span className="ml-auto shrink-0 pl-1 font-mono text-[10.5px] text-dim tabular-nums">{card.age}</span>
      </div>
      <h3 className="text-ui leading-[1.32] font-semibold break-words">
        <button type="button" onClick={() => onOpen?.(card.ref)} aria-current={selected ? "true" : undefined}
          className="text-left outline-none after:absolute after:inset-0 after:rounded-md">
          {card.title}
        </button>
      </h3>
      {(card.proposed || blocked || card.checkout || card.artifacts > 0) && (
        <div className="flex flex-wrap items-center gap-1 text-[11px] text-muted-foreground">
          {card.proposed && <Pill tone="proposed">propus de {card.proposedBy ?? "agent"}</Pill>}
          {card.blockers.map((b) => <Pill key={b.ref} tone="blocked" className="relative z-10" title={b.title}>blocat de {b.ref}</Pill>)}
          {card.checkout && <BranchTag branch={card.checkout.branch} />}
          {card.artifacts > 0 && <Pill tone="artifact"><span className="tabular-nums">{card.artifacts}</span> artefacte</Pill>}
        </div>
      )}
      <div className="flex items-center gap-[5px] border-t border-hair pt-1 text-[11px] text-dim">
        <span className="flex items-center gap-[3px]">
          {card.sessions?.length
            ? card.sessions.map((s) => <StatusDot key={s.id} dot={sessionDot(s.status)} label={`${s.name}: ${statusLabel(s.status)}`} />)
            : <span aria-label="Nicio sesiune">—</span>}
        </span>
        <span className="flex-1" />
        {actions && <span className="relative z-10 flex flex-wrap items-center justify-end gap-1">{actions}</span>}
      </div>
    </article>
  );
}
