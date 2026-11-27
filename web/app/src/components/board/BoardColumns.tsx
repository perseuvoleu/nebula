import type { ReactNode } from "react";
import { PlusIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { type BoardCard, type Stage, STAGES, STAGE_LABEL } from "@/lib/board";
import { StagePill } from "@/components/ui/tags";
import { CardTile } from "./CardTile";

const DRAGGED = "application/x-nebula-card";

export function BoardColumns({ columns, selected, showProject = false, onOpen, onAdd, onMove, renderActions, waiting }: {
  columns: Record<Stage, BoardCard[]>;
  selected?: string | null;
  showProject?: boolean;
  onOpen?: (ref: string) => void;
  onAdd?: (stage: Stage) => void;
  onMove?: (ref: string, stage: Stage) => void;
  renderActions?: (card: BoardCard) => ReactNode;
  waiting?: (card: BoardCard) => string[];
}) {
  return (
    <div className="flex min-h-0 flex-1 snap-x snap-mandatory gap-[9px] overflow-x-auto px-3 py-2.5">
      {STAGES.map((stage) => {
        const cards = columns[stage];
        return (
          <section key={stage} aria-label={STAGE_LABEL[stage]}
            onDragOver={(e) => { if (onMove && e.dataTransfer.types.includes(DRAGGED)) e.preventDefault(); }}
            onDrop={(e) => {
              const ref = e.dataTransfer.getData(DRAGGED);
              if (!onMove || !ref) return;
              e.preventDefault();
              onMove(ref, stage);
            }}
            className="flex min-h-0 w-72 shrink-0 snap-start flex-col rounded-lg border bg-bg1 md:w-auto md:max-w-[340px] md:min-w-[196px] md:flex-1 md:shrink">
            <header className="flex shrink-0 items-center gap-1.5 border-b border-hair px-2.5 pt-2 pb-[7px]">
              <StagePill stage={stage} />
              <span className="font-mono text-[11px] text-muted-foreground tabular-nums">{cards.length}</span>
              <span className="flex-1" />
              {onAdd && (
                <Button type="button" variant="ghost" size="icon-xs" className="text-dim" aria-label={`Card nou în ${STAGE_LABEL[stage]}`} onClick={() => onAdd(stage)}>
                  <PlusIcon aria-hidden />
                </Button>
              )}
            </header>
            <div className="flex min-h-0 flex-1 flex-col gap-[7px] overflow-y-auto p-[7px]">
              {cards.length
                ? cards.map((card) => (
                  <div key={card.ref} draggable={!!onMove} onDragStart={(e) => e.dataTransfer.setData(DRAGGED, card.ref)}>
                    <CardTile card={card} selected={card.ref === selected} showProject={showProject} waiting={waiting?.(card)}
                      onOpen={onOpen} actions={renderActions?.(card)} />
                  </div>
                ))
                : <p className="p-3 text-center text-xs text-muted-foreground">Gol</p>}
            </div>
          </section>
        );
      })}
    </div>
  );
}
