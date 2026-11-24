import test from "node:test";
import assert from "node:assert/strict";
import { awayDigest, byStage, isBlocked, writerSlots, type BoardData, type Card } from "./board.ts";

const card = (number: number, stage: Card["stage"], blocked_by: string[] = []): Card => ({
  id: `01JD${String(number).padStart(22, "0")}`, number, ref: `#${number}@leaseweb`, title: `Card ${number}`, stage,
  project: "nebula", proposed: false, archived: false, created_by: { host: "leaseweb", agent: null, name: "web" },
  checkout: null, blocked_by,
});

test("byStage puts every card in its stage column, in board order, with every column present", () => {
  const cards = [card(1, "doing"), card(2, "backlog"), card(3, "doing")];
  assert.deepEqual(Object.fromEntries(Object.entries(byStage(cards)).map(([stage, list]) => [stage, list.map((c) => c.number)])),
    { backlog: [2], next: [], doing: [1, 3], review: [], done: [] });
});

test("a card stays blocked until every blocker is done, and a blocker the board cannot see keeps it blocked", () => {
  const done = card(1, "done");
  const review = card(2, "review");
  assert.equal(isBlocked(card(3, "next", [done.ref]), [done, review]), false);
  assert.equal(isBlocked(card(3, "next", [done.ref, review.ref]), [done, review]), true);
  assert.equal(isBlocked(card(3, "next", ["#99@leaseweb"]), [done, review]), true);
  assert.equal(isBlocked(card(3, "next"), [done, review]), false);
});

test("writerSlots reports live writers per host and the slot cap only when the host has one", () => {
  const host = (name: string, writers: number, writer_slots: number | null) =>
    ({ host: name, writers, writer_slots, checkouts: [], needs_you: 0, stale: false });
  assert.deepEqual(writerSlots([host("leaseweb", 1, null), host("perseu-mbp", 2, 4)]), [
    { host: "leaseweb", writers: 1, slots: null },
    { host: "perseu-mbp", writers: 2, slots: 4 },
  ]);
});

test("awayDigest resolves finished card ids to cards and keeps needs-you sessions and new decisions", () => {
  const finished = card(4, "review");
  const board: BoardData = {
    cards: [card(1, "doing"), finished],
    hosts: [],
    away: { finished: [finished.id, "01JD9999999999999999999999"], needs_you: [{ host: "leaseweb", id: "01JF0000000000000000000002" }],
      decisions: ["01JG0000000000000000000002"] },
  };
  const digest = awayDigest(board);
  assert.deepEqual(digest.finished.map((c) => c.number), [4]);
  assert.deepEqual(digest.needsYou, [{ host: "leaseweb", id: "01JF0000000000000000000002" }]);
  assert.deepEqual(digest.decisions, ["01JG0000000000000000000002"]);
  assert.equal(digest.empty, false);
  assert.equal(awayDigest({ cards: [], hosts: [], away: { finished: [], needs_you: [], decisions: [] } }).empty, true);
});
