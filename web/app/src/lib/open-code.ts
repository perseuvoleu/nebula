import { useSyncExternalStore } from "react";
import { ApiError, authGeneration, describeError, onTokenCleared } from "./api.ts";
import { cardAction } from "./board-actions.ts";

export type Target = { id: string; host: string };
/**
 * label and scope (the opener's data-open-scope: tile, strip, card) let focus find the opener's copy
 * when a poll re-rendered its row; cardId lets a card page reached by id find the result.
 */
export type OpenDialog = { cardRef: string; cardId: string | null; target: Target; on: string; hosts: string[]; opener: HTMLElement | null; label: string | null; scope: string | null };
export type OpenResult = { cardRef: string; cardId: string | null; on: string; command: string | null; error: string | null; at: number };

/** key is a card route slug: "12@leaseweb" or the card's id. */
export const resultFor = (r: OpenResult, key: string | null) => key !== null && (r.cardId === key || r.cardRef === `#${key}`);
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
  report: (cardRef: string, cardId: string | null, error: string) => keep({ cardRef, cardId, on: "", command: null, error, at: Date.now() }),
  /** Another board dialog opened: a lookup still running must not open over it. */
  invalidate: () => { gesture += 1; },
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
    const { cardRef, cardId, target, on } = dialog;
    const signedIn = authGeneration();
    try { localStorage.setItem(LAPTOP, on); } catch { /* Without storage the choice lasts this visit. */ }
    set({ dialog: null, pending: [...state.pending, cardRef], results: state.results.filter((r) => r.cardRef !== cardRef) });
    let result: OpenResult;
    try {
      await cardAction(cardRef, "open-code", { session: target.id, host: target.host, on });
      result = { cardRef, cardId, on, command: null, error: null, at: Date.now() };
    } catch (failure) {
      const command = failure instanceof ApiError && failure.status === 409 && typeof failure.data.command === "string" ? failure.data.command : null;
      result = { cardRef, cardId, on, command, error: command ? null : describeError(failure), at: Date.now() };
    }
    if (signedIn !== authGeneration()) return;
    set({ pending: state.pending.filter((r) => r !== cardRef) });
    keep(result);
  },
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useOpenCode = () => useSyncExternalStore(subscribe, () => state);

/**
 * Where focus goes when the opener left the page: its copy in the same kind of place, then any copy, then the fallback.
 * Without force it moves focus only when focus was lost (on body); a closing dialog passes force.
 */
export function focusBack(opener: HTMLElement | null, label: string | null, scope: string | null, fallback: () => HTMLElement | null, force = false) {
  if (!force && document.activeElement && document.activeElement !== document.body) return;
  const copies = label ? [...document.querySelectorAll<HTMLElement>("[aria-label]")].filter((el) => el.getAttribute("aria-label") === label) : [];
  const replacement = copies.find((el) => scope !== null && el.closest("[data-open-scope]")?.getAttribute("data-open-scope") === scope) ?? copies[0];
  (opener?.isConnected ? opener : replacement ?? fallback())?.focus();
}
