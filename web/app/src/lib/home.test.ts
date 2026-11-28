import test from "node:test";
import assert from "node:assert/strict";
import type { BoardData, Card } from "./board.ts";
import { approvalKey, chatThread, filterByProject, homeBoard, homeOrder, homeProjects, refHost } from "./home.ts";
import type { Message } from "./types.ts";

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

test("j/k walks NEEDS first, then the visible stages, each card once", () => {
  const home = homeBoard(board([card(1, "review"), card(2, "doing"), card(3, "backlog"), card(6, "done")]), new Set());
  assert.deepEqual(homeOrder(home).map((c) => c.ref), ["#1", "#3", "#2", "#6"]);
  assert.deepEqual(homeOrder(home, "doing").map((c) => c.ref), ["#1", "#3", "#2"]);
  assert.equal(refHost("#12@leaseweb"), "leaseweb");
  assert.equal(refHost("#12"), null);
});

test("projectScope filters cards, NEEDS, counts, order and decisions; homeProjects merges board and workspace projects", () => {
  const cards = [
    card(1, "review", { project: "nebula" }),
    card(2, "doing", { project: "alpha" }),
    card(3, "backlog", { project: "nebula" }),
    card(4, "done", { project: "alpha", archived: true }),
    card(5, "next", { project: null }),
  ];
  const b = board(cards, [wait("c1", 10), wait("c2", 20)]);
  const scoped = homeBoard(b, new Set(), "alpha");
  assert.deepEqual(scoped.cards.map((c) => c.ref), ["#2"]);
  assert.deepEqual(scoped.needs.map((c) => c.ref), ["#2"]);
  assert.deepEqual(scoped.counts, { all: 1, needs: 1, working: 1 });
  assert.deepEqual(homeOrder(scoped).map((c) => c.ref), ["#2"]);

  assert.deepEqual(homeProjects(b, new Set(), ["beta", "alpha"]), [
    { name: "alpha", cards: 1, needs: 1, working: 1 },
    { name: "beta", cards: 0, needs: 0, working: 0 },
    { name: "nebula", cards: 2, needs: 1, working: 1 },
  ]);

  const items = [{ id: "d1", project: "alpha" }, { id: "d2", project: "nebula" }, { id: "d3", project: null }];
  assert.deepEqual(filterByProject(items, null).map((d) => d.id), ["d1", "d2", "d3"]);
  assert.deepEqual(filterByProject(items, "alpha").map((d) => d.id), ["d1"]);
});

test("chatThread keeps the last text turns of user and assistant, joining text parts and dropping tools and notices", () => {
  const msg = (id: string, role: Message["role"], parts: Message["parts"]): Message => ({ id, role, timestamp: null, parts });
  const text = (t: string) => ({ type: "text" as const, text: t });
  const thread = chatThread([
    msg("u1", "user", [text("first")]),
    msg("a1", "assistant", [text("one"), text("two")]),
    msg("t1", "tool", [text("tool output")]),
    msg("n1", "notice", [text("notice")]),
    msg("a2", "assistant", [text("  ")]),
    msg("u2", "user", [text("latest")]),
  ], 2);
  assert.deepEqual(thread.map((l) => [l.id, l.role, l.text]), [["a1", "assistant", "one\ntwo"], ["u2", "user", "latest"]]);
});
