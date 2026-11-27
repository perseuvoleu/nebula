import test from "node:test";
import assert from "node:assert/strict";
import type { BoardData, Card } from "./board.ts";
import { approvalKey, homeBoard, homeOrder, refHost } from "./home.ts";

const card = (n: number, stage: Card["stage"], extra: Partial<Card> = {}): Card => ({
  id: `c${n}`, number: n, ref: `#${n}`, title: `Card ${n}`, stage, project: "nebula", proposed: false, archived: false,
  created_by: { host: "h", agent: null, name: "you" }, checkout: null, blocked_by: [], cleared_blockers: [], created_at: 0, updated_at: 0, ...extra,
});
const wait = (cardId: string, at: number) => ({ host: "h", id: `s-${cardId}`, name: "worker", card: cardId, status_changed_at: at });
const board = (cards: Card[], needs_you = [wait("c3", 20), wait("c1", 10)]): BoardData =>
  ({ cards, hosts: [], needs_you, away: { finished: [], needs_you: [], decisions: [] } });

test("NEEDS lists the cards a session waits on, longest wait first; counts skip archived and proposed cards", () => {
  const home = homeBoard(board([card(1, "review"), card(2, "doing"), card(3, "backlog"), card(4, "done", { archived: true }), card(5, "next", { proposed: true })]), new Set());
  assert.deepEqual(home.needs.map((c) => c.ref), ["#1", "#3"]);
  assert.deepEqual(home.counts, { all: 3, needs: 2, working: 2 });
  assert.deepEqual(home.byStage.doing.map((c) => c.ref), ["#2"]);
});

test("an approved wait leaves NEEDS at once, and a new wait of the same session brings it back", () => {
  const cards = [card(1, "review"), card(3, "backlog")];
  const approved = new Set([approvalKey(wait("c1", 10))]);
  assert.deepEqual(homeBoard(board(cards), approved).needs.map((c) => c.ref), ["#3"]);
  assert.deepEqual(homeBoard(board(cards, [wait("c1", 99)]), approved).needs.map((c) => c.ref), ["#1"]);
});

test("j/k walks NEEDS first, then the stages, each card once", () => {
  const home = homeBoard(board([card(1, "review"), card(2, "doing"), card(3, "backlog"), card(6, "done")]), new Set());
  assert.deepEqual(homeOrder(home).map((c) => c.ref), ["#1", "#3", "#2", "#6"]);
  assert.equal(refHost("#12@leaseweb"), "leaseweb");
  assert.equal(refHost("#12"), null);
});
