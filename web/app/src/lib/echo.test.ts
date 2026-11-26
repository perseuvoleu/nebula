import test from "node:test";
import assert from "node:assert/strict";
import { echoWatch } from "./echo.ts";

type State = "sending" | "accepted";
const attempt = (id: string, text: string, state: State = "accepted") => ({ id, text, state });
const lines = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, i) => `line ${from + i}`).join("\n") + "\n";

function sentAt(output: string, ...sends: { id: string; text: string }[]) {
  const update = echoWatch();
  update([], output);
  for (let i = 0; i < sends.length; i++) update([...sends.slice(0, i).map((s) => attempt(s.id, s.text)), attempt(sends[i].id, sends[i].text, "sending")], output);
  update(sends.map((s) => attempt(s.id, s.text)), output);
  return update;
}

test("a message wider than the pane is recognised when the terminal wraps its echo across rows, even mid-word", () => {
  const text = "Refactorizează modulul de autentificare și păstrează testele verzi";
  const update = sentAt("$ gata\n", { id: "a", text });
  assert.deepEqual(update([attempt("a", text)], "$ gata\n> Refactorizează modulul de autentifi\ncare și păstrează testele verzi\n"), ["a"]);
});

test("a short message waits for its own echo, not for output that happens to contain its letters", () => {
  const update = sentAt("$ gata\n", { id: "a", text: "ok" });
  assert.deepEqual(update([attempt("a", "ok")], "$ gata\n2 tokens\n"), []);
  assert.deepEqual(update([attempt("a", "ok")], "$ gata\n2 tokens\n> ok\n"), ["a"]);
});

test("two identical messages each wait for an echo of their own", () => {
  const update = sentAt("$ gata\n", { id: "a1", text: "continuă" }, { id: "a2", text: "continuă" });
  assert.deepEqual(update([attempt("a1", "continuă"), attempt("a2", "continuă")], "$ gata\n> continuă\n"), ["a1"]);
  assert.deepEqual(update([attempt("a2", "continuă")], "$ gata\n> continuă\nlucrez\n"), []);
  assert.deepEqual(update([attempt("a2", "continuă")], "$ gata\n> continuă\nlucrez\n> continuă\n"), ["a2"]);
});

test("an echo that arrives before Nebula accepts the send clears it on acceptance, without further output", () => {
  const update = echoWatch();
  update([], "$ gata\n");
  update([attempt("a", "Oprește", "sending")], "$ gata\n");
  assert.deepEqual(update([attempt("a", "Oprește", "sending")], "$ gata\n> Oprește\n"), []);
  assert.deepEqual(update([attempt("a", "Oprește")], "$ gata\n> Oprește\n"), ["a"]);
});

test("an older read that brings an earlier echo back does not count as delivery, and one that also brings the new echo does", () => {
  const older = `${lines(1, 10)}> continuă\n${lines(11, 50)}`;
  const update = sentAt(lines(51, 100), { id: "a", text: "continuă" });
  assert.deepEqual(update([attempt("a", "continuă")], older + lines(51, 100), true), []);
  assert.deepEqual(sentAt(lines(51, 100), { id: "a", text: "continuă" })([attempt("a", "continuă")], `${older}${lines(51, 100)}> continuă\n`, true), ["a"]);
});

test("an earlier echo scrolling out of the retained window does not hide the new one", () => {
  const update = sentAt(`> continuă\n${lines(1, 3)}`, { id: "a", text: "continuă" });
  assert.deepEqual(update([attempt("a", "continuă")], lines(2, 5)), []);
  assert.deepEqual(update([attempt("a", "continuă")], `${lines(3, 5)}> continuă\n`), ["a"]);
});
