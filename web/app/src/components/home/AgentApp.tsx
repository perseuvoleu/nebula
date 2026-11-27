import { type ReactNode, useState } from "react";
import { api, describeError } from "@/lib/api";
import { type Card, type CardSession, type CardShow, type NeedsYou, type Stage, type Workspace, STAGE_LABEL, cardSlug, firstWait, hostProjects, knownHosts, openTarget, sessionProject } from "@/lib/board";
import { cardAction } from "@/lib/board-actions";
import { decisionRow, useCard, useDecisions, useHomeSessions } from "@/lib/board-reads";
import { approvalKey, filterByProject, homeBoard, homeOrder, homeProjects } from "@/lib/home";
import { TAB_KEYS, usePageKeys } from "@/lib/keys";
import { openCode, rememberedLaptop } from "@/lib/open-code";
import { type Selection, type Tab, tabOf } from "@/lib/route";
import { useBoard } from "@/lib/useBoard";
import { DecisionsPage } from "@/components/board/DecisionsPage";
import { OpenCodeDialog, OpenCodeToast } from "@/components/board/CardActions";
import { StatusDot } from "@/components/ui/status-dot";
import { KeysHelp, ModeLine, TabBar } from "./AppFrame";
import { HomePage } from "./HomePage";

const TAB_ROUTE: Record<Tab, Selection> = {
  home: { project: null, session: null, all: false, view: "home", card: null },
  code: { project: null, session: null, all: false },
  rag: { project: null, session: null, all: false, view: "rag" },
  decisions: { project: null, session: null, all: false, view: "decisions" },
};
const BOARD: Selection = { project: null, session: null, all: false, board: { project: null, card: null, decisions: false, table: false, peek: null } };

const HINTS: Record<Tab, [string, string][]> = {
  home: [["i", "reply"], ["c", "code"], ["⏎", "open"], ["v", "artifacts"], ["a", "approve"], ["0", "all"], ["b", "kanban"], ["?", "keys"]],
  code: [["1", "home"], ["3", "rag"], ["4", "decisions"], ["0", "all"], ["b", "kanban"], ["?", "keys"]],
  rag: [["1", "home"], ["2", "code"], ["4", "decisions"], ["0", "all"]],
  decisions: [["1", "home"], ["2", "code"], ["3", "rag"], ["0", "all"], ["?", "keys"]],
};

function DecisionsTab({ workspace, board, cards, projectScope }: {
  workspace: Workspace;
  board: ReturnType<typeof useBoard>["board"];
  cards: Card[];
  projectScope: string | null;
}) {
  const [query, setQuery] = useState("");
  const listed = useDecisions(true, [], query, board);
  const items = filterByProject(listed.items, projectScope);
  const homeSessions = useHomeSessions(items.flatMap((d) => d.project === null && d.session ? [d.session] : []), workspace);
  return (
    <DecisionsPage project={projectScope} query={query} onQueryChange={setQuery} loading={listed.loading} error={listed.error}
      decisions={items.map((d) => decisionRow(d, cards, workspace, homeSessions))} />
  );
}

function RagTab({ projectScope }: { projectScope: string | null }) {
  return (
    <section aria-label="RAG" className="min-h-0 flex-1 overflow-y-auto px-5 py-3.5 font-mono text-[11.5px]">
      <h1 className="mb-2 font-bold text-primary">RAG · Shared Knowledge Store</h1>
      <p className="mb-1 text-muted-foreground">
        Filtru activ: <span className="font-semibold text-foreground">{projectScope ? `@${projectScope}` : "toate proiectele"}</span>
      </p>
      <p className="text-muted-foreground">În curând: căutare în <code>nebula rag</code> (note și documente din {projectScope ? `@${projectScope}` : "toate proiectele"}).</p>
    </section>
  );
}

async function findSessionProject(host: string, id: string, workspace: Workspace): Promise<string | null> {
  const projects = hostProjects(host, workspace);
  if (!projects.length) return null;
  const results = await Promise.allSettled(
    projects.map(async (project) => {
      const data = await api<{ sessions: { id: string }[] }>("sessions", { project });
      return data.sessions.some((s) => s.id === id) ? project : null;
    }),
  );
  for (const r of results) {
    if (r.status === "fulfilled" && r.value) return r.value;
  }
  return null;
}

export function AgentApp({ selection, navigate, workspace, phone, code }: {
  selection: Selection;
  navigate: (next: Selection, replace?: boolean) => void;
  workspace: Workspace;
  phone: boolean;
  code: ReactNode;
}) {
  const tab = tabOf(selection) ?? "home";
  const { board, stale, readAt } = useBoard(null);
  const [approved, setApproved] = useState<Set<string>>(new Set());
  const [projectScope, setProjectScope] = useState<string | null>(null);
  const [stage, setStage] = useState<Stage | null>(null);
  const [picked, setPicked] = useState<{ card: string; idx: number } | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [help, setHelp] = useState(false);
  const allHome = homeBoard(board, approved);
  const projects = homeProjects(board, approved, workspace.projects);
  const home = homeBoard(board, approved, projectScope);
  const order = homeOrder(home, stage);
  const bySlug = (slug: string) => home.cards.find((c) => cardSlug(c.ref) === slug || c.id === slug) ?? null;
  const routed = tab === "home" && selection.card ? bySlug(selection.card) : null;
  const missingCard = tab === "home" && !!selection.card && !!board && !routed;
  const card = selection.card ? routed : (phone ? null : order[0] ?? null);
  const shown = useCard(tab === "home" && card ? cardSlug(card.ref) : null, board);
  const show = shown?.show ?? null;
  const decisions = filterByProject(useDecisions(tab === "home", [], "", board).items, projectScope);

  const codeRoute = (scope: string | null): Selection =>
    scope && workspace.projects.includes(scope)
      ? { project: scope, session: null, all: false }
      : { project: null, session: null, all: true };
  const goTab = (next: Tab) => navigate(next === "code" ? codeRoute(projectScope) : TAB_ROUTE[next]);
  const setScope = (next: string | null) => {
    setProjectScope(next);
    if (tab === "home" && selection.card) {
      const nextHome = homeBoard(board, approved, next);
      if (!nextHome.cards.some((c) => cardSlug(c.ref) === selection.card || c.id === selection.card)) {
        navigate(TAB_ROUTE.home, true);
      }
    } else if (tab === "code") {
      navigate(codeRoute(next));
    }
  };
  const select = (next: Card, replace = false) => navigate({ ...TAB_ROUTE.home, card: cardSlug(next.ref) }, replace);
  const approve = (target: Card, wait: NeedsYou) => {
    const key = approvalKey(wait);
    setNotice(null);
    setApproved((old) => new Set([...old, key]));
    cardAction(target.ref, "approve", { session: wait.id, host: wait.host, expectedStatusAt: wait.status_changed_at })
      .then(() => setNotice(`${target.ref} · aprobat ${wait.name}`))
      .catch((failure) => {
        setApproved((old) => new Set([...old].filter((k) => k !== key)));
        setNotice(describeError(failure));
      });
  };
  const openInCode = async (target: Card, known: CardSession[] | null, chosen: CardSession | null = null) => {
    setNotice(null);
    let sessions = known;
    if (!sessions) {
      try {
        sessions = (await api<CardShow>(`cards/${encodeURIComponent(cardSlug(target.ref))}`)).sessions;
      } catch (failure) {
        setNotice(`${target.ref} · ${describeError(failure)}`);
        return;
      }
    }
    const session = chosen ?? firstWait(home.waiting.filter((w) => w.card === target.id)) ?? openTarget(sessions);
    if (!session) { setNotice(`${target.ref} · cardul nu are încă nicio sesiune.`); return; }
    const project = target.project
      ? sessionProject(target.project, session.host, workspace)
      : await findSessionProject(session.host, session.id, workspace);
    if (project) { navigate({ project, session: session.id, all: false }); return; }
    const hosts = knownHosts(board, sessions.map((s) => s.host)).map((h) => h.host);
    const on = rememberedLaptop() ?? hosts.find((h) => h !== "leaseweb") ?? hosts[0];
    const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (on) openCode.open(openCode.begin(), { cardRef: target.ref, cardId: target.id, target: session, on, hosts, opener, label: null, scope: null });
  };

  usePageKeys((key) => {
    if (TAB_KEYS[key]) { goTab(TAB_KEYS[key]); return true; }
    if (key === "0") { setScope(null); return true; }
    if (key === "?") { setHelp(true); return true; }
    if (key === "b") { navigate(BOARD); return true; }
    if (key === "c") { goTab(tab === "code" ? "home" : "code"); return true; }
    if (tab !== "home") return false;
    if (key === "j" || key === "k" || key === "ArrowDown" || key === "ArrowUp") {
      const at = card ? order.findIndex((c) => c.id === card.id) : -1;
      const next = order[Math.min(order.length - 1, Math.max(0, at + (key === "j" || key === "ArrowDown" ? 1 : -1)))];
      if (next) select(next, true);
      return !!next;
    }
    if (!card) return false;
    if (key === "i") {
      const input = document.querySelector<HTMLInputElement>("[data-home-reply]");
      if (input) { input.focus(); return true; }
      return false;
    }
    if (key === "Enter") {
      const sessions = show?.id === card.id ? show.sessions : null;
      const chosen = sessions && picked?.card === card.id && picked.idx < sessions.length ? sessions[picked.idx] : null;
      void openInCode(card, sessions, chosen);
      return true;
    }
    const wait = firstWait(home.waiting.filter((w) => w.card === card.id));
    if (key === "a" && wait) { approve(card, wait); return true; }
    if (key === "v") { document.querySelector<HTMLElement>("[data-artifact]")?.click(); return true; }
    return false;
  });

  const top = home.needs[0] ?? null;
  const right = tab === "home" && (
    <>
      {top ? (
        <button type="button" onClick={() => select(top)}
          className="inline-flex items-center gap-[5px] rounded-xs border border-line-err bg-tint-need px-[7px] text-foreground outline-none hover:border-err focus-visible:ring-2 focus-visible:ring-ring">
          <span className="font-semibold text-err">{home.counts.needs} needs you</span><span className="text-dim">·</span>
          <StatusDot dot="needs" label={null} className="size-1.5" /><span className="max-w-[210px] truncate">{top.title}</span>
        </button>
      ) : <span className="rounded-xs border border-line-ok bg-tint-ok px-[7px] text-ok">✓ 0 needs you</span>}
      {card && <><span className="text-dim">{projectScope ? `@${projectScope}` : "all"} ·</span><span className="text-dim">{card.ref}</span><span>{STAGE_LABEL[card.stage]}</span></>}
    </>
  );

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <TabBar tab={tab} counts={board ? home.counts : null} totalCount={allHome.counts.all} projectScope={projectScope} projects={projects} onProjectScope={setScope} onTab={goTab} onBoard={() => navigate(BOARD)} onHelp={() => setHelp(true)} />
      {tab === "home" && <OpenCodeToast id="open-code-status" cardHref={(ref) => `/board/c/${encodeURIComponent(cardSlug(ref))}`}
        onCardPage={(ref) => navigate({ ...BOARD, board: { ...BOARD.board!, card: cardSlug(ref) } })} />}
      {tab === "home" && notice && (
        <p role="status" className="flex shrink-0 items-center gap-2 border-b bg-bg1 px-3 py-1 font-mono text-[11px] text-primary">
          <span className="min-w-0 flex-1 truncate">{notice}</span>
          <button type="button" onClick={() => setNotice(null)} className="text-dim hover:text-foreground" aria-label="Închide mesajul">×</button>
        </p>
      )}
      <main id="board-main" tabIndex={-1} className="flex min-h-0 min-w-0 flex-1 flex-col outline-none">
        {tab === "home" && (
          <HomePage home={home} projects={projects} totalCount={allHome.counts.all} projectScope={projectScope} onProjectScope={setScope}
            card={card} missingCard={missingCard} show={show} workspace={workspace} decisions={decisions} phone={phone} stale={stale} loaded={!!board}
            stage={stage} onStage={setStage} picked={picked} onPick={setPicked}
            onSelect={(c) => select(c)} onBack={() => goTab("home")} onApprove={approve} onOpenCode={(c, sessions, session) => void openInCode(c, sessions, session ?? null)} onDecisions={() => goTab("decisions")} version={readAt} />
        )}
        {tab === "code" && code}
        {tab === "rag" && <RagTab projectScope={projectScope} />}
        {tab === "decisions" && <DecisionsTab workspace={workspace} board={board} cards={board?.cards ?? []} projectScope={projectScope} />}
      </main>
      <ModeLine tab={tab} hints={HINTS[tab]} right={right} />
      <KeysHelp open={help} onOpenChange={setHelp} />
      {tab === "home" && <OpenCodeDialog fallback={() => document.getElementById("board-main")} />}
    </div>
  );
}
