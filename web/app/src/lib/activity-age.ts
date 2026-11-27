/** Romanian elapsed time since persisted session activity; the caller supplies the current clock. */
export function activityAge(timestamp: number | undefined, now: number): string {
  if (!Number.isFinite(timestamp) || timestamp! <= 0) return "Activitate necunoscută";
  const minutes = Math.floor(Math.max(0, now - timestamp!) / 60_000);
  if (minutes < 1) return "acum câteva secunde";
  if (minutes < 60) return `acum ${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `acum ${hours} h`;
  const days = Math.floor(hours / 24);
  return `acum ${days} ${days === 1 ? "zi" : "zile"}`;
}

/** The same age in the few characters a board tile has: 5m, 3h, 12z. */
export function compactAge(timestamp: number | undefined, now: number): string {
  if (!Number.isFinite(timestamp) || timestamp! <= 0) return "";
  const minutes = Math.floor(Math.max(0, now - timestamp!) / 60_000);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  return hours < 24 ? `${hours}h` : `${Math.floor(hours / 24)}z`;
}
