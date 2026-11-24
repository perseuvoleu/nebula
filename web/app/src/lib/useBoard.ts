import { useEffect, useRef, useState } from "react";
import { api, ApiError } from "@/lib/api";
import type { BoardData, BoardReply } from "@/lib/board";

const BOARD_POLL_MS = 3000;

async function readBoard(since: string | undefined, awaySince: number | null, signal: AbortSignal) {
  const first = await api<BoardReply>("board", { since, awaySince: awaySince === null ? undefined : String(awaySince) }, undefined, { signal });
  if ("unchanged" in first) return null;
  const board = { ...first.board, cards: [...first.board.cards] };
  for (let page = first.next; page;) {
    const next = await api<BoardReply>("board", { page }, undefined, { signal });
    if ("unchanged" in next) break;
    board.cards.push(...next.board.cards);
    page = next.next;
  }
  return { version: first.version, board };
}

/** Polls the board every 3 s with one request in flight, skips hidden tabs, and keeps the last board marked stale on failure. */
export function useBoard(awaySince: number | null): { board: BoardData | null; stale: boolean } {
  const [state, setState] = useState<{ board: BoardData | null; stale: boolean }>({ board: null, stale: false });
  const version = useRef<string | undefined>(undefined);
  useEffect(() => {
    version.current = undefined;
    const abort = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    let busy = false;
    const poll = async () => {
      clearTimeout(timer);
      if (busy || abort.signal.aborted) return;
      if (!document.hidden) {
        busy = true;
        try {
          const next = await readBoard(version.current, awaySince, abort.signal);
          if (next) version.current = next.version;
          setState((old) => next ? { board: next.board, stale: false } : old.stale ? { ...old, stale: false } : old);
        } catch (failure) {
          if (!abort.signal.aborted && !(failure instanceof ApiError && failure.status === 401)) setState((old) => ({ ...old, stale: true }));
        } finally {
          busy = false;
        }
      }
      if (!abort.signal.aborted) timer = setTimeout(poll, BOARD_POLL_MS);
    };
    const wake = () => { if (!document.hidden) void poll(); };
    void poll();
    document.addEventListener("visibilitychange", wake);
    return () => { abort.abort(); clearTimeout(timer); document.removeEventListener("visibilitychange", wake); };
  }, [awaySince]);
  return state;
}
