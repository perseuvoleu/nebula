import { useEffect, useState } from "react";
import { ArrowLeftIcon, MenuIcon } from "lucide-react";
import { api, describeError } from "@/lib/api";
import { activityAge } from "@/lib/activity-age";
import {
  type BoardData, type Card, type CardShow, type SidebarScope, type SortKey, type TableSort,
  awayDigest, byStage, cardSlug, inScope, projectName, sidebarProjects, sortCards, toBoardCard, writerSlots,
} from "@/lib/board";
import { type BoardRoute, selectionPath } from "@/lib/route";
import { BOARD_POLL_MS, useBoard } from "@/lib/useBoard";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { AwayDigest, type AwayDecision } from "./AwayDigest";
import { BoardColumns } from "./BoardColumns";
import { CardArtifacts } from "./CardArtifacts";
import { CardDetail } from "./CardDetail";
import type { DecisionRow } from "./DecisionCard";
import { DecisionsPage } from "./DecisionsPage";
import { NeedsYouStrip } from "./NeedsYouStrip";
import { ProjectSidebar } from "./ProjectSidebar";
import { RecordDecision } from "./RecordDecision";
import { TableView } from "./TableView";
import { WriterSlots } from "./WriterSlots";

const LAST_SEEN = "nebula-board-last-seen";

type Decision = { id: string; title: string; card: string | null; project: string | null; session: { id: string; name: string } | null;
  content: { body?: string }; created_at: number };

const boardHref = (route: Partial<BoardRoute>) => selectionPath({ project: null, session: null, all: false,
  board: { project: null, card: null, decisions: false, table: false, peek: null, ...route } });

function decisionRow(d: Decision, cards: Card[]): DecisionRow {
  const card = cards.find((c) => c.id === d.card);
  return {
    id: d.id, title: d.title, why: d.content.body || null, project: d.project, createdAt: new Date(d.created_at).toISOString(),
    session: d.session && d.project ? { name: d.session.name, href: selectionPath({ project: d.project, session: d.session.id, all: false }) } : null,
    card: card ? { ref: card.ref, title: card.title, href: boardHref({ project: card.project, home: card.project === null, card: cardSlug(card.ref) }) }
      : d.card ? { ref: "", title: "Cardul deciziei", href: boardHref({ card: d.card }) } : null,
  };
}

function loadLastSeen(): number {
  const now = Date.now();
  try {
    const stored = Number(localStorage.getItem(LAST_SEEN));
    if (stored > 0) return stored;
    localStorage.setItem(LAST_SEEN, String(now));
  } catch { /* Without storage the digest starts from this visit. */ }
  return now;
}

function useCard(slug: string | null, board: BoardData | null) {
  const [state, setState] = useState<{ slug: string; show: CardShow | null; error: string | null } | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    if (!slug) return;
    const abort = new AbortController();
    let retry: ReturnType<typeof setTimeout> | undefined;
    api<CardShow>(`cards/${encodeURIComponent(slug)}`, {}, undefined, { signal: abort.signal })
      .then((show) => setState({ slug, show, error: null }))
      .catch((failure) => {
        if (abort.signal.aborted) return;
        setState((old) => ({ slug, show: old?.slug === slug ? old.show : null, error: describeError(failure) }));
        retry = setTimeout(() => setAttempt((n) => n + 1), BOARD_POLL_MS);
      });
    return () => { abort.abort(); clearTimeout(retry); };
  }, [slug, board, attempt]);
  return state?.slug === slug ? { ...state, refresh: () => setAttempt((n) => n + 1) } : null;
}

function useDecisions(enabled: boolean, query: string, board: BoardData | null): Decision[] {
  const [items, setItems] = useState<Decision[]>([]);
  useEffect(() => {
    if (!enabled) return;
    const abort = new AbortController();
    api<{ items: Decision[] }>("decisions", { query: query || undefined }, undefined, { signal: abort.signal })
      .then((data) => setItems(data.items))
      .catch(() => { /* The list keeps its last answer; the board header already shows when the host is unreachable. */ });
    return () => abort.abort();
  }, [enabled, query, board]);
  return enabled ? items : [];
}

function CardPanel({ slug, board, mode, onOpenCard }: { slug: string; board: BoardData | null; mode: "peek" | "page"; onOpenCard: (ref: string) => void }) {
  const card = useCard(slug, board);
  if (!card) return <p className="text-sm text-muted-foreground" aria-busy="true">Se încarcă…</p>;
  if (!card.show) return <p role="alert" className="text-sm">{card.error}</p>;
  const now = Date.now();
  const log = card.show.log.map((n) => ({ id: n.id, author: n.author.name, text: n.text, age: activityAge(n.created_at, now) }));
  return (
    <>
      {card.error && <span role="status" className="self-start rounded-md border border-dashed px-1.5 py-0.5 text-xs">Date neactualizate</span>}
      <CardDetail card={toBoardCard(card.show, board?.cards ?? [], card.show)} log={log} mode={mode} onOpenCard={onOpenCard} />
      {mode === "page" && (
        <div className="mx-auto mt-4 flex w-full max-w-3xl flex-col gap-4">
          <RecordDecision cardRef={card.show.ref} onRecorded={card.refresh} />
          <CardArtifacts artifacts={card.show.artifacts} />
        </div>
      )}
    </>
  );
}

export function BoardPage({ route, onNavigate, onSessions }: {
  route: BoardRoute;
  onNavigate: (route: BoardRoute, replace?: boolean) => void;
  onSessions: () => void;
}) {
  const [lastSeen, setLastSeen] = useState(loadLastSeen);
  const { board, stale } = useBoard(lastSeen);
  const [local, setLocal] = useState<SidebarScope | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [expanded, setExpanded] = useState<(string | null)[]>([]);
  const [showQuiet, setShowQuiet] = useState(false);
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<TableSort>({ key: "ref", asc: true });
  const [collapsed, setCollapsed] = useState<string[]>([]);
  const [digestClosed, setDigestClosed] = useState(false);
  const [decisionQuery, setDecisionQuery] = useState("");

  const routed = (s: SidebarScope) => s.kind === "all" || s.kind === "needs"
    ? { project: null, home: false, decisions: false }
    : { project: s.project, home: s.project === null, decisions: s.kind === "decisions" };
  const fromRoute: SidebarScope = !route.project && !route.home ? { kind: "all" }
    : route.decisions ? { kind: "decisions", project: route.project } : { kind: "project", project: route.project };
  const same = (r: ReturnType<typeof routed>) => r.project === route.project && r.home === !!route.home && r.decisions === route.decisions;
  const scope = local && same(routed(local)) ? local : fromRoute;
  const go = (next: Partial<BoardRoute>) => {
    onNavigate({ ...route, card: null, peek: null, decisions: false, ...next });
    setMenuOpen(false);
  };
  const select = (next: SidebarScope) => {
    setLocal(next);
    go(routed(next));
  };
  const peek = (ref: string) => onNavigate({ ...route, peek: cardSlug(ref) });
  const openPage = (ref: string) => go({ card: cardSlug(ref) });
  const toggle = <T,>(list: T[], name: T) => list.includes(name) ? list.filter((n) => n !== name) : [...list, name];

  const live = (board?.cards ?? []).filter((c) => !c.archived);
  const all = live.map((c) => toBoardCard(c, live));
  const cards = all.filter((c) => inScope(c, scope));
  const bySlug = (slug: string | null): Card | undefined => live.find((c) => cardSlug(c.ref) === slug);
  const peeked = bySlug(route.peek);
  const decisions = useDecisions(route.decisions || !!board?.away.decisions.length, route.decisions ? decisionQuery : "", board);
  const digest = board && awayDigest(board, scope, decisions);
  const now = Date.now();
  const awayDecisions: AwayDecision[] = (digest?.decisions ?? [])
    .map((d) => ({ id: d.id, title: d.title, age: activityAge(d.created_at, now), cardRef: live.find((c) => c.id === d.card)?.ref ?? null }));

  const sidebar = (
    <ProjectSidebar projects={board ? sidebarProjects(board) : []} total={live.filter((c) => c.stage !== "done").length}
      scope={scope} expanded={expanded} showQuiet={showQuiet}
      query={search} onQueryChange={setSearch} onSelect={select}
      onToggleProject={(name) => setExpanded((old) => toggle(old, name))} onToggleQuiet={() => setShowQuiet((v) => !v)} />
  );

  const main = route.card ? (
    <div className="min-h-0 flex-1 overflow-y-auto p-4">
      <Button type="button" variant="ghost" size="sm" className="mb-3" onClick={() => go({})}><ArrowLeftIcon aria-hidden />Board</Button>
      <CardPanel slug={route.card} board={board} mode="page" onOpenCard={openPage} />
    </div>
  ) : route.decisions ? (
    <DecisionsPage project={scope.kind === "decisions" ? projectName(scope.project) : null} query={decisionQuery} onQueryChange={setDecisionQuery}
      decisions={decisions.filter((d) => scope.kind !== "decisions" || d.project === scope.project).map((d) => decisionRow(d, live))} />
  ) : !board ? (
    <p role={stale ? "alert" : undefined} aria-busy={!stale} className="p-6 text-sm text-muted-foreground">
      {stale ? "Board-ul nu poate fi citit acum." : "Se încarcă board-ul…"}
    </p>
  ) : route.table ? (
    <TableView cards={sortCards(cards, sort)} groupByProject={scope.kind === "all"} selected={peeked?.ref} onOpen={peek}
      sort={sort} onSort={(key: SortKey) => setSort((old) => ({ key, asc: old.key === key ? !old.asc : true }))}
      collapsed={collapsed} onToggleGroup={(name) => setCollapsed((old) => toggle(old, name))} />
  ) : (
    <BoardColumns columns={byStage(cards)} selected={peeked?.ref} showProject={scope.kind === "all"} onOpen={peek} />
  );

  return (
    <div className="flex min-h-0 flex-1">
      <div className="hidden min-h-0 md:flex">{sidebar}</div>
      <main className="flex min-h-0 min-w-0 flex-1 flex-col">
        <header className="flex min-h-14 flex-wrap items-center gap-2 border-b px-3 py-2">
          <Button type="button" variant="ghost" size="icon-lg" aria-label="Meniu" onClick={() => setMenuOpen(true)} className="size-11 md:hidden">
            <MenuIcon aria-hidden className="size-5" />
          </Button>
          <h1 className="text-[15px] font-semibold">Board</h1>
          {route.project && <span className="font-mono text-xs text-muted-foreground">{route.project}</span>}
          {stale && board && <span role="status" className="rounded-md border border-dashed px-1.5 py-0.5 text-xs">Date neactualizate</span>}
          <span className="flex-1" />
          {board && board.hosts.length > 0 && <WriterSlots hosts={writerSlots(board.hosts)} />}
          {!route.card && !route.decisions && (
            <div role="group" aria-label="Vizualizare" className="flex rounded-lg border p-0.5">
              {([false, true] as const).map((table) => (
                <button key={String(table)} type="button" aria-pressed={route.table === table} onClick={() => onNavigate({ ...route, table })}
                  className={cn("h-8 rounded-md px-2.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring", route.table === table ? "bg-secondary font-medium" : "text-muted-foreground hover:text-foreground")}>
                  {table ? "Tabel" : "Coloane"}
                </button>
              ))}
            </div>
          )}
          <Button type="button" variant="ghost" size="sm" onClick={onSessions}>Sesiuni</Button>
        </header>
        {digest && !digest.empty && !digestClosed && !route.card && (
          <AwayDigest lastSeen={new Date(lastSeen).toLocaleString("ro-RO", { day: "numeric", month: "short", hour: "2-digit", minute: "2-digit" })}
            scope={scope.kind === "all" || scope.kind === "needs" ? "toate proiectele" : projectName(scope.project)}
            finished={digest.finished.map((c) => toBoardCard(c, live))} decisions={awayDecisions} onOpen={peek} onClose={() => setDigestClosed(true)}
            onCaughtUp={() => {
              const seen = Date.now();
              try { localStorage.setItem(LAST_SEEN, String(seen)); } catch { /* The digest still resets for this visit. */ }
              setLastSeen(seen);
            }} />
        )}
        {!route.card && !route.decisions && <NeedsYouStrip cards={cards.filter((c) => c.needsYou)} onOpen={peek} />}
        {main}
      </main>
      <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
        <SheetContent side="left" className="w-[88vw] max-w-sm gap-0 p-0" aria-describedby={undefined}>
          <SheetTitle className="sr-only">Proiecte</SheetTitle>
          {sidebar}
        </SheetContent>
      </Sheet>
      <Sheet open={!!route.peek && !route.card} onOpenChange={(open) => { if (!open) onNavigate({ ...route, peek: null }); }}>
        <SheetContent side="right" className="w-full gap-3 overflow-y-auto p-4 sm:max-w-md" aria-describedby={undefined}>
          <SheetTitle className="sr-only">{peeked?.title ?? "Card"}</SheetTitle>
          {route.peek && (
            <>
              <a href={`/board/c/${encodeURIComponent(route.peek)}`} onClick={(e) => { e.preventDefault(); openPage(route.peek!); }}
                className="self-start text-sm underline underline-offset-4 outline-none focus-visible:ring-2 focus-visible:ring-ring">Deschide pagina</a>
              <CardPanel slug={route.peek} board={board} mode="peek" onOpenCard={peek} />
            </>
          )}
        </SheetContent>
      </Sheet>
    </div>
  );
}
