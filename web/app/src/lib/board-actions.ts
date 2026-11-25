import { api } from "@/lib/api";
import { type Card, type Stage, cardSlug } from "@/lib/board";

const CROCKFORD = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/** A client-minted id, so repeating an action whose outcome is unknown never does it twice. */
export function ulid(now = Date.now()): string {
  let time = "";
  for (let rest = now, i = 0; i < 10; i++, rest = Math.floor(rest / 32)) time = CROCKFORD[rest % 32] + time;
  const random = crypto.getRandomValues(new Uint8Array(16));
  return time + Array.from(random, (b) => CROCKFORD[b % 32]).join("");
}

export type NewCard = { title: string; project: string | null; stage: Stage; id: string };

export const createCard = ({ title, project, stage, id }: NewCard) =>
  api<Card>("cards", {}, { title, stage, id, ...(project === null ? { home: true } : { project }) });

type Verbs = {
  move: { stage: Stage; expect?: Stage };
  note: { text: string; id: string };
  accept: Record<string, never>;
  dismiss: Record<string, never>;
  block: { by: string };
  unblock: { by: string };
  run: { role: string; kind: string; host: string; worktree: string; dispatch: string };
  approve: { session: string; host: string; expectedStatusAt: number };
  "open-code": { session: string; host: string; on: string };
};

export const cardAction = <V extends keyof Verbs>(ref: string, verb: V, body: Verbs[V]) =>
  api<unknown>(`cards/${encodeURIComponent(cardSlug(ref))}/${verb}`, {}, body);
