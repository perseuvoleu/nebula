import type { SessionRow } from "./types.ts";

/** Stable identity within this gateway host, including the real project scope. */
export function sessionKey(row: Pick<SessionRow, "project" | "id">): string {
  return `${row.project}/${row.id}`;
}

/** Attention states (including feedback-blocked), unread finished, then the rest; newest first. */
export function orderSessions(rows: SessionRow[], unseen: Set<string>): SessionRow[] {
  const priority = (row: SessionRow) => row.status === "running" || row.status === "needs_feedback" ? 0
    : row.status === "finished" && unseen.has(sessionKey(row)) ? 1 : 2;
  const stamp = (row: SessionRow) => Number.isFinite(row.status_changed_at) && row.status_changed_at! > 0 ? row.status_changed_at! : 0;
  return [...rows].sort((a, b) => priority(a) - priority(b) || stamp(b) - stamp(a)
    || (sessionKey(a) < sessionKey(b) ? -1 : sessionKey(a) > sessionKey(b) ? 1 : 0));
}
