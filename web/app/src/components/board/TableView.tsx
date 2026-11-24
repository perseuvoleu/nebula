import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import { statusLabel } from "@/lib/status";
import { type BoardCard, cardDot, projectName, sessionDot } from "@/lib/board";
import { CardRef, StagePill, StatusDot } from "./CardTile";

const Empty = () => <span className="text-muted-foreground">—</span>;

function Row({ card, selected, onOpen, actions }: {
  card: BoardCard; selected: boolean; onOpen?: (ref: string) => void; actions?: ReactNode;
}) {
  const last = card.sessions.at(-1);
  const blocked = card.blockers.length > 0;
  return (
    <tr className={cn("border-b hover:bg-card", selected && "bg-secondary", blocked && "opacity-60")}>
      <td className="px-2 py-1.5"><CardRef cardRef={card.ref} /></td>
      <td className="w-full px-2 py-1.5">
        <span className="flex min-w-48 items-center gap-1.5">
          <StatusDot dot={cardDot(card)} />
          <button type="button" onClick={() => onOpen?.(card.ref)} aria-current={selected ? "true" : undefined}
            className="min-w-0 truncate rounded-sm text-left font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring">
            {card.title}
          </button>
          {blocked && <span className="shrink-0 rounded-md border border-dashed px-1.5 py-0.5 font-mono text-xs">{card.blockers[0].ref}</span>}
        </span>
      </td>
      <td className="px-2 py-1.5"><StagePill stage={card.stage} /></td>
      <td className="hidden max-w-36 truncate px-2 py-1.5 font-mono text-xs text-muted-foreground md:table-cell">{projectName(card.project)}</td>
      <td className="hidden max-w-40 truncate px-2 py-1.5 font-mono text-xs lg:table-cell">{card.checkout ? card.checkout.branch : <Empty />}</td>
      <td className="hidden max-w-36 px-2 py-1.5 lg:table-cell">
        {last
          ? <span className="flex min-w-0 items-center gap-1.5"><StatusDot dot={sessionDot(last.status)} label={statusLabel(last.status)} /><span className="truncate">{last.name}</span></span>
          : <Empty />}
      </td>
      <td className="hidden px-2 py-1.5 font-mono text-xs tabular-nums lg:table-cell">{card.artifacts || <Empty />}</td>
      {actions !== undefined && <td className="px-2 py-1.5"><span className="flex items-center gap-1">{actions}</span></td>}
    </tr>
  );
}

export function TableView({ cards, groupByProject = false, selected, onOpen, renderActions }: {
  cards: BoardCard[];
  groupByProject?: boolean;
  selected?: string | null;
  onOpen?: (ref: string) => void;
  renderActions?: (card: BoardCard) => ReactNode;
}) {
  const columns = renderActions ? 8 : 7;
  const groups = groupByProject
    ? [...new Set(cards.map((c) => projectName(c.project)))].map((name) => ({ name, cards: cards.filter((c) => projectName(c.project) === name) }))
    : [{ name: null, cards }];
  const head = "sticky top-0 z-10 bg-background px-2 py-2 text-left text-xs font-medium text-muted-foreground";
  return (
    <div className="min-h-0 flex-1 overflow-auto px-3 pb-3">
      <table className="w-full border-collapse text-sm whitespace-nowrap">
        <thead>
          <tr className="border-b">
            <th scope="col" className={head}>ID</th>
            <th scope="col" className={head}>Titlu</th>
            <th scope="col" className={head}>Etapă</th>
            <th scope="col" className={cn(head, "hidden md:table-cell")}>Proiect</th>
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Checkout</th>
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Sesiune</th>
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Artefacte</th>
            {renderActions && <th scope="col" className={head}><span className="sr-only">Acțiuni</span></th>}
          </tr>
        </thead>
        {groups.map((group) => (
          <tbody key={group.name ?? ""}>
            {group.name !== null && (
              <tr className="border-y bg-card">
                <th scope="colgroup" colSpan={columns} className="px-2 py-1.5 text-left font-mono text-xs font-medium">
                  {group.name} <span className="text-muted-foreground tabular-nums">{group.cards.length}</span>
                </th>
              </tr>
            )}
            {group.cards.map((card) => (
              <Row key={card.ref} card={card} selected={card.ref === selected} onOpen={onOpen} actions={renderActions?.(card)} />
            ))}
          </tbody>
        ))}
        {cards.length === 0 && (
          <tbody><tr><td colSpan={columns} className="p-6 text-center text-sm text-muted-foreground">Niciun card nu se potrivește.</td></tr></tbody>
        )}
      </table>
    </div>
  );
}
