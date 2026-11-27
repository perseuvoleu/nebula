import { type ReactNode, useId } from "react";
import type { BoardCard } from "@/lib/board";
import { StatusDot } from "@/components/ui/status-dot";
import { CardRef } from "./CardTile";

export function NeedsYouStrip({ cards, onOpen, renderActions }: {
  cards: BoardCard[];
  onOpen?: (ref: string) => void;
  renderActions?: (card: BoardCard) => ReactNode;
}) {
  const titleId = useId();
  if (cards.length === 0) return null;
  return (
    <section aria-labelledby={titleId} data-open-scope="strip" className="flex shrink-0 items-center gap-2 overflow-x-auto border-b bg-bg1 px-3.5 py-[5px]">
      <h2 id={titleId} className="shrink-0 text-[11px] font-bold tracking-[0.05em] text-err uppercase">
        Te așteaptă <span className="font-mono tabular-nums">({cards.length})</span>
      </h2>
      <ul className="flex items-center gap-2">
        {cards.map((card) => (
          <li key={card.ref} className="flex shrink-0 items-center gap-[5px] rounded-sm border border-line-err bg-tint-need py-[3px] pr-1 pl-[7px] text-meta hover:border-err">
            <StatusDot dot="needs" />
            <CardRef cardRef={card.ref} />
            <button type="button" onClick={() => onOpen?.(card.ref)}
              className="max-w-48 truncate rounded-sm text-left outline-none hover:underline focus-visible:ring-2 focus-visible:ring-ring">
              {card.title}
            </button>
            {renderActions?.(card)}
          </li>
        ))}
      </ul>
    </section>
  );
}
