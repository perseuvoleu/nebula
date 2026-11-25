import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { CloudIcon, ExternalLinkIcon, KanbanIcon, LogOutIcon, MenuIcon } from "lucide-react";
import { api, ApiError, describeError, getToken, onAuthChange, setToken } from "@/lib/api";
import { useConversation } from "@/lib/conversation";
import { fetchSessionList } from "@/lib/session-fetch";
import { orderSessions, sessionKey } from "@/lib/session-list";
import { clearDrafts } from "@/lib/drafts";
import { useSelection } from "@/lib/route";
import type { NewResponse, SessionRow, Worktree } from "@/lib/types";
import { unsubscribePush } from "@/lib/push";
import { loadUnseen, reconcileUnseen, saveUnseen } from "@/lib/unseen";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { ConversationView } from "./components/ConversationView";
import { Login } from "./components/Login";
import { NewAgentDialog } from "./components/NewAgentDialog";
import { SessionList, type ListPhase } from "./components/SessionList";
import { CheckoutPanel } from "./components/CheckoutPanel";
import { InstallApp } from "./components/InstallApp";
import { UsageNavbar } from "./components/UsageNavbar";
import { PushNotifications } from "./components/PushNotifications";
import { ServerlessRuns } from "./components/ServerlessRuns";
import { BoardPage } from "./components/board/BoardPage";

interface Workspace { projects: string[]; host: string }

const SESSIONS_POLL_MS = 5000;

function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const onChange = () => setMatches(media.matches);
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [query]);
  return matches;
}

function usePreview(project: string | null): string | null {
  const [preview, setPreview] = useState<{ project: string; url: string | null } | null>(null);
  useEffect(() => {
    if (!project) return;
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    const refresh = async () => {
      let url: string | null = null;
      try {
        const data = await api<{ url: string | null; phase: string | null }>("preview", { project }, undefined, { signal: abort.signal });
        if (data.phase === "running" && typeof data.url === "string") url = data.url;
      } catch { /* An unavailable preview must hide any previously published link. */ }
      if (!abort.signal.aborted) {
        setPreview({ project, url });
        timer = setTimeout(refresh, SESSIONS_POLL_MS);
      }
    };
    void refresh();
    return () => { abort.abort(); clearTimeout(timer); };
  }, [project]);
  // Do not render the previous project's link while its replacement request is pending.
  return preview?.project === project ? preview.url : null;
}

function PreviewButton({ url, className }: { url: string | null; className: string }) {
  return url && (
    <Button asChild variant="outline" size="icon-lg" className={className}>
      <a href={url} target="_blank" rel="noopener noreferrer" aria-label="Deschide preview-ul proiectului" title={url}>
        <ExternalLinkIcon aria-hidden />
      </a>
    </Button>
  );
}

function useSessions(projects: string[], keepLastList: boolean) {
  const [snapshot, setSnapshot] = useState<{ projects: string[]; rows: SessionRow[]; loaded: string[] } | null>(null);
  const [phase, setPhase] = useState<ListPhase>("loading");
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const activeRequest = useRef<AbortController | null>(null);
  const refresh = useCallback(async () => {
    const version = ++generation.current;
    activeRequest.current?.abort();
    const abort = new AbortController();
    activeRequest.current = abort;
    try {
      const data = await fetchSessionList(projects, {
        sessions: async (project) => (await api<{ sessions: SessionRow[] }>("sessions", { project }, undefined, { timeoutMs: 20000, signal: abort.signal })).sessions,
      }, abort.signal);
      if (version !== generation.current) return;
      setSnapshot(old => !data.loaded.length && keepLastList && old?.projects === projects ? { ...old, loaded: [] } : { projects, rows: data.rows, loaded: data.loaded });
      setPhase(data.loaded.length ? "ready" : navigator.onLine ? "error" : "offline");
      setError(data.errors.length ? data.errors.map(e => `${e.project}: ${describeError(e.failure)}`).join("; ") : null);
    } catch (failure) {
      if (version !== generation.current) return;
      if (failure instanceof ApiError && failure.status === 401) return;
      setError(describeError(failure));
      setPhase((p) => (p === "loading" ? "error" : navigator.onLine ? p : "offline"));
    }
  }, [projects, keepLastList]);
  useEffect(() => {
    setSnapshot(null); setPhase("loading"); setError(null);
    let timer: ReturnType<typeof setTimeout> | undefined;
    let cancelled = false;
    const loop = async () => {
      if (!document.hidden) await refresh();
      if (!cancelled) timer = setTimeout(loop, SESSIONS_POLL_MS);
    };
    void loop();
    const wake = () => { if (!document.hidden && !cancelled) void refresh(); };
    document.addEventListener("visibilitychange", wake);
    window.addEventListener("online", wake);
    return () => { cancelled = true; generation.current += 1; activeRequest.current?.abort(); if (timer) clearTimeout(timer); document.removeEventListener("visibilitychange", wake); window.removeEventListener("online", wake); };
  }, [refresh]);
  const remove = (row: SessionRow) => {
    generation.current += 1;
    setSnapshot(old => old ? { ...old, rows: old.rows.filter(r => sessionKey(r) !== sessionKey(row)) } : old);
    void refresh();
  };
  const current = snapshot?.projects === projects;
  return { rows: current ? snapshot.rows : [], loaded: current ? snapshot.loaded : [],
    phase: current ? phase : "loading" as ListPhase, error: current ? error : null, refresh, remove };
}

export default function App() {
  const [workspace, setWorkspace] = useState<Workspace | null>(null);
  const [authed, setAuthed] = useState(() => Boolean(getToken()));
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => onAuthChange(() => {
    const has = Boolean(getToken());
    setAuthed(has);
    if (!has) { setWorkspace(null); clearDrafts(); }
  }), []);

  useEffect(() => {
    if (!authed || workspace) return;
    let cancelled = false;
    api<Workspace>("projects").then((data) => { if (!cancelled) setWorkspace(data); })
      .catch((failure) => { if (!cancelled && !(failure instanceof ApiError && failure.status === 401)) setNotice(describeError(failure)); });
    return () => { cancelled = true; };
  }, [authed, workspace]);

  if (!authed || !workspace) {
    if (authed) return <main className="flex flex-1 items-center justify-center text-sm text-muted-foreground" aria-busy="true">{notice ?? "Se conectează…"}</main>;
    return <Login onAuthenticated={(projects, host) => setWorkspace({ projects, host })} />;
  }
  return <><UsageNavbar /><Routes workspace={workspace} /></>;
}

function Routes({ workspace }: { workspace: Workspace }) {
  const route = useSelection();
  const [selection, navigate] = route;
  if (!selection.board) return <Shell workspace={workspace} route={route} />;
  return <BoardPage route={selection.board} workspace={workspace} onSessions={() => navigate({ project: null, session: null, all: false })}
    onNavigate={(board) => navigate({ project: null, session: null, all: false, board })} />;
}

function Shell({ workspace, route }: { workspace: Workspace; route: ReturnType<typeof useSelection> }) {
  const [selection, navigate] = route;
  const project = selection.project && workspace.projects.includes(selection.project) ? selection.project : workspace.projects[0];
  const sessionId = selection.project && !workspace.projects.includes(selection.project) ? null : selection.session;
  const listProjects = useMemo(() => selection.all ? workspace.projects : [project], [selection.all, workspace.projects, project]);
  const sessions = useSessions(listProjects, !selection.all);
  const previewUrl = usePreview(selection.all && !selection.session ? null : project);
  const latestSelection = useRef(selection);
  const latestSessions = useRef(sessions);
  useLayoutEffect(() => { latestSelection.current = selection; latestSessions.current = sessions; });
  const onDeleted = (deleted: SessionRow) => {
    latestSessions.current.remove(deleted);
    const current = latestSelection.current;
    if (current.project === deleted.project && current.session === deleted.id) navigate({ project: deleted.project, session: null, all: current.all });
  };
  const desktop = useMediaQuery("(min-width: 768px)");
  const [menuOpen, setMenuOpen] = useState(false);
  const [chooseProject, setChooseProject] = useState(false);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<string | null>(null);
  const [checkout, setCheckout] = useState<{ project: string; path: string } | null>(null);
  const [unseenState, setUnseenState] = useState(loadUnseen);

  // Normalise the URL to a valid project once the allowlist is known.
  useEffect(() => {
    if (selection.serverless) return;
    if (selection.project && !workspace.projects.includes(selection.project)) {
      navigate({ project: selection.all ? null : project, session: null, all: selection.all }, true);
    } else if (!selection.all && selection.project !== project) {
      navigate({ project, session: selection.session, all: false }, true);
    }
  }, [project, selection.project, selection.session, selection.all, selection.serverless, workspace.projects, navigate]);

  const row = sessions.rows.find((r) => r.project === project && r.id === sessionId) ?? null;
  const currentPath = row?.path ?? (checkout?.project === project ? checkout.path : undefined);
  const chooseCheckout = (tree: Worktree) => {
    void sessions.refresh();
    setCheckout({ project, path: tree.path });
    navigate({ project, session: null, all: false });
    setMenuOpen(false);
  };
  const checkoutPanel = !selection.all && <CheckoutPanel key={project} project={project} currentPath={currentPath} onSelect={chooseCheckout} />;
  const missing = sessionId !== null && sessions.phase === "ready" && sessions.loaded.includes(project) && !row;
  const { state, controller } = useConversation(project, sessionId && !missing ? sessionId : null);
  useEffect(() => {
    if (sessions.phase !== "ready") return;
    setUnseenState((previous) => {
      const next = sessions.loaded.reduce((state, scope) => reconcileUnseen(state, scope,
        sessions.rows.filter(r => r.project === scope), scope === selection.project ? sessionId : null), previous);
      saveUnseen(next);
      return next;
    });
  }, [selection.project, sessionId, sessions.phase, sessions.rows, sessions.loaded]);
  const unseenFinished = new Set(unseenState.unseen);
  const orderedRows = orderSessions(sessions.rows, unseenFinished);
  const selectedKey = sessionId ? sessionKey({ project, id: sessionId }) : null;

  const select = (r: SessionRow) => { navigate({ project: r.project, session: r.id, all: selection.all }); setMenuOpen(false); setCreated(null); };
  const onCreated = (result: NewResponse) => {
    void sessions.refresh();
    // Identity comes from the gateway; a title is never used to guess it (the agent may rename).
    if (result.id) navigate({ project, session: result.id, all: false }); else setCreated(result.name);
    setMenuOpen(false);
  };
  const logout = async () => {
    controller?.dispose();
    try { await unsubscribePush(); } catch { /* Logging out must not depend on push delivery. */ }
    setToken(""); navigate({ project: null, session: null, all: false }, true);
  };

  const changeProject = (value: string) => {
    navigate({ project: value || null, session: null, all: value === "" });
    if (chooseProject && value) { setChooseProject(false); setCreating(true); }
  };
  const newAgent = () => {
    if (!selection.all) { setCreating(true); return; }
    setChooseProject(true);
    document.querySelector<HTMLSelectElement>('select[aria-label="Proiect"]')?.focus();
  };
  const openServerless = () => { navigate({ project: selection.all ? null : project, session: null, all: selection.all, serverless: true }); setMenuOpen(false); };
  const serverlessLink = (
    <button type="button" onClick={openServerless} aria-current={selection.serverless ? "page" : undefined}
      className={`flex min-h-11 items-center gap-2 border-t border-sidebar-border px-3 text-left text-sm focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring ${selection.serverless ? "bg-secondary font-medium" : "hover:bg-muted/60"}`}>
      <CloudIcon aria-hidden className="size-4" />Rulări serverless
    </button>
  );
  const boardLink = (
    <button type="button" onClick={() => navigate({ project: null, session: null, all: false, board: { project: null, card: null, decisions: false, table: false, peek: null } })}
      className="flex min-h-11 items-center gap-2 border-t border-sidebar-border px-3 text-left text-sm hover:bg-muted/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring">
      <KanbanIcon aria-hidden className="size-4" />Board
    </button>
  );
  const targetNotice = chooseProject && <p role="status" className="px-3 py-2 text-sm">Alege un proiect din selector pentru agentul nou.</p>;

  const sidebar = (
    <aside className="flex h-full min-h-0 flex-col bg-sidebar" aria-label="Proiecte și sesiuni">
      <div className="flex items-center gap-2 border-b border-sidebar-border px-3 py-2.5">
        <label className="min-w-0 flex-1 text-sm">
          <span className="sr-only">Proiect</span>
          <select value={selection.all ? "" : project} onChange={(e) => changeProject(e.target.value)} aria-label="Proiect"
            className="h-10 w-full rounded-lg border border-input bg-card px-2.5 text-[15px] font-medium outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60">
            <option value="">All</option>
            {workspace.projects.map((p) => <option key={p} value={p}>{p}</option>)}
          </select>
        </label>
        <span className="max-w-24 shrink-0 truncate rounded border border-border px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground" title={`Execuția are loc pe ${workspace.host}`}>@{workspace.host}</span>
        <PreviewButton url={previewUrl} className="size-10" />
        <Button type="button" variant="ghost" size="icon-lg" aria-label="Deconectare" onClick={logout} className="size-10"><LogOutIcon aria-hidden /></Button>
      </div>
      {targetNotice}
      {checkoutPanel}
      {created && <p role="status" className="border-b border-sidebar-border px-3 py-2 text-xs">Agent pornit ca „{created}”. Identitatea nu a putut fi confirmată încă: alege-l din listă.</p>}
      <SessionList sessions={orderedRows} phase={sessions.phase} error={sessions.error} selected={selectedKey} unseen={unseenFinished} onSelect={select} onNew={newAgent} project={selection.all ? "All" : project} showProject={selection.all} onDeleted={onDeleted} />
      {boardLink}
      {serverlessLink}
      {desktop && <PushNotifications />}
      <InstallApp />
      <p className="border-t border-sidebar-border px-3 py-2 text-[11px] text-muted-foreground">Telefonul se poate deconecta. Agenții rămân pe server.</p>
    </aside>
  );

  const closeServerless = () => navigate({ project: selection.all ? null : project, session: null, all: selection.all });
  const conversation = selection.serverless ? <ServerlessRuns onBack={desktop ? undefined : closeServerless} /> : sessionId && controller && state ? (
    <ConversationView key={`${project}/${sessionId}`} project={project} previewUrl={previewUrl} row={row} state={state} controller={controller} sessions={orderedRows} unseen={unseenFinished} onSelect={select} showProjects={selection.all} showBack={!desktop} onBack={() => navigate({ project, session: null, all: selection.all })} />
  ) : (
    <section className="flex min-h-0 flex-1 flex-col">
      {!desktop && (
        <header className="flex min-h-14 items-center gap-2 border-b border-border px-2">
          <Button type="button" variant="ghost" size="icon-lg" aria-label="Meniu" onClick={() => setMenuOpen(true)} className="size-11"><MenuIcon aria-hidden className="size-5" /></Button>
          <h1 className="text-[15px] font-semibold">{project}</h1>
        </header>
      )}
      <div className="flex flex-1 flex-col items-center justify-center gap-2 px-6 text-center">
        {missing ? (
          <p role="alert" className="text-sm">Sesiunea nu mai este disponibilă. Alege alta din listă.</p>
        ) : (
          <>
            <p className="text-lg font-medium">Alege o sesiune</p>
            <p className="max-w-xs text-sm text-muted-foreground">Conversațiile Pi apar ca mesaje; ceilalți agenți arată outputul de terminal.</p>
          </>
        )}
        {!desktop && <Button type="button" variant="outline" onClick={() => setMenuOpen(true)} className="mt-2">Vezi sesiunile</Button>}
      </div>
    </section>
  );

  if (desktop) {
    return (
      <div className="grid min-h-0 flex-1 grid-cols-[18rem_minmax(0,1fr)] xl:grid-cols-[20rem_minmax(0,1fr)]">
        <div className="min-h-0 border-r border-sidebar-border">{sidebar}</div>
        <main className="flex min-h-0 min-w-0 flex-col">{conversation}</main>
        <NewAgentDialog key={project} project={project} initialWorktree={currentPath} open={creating && !selection.all} onOpenChange={setCreating} onCreated={onCreated} />
      </div>
    );
  }
  return (
    <>
      {sessionId || selection.serverless ? <main className="flex min-h-0 flex-1 flex-col">{conversation}</main> : (
        <main className="flex min-h-0 flex-1 flex-col">
          <header className="flex min-h-14 items-center gap-2 border-b border-border px-3">
            <h1 className="text-[15px] font-semibold">Sesiuni</h1>
            <span className="rounded border border-border px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">@{workspace.host}</span>
            <Button type="button" variant="ghost" size="icon-lg" aria-label="Deconectare" onClick={logout} className="ml-auto size-11"><LogOutIcon aria-hidden /></Button>
          </header>
          <div className="flex items-center gap-2 border-b border-border px-3 py-2">
            <label className="min-w-0 flex-1 text-sm">
              <span className="sr-only">Proiect</span>
              <select value={selection.all ? "" : project} onChange={(e) => changeProject(e.target.value)} aria-label="Proiect"
                className="h-11 w-full rounded-lg border border-input bg-card px-3 text-base font-medium outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60">
                <option value="">All</option>
                {workspace.projects.map((p) => <option key={p} value={p}>{p}</option>)}
              </select>
            </label>
            <PreviewButton url={previewUrl} className="size-11" />
          </div>
          {targetNotice}
          {checkoutPanel}
          <SessionList sessions={orderedRows} phase={sessions.phase} error={sessions.error} selected={selectedKey} unseen={unseenFinished} onSelect={select} onNew={newAgent} project={selection.all ? "All" : project} showProject={selection.all} onDeleted={onDeleted} />
          {boardLink}
          {serverlessLink}
          <PushNotifications />
          <InstallApp />
        </main>
      )}
      <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
        <SheetContent side="left" className="w-[88vw] max-w-sm gap-0 p-0" aria-describedby={undefined}>
          <SheetTitle className="sr-only">Proiecte și sesiuni</SheetTitle>
          {sidebar}
        </SheetContent>
      </Sheet>
      <NewAgentDialog key={project} project={project} initialWorktree={currentPath} open={creating && !selection.all} onOpenChange={setCreating} onCreated={onCreated} />
    </>
  );
}
