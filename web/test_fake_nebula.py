import json
import os
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from unittest.mock import patch

from e2e import serve

E2E = Path(serve.__file__).parent

SCOPE = ["--project=nebula", "--include-home"]


class FakeBoardTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.state = Path(tmp.name)
        with patch.object(serve, "STATE", self.state):
            serve.seed_board()
        (self.state / "agents.json").write_text("[]")
        board = json.loads((self.state / "board.json").read_text())
        other = {**board["decisions"][0], "id": "01JG0000000000000000000099", "project": "other", "title": "Elsewhere"}
        board["decisions"].append(other)
        (self.state / "board.json").write_text(json.dumps(board))

    def cli(self, *argv):
        result = subprocess.run([sys.executable, str(E2E / "fake_nebula.py"), *argv], capture_output=True, text=True,
                                env={**os.environ, "FAKE_NEBULA_STATE": str(self.state), "PYTHONDONTWRITEBYTECODE": "1"})
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_decisions_and_hosts_follow_the_scope(self):
        titles = [d["title"] for d in self.cli("decision", "list", *SCOPE, "--json")["items"]]
        self.assertEqual(titles, ["Copy published files"])
        projects = {c["project"] for h in self.cli("todo", "board", "--project=other", "--json")["board"]["hosts"]
                    for c in h["checkouts"]}
        self.assertEqual(projects, set())

    def wait(self):
        board = json.loads((self.state / "board.json").read_text())
        board["details"]["01JD0000000000000000000012"]["sessions"][0]["status"] = "needs_feedback"
        (self.state / "board.json").write_text(json.dumps(board))

    def test_needs_you_lists_every_waiting_session_with_no_time_window(self):
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--json")["board"]["needs_you"], [])
        self.wait()
        late = self.cli("todo", "board", *SCOPE, "--away-since=1764999999999", "--json")["board"]
        self.assertEqual([(w["id"], w["name"]) for w in late["needs_you"]], [("01JF0000000000000000000002", "worker")])
        self.assertEqual(late["away"]["needs_you"], [])
        self.assertEqual(self.cli("todo", "board", "--project=other", "--json")["board"]["needs_you"], [])

    def test_decision_list_pages_with_a_continuation(self):
        board = json.loads((self.state / "board.json").read_text())
        board["decisions"] += [{**board["decisions"][0], "id": f"01JG00000000000000000000{n}", "title": f"More {n}"} for n in (50, 51)]
        (self.state / "board.json").write_text(json.dumps(board))
        first = self.cli("decision", "list", *SCOPE, "--json")
        second = self.cli("decision", "list", *SCOPE, f"--page={first['next']}", "--json")
        self.assertEqual([d["title"] for d in first["items"] + second["items"]], ["Copy published files", "More 50", "More 51"])
        self.assertEqual(second["next"], None)

    def test_away_digest_lists_only_what_happened_after_away_since(self):
        empty = {"finished": [], "needs_you": [], "decisions": []}
        self.wait()
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--json")["board"]["away"], empty)
        before = self.cli("todo", "board", *SCOPE, "--away-since=1764000000000", "--json")["board"]["away"]
        self.assertEqual(before["finished"], ["01JD0000000000000000000012"])
        self.assertEqual(before["decisions"], ["01JG0000000000000000000002"])
        self.assertEqual(len(before["needs_you"]), 1)
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--away-since=1764999999999", "--json")["board"]["away"], empty)

    def test_away_needs_you_lists_only_sessions_of_cards_in_scope(self):
        other = self.cli("todo", "board", "--project=other", "--away-since=1764000000000", "--json")["board"]["away"]
        self.assertEqual(other["needs_you"], [])

    def test_a_title_after_the_delimiter_is_not_a_flag(self):
        card = self.cli("todo", "add", "--project=nebula", "--json", "--", "--propose")
        self.assertEqual((card["title"], card["proposed"]), ("--propose", False))
        card = self.cli("todo", "add", "--home", "--json", "--", "--project")
        self.assertEqual((card["title"], card["project"]), ("--project", None))

    def test_concurrent_notes_all_land_and_each_bumps_the_version(self):
        version = int(self.cli("todo", "board", *SCOPE, "--json")["version"].split(".")[1])
        with ThreadPoolExecutor(24) as pool:
            list(pool.map(lambda i: self.cli("todo", "note", *SCOPE, "--json", "--", "#12", f"note {i}"), range(24)))
        texts = [entry["text"] for entry in self.cli("todo", "show", *SCOPE, "--json", "--", "#12")["log"]]
        self.assertEqual(sorted(t for t in texts if t.startswith("note ")), sorted(f"note {i}" for i in range(24)))
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--json")["version"].split(".")[1], str(version + 24))


if __name__ == "__main__":
    unittest.main()
