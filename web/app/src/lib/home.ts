import { type BoardData, type Card, type NeedsYou, STAGES, type Stage, firstWait } from "./board.ts";

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

/** HOME's reading of the board: live cards, the waits not yet approved here, and the cards they wait on, oldest wait first. */
export function homeBoard(board: BoardData | null, approved: ReadonlySet<string>): HomeBoard {
  const cards = (board?.cards ?? []).filter((c) => !c.archived && !c.proposed);
  const waiting = (board?.needs_you ?? []).filter((w) => !approved.has(approvalKey(w)) && cards.some((c) => c.id === w.card));
  const needs = cards.filter((c) => waiting.some((w) => w.card === c.id))
    .sort((a, b) => firstWait(waiting.filter((w) => w.card === a.id))!.status_changed_at - firstWait(waiting.filter((w) => w.card === b.id))!.status_changed_at);
  const byStage = Object.fromEntries(STAGES.map((s) => [s, cards.filter((c) => c.stage === s)])) as Record<Stage, Card[]>;
  return { cards, waiting, needs, byStage,
    counts: { all: cards.length, needs: needs.length, working: byStage.doing.length + byStage.review.length } };
}

/** The order j/k walks: the NEEDS list, then the visible stages in board order, each card once. */
export function homeOrder(home: HomeBoard, stage: Stage | null = null): Card[] {
  const seen = new Set<string>();
  const stages = stage ? [stage] : STAGES;
  return [...home.needs, ...stages.flatMap((s) => home.byStage[s])].filter((c) => !seen.has(c.id) && !!seen.add(c.id));
}

/** The host part of a ref, shown where the prototype shows @host: "#12@leaseweb" → "leaseweb". */
export const refHost = (ref: string): string | null => ref.split("@")[1] ?? null;
