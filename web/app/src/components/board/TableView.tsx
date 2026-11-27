import type { ReactNode } from "react";
import { cn } from "@/lib/utils";
import { statusLabel } from "@/lib/status";
import { type BoardCard, type SortKey, type TableSort, cardDot, projectName, sessionDot } from "@/lib/board";
import { Pill } from "@/components/ui/pill";
import { BranchTag, CardRef, ProjectTag, StagePill, StatusDot } from "./CardTile";

const Empty = () => <span className="text-dim">—</span>;

function Row({ card, selected, onOpen, hasActions, actions }: {
  card: BoardCard; selected: boolean; onOpen?: (ref: string) => void; hasActions: boolean; actions?: ReactNode;
}) {
  const last = card.sessions?.at(-1);
  const blocked = card.blockers.length > 0;
  const dot = cardDot(card);
  return (
    <tr className={cn("border-b border-hair hover:bg-bg1", selected && "bg-sel2 hover:bg-sel2", blocked && "opacity-68")}>
      <td className="px-2 py-1.5"><CardRef cardRef={card.ref} /></td>
      <td className="w-full px-2 py-1.5">
        <span className="flex min-w-48 items-center gap-1.5">
          {dot && <StatusDot dot={dot} />}
          <button type="button" onClick={() => onOpen?.(card.ref)} aria-current={selected ? "true" : undefined}
            className="min-w-0 truncate rounded-sm text-left font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring">
            {card.title}
          </button>
          {blocked && <Pill tone="blocked" className="font-mono">{card.blockers[0].ref}</Pill>}
        </span>
      </td>
      <td className="px-2 py-1.5"><StagePill stage={card.stage} /></td>
      <td className="hidden max-w-36 px-2 py-1.5 md:table-cell"><ProjectTag project={card.project} /></td>
      <td className="hidden max-w-40 px-2 py-1.5 lg:table-cell">{card.checkout ? <BranchTag branch={card.checkout.branch} /> : <Empty />}</td>
      <td className="hidden max-w-36 px-2 py-1.5 lg:table-cell">
        {last
          ? <span className="flex min-w-0 items-center gap-1.5"><StatusDot dot={sessionDot(last.status)} label={statusLabel(last.status)} /><span className="truncate">{last.name}</span></span>
          : <Empty />}
      </td>
      <td className="hidden px-2 py-1.5 font-mono text-meta tabular-nums lg:table-cell">{card.artifacts || <Empty />}</td>
      {hasActions && <td className="px-2 py-1.5"><span className="flex items-center gap-1">{actions}</span></td>}
    </tr>
  );
}

function SortHead({ label, column, sort, onSort, className }: {
  label: string; column: SortKey; sort?: TableSort; onSort?: (key: SortKey) => void; className?: string;
}) {
  const active = sort?.key === column;
  return (
    <th scope="col" aria-sort={active ? (sort.asc ? "ascending" : "descending") : undefined} className={className}>
      {onSort
        ? (
          <button type="button" onClick={() => onSort(column)} className="inline-flex items-center gap-1 rounded-sm outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring">
            {label}{active && <span aria-hidden>{sort.asc ? "↑" : "↓"}</span>}
          </button>
        )
        : label}
    </th>
  );
}

function GroupLabel({ name, count }: { name: string; count: number }) {
  return <>{name} <span className="text-dim tabular-nums">{count}</span></>;
}

export function TableView({ cards, groupByProject = false, selected, onOpen, renderActions, sort, onSort, collapsed = [], onToggleGroup }: {
  cards: BoardCard[];
  groupByProject?: boolean;
  selected?: string | null;
  onOpen?: (ref: string) => void;
  renderActions?: (card: BoardCard) => ReactNode;
  sort?: TableSort;
  onSort?: (key: SortKey) => void;
  collapsed?: string[];
  onToggleGroup?: (project: string) => void;
}) {
  const columns = renderActions ? 8 : 7;
  const groups = groupByProject
    ? [...new Set(cards.map((c) => projectName(c.project)))].map((name) => ({ name, cards: cards.filter((c) => projectName(c.project) === name) }))
    : [{ name: null, cards }];
  const head = "label-caps sticky top-0 z-10 bg-bg0 px-2 py-2 text-left font-semibold tracking-[0.04em] hover:text-foreground";
  return (
    <div className="min-h-0 flex-1 overflow-auto px-3.5 pb-3.5">
      <table className="w-full border-collapse text-xs whitespace-nowrap">
        <thead>
          <tr className="border-b">
            <SortHead label="ID" column="ref" sort={sort} onSort={onSort} className={head} />
            <SortHead label="Titlu" column="title" sort={sort} onSort={onSort} className={head} />
            <SortHead label="Etapă" column="stage" sort={sort} onSort={onSort} className={head} />
            <SortHead label="Proiect" column="project" sort={sort} onSort={onSort} className={cn(head, "hidden md:table-cell")} />
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Checkout</th>
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Sesiune</th>
            <th scope="col" className={cn(head, "hidden lg:table-cell")}>Artefacte</th>
            {renderActions && <th scope="col" className={head}><span className="sr-only">Acțiuni</span></th>}
          </tr>
        </thead>
        {groups.map((group) => (
          <tbody key={group.name ?? ""}>
            {group.name !== null && (
              <tr className="border-y bg-bg1">
                <th scope="colgroup" colSpan={columns} className="px-2 py-1.5 text-left font-mono text-xs font-semibold">
                  {onToggleGroup
                    ? (
                      <button type="button" aria-expanded={!collapsed.includes(group.name)} onClick={() => onToggleGroup(group.name!)}
                        className="inline-flex items-center gap-1.5 rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring">
                        <span aria-hidden>{collapsed.includes(group.name) ? "▸" : "▾"}</span>
                        <GroupLabel name={group.name} count={group.cards.length} />
                      </button>
                    )
                    : <GroupLabel name={group.name} count={group.cards.length} />}
                </th>
              </tr>
            )}
            {!(group.name !== null && collapsed.includes(group.name)) && group.cards.map((card) => (
              <Row key={card.ref} card={card} selected={card.ref === selected} onOpen={onOpen} hasActions={!!renderActions} actions={renderActions?.(card)} />
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
