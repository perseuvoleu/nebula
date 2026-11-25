/** One optimistic write: its patch shows over the board until a read that began after the write settled. */
export type Pending<P> = { token: string; key: string; patch: P; settledAt?: number };

export const propose = <P>(pending: Pending<P>[], token: string, key: string, patch: P): Pending<P>[] =>
  [...pending, { token, key, patch }];

export const settle = <P>(pending: Pending<P>[], token: string, at: number): Pending<P>[] =>
  pending.map((p) => p.token === token ? { ...p, settledAt: at } : p);

export const drop = <P>(pending: Pending<P>[], token: string): Pending<P>[] => pending.filter((p) => p.token !== token);

/** Releases every write that a read begun at `readAt` already includes, except those the read still `holds`. */
export function reconcile<P>(pending: Pending<P>[], readAt: number, holds: (key: string) => boolean = () => false): Pending<P>[] {
  const kept = pending.filter((p) => p.settledAt === undefined || p.settledAt >= readAt || holds(p.key));
  return kept.length === pending.length ? pending : kept;
}

export function patchOf<P extends object>(pending: Pending<P>[], key: string): P | undefined {
  const own = pending.filter((p) => p.key === key);
  return own.length ? Object.assign({}, ...own.map((p) => p.patch)) : undefined;
}
