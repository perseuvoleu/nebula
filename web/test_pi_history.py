"""Pi native-history adapter tests with synthetic fixtures (no real transcripts)."""
import json
import os
import tempfile
import unittest
from pathlib import Path

from pi_history import (PiHistoryError, apply_cursor, locate_session_file, normalize,
                        read_entries, session_dir_for, snapshot)

SID_A = "11111111-aaaa-4bbb-8ccc-111111111111"
SID_B = "22222222-aaaa-4bbb-8ccc-222222222222"


def entry(kind, eid, parent, **fields):
    return {"type": kind, "id": eid, "parentId": parent, "timestamp": "2026-09-13T10:00:00.000Z", **fields}


def user(eid, parent, text):
    return entry("message", eid, parent, message={"role": "user", "content": text, "timestamp": 1})


def assistant(eid, parent, content, stop="stop"):
    return entry("message", eid, parent, message={"role": "assistant", "content": content, "api": "x",
                                                 "provider": "p", "model": "m", "usage": {}, "stopReason": stop,
                                                 "timestamp": 2})


def tool_result(eid, parent, call_id, text, error=False):
    return entry("message", eid, parent, message={"role": "toolResult", "toolCallId": call_id, "toolName": "bash",
                                                 "content": [{"type": "text", "text": text}], "isError": error,
                                                 "timestamp": 3})


def header(sid, cwd):
    return {"type": "session", "version": 3, "id": sid, "timestamp": "2026-09-13T10:00:00.000Z", "cwd": cwd}


def write_session(agent_dir, cwd, sid, entries, name=None):
    directory = session_dir_for(agent_dir, cwd)
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / f"{name or '2026-09-13T10-00-00-000Z'}_{sid}.jsonl"
    with open(path, "w") as handle:
        for item in [header(sid, cwd), *entries]:
            handle.write(json.dumps(item) + "\n")
    return path


class LocateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.agent_dir = Path(self.tmp.name) / "agent"
        self.cwd = "/srv/checkout"

    def tearDown(self):
        self.tmp.cleanup()

    def test_two_sessions_in_one_checkout_resolve_by_native_id(self):
        a = write_session(self.agent_dir, self.cwd, SID_A, [user("u1", None, "alpha")])
        b = write_session(self.agent_dir, self.cwd, SID_B, [user("u1", None, "beta")], name="2026-09-13T11-00-00-000Z")
        self.assertEqual(locate_session_file(self.agent_dir, self.cwd, SID_A), a)
        self.assertEqual(locate_session_file(self.agent_dir, self.cwd, SID_B), b)

    def test_missing_ambiguous_or_mismatched_sessions_fail_closed(self):
        with self.assertRaises(PiHistoryError):
            locate_session_file(self.agent_dir, self.cwd, SID_A)  # nothing persisted yet
        write_session(self.agent_dir, self.cwd, SID_A, [])
        write_session(self.agent_dir, self.cwd, SID_A, [], name="2026-09-13T12-00-00-000Z")
        with self.assertRaises(PiHistoryError):
            locate_session_file(self.agent_dir, self.cwd, SID_A)  # duplicate files for one id
        directory = session_dir_for(self.agent_dir, self.cwd)
        (directory / f"x_{SID_B}.jsonl").write_text(json.dumps(header(SID_A, self.cwd)) + "\n")
        with self.assertRaises(PiHistoryError):
            locate_session_file(self.agent_dir, self.cwd, SID_B)  # header id mismatch

    def test_rejects_unsafe_identifiers_and_symlink_escapes(self):
        for bad in ["../../etc/passwd", "*", "", "a/b", "x" * 200]:
            with self.assertRaises(PiHistoryError):
                locate_session_file(self.agent_dir, self.cwd, bad)
        outside = Path(self.tmp.name) / "outside.jsonl"
        outside.write_text(json.dumps(header(SID_A, self.cwd)) + "\n")
        directory = session_dir_for(self.agent_dir, self.cwd)
        directory.mkdir(parents=True)
        os.symlink(outside, directory / f"z_{SID_A}.jsonl")
        with self.assertRaises(PiHistoryError):
            locate_session_file(self.agent_dir, self.cwd, SID_A)


class NormalizeTests(unittest.TestCase):
    def test_roles_tools_hidden_and_unknown_entries(self):
        entries = [
            user("u1", None, "Run the tests"),
            assistant("a1", "u1", [{"type": "thinking", "thinking": "secret plan"},
                                   {"type": "text", "text": "Running."},
                                   {"type": "toolCall", "id": "call1", "name": "bash", "arguments": {"command": "make test"}}],
                      stop="toolUse"),
            tool_result("t1", "a1", "call1", "ok\n" * 3),
            entry("custom_message", "c1", "t1", customType="nebula-context", content="hidden ctx", display=False),
            entry("custom_message", "c2", "c1", customType="my-ext", content="visible note", display=True),
            entry("custom", "c3", "c2", customType="my-ext", data={"count": 1}),
            entry("model_change", "m1", "c3", provider="p", modelId="m2"),
            entry("mystery_future_type", "z1", "m1", payload={"x": 1}),
            entry("message", "b1", "z1", message={"role": "bashExecution", "command": "ls", "output": "a\nb",
                                                 "exitCode": 0, "cancelled": False, "truncated": False, "timestamp": 4}),
            assistant("a2", "b1", [{"type": "text", "text": "Done."}]),
        ]
        result = normalize(entries)
        roles = [(m["id"], m["role"]) for m in result["messages"]]
        self.assertEqual(roles, [("u1", "user"), ("a1", "assistant"), ("c2", "notice"), ("b1", "tool"), ("a2", "assistant")])
        assistant_parts = result["messages"][1]["parts"]
        self.assertEqual([p["type"] for p in assistant_parts], ["text", "tool"])
        tool = assistant_parts[1]
        self.assertEqual((tool["callId"], tool["name"], tool["state"]), ("call1", "bash", "done"))
        self.assertEqual(tool["output"], "ok\nok\nok\n")
        self.assertNotIn("secret plan", json.dumps(result))
        self.assertNotIn("hidden ctx", json.dumps(result))
        self.assertEqual(result["unknownEntries"], 1)
        self.assertEqual(result["messages"][3]["parts"][0]["name"], "bash!")

    def test_active_branch_excludes_abandoned_path_and_marks_compaction(self):
        entries = [
            user("u1", None, "first"),
            assistant("a1", "u1", [{"type": "text", "text": "one"}]),
            user("u2", "a1", "abandoned question"),
            assistant("a2", "u2", [{"type": "text", "text": "abandoned answer"}]),
            entry("branch_summary", "bs", "a1", fromId="a2", summary="Left branch tried X"),
            user("u3", "bs", "second try"),
            assistant("a3", "u3", [{"type": "text", "text": "two"}]),
            entry("compaction", "cp", "a3", summary="Earlier work summarized", tokensBefore=100,
                  retainedTail=[{"role": "user", "content": "second try", "timestamp": 1}]),
            user("u4", "cp", "after compaction"),
        ]
        result = normalize(entries)
        ids = [m["id"] for m in result["messages"]]
        self.assertEqual(ids, ["u1", "a1", "bs", "u3", "a3", "cp", "u4"])
        self.assertEqual(result["messages"][5]["role"], "notice")
        self.assertNotIn("abandoned answer", json.dumps(result))

    def test_pending_tool_and_bounded_outputs(self):
        big = "x" * 50000
        entries = [
            user("u1", None, "go"),
            assistant("a1", "u1", [{"type": "toolCall", "id": "c1", "name": "read", "arguments": {"path": big}},
                                   {"type": "toolCall", "id": "c2", "name": "bash", "arguments": {"command": "ls"}}],
                      stop="toolUse"),
            tool_result("t1", "a1", "c1", big, error=True),
        ]
        result = normalize(entries)
        parts = result["messages"][1]["parts"]
        self.assertEqual(parts[0]["state"], "error")
        self.assertTrue(parts[0]["truncated"])
        self.assertLess(len(parts[0]["output"]), 10000)
        self.assertEqual(parts[0]["outputLength"], 50000)
        self.assertLess(len(parts[0]["args"]), 10000)
        self.assertEqual(parts[1]["state"], "pending")


class ReplayTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.agent_dir = Path(self.tmp.name)
        self.cwd = "/srv/checkout"
        self.path = write_session(self.agent_dir, self.cwd, SID_A,
                                  [user("u1", None, "hi"), assistant("a1", "u1", [{"type": "text", "text": "hello"}])])

    def tearDown(self):
        self.tmp.cleanup()

    def test_partial_trailing_line_is_not_consumed(self):
        with open(self.path, "a") as handle:
            handle.write('{"type":"message","id":"u2","parentId":"a1","message":{"role":"user","con')
        entries, offset, partial = read_entries(self.path)
        self.assertEqual([e["id"] for e in entries], ["u1", "a1"])
        self.assertTrue(partial)
        with open(self.path, "a") as handle:
            handle.write('tent":"more"}}\n')
        more, offset2, partial2 = read_entries(self.path, offset)
        self.assertEqual([e["id"] for e in more], ["u2"])
        self.assertFalse(partial2)
        self.assertGreater(offset2, offset)

    def test_incremental_append_and_resets(self):
        first = snapshot(self.path, SID_A)
        self.assertEqual([m["id"] for m in first["messages"]], ["u1", "a1"])
        with open(self.path, "a") as handle:
            handle.write(json.dumps(user("u2", "a1", "again")) + "\n")
            handle.write(json.dumps(tool_result("t9", "u2", "c-missing", "orphan")) + "\n")
        delta = apply_cursor(self.path, SID_A, first["cursor"])
        self.assertEqual(delta["kind"], "delta")
        self.assertEqual([m["id"] for m in delta["messages"]], ["u2"])
        self.assertEqual([r["callId"] for r in delta["toolResults"]], ["c-missing"])
        # A branch jump (new entry parented to an older node) invalidates the path.
        with open(self.path, "a") as handle:
            handle.write(json.dumps(user("u3", "u1", "branch")) + "\n")
        reset = apply_cursor(self.path, SID_A, delta["cursor"])
        self.assertEqual(reset["kind"], "reset")
        self.assertEqual([m["id"] for m in reset["messages"]], ["u1", "u3"])
        # A different native session generation never continues an old cursor.
        other = apply_cursor(self.path, SID_B, reset["cursor"])
        self.assertEqual(other["kind"], "reset")
        # A rewritten/shorter file also resets instead of replaying garbage.
        self.path.write_text(json.dumps(header(SID_A, self.cwd)) + "\n" + json.dumps(user("u1", None, "hi")) + "\n")
        self.assertEqual(apply_cursor(self.path, SID_A, reset["cursor"])["kind"], "reset")
        unchanged = apply_cursor(self.path, SID_A, snapshot(self.path, SID_A)["cursor"])
        self.assertEqual(unchanged["kind"], "none")

    def test_snapshot_paging_is_bounded(self):
        entries, parent = [], None
        for index in range(30):
            entries.append(user(f"u{index}", parent, f"q{index}"))
            entries.append(assistant(f"a{index}", f"u{index}", [{"type": "text", "text": f"r{index}"}]))
            parent = f"a{index}"
        path = write_session(self.agent_dir, "/srv/other", SID_B, entries)
        tail = snapshot(path, SID_B, limit=10)
        self.assertEqual(len(tail["messages"]), 10)
        self.assertEqual(tail["messages"][0]["id"], "u25")
        self.assertTrue(tail["hasMore"])
        older = snapshot(path, SID_B, limit=10, before="u25")
        self.assertEqual([older["messages"][0]["id"], older["messages"][-1]["id"]], ["u20", "a24"])
        self.assertEqual(snapshot(path, SID_B, limit=1000)["hasMore"], False)


if __name__ == "__main__":
    unittest.main()
