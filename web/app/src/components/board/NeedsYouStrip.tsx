import type { ReactNode } from "react";
import type { BoardCard } from "./model";
import { CardRef, StatusDot } from "./CardTile";

export function NeedsYouStrip({ cards, onOpen, renderActions }: {
  cards: BoardCard[];
  onOpen?: (ref: string) => void;
  renderActions?: (card: BoardCard) => ReactNode;
}) {
  if (cards.length === 0) return null;
  return (
    <section aria-labelledby="needs-you-title" className="flex shrink-0 items-center gap-2 overflow-x-auto border-b bg-sidebar px-3 py-1.5">
      <h2 id="needs-you-title" className="shrink-0 text-xs font-medium">
        Te așteaptă <span className="font-mono text-muted-foreground tabular-nums">{cards.length}</span>
      </h2>
      <ul className="flex items-center gap-2">
        {cards.map((card) => (
          <li key={card.ref} className="flex shrink-0 items-center gap-1.5 rounded-md border border-foreground/40 py-1 pr-1 pl-2 text-xs">
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
