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

export const statusLabel = (status: string): string => STATUS_LABEL[status] ?? status;
export const kindLabel = (kind: string): string => KIND_LABEL[kind] ?? kind;

export function formatTime(value: string | number | null | undefined): string {
  if (!value) return "";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  return date.toLocaleTimeString("ro-RO", { hour: "2-digit", minute: "2-digit" });
}
