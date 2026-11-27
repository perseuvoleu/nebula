import { useEffect, useState } from "react";
import { api, describeError } from "@/lib/api";
import { type BoardData, type Card, type CardShow, type SessionLink, type Workspace, GRANT_RENEW_MS, cardSlug, hostProjects, sessionProject } from "@/lib/board";
import { type BoardRoute, selectionPath } from "@/lib/route";
import { BOARD_POLL_MS } from "@/lib/useBoard";
import type { DecisionRow } from "@/components/board/DecisionCard";

export type Decision = { id: string; title: string; card: string | null; project: string | null; session: SessionLink | null;
  content: { body?: string }; created_at: number };

export const boardHref = (route: Partial<BoardRoute>) => selectionPath({ project: null, session: null, all: false,
  board: { project: null, card: null, decisions: false, table: false, peek: null, ...route } });

const sessionKey = (s: SessionLink) => `${s.host}/${s.id}`;

export function decisionRow(d: Decision, cards: Card[], workspace: Workspace, homeSessions: Map<string, string>): DecisionRow {
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

export function useCard(slug: string | null, board: BoardData | null) {
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
  const expires = Math.min(...(state?.slug === slug ? state.show?.artifacts ?? [] : []).flatMap((a) => a.expires ?? []));
  useEffect(() => {
    if (!Number.isFinite(expires)) return;
    const renew = setTimeout(() => setAttempt((n) => n + 1), Math.max(BOARD_POLL_MS, expires * 1000 - GRANT_RENEW_MS - Date.now()));
    return () => clearTimeout(renew);
  }, [expires]);
  return state?.slug === slug ? { ...state, refresh: () => setAttempt((n) => n + 1) } : null;
}

async function readDecisions(query: string, wanted: string[] | null, signal: AbortSignal): Promise<Decision[]> {
  const items: Decision[] = [];
  let page: string | null = null;
  do {
    const data: { items: Decision[]; next: string | null } = await api("decisions", { query: query || undefined, page: page ?? undefined }, undefined, { signal });
    items.push(...data.items);
    page = data.next;
  } while (page && !wanted?.every((id) => items.some((d) => d.id === id)));
  return items;
}

export function useDecisions(page: boolean, wanted: string[], query: string, board: BoardData | null) {
  const [state, setState] = useState<{ query: string; items: Decision[]; error: string | null } | null>(null);
  const [attempt, setAttempt] = useState(0);
  const enabled = page || wanted.length > 0;
  const want = page ? null : wanted.join(" ");
  const changed = page ? board : want;
  useEffect(() => {
    if (!enabled) return;
    const abort = new AbortController();
    let retry: ReturnType<typeof setTimeout> | undefined;
    readDecisions(query, want === null ? null : want.split(" "), abort.signal)
      .then((items) => setState({ query, items, error: null }))
      .catch((failure) => {
        if (abort.signal.aborted) return;
        setState((old) => ({ query, items: old?.query === query ? old.items : [], error: describeError(failure) }));
        retry = setTimeout(() => setAttempt((n) => n + 1), BOARD_POLL_MS);
      });
    return () => { abort.abort(); clearTimeout(retry); };
  }, [enabled, query, want, changed, attempt]);
  const current = enabled && state?.query === query ? state : null;
  return { items: current?.items ?? [], error: current?.error ?? null, loading: enabled && !current };
}

/** Home decisions carry no project, so their sessions are found in the lists of the projects on their host. */
export function useHomeSessions(sessions: SessionLink[], workspace: Workspace): Map<string, string> {
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
