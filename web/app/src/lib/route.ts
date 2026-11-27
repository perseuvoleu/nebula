// Selection ↔ URL. HOME is / (a selected card /c/<ref>), CODE is /code, /p/<project>/s/<session id> and /serverless,
// then /rag, /decisions and the kanban under /board. Non-secret identifiers only.
import { useCallback, useEffect, useState } from "react";

export interface Selection {
  /** Real project identity; never the virtual All label. */
  project: string | null;
  session: string | null;
  /** Keep the aggregate list context when opening a real project session. */
  all: boolean;
  /** The serverless runs section; the URL keeps only the list it was opened from (`?from=<project>` or `?view=all`). */
  serverless?: boolean;
  board?: BoardRoute;
  /** HOME, RAG or DECISIONS; absent for CODE and the board. */
  view?: "home" | "rag" | "decisions";
  /** HOME's selected card, without the ref's leading "#". */
  card?: string | null;
}

export type Tab = "home" | "code" | "rag" | "decisions";

/** The tab a route belongs to; null for the kanban, which has no tab. */
export const tabOf = (selection: Selection): Tab | null => selection.board ? null : selection.view ?? "code";

export interface BoardRoute {
  project: string | null;
  /** Cards without a project; apart from a project named "home". */
  home?: boolean;
  /** Without the ref's leading "#", which a URL reads as a fragment. */
  card: string | null;
  decisions: boolean;
  table: boolean;
  peek: string | null;
}

const NAME = "([A-Za-z0-9._@-]{1,80})";
const SCOPE = `(?:/p/${NAME}|/(home))?`;
const BOARD = new RegExp(`^/board(?:/decisions${SCOPE}|${SCOPE}(?:/c/${NAME})?)/?$`);
const REF = /^[A-Za-z0-9._@-]{1,80}$/;

function parseBoard(pathname: string, params: URLSearchParams): BoardRoute | null {
  const match = BOARD.exec(pathname.replace(/%40/gi, "@"));
  if (!match) return null;
  const peek = params.get("peek");
  const home = !!(match[2] ?? match[4]);
  return { project: match[1] ?? match[3] ?? null, ...(home && { home }), card: match[5] ?? null, decisions: pathname.startsWith("/board/decisions"),
    table: params.get("view") === "table", peek: peek && REF.test(peek) ? peek : null };
}

function boardPath(route: BoardRoute): string {
  const scope = route.home ? "/home" : route.project ? `/p/${encodeURIComponent(route.project)}` : "";
  const path = route.card ? `/board${scope}/c/${encodeURIComponent(route.card)}` : route.decisions ? `/board/decisions${scope}` : `/board${scope}`;
  const params = new URLSearchParams();
  if (route.table) params.set("view", "table");
  if (route.peek) params.set("peek", route.peek);
  return params.size ? `${path}?${params}` : path;
}

const PATTERN = /^\/(?:p\/([A-Za-z0-9._@-]{1,80})(?:\/s\/([A-Za-z0-9._-]{1,80}))?)?\/?$/;

/** Decode a list or conversation route, including its aggregate list context. */
export function parseSelection(path: string): Selection {
  const [pathname, query] = path.split("?");
  if (/^\/serverless\/?$/.test(pathname)) {
    const params = new URLSearchParams(query);
    const from = params.get("from");
    const all = params.get("view") === "all";
    return { project: !all && from && REF.test(from) ? from : null, session: null, all, serverless: true };
  }
  const board = parseBoard(pathname, new URLSearchParams(query));
  if (board) return { project: null, session: null, all: false, board };
  const all = new URLSearchParams(query).get("view") === "all";
  if (/^\/?$/.test(pathname) && !all) return { project: null, session: null, all: false, view: "home", card: null };
  const card = /^\/c\/([A-Za-z0-9._@-]{1,80})\/?$/.exec(pathname.replace(/%40/gi, "@"));
  if (card) return { project: null, session: null, all: false, view: "home", card: card[1] };
  if (/^\/(rag|decisions)\/?$/.test(pathname)) return { project: null, session: null, all: false, view: pathname.replace(/\//g, "") as "rag" | "decisions" };
  if (/^\/code\/?$/.test(pathname)) return { project: null, session: null, all };
  const match = PATTERN.exec(pathname);
  return { project: match?.[1] ?? null, session: match?.[2] ?? null,
    all: new URLSearchParams(query).get("view") === "all" };
}

/** Encode real session identity separately from the virtual All filter. */
export function selectionPath(selection: Selection): string {
  if (selection.serverless) {
    if (selection.all) return "/serverless?view=all";
    return selection.project ? `/serverless?from=${encodeURIComponent(selection.project)}` : "/serverless";
  }
  if (selection.board) return boardPath(selection.board);
  if (selection.view === "home") return selection.card ? `/c/${encodeURIComponent(selection.card)}` : "/";
  if (selection.view) return `/${selection.view}`;
  if (selection.all && !selection.session) return "/code?view=all";
  if (!selection.project) return "/code";
  return `/p/${encodeURIComponent(selection.project)}${selection.session ? `/s/${encodeURIComponent(selection.session)}` : ""}${selection.all ? "?view=all" : ""}`;
}

export function useSelection(): [Selection, (next: Selection, replace?: boolean) => void] {
  const [selection, setSelection] = useState(() => parseSelection(window.location.pathname + window.location.search));
  useEffect(() => {
    const onPop = () => setSelection(parseSelection(window.location.pathname + window.location.search));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = useCallback((next: Selection, replace = false) => {
    const path = selectionPath(next);
    if (path !== window.location.pathname + window.location.search) {
      if (replace) window.history.replaceState(null, "", path);
      else window.history.pushState(null, "", path);
    }
    setSelection(next);
  }, []);
  return [selection, navigate];
}
