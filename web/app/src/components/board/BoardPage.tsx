import { useEffect, useState } from "react";
import { ArrowLeftIcon, MenuIcon, PlusIcon } from "lucide-react";
import { api, describeError } from "@/lib/api";
import { activityAge } from "@/lib/activity-age";
import {
  type BoardCard, type BoardData, type Card, type CardShow, type NeedsYou, type SessionLink, type SidebarScope, type SortKey, type TableSort, type Stage, type Workspace,
  awayDigest, byStage, cardSlug, hostProjects, inScope, projectName, sessionProject, sidebarProjects, sortCards, toBoardCard, writerSlots,
} from "@/lib/board";
import { cardAction, createCard, ulid } from "@/lib/board-actions";
import { type Pending, drop, patchOf, propose, reconcile, settle } from "@/lib/optimistic";
import { type BoardRoute, selectionPath } from "@/lib/route";
import { BOARD_POLL_MS, useBoard } from "@/lib/useBoard";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { AwayDigest, type AwayDecision } from "./AwayDigest";
import { BoardColumns } from "./BoardColumns";
import { CardActions } from "./CardActions";
import { CardArtifacts } from "./CardArtifacts";
import { CardDetail } from "./CardDetail";
import type { DecisionRow } from "./DecisionCard";
import { DecisionsPage } from "./DecisionsPage";
import { type CardDraft, NewCardDialog } from "./NewCardDialog";
import { NeedsYouStrip } from "./NeedsYouStrip";
import { ProjectSidebar } from "./ProjectSidebar";
import { ProposalTray } from "./ProposalTray";
import { RecordDecision } from "./RecordDecision";
import { TableView } from "./TableView";
import { WriterSlots } from "./WriterSlots";

const LAST_SEEN = "nebula-board-last-seen";

/** An approval answers one waiting state, so it stays hidden until the session leaves needs_you or waits anew. */
const approval = (w: NeedsYou) => `approve:${w.host}/${w.id}@${w.status_changed_at}`;

type RawCard = BoardData["cards"][number];

type Decision = { id: string; title: string; card: string | null; project: string | null; session: SessionLink | null;
  content: { body?: string }; created_at: number };

const boardHref = (route: Partial<BoardRoute>) => selectionPath({ project: null, session: null, all: false,
  board: { project: null, card: null, decisions: false, table: false, peek: null, ...route } });

const sessionKey = (s: SessionLink) => `${s.host}/${s.id}`;

function decisionRow(d: Decision, cards: Card[], workspace: Workspace, homeSessions: Map<string, string>): DecisionRow {
  const card = cards.find((c) => c.id === d.card);
  const project = d.session && (d.project === null ? homeSessions.get(sessionKey(d.session)) ?? null : sessionProject(d.project, d.session.host, workspace));
  return {
    id: d.id, title: d.title, why: d.content.body || null, project: d.project, createdAt: new Date(d.created_at).toISOString(),
    session: d.session && { name: d.session.name, host: d.session.host,
      href: project ? selectionPath({ project, session: d.session.id, all: false }) : null },
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

async function allDecisions(query: string, signal: AbortSignal): Promise<Decision[]> {
  const items: Decision[] = [];
  let page: string | null = null;
  do {
    const data: { items: Decision[]; next: string | null } = await api("decisions", { query: query || undefined, page: page ?? undefined }, undefined, { signal });
    items.push(...data.items);
    page = data.next;
  } while (page);
  return items;
}

function useDecisions(enabled: boolean, query: string, board: BoardData | null) {
  const [state, setState] = useState<{ query: string; items: Decision[]; error: string | null } | null>(null);
  useEffect(() => {
    if (!enabled) return;
    const abort = new AbortController();
    allDecisions(query, abort.signal)
      .then((items) => setState({ query, items, error: null }))
      .catch((failure) => {
        if (!abort.signal.aborted) setState((old) => ({ query, items: old?.query === query ? old.items : [], error: describeError(failure) }));
      });
    return () => abort.abort();
  }, [enabled, query, board]);
  const current = enabled && state?.query === query ? state : null;
  return { items: current?.items ?? [], error: current?.error ?? null, loading: enabled && !current };
}

/** Home decisions carry no project, so their sessions are found in the lists of the projects on their host. */
function useHomeSessions(sessions: SessionLink[], workspace: Workspace): Map<string, string> {
  const hosts = [...new Set(sessions.map((s) => s.host))].sort().join(" ");
  const [found, setFound] = useState(new Map<string, string>());
  useEffect(() => {
    if (!hosts) return;
    const abort = new AbortController();
    const lists = hosts.split(" ").flatMap((host) => hostProjects(host, workspace).map((project) =>
      api<{ sessions: { id: string }[] }>("sessions", { project }, undefined, { signal: abort.signal })
        .then((data) => data.sessions.map((s): [string, string] => [sessionKey({ host, id: s.id, name: "" }), project]))));
    void Promise.allSettled(lists).then((results) => {
      if (!abort.signal.aborted) setFound(new Map(results.flatMap((r) => r.status === "fulfilled" ? r.value : [])));
    });
    return () => abort.abort();
  }, [hosts, workspace]);
  return found;
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
      <CardDetail card={toBoardCard(card.show, board?.cards ?? [], card.show)} log={log} mode={mode} onOpenCard={onOpenCard}>
        <CardActions show={card.show} board={board} />
      </CardDetail>
      {mode === "page" && (
        <div className="mx-auto mt-4 flex w-full max-w-3xl flex-col gap-4">
          <RecordDecision cardRef={card.show.ref} onRecorded={card.refresh} />
          <CardArtifacts artifacts={card.show.artifacts} />
        </div>
      )}
    </>
  );
}

export function BoardPage({ route, workspace, onNavigate, onSessions }: {
  route: BoardRoute;
  workspace: Workspace;
  onNavigate: (route: BoardRoute, replace?: boolean) => void;
  onSessions: () => void;
}) {
  const [lastSeen, setLastSeen] = useState(loadLastSeen);
  const { board, stale, readAt } = useBoard(lastSeen);
  const [local, setLocal] = useState<SidebarScope | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const [expanded, setExpanded] = useState<(string | null)[]>([]);
  const [showQuiet, setShowQuiet] = useState(false);
  const [search, setSearch] = useState("");
  const [sort, setSort] = useState<TableSort>({ key: "ref", asc: true });
  const [collapsed, setCollapsed] = useState<string[]>([]);
  const [digestClosed, setDigestClosed] = useState(false);
  const [decisionQuery, setDecisionQuery] = useState("");
  const [overlay, setOverlay] = useState<Pending<Partial<RawCard>>[]>([]);
  const [newCard, setNewCard] = useState<(CardDraft & { id: string }) | null>(null);
  const [creating, setCreating] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  useEffect(() => setOverlay((old) => reconcile(old, readAt, (key) => (board?.needs_you ?? []).some((w) => approval(w) === key))), [readAt, board]);
  const optimistic = (key: string, patch: Partial<RawCard>, request: Promise<unknown>) => {
    const token = ulid();
    setActionError(null);
    setOverlay((old) => propose(old, token, key, patch));
    request
      .then(() => setOverlay((old) => settle(old, token, Date.now())))
      .catch((failure) => {
        setOverlay((old) => drop(old, token));
        setActionError(describeError(failure));
      });
  };

  const routed = (s: SidebarScope) => s.kind === "all" || s.kind === "needs" || s.kind === "all-decisions"
    ? { project: null, home: false, decisions: s.kind === "all-decisions" }
    : { project: s.project, home: s.project === null, decisions: s.kind === "decisions" };
  const fromRoute: SidebarScope = !route.project && !route.home ? { kind: route.decisions ? "all-decisions" : "all" }
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
  const cardPage = (ref: string): BoardRoute => ({ ...route, peek: null, decisions: false, card: cardSlug(ref) });
  const openPage = (ref: string) => go(cardPage(ref));
  const toggle = <T,>(list: T[], name: T) => list.includes(name) ? list.filter((n) => n !== name) : [...list, name];

  const live = (board?.cards ?? []).map((c) => ({ ...c, ...patchOf(overlay, c.ref) })).filter((c) => !c.archived);
  const waiting = (board?.needs_you ?? []).filter((w) => !patchOf(overlay, approval(w)));
  const all = live.map((c) => ({ ...toBoardCard(c, live), needsYou: waiting.some((w) => w.card === c.id), age: activityAge(c.updated_at, Date.now()) }));
  const scoped = all.filter((c) => inScope(c, scope));
  const cards = scoped.filter((c) => !c.proposed);
  const bySlug = (slug: string | null): Card | undefined => live.find((c) => cardSlug(c.ref) === slug);
  const peeked = bySlug(route.peek);
  const listed = useDecisions(route.decisions || !!board?.away.decisions.length, route.decisions ? decisionQuery : "", board);
  const decisions = listed.items;
  const homeSessions = useHomeSessions(route.decisions ? decisions.flatMap((d) => d.project === null && d.session ? [d.session] : []) : [], workspace);
  const digest = board && awayDigest(board, scope, decisions);
  const now = Date.now();
  const awayDecisions: AwayDecision[] = (digest?.decisions ?? [])
    .map((d) => ({ id: d.id, title: d.title, age: activityAge(d.created_at, now), cardRef: live.find((c) => c.id === d.card)?.ref ?? null }));

  const move = (ref: string, stage: Stage) => {
    const card = live.find((c) => c.ref === ref);
    if (card && card.stage !== stage) optimistic(ref, { stage }, cardAction(ref, "move", { stage, expect: card.stage }));
  };
  const openNew = (stage: Stage = "backlog") => {
    setCreateError(null);
    setNewCard({ title: "", project: scope.kind === "all" || scope.kind === "needs" || scope.kind === "all-decisions" ? null : scope.project, stage, id: ulid() });
  };
  const create = async () => {
    if (!newCard) return;
    setCreating(true);
    setCreateError(null);
    try {
      await createCard({ ...newCard, title: newCard.title.trim() });
      setNewCard(null);
    } catch (failure) {
      setCreateError(describeError(failure));
    } finally {
      setCreating(false);
    }
  };
  const waitingCards = cards.filter((c) => c.needsYou);
  const approve = (ref: string, session: NeedsYou) =>
    optimistic(approval(session), {}, cardAction(ref, "approve", { session: session.id, host: session.host, expectedStatusAt: session.status_changed_at }));
  const approveButtons = (card: BoardCard) => waiting.filter((w) => w.card === live.find((c) => c.ref === card.ref)?.id).map((w) => (
    <Button key={`${w.host}/${w.id}`} type="button" variant="outline" size="xs" onClick={() => approve(card.ref, w)}>Aprobă {w.name}</Button>
  ));

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
      loading={listed.loading} error={listed.error}
      decisions={decisions.filter((d) => scope.kind !== "decisions" || d.project === scope.project).map((d) => decisionRow(d, live, workspace, homeSessions))} />
  ) : !board ? (
    <p role={stale ? "alert" : undefined} aria-busy={!stale} className="p-6 text-sm text-muted-foreground">
      {stale ? "Board-ul nu poate fi citit acum." : "Se încarcă board-ul…"}
    </p>
  ) : route.table ? (
    <TableView cards={sortCards(cards, sort)} groupByProject={scope.kind === "all"} selected={peeked?.ref} onOpen={peek}
      sort={sort} onSort={(key: SortKey) => setSort((old) => ({ key, asc: old.key === key ? !old.asc : true }))}
      collapsed={collapsed} onToggleGroup={(name) => setCollapsed((old) => toggle(old, name))} />
  ) : (
    <BoardColumns columns={byStage(cards)} selected={peeked?.ref} showProject={scope.kind === "all"} onOpen={peek} onAdd={openNew} onMove={move} />
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
          {board && !route.card && !route.decisions && (
            <Button type="button" variant="outline" size="sm" onClick={() => openNew()}><PlusIcon aria-hidden />Card nou</Button>
          )}
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
            scope={scope.kind === "all" || scope.kind === "needs" || scope.kind === "all-decisions" ? "toate proiectele" : projectName(scope.project)}
            finished={digest.finished.map((c) => toBoardCard(c, live))} needsYou={digest.waiting.length ? digest.waiting.map((c) => toBoardCard(c, live)) : undefined} decisions={awayDecisions} onOpen={peek} onClose={() => setDigestClosed(true)}
            onCaughtUp={() => {
              const seen = Date.now();
              try { localStorage.setItem(LAST_SEEN, String(seen)); } catch { /* The digest still resets for this visit. */ }
              setLastSeen(seen);
            }} />
        )}
        {actionError && (
          <p role="alert" className="flex items-center gap-2 border-b px-3 py-1.5 text-sm">
            <span className="flex-1">{actionError}</span>
            <Button type="button" variant="ghost" size="sm" onClick={() => setActionError(null)}>Închide</Button>
          </p>
        )}
        {!route.card && !route.decisions && (
          <NeedsYouStrip cards={waitingCards} onOpen={peek}
            renderActions={approveButtons} />
        )}
        {!route.card && !route.decisions && (
          <ProposalTray proposals={scoped.filter((c) => c.proposed).map((c) => ({ id: c.ref, ref: c.ref, title: c.title, project: c.project, proposedBy: c.proposedBy ?? "agent" }))}
            onOpen={peek} onAccept={(ref) => optimistic(ref, { proposed: false }, cardAction(ref, "accept", {}))}
            onDismiss={(ref) => optimistic(ref, { archived: true }, cardAction(ref, "dismiss", {}))} />
        )}
        {main}
      </main>
      {newCard && (
        <NewCardDialog open onOpenChange={(open) => { if (!open) setNewCard(null); }} draft={newCard} pending={creating}
          projects={(board ? sidebarProjects(board) : []).flatMap((p) => p.project === null ? [] : [p.project])}
          onDraftChange={(draft) => setNewCard({ ...newCard, ...draft })} onCreate={create} error={createError} />
      )}
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
              <a href={boardHref(cardPage(route.peek))} onClick={(e) => { e.preventDefault(); openPage(route.peek!); }}
                className="self-start text-sm underline underline-offset-4 outline-none focus-visible:ring-2 focus-visible:ring-ring">Deschide pagina</a>
              <CardPanel slug={route.peek} board={board} mode="peek" onOpenCard={peek} />
            </>
          )}
        </SheetContent>
      </Sheet>
    </div>
  );
}
