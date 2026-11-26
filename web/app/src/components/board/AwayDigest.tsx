import { type ReactNode, useId, useState } from "react";
import { XIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { BoardCard } from "@/lib/board";
import { CardRef, StatusDot } from "./CardTile";

export type AwayDecision = { id: string; title: string; age: string; cardRef: string | null };

const CAP = 3;

function Column({ title, count, empty, children }: { title: ReactNode; count: number; empty: string; children: ReactNode[] }) {
  const [all, setAll] = useState(false);
  return (
    <section className="flex min-w-0 flex-col gap-1 rounded-lg border bg-card p-2.5">
      <h3 className="flex items-center gap-1.5 px-1 text-xs font-medium text-muted-foreground">
        {title} <span className="font-mono tabular-nums">{count}</span>
      </h3>
      {count ? <ul className="flex flex-col">{all ? children : children.slice(0, CAP)}</ul> : <p className="px-1 text-xs text-muted-foreground">{empty}</p>}
      {!all && children.length > CAP && (
        <Button type="button" variant="ghost" size="xs" className="self-start" onClick={() => setAll(true)}>Arată toate ({children.length})</Button>
      )}
    </section>
  );
}

function Row({ cardRef, title, trailing, onOpen }: { cardRef: string | null; title: string; trailing?: ReactNode; onOpen?: (ref: string) => void }) {
  return (
    <li className="flex min-w-0 items-center gap-1.5 rounded-md px-1 py-1 text-sm hover:bg-secondary">
      {cardRef && <CardRef cardRef={cardRef} />}
      {cardRef && onOpen
        ? <button type="button" onClick={() => onOpen(cardRef)} className="min-w-0 flex-1 truncate rounded-sm text-left outline-none focus-visible:ring-2 focus-visible:ring-ring">{title}</button>
        : <span className="min-w-0 flex-1 truncate">{title}</span>}
      {trailing}
    </li>
  );
}

export function AwayDigest({ lastSeen, scope, finished, needsYou, decisions, onOpen, onCaughtUp, onClose, renderNeedsActions }: {
  lastSeen: string;
  scope: string;
  finished: BoardCard[];
  needsYou?: BoardCard[];
  decisions: AwayDecision[];
  onOpen?: (ref: string) => void;
  onCaughtUp?: () => void;
  onClose?: () => void;
  renderNeedsActions?: (card: BoardCard) => ReactNode;
}) {
  const titleId = useId();
  const age = (text: string) => <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{text}</span>;
  return (
    <section aria-labelledby={titleId} className="flex shrink-0 flex-col gap-2 border-b bg-sidebar px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <h2 id={titleId} className="text-sm font-medium">De când ai lipsit</h2>
        <span className="font-mono text-xs text-muted-foreground">ultima vizită {lastSeen} · {scope}</span>
        <span className="flex-1" />
        {onCaughtUp && <Button type="button" variant="outline" size="xs" onClick={onCaughtUp}>Am văzut tot</Button>}
        {onClose && <Button type="button" variant="ghost" size="icon-xs" aria-label="Închide rezumatul" onClick={onClose}><XIcon aria-hidden /></Button>}
      </div>
      <div className={cn("grid gap-2", needsYou ? "md:grid-cols-3" : "md:grid-cols-2")}>
        <Column title={<><StatusDot dot="done" label={null} />Terminate</>} count={finished.length} empty="Nimic terminat.">
          {finished.map((c) => <Row key={c.ref} cardRef={c.ref} title={c.title} trailing={age(c.age)} onOpen={onOpen} />)}
        </Column>
        {needsYou && (
          <Column title={<><StatusDot dot="needs" label={null} />Te așteaptă</>} count={needsYou.length} empty="Nimic în așteptare.">
            {needsYou.map((c) => <Row key={c.ref} cardRef={c.ref} title={c.title} trailing={renderNeedsActions?.(c)} onOpen={onOpen} />)}
          </Column>
        )}
        <Column title="Decizii noi" count={decisions.length} empty="Nicio decizie nouă.">
          {decisions.map((d) => <Row key={d.id} cardRef={d.cardRef} title={d.title} trailing={age(d.age)} onOpen={onOpen} />)}
        </Column>
      </div>
    </section>
  );
}
