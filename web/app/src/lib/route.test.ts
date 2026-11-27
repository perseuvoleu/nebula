import test from "node:test";
import assert from "node:assert/strict";
import { parseSelection, selectionPath, tabOf, type BoardRoute } from "./route.ts";

test("All is a virtual list route; opening preserves real project identity and aggregate context", () => {
  assert.equal(selectionPath({ project: null, session: null, all: true }), "/code?view=all");
  assert.deepEqual(parseSelection("/code?view=all"), { project: null, session: null, all: true });
  assert.deepEqual(parseSelection("/?view=all"), { project: null, session: null, all: true }, "an old All link still opens the list");
  for (const project of ["alpha", "beta"]) {
    const selection = { project, session: "same", all: true };
    assert.equal(selectionPath(selection), `/p/${project}/s/same?view=all`);
    assert.deepEqual(parseSelection(selectionPath(selection)), selection);
  }
  assert.deepEqual(parseSelection("/p/alpha/s/same"), { project: "alpha", session: "same", all: false });
  assert.equal(selectionPath({ project: "alpha@findl", session: "one", all: false }), "/p/alpha%40findl/s/one");
  assert.deepEqual(parseSelection("/p/alpha%40findl/s/one"), { project: "alpha@findl", session: "one", all: false });
  assert.deepEqual(parseSelection("/p/alpha@findl/s/one"), { project: "alpha@findl", session: "one", all: false });
});

test("/serverless is its own section and does not shadow project or session routes", () => {
  assert.deepEqual(parseSelection("/serverless"), { project: null, session: null, all: false, serverless: true });
  for (const origin of [{ project: "beta", all: false }, { project: "all", all: false }, { project: null, all: true }]) {
    const opened = parseSelection(selectionPath({ ...origin, session: "one", serverless: true }));
    assert.deepEqual(opened, { ...origin, session: null, serverless: true }, "the list it was opened from survives a reload");
  }
  assert.deepEqual(parseSelection("/p/serverless/s/one"), { project: "serverless", session: "one", all: false });
  assert.deepEqual(parseSelection("/serverless/x"), { project: null, session: null, all: false });
});

test("board routes parse and round-trip: project, card page, decisions, table view and peek", () => {
  const board = (route: Partial<BoardRoute>) => ({ project: null, session: null, all: false,
    board: { project: null, card: null, decisions: false, table: false, peek: null, ...route } });
  const cases: [string, ReturnType<typeof board>][] = [
    ["/board", board({})],
    ["/board/p/nebula", board({ project: "nebula" })],
    ["/board/p/nebula?view=table", board({ project: "nebula", table: true })],
    ["/board?peek=12%40leaseweb", board({ peek: "12@leaseweb" })],
    ["/board/p/nebula?view=table&peek=12", board({ project: "nebula", table: true, peek: "12" })],
    ["/board/c/12%40leaseweb", board({ card: "12@leaseweb" })],
    ["/board/decisions", board({ decisions: true })],
    ["/board/decisions/p/nebula", board({ decisions: true, project: "nebula" })],
    ["/board/home", board({ home: true })],
    ["/board/home?view=table&peek=3", board({ home: true, table: true, peek: "3" })],
    ["/board/home/c/3%40leaseweb", board({ home: true, card: "3@leaseweb" })],
    ["/board/p/nebula/c/12%40leaseweb", board({ project: "nebula", card: "12@leaseweb" })],
    ["/board/decisions/home", board({ decisions: true, home: true })],
    ["/board/p/home", board({ project: "home" })],
  ];
  for (const [path, selection] of cases) {
    assert.deepEqual(parseSelection(path), selection, path);
    assert.equal(selectionPath(selection), path);
  }
});

test("board routes do not shadow session routes, and a malformed board path falls back to the session list", () => {
  assert.deepEqual(parseSelection("/p/board/s/one"), { project: "board", session: "one", all: false });
  assert.deepEqual(parseSelection("/p/alpha"), { project: "alpha", session: null, all: false });
  assert.deepEqual(parseSelection("/board/x/y"), { project: null, session: null, all: false });
});

test("the app's tabs: / is HOME, a selected card is /c/<ref>, CODE is /code and the session routes, RAG and DECISIONS have their own", () => {
  const home = (card: string | null) => ({ project: null, session: null, all: false, view: "home" as const, card });
  const cases: [string, ReturnType<typeof parseSelection>][] = [
    ["/", home(null)],
    ["/c/12%40leaseweb", home("12@leaseweb")],
    ["/c/3", home("3")],
    ["/code", { project: null, session: null, all: false }],
    ["/rag", { project: null, session: null, all: false, view: "rag" }],
    ["/decisions", { project: null, session: null, all: false, view: "decisions" }],
  ];
  for (const [path, selection] of cases) {
    assert.deepEqual(parseSelection(path), selection, path);
    assert.equal(selectionPath(selection), path);
  }
  assert.equal(tabOf(parseSelection("/")), "home");
  assert.equal(tabOf(parseSelection("/p/alpha/s/one")), "code");
  assert.equal(tabOf(parseSelection("/serverless")), "code");
  assert.equal(tabOf(parseSelection("/rag")), "rag");
  assert.equal(tabOf(parseSelection("/decisions")), "decisions");
  assert.equal(tabOf(parseSelection("/board")), null);
});
