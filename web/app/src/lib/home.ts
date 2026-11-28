import { type BoardData, type Card, type NeedsYou, STAGES, type Stage, firstWait } from "./board.ts";
import type { Message } from "./types.ts";

export const STAGE_LETTER: Record<Stage, string> = { backlog: "B", next: "N", doing: "D", review: "R", done: "✓" };

export const STAGE_TEXT: Record<Stage, string> = {
  backlog: "text-backlog", next: "text-info", doing: "text-warn", review: "text-special", done: "text-ok",
};

/** An approval answers one waiting state, so it stays hidden until the session leaves needs_you or waits anew. */
export const approvalKey = (w: NeedsYou) => `${w.host}/${w.id}@${w.status_changed_at}`;

export type HomeBoard = {
  cards: Card[];
  waiting: NeedsYou[];
  needs: Card[];
  byStage: Record<Stage, Card[]>;
  counts: { all: number; needs: number; working: number };
};

export type HomeProject = {
  name: string;
  cards: number;
  needs: number;
  working: number;
};

/** HOME's reading of the board: live cards, the waits not yet approved here, and the cards they wait on, oldest wait first. */
export function homeBoard(board: BoardData | null, approved: ReadonlySet<string>, projectScope: string | null = null): HomeBoard {
  const cards = (board?.cards ?? []).filter((c) => !c.archived && !c.proposed && (projectScope === null || c.project === projectScope));
  const waiting = (board?.needs_you ?? []).filter((w) => !approved.has(approvalKey(w)) && cards.some((c) => c.id === w.card));
  const needs = cards.filter((c) => waiting.some((w) => w.card === c.id))
    .sort((a, b) => firstWait(waiting.filter((w) => w.card === a.id))!.status_changed_at - firstWait(waiting.filter((w) => w.card === b.id))!.status_changed_at);
  const byStage = Object.fromEntries(STAGES.map((s) => [s, cards.filter((c) => c.stage === s)])) as Record<Stage, Card[]>;
  return { cards, waiting, needs, byStage,
    counts: { all: cards.length, needs: needs.length, working: byStage.doing.length + byStage.review.length } };
}

/** Every named project present in live cards or the gateway's workspace, with its live card counts. */
export function homeProjects(board: BoardData | null, approved: ReadonlySet<string>, workspaceProjects: readonly string[] = []): HomeProject[] {
  const live = (board?.cards ?? []).filter((c) => !c.archived && !c.proposed);
  const waitingCards = new Set(
    (board?.needs_you ?? []).filter((w) => !approved.has(approvalKey(w))).map((w) => w.card),
  );
  const map = new Map<string, HomeProject>();
  const ensure = (name: string) => {
    let entry = map.get(name);
    if (!entry) {
      entry = { name, cards: 0, needs: 0, working: 0 };
      map.set(name, entry);
    }
    return entry;
  };
  for (const name of workspaceProjects) ensure(name);
  for (const card of live) {
    if (!card.project) continue;
    const entry = ensure(card.project);
    entry.cards += 1;
    if (waitingCards.has(card.id)) entry.needs += 1;
    if (card.stage === "doing" || card.stage === "review") entry.working += 1;
  }
  return [...map.values()].sort((a, b) => a.name.localeCompare(b.name));
}

export function filterByProject<T extends { project: string | null }>(items: T[], projectScope: string | null): T[] {
  return projectScope === null ? items : items.filter((item) => item.project === projectScope);
}

/** The order j/k walks: the NEEDS list, then the visible stages in board order, each card once. */
export function homeOrder(home: HomeBoard, stage: Stage | null = null): Card[] {
  const seen = new Set<string>();
  const stages = stage ? [stage] : STAGES;
  return [...home.needs, ...stages.flatMap((s) => home.byStage[s])].filter((c) => !seen.has(c.id) && !!seen.add(c.id));
}

/** The host part of a ref, shown where the prototype shows @host: "#12@leaseweb" → "leaseweb". */
export const refHost = (ref: string): string | null => ref.split("@")[1] ?? null;

export type ChatLine = { id: string; role: "user" | "assistant"; text: string; at: string | null };

/** The last `limit` user and assistant turns that carry text, for the compact thread on a HOME card. */
export function chatThread(messages: Message[], limit = 6): ChatLine[] {
  return messages.flatMap((m): ChatLine[] => {
    if (m.role !== "user" && m.role !== "assistant") return [];
    const text = m.parts.flatMap((p) => p.type === "text" && p.text.trim() ? [p.text] : []).join("\n");
    return text ? [{ id: m.id, role: m.role, text, at: m.timestamp }] : [];
  }).slice(-limit);
}
