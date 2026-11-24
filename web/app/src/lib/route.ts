// Selection ↔ URL: /p/<project>/s/<session id> or /serverless. Non-secret identifiers only.
import { useCallback, useEffect, useState } from "react";

export interface Selection {
  /** Real project identity; never the virtual All label. */
  project: string | null;
  session: string | null;
  /** Keep the aggregate list context when opening a real project session. */
  all: boolean;
  /** The serverless runs section; overrides project and session in the URL. */
  serverless?: boolean;
  board?: BoardRoute;
}

export interface BoardRoute {
  project: string | null;
  /** Without the ref's leading "#", which a URL reads as a fragment. */
  card: string | null;
  decisions: boolean;
  table: boolean;
  peek: string | null;
}

const NAME = "([A-Za-z0-9._@-]{1,80})";
const BOARD = new RegExp(`^/board(?:/p/${NAME}|/c/${NAME}|/decisions(?:/p/${NAME})?)?/?$`);
const REF = /^[A-Za-z0-9._@-]{1,80}$/;

function parseBoard(pathname: string, params: URLSearchParams): BoardRoute | null {
  const match = BOARD.exec(pathname.replace(/%40/gi, "@"));
  if (!match) return null;
  const peek = params.get("peek");
  return { project: match[1] ?? match[3] ?? null, card: match[2] ?? null, decisions: pathname.startsWith("/board/decisions"),
    table: params.get("view") === "table", peek: peek && REF.test(peek) ? peek : null };
}

function boardPath(route: BoardRoute): string {
  const project = route.project ? `/p/${encodeURIComponent(route.project)}` : "";
  const path = route.card ? `/board/c/${encodeURIComponent(route.card)}` : route.decisions ? `/board/decisions${project}` : `/board${project}`;
  const params = new URLSearchParams();
  if (route.table) params.set("view", "table");
  if (route.peek) params.set("peek", route.peek);
  return params.size ? `${path}?${params}` : path;
}

const PATTERN = /^\/(?:p\/([A-Za-z0-9._@-]{1,80})(?:\/s\/([A-Za-z0-9._-]{1,80}))?)?\/?$/;

/** Decode a list or conversation route, including its aggregate list context. */
export function parseSelection(path: string): Selection {
  const [pathname, query] = path.split("?");
  if (/^\/serverless\/?$/.test(pathname)) return { project: null, session: null, all: false, serverless: true };
  const board = parseBoard(pathname, new URLSearchParams(query));
  if (board) return { project: null, session: null, all: false, board };
  const match = PATTERN.exec(pathname);
  return { project: match?.[1] ?? null, session: match?.[2] ?? null,
    all: new URLSearchParams(query).get("view") === "all" };
}

/** Encode real session identity separately from the virtual All filter. */
export function selectionPath(selection: Selection): string {
  if (selection.serverless) return "/serverless";
  if (selection.board) return boardPath(selection.board);
  if (selection.all && !selection.session) return "/?view=all";
  if (!selection.project) return "/";
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
