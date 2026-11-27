// Romanian labels for daemon statuses and agent kinds. Status is conveyed by text, never color alone.
export const STATUS_LABEL: Record<string, string> = {
  fresh: "Nou",
  running: "Lucrează",
  finished: "Gata",
  needs_feedback: "Așteaptă răspuns",
  terminated: "Oprit",
  disconnected: "Deconectat",
};

export const KIND_LABEL: Record<string, string> = {
  pi: "Pi",
  claude: "Claude",
  codex: "Codex",
  cursor: "Cursor",
};

/** The coloured dot a status shows, shared by board tiles and session rows. */
export type Dot = "needs" | "running" | "done" | "queued" | "off";

export const DOT_LABEL: Record<Dot, string> = {
  needs: STATUS_LABEL.needs_feedback,
  running: STATUS_LABEL.running,
  done: STATUS_LABEL.finished,
  queued: STATUS_LABEL.fresh,
  off: "Inactiv",
};

export function sessionDot(status: string): Dot {
  if (status === "needs_feedback") return "needs";
  if (status === "running") return "running";
  if (status === "finished") return "done";
  if (status === "fresh") return "queued";
  return "off";
}

export const statusLabel = (status: string): string => STATUS_LABEL[status] ?? status;
export const kindLabel = (kind: string): string => KIND_LABEL[kind] ?? kind;

export function formatTime(value: string | number | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString("ro-RO", { hour: "2-digit", minute: "2-digit" });
}

export function formatWhen(value: string | number): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const year = date.getFullYear() === new Date().getFullYear() ? undefined : "numeric";
  return date.toLocaleString("ro-RO", { day: "numeric", month: "short", year, hour: "2-digit", minute: "2-digit" });
}
