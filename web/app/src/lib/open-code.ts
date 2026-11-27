import { useSyncExternalStore } from "react";
import { ApiError, describeError } from "./api.ts";
import { cardAction } from "./board-actions.ts";

export type Target = { id: string; host: string };
export type OpenDialog = { cardRef: string; target: Target; on: string; opener: HTMLElement | null };
export type OpenResult = { cardRef: string; on: string; command: string | null; error: string | null };
type State = { dialog: OpenDialog | null; pending: string[]; results: OpenResult[] };

const LAPTOP = "nebula-open-laptop";
/** A recovery command is worth showing on the card page for this long after the gateway returned it. */
const KEEP_MS = 10 * 60_000;

let state: State = { dialog: null, pending: [], results: [] };
const listeners = new Set<() => void>();
const set = (next: Partial<State>) => {
  state = { ...state, ...next };
  listeners.forEach((l) => l());
};

export function rememberedLaptop(): string | null {
  try {
    return localStorage.getItem(LAPTOP);
  } catch {
    return null;
  }
}

/** The dialog lives here, not in the row that opened it, so a poll that unmounts the row cannot close it. */
export const openCode = {
  open: (dialog: OpenDialog) => set({ dialog }),
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
      result = { cardRef, on, command: null, error: null };
    } catch (failure) {
      const command = failure instanceof ApiError && failure.status === 409 && typeof failure.data.command === "string" ? failure.data.command : null;
      result = { cardRef, on, command, error: command ? null : describeError(failure) };
    }
    set({ pending: state.pending.filter((r) => r !== cardRef), results: [...state.results.filter((r) => r.cardRef !== cardRef), result] });
    setTimeout(() => set({ results: state.results.filter((r) => r !== result) }), KEEP_MS);
  },
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  return () => listeners.delete(listener);
};

export const useOpenCode = () => useSyncExternalStore(subscribe, () => state);
