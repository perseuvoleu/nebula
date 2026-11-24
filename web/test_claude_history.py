"""Claude history boundary tests use synthetic transcripts, never personal sessions."""
import json
from pathlib import Path
import tempfile
import unittest

from claude_history import ClaudeHistoryError, apply_cursor, locate_session_file, snapshot

SID = "dddddddd-4444-4444-8444-dddddddddddd"


class ClaudeHistoryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.config = Path(self.tmp.name)
        self.directory = self.config / "projects" / "-srv-checkout"
        self.directory.mkdir(parents=True)
        self.file = self.directory / f"{SID}.jsonl"

    def tearDown(self):
        self.tmp.cleanup()

    def test_mismatched_native_transcript_is_unavailable_instead_of_an_empty_chat(self):
        self.file.write_text(json.dumps({"type": "user", "sessionId": "another-session",
                                        "uuid": "u1", "message": {"content": "private"}}) + "\n")
        with self.assertRaises(ClaudeHistoryError):
            locate_session_file(self.config, "/srv/checkout", SID)

    def test_removed_history_has_a_recoverable_error(self):
        with self.assertRaises(ClaudeHistoryError):
            snapshot(self.file, SID)

    def test_malformed_records_do_not_hide_valid_saved_messages(self):
        self.file.write_text(json.dumps({"type": {"bad": "record"}}) + "\n" + json.dumps({
            "type": "assistant", "sessionId": SID, "uuid": "a1", "message": {"content": [
                {"type": "text", "text": "Saved answer"}, {"type": "thinking", "thinking": "hidden"}
            ]}}) + "\n")
        self.assertEqual([m["parts"] for m in snapshot(self.file, SID)["messages"]],
                         [[{"type": "text", "text": "Saved answer"}]])

    def test_large_records_are_bounded_and_private_content_stays_out_of_pages_and_updates(self):
        def record(n, content, **fields):
            return {"type": "user", "sessionId": SID, "uuid": str(n),
                    "message": {"content": content}, **fields}
        hidden = [record(1, "secret-meta", isMeta=True),
                  record(2, "secret-sidechain", isSidechain=True),
                  record(3, "secret-team", teamName="team"),
                  record(4, "secret-system", type="system"),
                  record(5, [{"type": "tool_result", "content": "secret-tool"}]),
                  record(6, "secret-other-session", sessionId="other")]
        huge = record(7, [{"type": "tool_result", "content": "secret-large" * 200000}])
        self.file.write_text("".join(json.dumps(e) + "\n" for e in [
            record(0, "before"), huge, *hidden, record(8, "after")]))
        page = snapshot(self.file, SID)
        self.assertEqual([m["role"] for m in page["messages"]], ["user", "notice", "user"])
        self.assertNotIn("secret-", json.dumps(page))
        with self.file.open("a") as handle:
            for e in [*hidden, huge, record(9, "new reply")]:
                handle.write(json.dumps(e) + "\n")
        update = apply_cursor(self.file, SID, page["cursor"])
        self.assertEqual([m["role"] for m in update["messages"]], ["notice", "user"])
        self.assertNotIn("secret-", json.dumps(update))
        self.assertEqual(update["messages"][-1]["parts"][0]["text"], "new reply")
        self.assertEqual(apply_cursor(self.file, SID, update["cursor"])["kind"], "none")

    def test_deeply_nested_hidden_record_does_not_block_history_or_new_messages(self):
        def visible(text):
            return json.dumps({"type": "assistant", "sessionId": SID, "uuid": text,
                               "message": {"content": text}}) + "\n"
        self.file.write_text(visible("before"))
        cursor = snapshot(self.file, SID)["cursor"]
        with self.file.open("a") as handle:
            handle.write('{"hidden":' + '[' * 10000 + '0' + ']' * 10000 + '}\n' + visible("after"))
        self.assertEqual([m["parts"][0]["text"] for m in snapshot(self.file, SID)["messages"]],
                         ["before", "after"])
        self.assertEqual([m["parts"][0]["text"] for m in apply_cursor(self.file, SID, cursor)["messages"]],
                         ["after"])

    def test_older_pages_remain_accessible_across_the_exact_record_size_boundary(self):
        def record(text):
            return {"type": "user", "sessionId": SID, "uuid": text[:6], "message": {"content": text}}
        large = record("large-")
        padding = 2 * 1024 * 1024 - len(json.dumps(large).encode())
        large["message"]["content"] += "x" * padding
        self.file.write_text("".join(json.dumps(e) + "\n" for e in [record("before"), large, record("after")]))
        page = snapshot(self.file, SID, limit=2)
        older = snapshot(self.file, SID, before=page["messages"][0]["id"])
        self.assertEqual([m["parts"][0]["text"] for m in older["messages"]], ["before"])
