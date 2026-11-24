import type { SessionRow } from "./types.ts";
import { sessionKey } from "./session-list.ts";

/** Project-scoped gateway boundary; only the supplied allowlist is queried. */
export interface SessionSource {
  /** Fetch the current sessions of one real, accessible project. */
  sessions(project: string): Promise<SessionRow[]>;
}

/** Load each requested project once; one failed project never substitutes another project's rows. */
export async function fetchSessionList(projects: readonly string[], source: SessionSource, signal: AbortSignal) {
  const rows = new Map<string, SessionRow>();
  const loaded: string[] = [];
  const errors: Array<{ project: string; failure: unknown }> = [];
  for (const project of new Set(projects)) {
    signal.throwIfAborted();
    try {
      const sessions = await source.sessions(project);
      signal.throwIfAborted();
      loaded.push(project);
      for (const session of sessions) {
        const row = { ...session, project };
        if (!row.archived) rows.set(sessionKey(row), row);
      }
    } catch (failure) {
      signal.throwIfAborted();
      errors.push({ project, failure });
    }
  }
  return { rows: [...rows.values()], loaded, errors };
}
