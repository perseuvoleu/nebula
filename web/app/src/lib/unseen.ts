const STORAGE_KEY = "nebula-unseen-finished";

export interface UnseenState {
  statuses: Record<string, string>;
  unseen: string[];
}

const key = (project: string, id: string) => `${project}/${id}`;

export function reconcileUnseen(
  state: UnseenState,
  project: string,
  rows: Array<{ id: string; status: string }>,
  selected: string | null,
): UnseenState {
  const statuses = { ...state.statuses };
  const unseen = new Set(state.unseen);
  const present = new Set(rows.map((row) => key(project, row.id)));

  for (const row of rows) {
    const id = key(project, row.id);
    const previous = statuses[id];
    if (row.status !== "finished" || row.id === selected) unseen.delete(id);
    else if (previous !== undefined && previous !== "finished") unseen.add(id);
    statuses[id] = row.status;
  }

  for (const id of Object.keys(statuses)) {
    if (id.startsWith(`${project}/`) && !present.has(id)) {
      delete statuses[id];
      unseen.delete(id);
    }
  }
  return { statuses, unseen: [...unseen] };
}

export function loadUnseen(): UnseenState {
  try {
    const value = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null");
    if (value && typeof value.statuses === "object" && !Array.isArray(value.statuses)
      && Array.isArray(value.unseen) && value.unseen.every((item: unknown) => typeof item === "string")) return value;
  } catch { /* unavailable or stale browser storage */ }
  return { statuses: {}, unseen: [] };
}

export function saveUnseen(state: UnseenState): void {
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(state)); } catch { /* storage is optional */ }
}
