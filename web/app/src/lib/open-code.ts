import { useSyncExternalStore } from "react";
import { ApiError, describeError, onTokenCleared } from "./api.ts";
import { cardAction } from "./board-actions.ts";

export type Target = { id: string; host: string };
/** label names the opener, so focus can find a replacement button when a poll re-rendered the row. */
export type OpenDialog = { cardRef: string; target: Target; on: string; hosts: string[]; opener: HTMLElement | null; label: string | null };
export type OpenResult = { cardRef: string; on: string; command: string | null; error: string | null; at: number };
type State = { dialog: OpenDialog | null; pending: string[]; results: OpenResult[] };

const LAPTOP = "nebula-open-laptop";
const RESULTS = "nebula-open-results";
/** A recovery command is worth keeping this long; a plain success only long enough to be read. */
const KEEP_MS = 10 * 60_000;
const SUCCESS_MS = 15_000;

const lifetime = (r: OpenResult) => (r.command || r.error ? KEEP_MS : SUCCESS_MS);
const live = (results: OpenResult[], now = Date.now()) => results.filter((r) => now - r.at < lifetime(r));

function loadResults(): OpenResult[] {
  try {
    return live(JSON.parse(sessionStorage.getItem(RESULTS) ?? "[]") as OpenResult[]);
  } catch {
    return [];
  }
}

let state: State = { dialog: null, pending: [], results: loadResults() };
/** Bumped by every Open gesture and by leaving the route; a lookup that finishes under an older value is dropped. */
let gesture = 0;
const listeners = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  if (next.results) {
    try { sessionStorage.setItem(RESULTS, JSON.stringify(state.results)); } catch { /* Results then last this page. */ }
  }
  listeners.forEach((l) => l());
};
const keep = (result: OpenResult) => {
  set({ results: [...live(state.results).filter((r) => r.cardRef !== result.cardRef), result] });
  setTimeout(() => set({ results: state.results.filter((r) => r !== result) }), lifetime(result));
};
state.results.forEach((r) => setTimeout(() => set({ results: state.results.filter((x) => x !== r) }), lifetime(r) - (Date.now() - r.at)));
onTokenCleared(() => set({ dialog: null, pending: [], results: [] }));

export function rememberedLaptop(): string | null {
  try {
    return localStorage.getItem(LAPTOP);
  } catch {
    return null;
  }
}

/** The dialog lives here, not in the row that opened it, so a poll that unmounts the row cannot close it. */
export const openCode = {
  /** Starts an Open gesture; pass the returned token to open() once the target is known. */
  begin: () => ++gesture,
  open: (token: number, dialog: OpenDialog) => {
    if (token !== gesture || state.dialog) return false;
    set({ dialog });
    return true;
  },
  report: (cardRef: string, error: string) => keep({ cardRef, on: "", command: null, error, at: Date.now() }),
  /** The route the dialog or lookup started on is gone: nothing it started may surface on the next one. */
  leave: () => {
    gesture += 1;
    if (state.dialog) set({ dialog: null });
  },
  current: (token: number) => token === gesture,
  choose: (on: string) => state.dialog && set({ dialog: { ...state.dialog, on } }),
  cancel: () => set({ dialog: null }),
  dismiss: (cardRef: string) => set({ results: state.results.filter((r) => r.cardRef !== cardRef) }),
  async confirm() {
    const dialog = state.dialog;
    if (!dialog) return;
    const { cardRef, target, on } = dialog;
    try { localStorage.setItem(LAPTOP, on); } catch { /* Without storage the choice lasts this visit. */ }
    set({ dialog: null, pending: [...state.pending, cardRef], results: state.results.filter((r) => r.cardRef !== cardRef) });
    let result: OpenResult;
    try {
      await cardAction(cardRef, "open-code", { session: target.id, host: target.host, on });
      result = { cardRef, on, command: null, error: null, at: Date.now() };
    } catch (failure) {
      const command = failure instanceof ApiError && failure.status === 409 && typeof failure.data.command === "string" ? failure.data.command : null;
      result = { cardRef, on, command, error: command ? null : describeError(failure), at: Date.now() };
    }
    set({ pending: state.pending.filter((r) => r !== cardRef) });
    keep(result);
  },
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useOpenCode = () => useSyncExternalStore(subscribe, () => state);

/** Where focus goes when the opener left the page: its replacement by name, else the fallback. */
export function focusBack(opener: HTMLElement | null, label: string | null, fallback: () => HTMLElement | null) {
  const replacement = label ? [...document.querySelectorAll<HTMLElement>("[aria-label]")].find((el) => el.getAttribute("aria-label") === label) : undefined;
  (opener?.isConnected ? opener : replacement ?? fallback())?.focus();
}
