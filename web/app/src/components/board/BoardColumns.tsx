import type { ReactNode } from "react";
import { PlusIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { type BoardCard, type Stage, STAGES, STAGE_LABEL } from "./model";
import { CardTile, StagePill } from "./CardTile";

export function BoardColumns({ columns, selected, showProject = false, onOpen, onAdd, renderActions }: {
  columns: Record<Stage, BoardCard[]>;
  selected?: string | null;
  showProject?: boolean;
  onOpen?: (ref: string) => void;
  onAdd?: (stage: Stage) => void;
  renderActions?: (card: BoardCard) => ReactNode;
}) {
  return (
    <div className="flex min-h-0 flex-1 snap-x snap-mandatory gap-2 overflow-x-auto p-3">
      {STAGES.map((stage) => {
        const cards = columns[stage];
        return (
          <section key={stage} aria-label={STAGE_LABEL[stage]}
            className="flex min-h-0 w-72 shrink-0 snap-start flex-col rounded-xl border bg-sidebar md:w-auto md:max-w-84 md:min-w-52 md:flex-1 md:shrink">
            <header className="flex shrink-0 items-center gap-1.5 border-b px-2.5 py-2">
              <StagePill stage={stage} />
              <span className="font-mono text-xs text-muted-foreground tabular-nums">{cards.length}</span>
              <span className="flex-1" />
              {onAdd && (
                <Button type="button" variant="ghost" size="icon-xs" aria-label={`Card nou în ${STAGE_LABEL[stage]}`} onClick={() => onAdd(stage)}>
                  <PlusIcon aria-hidden />
                </Button>
              )}
            </header>
            <div className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto p-2">
              {cards.length
                ? cards.map((card) => (
                  <CardTile key={card.ref} card={card} selected={card.ref === selected} showProject={showProject}
                    onOpen={onOpen} actions={renderActions?.(card)} />
                ))
                : <p className="p-3 text-center text-xs text-muted-foreground">Gol</p>}
            </div>
          </section>
        );
      })}
    </div>
  );
}
