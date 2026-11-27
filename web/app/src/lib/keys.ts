import { useEffect, useRef } from "react";
import type { Tab } from "./route.ts";

export const TAB_KEYS: Record<string, Tab> = { 1: "home", 2: "code", 3: "rag", 4: "decisions" };

/** A key belongs to the page only when nothing is being typed and no dialog is open. */
export function pageKey(e: KeyboardEvent): boolean {
  if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return false;
  const target = e.target instanceof HTMLElement ? e.target : null;
  if (target?.closest("input,textarea,select,[contenteditable=true],[role=dialog]")) return false;
  return !document.querySelector("[role=dialog],[role=alertdialog]");
}

/** Calls handler for page keys; a handler returning true consumed the key. */
export function usePageKeys(handler: (key: string) => boolean | void, enabled = true) {
  const latest = useRef(handler);
  useEffect(() => { latest.current = handler; });
  useEffect(() => {
    if (!enabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (pageKey(e) && latest.current(e.key)) e.preventDefault();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [enabled]);
}
