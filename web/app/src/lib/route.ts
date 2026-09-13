// Selection ↔ URL: /p/<project>/s/<session id>. Non-secret identifiers only.
import { useCallback, useEffect, useState } from "react";

export interface Selection {
  project: string | null;
  session: string | null;
}

const PATTERN = /^\/(?:p\/([A-Za-z0-9._@-]{1,80})(?:\/s\/([A-Za-z0-9._-]{1,80}))?)?\/?$/;

export function parseSelection(pathname: string): Selection {
  const match = PATTERN.exec(pathname);
  return { project: match?.[1] ?? null, session: match?.[2] ?? null };
}

export function selectionPath(selection: Selection): string {
  if (!selection.project) return "/";
  return `/p/${encodeURIComponent(selection.project)}${selection.session ? `/s/${encodeURIComponent(selection.session)}` : ""}`;
}

export function useSelection(): [Selection, (next: Selection, replace?: boolean) => void] {
  const [selection, setSelection] = useState(() => parseSelection(window.location.pathname));
  useEffect(() => {
    const onPop = () => setSelection(parseSelection(window.location.pathname));
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);
  const navigate = useCallback((next: Selection, replace = false) => {
    const path = selectionPath(next);
    if (path !== window.location.pathname) {
      if (replace) window.history.replaceState(null, "", path);
      else window.history.pushState(null, "", path);
    }
    setSelection(next);
  }, []);
  return [selection, navigate];
}
