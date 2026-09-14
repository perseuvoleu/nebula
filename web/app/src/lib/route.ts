// Selection ↔ URL: /p/<project>/s/<session id>. Non-secret identifiers only.
import { useCallback, useEffect, useState } from "react";

export interface Selection {
  /** Real project identity; never the virtual All label. */
  project: string | null;
  session: string | null;
  /** Keep the aggregate list context when opening a real project session. */
  all: boolean;
}

const PATTERN = /^\/(?:p\/([A-Za-z0-9._@-]{1,80})(?:\/s\/([A-Za-z0-9._-]{1,80}))?)?\/?$/;

/** Decode a list or conversation route, including its aggregate list context. */
export function parseSelection(path: string): Selection {
  const [pathname, query] = path.split("?");
  const match = PATTERN.exec(pathname);
  return { project: match?.[1] ?? null, session: match?.[2] ?? null,
    all: new URLSearchParams(query).get("view") === "all" };
}

/** Encode real session identity separately from the virtual All filter. */
export function selectionPath(selection: Selection): string {
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
