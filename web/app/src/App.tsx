import { useCallback, useEffect, useRef, useState } from "react";
import { LogOutIcon, MenuIcon } from "lucide-react";
import { api, ApiError, describeError, getToken, onAuthChange, setToken } from "@/lib/api";
import { useConversation } from "@/lib/conversation";
import { clearDrafts } from "@/lib/drafts";
import { useSelection } from "@/lib/route";
import type { NewResponse, SessionRow, Worktree } from "@/lib/types";
import { Button } from "@/components/ui/button";
import { Sheet, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { ConversationView } from "./components/ConversationView";
import { Login } from "./components/Login";
import { NewAgentDialog } from "./components/NewAgentDialog";
import { SessionList, type ListPhase } from "./components/SessionList";
import { CheckoutPanel } from "./components/CheckoutPanel";
import { InstallApp } from "./components/InstallApp";

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

function useSessions(project: string | null) {
  const [rows, setRows] = useState<SessionRow[]>([]);
  const [phase, setPhase] = useState<ListPhase>("loading");
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);
  const refresh = useCallback(async () => {
    if (!project) return;
    const version = ++generation.current;
    try {
      const data = await api<{ sessions: SessionRow[] }>("sessions", { project }, undefined, { timeoutMs: 20000 });
      if (version !== generation.current) return;
      setRows(data.sessions); setPhase("ready"); setError(null);
    } catch (failure) {
      if (version !== generation.current) return;
      if (failure instanceof ApiError && failure.status === 401) return;
      setError(describeError(failure));
      setPhase((p) => (p === "loading" ? "error" : navigator.onLine ? p : "offline"));
    }
  }, [project]);
  useEffect(() => {
    setRows([]); setPhase("loading"); setError(null);
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
    return () => { cancelled = true; generation.current += 1; if (timer) clearTimeout(timer); document.removeEventListener("visibilitychange", wake); window.removeEventListener("online", wake); };
  }, [refresh]);
  return { rows, phase, error, refresh };
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
  return <Shell workspace={workspace} />;
}

function Shell({ workspace }: { workspace: Workspace }) {
  const [selection, navigate] = useSelection();
  const project = selection.project && workspace.projects.includes(selection.project) ? selection.project : workspace.projects[0];
  const sessions = useSessions(project);
  const desktop = useMediaQuery("(min-width: 768px)");
  const [menuOpen, setMenuOpen] = useState(false);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<string | null>(null);
  const [checkout, setCheckout] = useState<{ project: string; path: string } | null>(null);

  // Normalise the URL to a valid project once the allowlist is known.
  useEffect(() => { if (selection.project !== project) navigate({ project, session: selection.session }, true); }, [project, selection.project, selection.session, navigate]);

  const row = sessions.rows.find((r) => r.id === selection.session) ?? null;
  const currentPath = row?.path ?? (checkout?.project === project ? checkout.path : undefined);
  const chooseCheckout = (tree: Worktree) => {
    setCheckout({ project, path: tree.path });
    navigate({ project, session: null });
    setMenuOpen(false);
  };
  const checkoutPanel = <CheckoutPanel key={project} project={project} currentPath={currentPath} onSelect={chooseCheckout} />;
  const sessionId = selection.session;
  const missing = sessionId !== null && sessions.phase === "ready" && !row;
  const { state, controller } = useConversation(project, sessionId && !missing ? sessionId : null);

  const select = (r: SessionRow) => { navigate({ project, session: r.id }); setMenuOpen(false); setCreated(null); };
  const onCreated = (result: NewResponse) => {
    void sessions.refresh();
    // Identity comes from the gateway; a title is never used to guess it (the agent may rename).
    if (result.id) navigate({ project, session: result.id }); else setCreated(result.name);
    setMenuOpen(false);
  };
  const logout = () => { controller?.dispose(); setToken(""); navigate({ project: null, session: null }, true); };

  const sidebar = (
    <aside className="flex h-full min-h-0 flex-col bg-sidebar" aria-label="Proiecte și sesiuni">
      <div className="flex items-center gap-2 border-b border-sidebar-border px-3 py-2.5">
        <label className="min-w-0 flex-1 text-sm">
          <span className="sr-only">Proiect</span>
          <select value={project} onChange={(e) => navigate({ project: e.target.value, session: null })} aria-label="Proiect"
            className="h-10 w-full rounded-lg border border-input bg-card px-2.5 text-[15px] font-medium outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60">
            {workspace.projects.map((p) => <option key={p} value={p}>{p}</option>)}
          </select>
        </label>
        <span className="max-w-24 shrink-0 truncate rounded border border-border px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground" title={`Execuția are loc pe ${workspace.host}`}>@{workspace.host}</span>
        <Button type="button" variant="ghost" size="icon-lg" aria-label="Deconectare" onClick={logout} className="size-10"><LogOutIcon aria-hidden /></Button>
      </div>
      {checkoutPanel}
      {created && <p role="status" className="border-b border-sidebar-border px-3 py-2 text-xs">Agent pornit ca „{created}”. Identitatea nu a putut fi confirmată încă: alege-l din listă.</p>}
      <SessionList sessions={sessions.rows} phase={sessions.phase} error={sessions.error} selected={sessionId} onSelect={select} onNew={() => setCreating(true)} project={project} />
      <InstallApp />
      <p className="border-t border-sidebar-border px-3 py-2 text-[11px] text-muted-foreground">Telefonul se poate deconecta. Agenții rămân pe server.</p>
    </aside>
  );

  const conversation = sessionId && controller && state ? (
    <ConversationView key={`${project}/${sessionId}`} project={project} row={row} state={state} controller={controller} sessions={sessions.rows} onSelect={select} showBack={!desktop} onBack={() => navigate({ project, session: null })} />
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
        <NewAgentDialog project={project} initialWorktree={currentPath} open={creating} onOpenChange={setCreating} onCreated={onCreated} />
      </div>
    );
  }
  return (
    <>
      {sessionId ? <main className="flex min-h-0 flex-1 flex-col">{conversation}</main> : (
        <main className="flex min-h-0 flex-1 flex-col">
          <header className="flex min-h-14 items-center gap-2 border-b border-border px-3">
            <h1 className="text-[15px] font-semibold">Sesiuni</h1>
            <span className="rounded border border-border px-1.5 py-0.5 font-mono text-[11px] text-muted-foreground">@{workspace.host}</span>
            <Button type="button" variant="ghost" size="icon-lg" aria-label="Deconectare" onClick={logout} className="ml-auto size-11"><LogOutIcon aria-hidden /></Button>
          </header>
          <div className="flex items-center gap-2 border-b border-border px-3 py-2">
            <label className="min-w-0 flex-1 text-sm">
              <span className="sr-only">Proiect</span>
              <select value={project} onChange={(e) => navigate({ project: e.target.value, session: null })} aria-label="Proiect"
                className="h-11 w-full rounded-lg border border-input bg-card px-3 text-base font-medium outline-none focus-visible:border-ring focus-visible:ring-2 focus-visible:ring-ring/60">
                {workspace.projects.map((p) => <option key={p} value={p}>{p}</option>)}
              </select>
            </label>
          </div>
          {checkoutPanel}
          <SessionList sessions={sessions.rows} phase={sessions.phase} error={sessions.error} selected={sessionId} onSelect={select} onNew={() => setCreating(true)} project={project} />
          <InstallApp />
        </main>
      )}
      <Sheet open={menuOpen} onOpenChange={setMenuOpen}>
        <SheetContent side="left" className="w-[88vw] max-w-sm gap-0 p-0" aria-describedby={undefined}>
          <SheetTitle className="sr-only">Proiecte și sesiuni</SheetTitle>
          {sidebar}
        </SheetContent>
      </Sheet>
      <NewAgentDialog project={project} initialWorktree={currentPath} open={creating} onOpenChange={setCreating} onCreated={onCreated} />
    </>
  );
}
