import test from "node:test";
import assert from "node:assert/strict";
import { awayDigest, byStage, cardDot, hostProjects, inScope, isBlocked, sessionProject, sidebarProjects, toBoardCard, writerSlots, type BoardData, type Card, type SidebarScope } from "./board.ts";

const card = (number: number, stage: Card["stage"], blocked_by: string[] = []): Card => ({
  id: `01JD${String(number).padStart(22, "0")}`, number, ref: `#${number}@leaseweb`, title: `Card ${number}`, stage,
  project: "nebula", proposed: false, archived: false, created_by: { host: "leaseweb", agent: null, name: "web" },
  checkout: null, blocked_by, cleared_blockers: [], created_at: 1764000000000, updated_at: 1764000000000,
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

test("a blocker the board marks cleared no longer blocks, though the board cannot see that archived card", () => {
  const archived = { ...card(3, "next", ["#1@leaseweb", "#2@leaseweb"]), cleared_blockers: ["#1@leaseweb"] };
  assert.deepEqual(toBoardCard(archived, [card(2, "review")]).blockers, [{ ref: "#2@leaseweb", title: "Card 2" }]);
  assert.equal(isBlocked({ ...archived, cleared_blockers: ["#1@leaseweb", "#2@leaseweb"] }, []), false);
});

test("writerSlots reports live writers per host and the slot cap only when the host has one", () => {
  const host = (name: string, writers: number, writer_slots: number | null) =>
    ({ host: name, writers, writer_slots, checkouts: [], needs_you: 0, stale: false });
  assert.deepEqual(writerSlots([host("leaseweb", 1, null), host("perseu-mbp", 2, 4)]), [
    { host: "leaseweb", writers: 1, slots: null },
    { host: "perseu-mbp", writers: 2, slots: 4 },
  ]);
});

test("sidebarProjects counts every unarchived card of a project, Done included", () => {
  const board: BoardData = { cards: [card(1, "doing"), card(2, "done"), { ...card(3, "done"), archived: true }], hosts: [], needs_you: [],
    away: { finished: [], needs_you: [], decisions: [] } };
  assert.deepEqual(sidebarProjects(board).map((p) => [p.name, p.cards]), [["nebula", 2]]);
});

test("projectless cards and a project named home are separate sidebar projects, each scoping only its own cards", () => {
  const home = { ...card(1, "doing"), project: null };
  const named = { ...card(2, "doing"), project: "home" };
  const projects = sidebarProjects({ cards: [home, named], hosts: [], needs_you: [], away: { finished: [], needs_you: [], decisions: [] } });
  assert.equal(projects.length, 2);
  for (const p of projects) {
    const scoped = [home, named].map((c) => toBoardCard(c, [])).filter((c) => inScope(c, { kind: "project", project: p.project }));
    assert.deepEqual([p.cards, scoped.map((c) => c.ref)], [1, [p.project === null ? home.ref : named.ref]]);
  }
});

test("a board card without session data makes no status claim", () => {
  const listed = toBoardCard(card(1, "doing"), []);
  assert.deepEqual([listed.sessions, listed.needsYou, cardDot(listed)], [null, false, null]);
});

test("awayDigest lists the finished cards, the cards waiting since last seen and the new decisions of the scope's project, and counts only those", () => {
  const finished = card(4, "review");
  const vela = { ...card(5, "done"), project: "vela" };
  const decisions = [{ id: "01JG0000000000000000000002", project: "nebula" }, { id: "01JG0000000000000000000003", project: "vela" }];
  const board: BoardData = {
    cards: [card(1, "doing"), finished, vela],
    hosts: [], needs_you: [], needs_you: [],
    away: { finished: [finished.id, vela.id, "01JD9999999999999999999999"], needs_you: [{ host: "leaseweb", id: "01JF0000000000000000000002", card: finished.id, status_changed_at: 1764000200000 }],
      decisions: decisions.map((d) => d.id) },
  };
  const listed = (scope: SidebarScope) => {
    const digest = awayDigest(board, scope, decisions);
    return [digest.finished.map((c) => c.number), digest.waiting.map((c) => c.number), digest.decisions.map((d) => d.project), digest.empty];
  };
  assert.deepEqual(listed({ kind: "all" }), [[4, 5], [4], ["nebula", "vela"], false]);
  assert.deepEqual(listed({ kind: "project", project: "nebula" }), [[4], [4], ["nebula"], false]);
  assert.deepEqual(listed({ kind: "project", project: "vela" }), [[5], [], ["vela"], false]);
  assert.deepEqual(listed({ kind: "project", project: null }), [[], [], [], true]);
  const onlyNeedsYou = { ...board, away: { ...board.away, finished: [], decisions: [] } };
  assert.equal(awayDigest(onlyNeedsYou, { kind: "all" }, decisions).empty, false);
});

test("a session is listed under its bare project on this gateway's host and under project@host for an anchored remote", () => {
  const workspace = { host: "leaseweb", projects: ["nebula", "vela@findl"] };
  assert.equal(sessionProject("nebula", "leaseweb", workspace), "nebula");
  assert.equal(sessionProject("vela", "findl", workspace), "vela@findl");
  assert.equal(sessionProject("nebula", "findl", workspace), null, "a host the gateway does not anchor has no session page here");
  assert.deepEqual(hostProjects("findl", workspace), ["vela@findl"]);
  assert.deepEqual(hostProjects("leaseweb", workspace), ["nebula"]);
});
