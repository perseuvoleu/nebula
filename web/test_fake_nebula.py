import json
import os
import subprocess
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

E2E = Path(__file__).resolve().parent / "e2e"
sys.path.insert(0, str(E2E))
import serve  # noqa: E402

SCOPE = ["--project=nebula", "--include-home"]


class FakeBoardTests(unittest.TestCase):
    def setUp(self):
        self.state = Path(tempfile.mkdtemp())
        serve.STATE = self.state
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

    def test_away_digest_lists_only_what_happened_after_away_since(self):
        empty = {"finished": [], "needs_you": [], "decisions": []}
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--json")["board"]["away"], empty)
        before = self.cli("todo", "board", *SCOPE, "--away-since=1764000000000", "--json")["board"]["away"]
        self.assertEqual(before["finished"], ["01JD0000000000000000000012"])
        self.assertEqual(before["decisions"], ["01JG0000000000000000000002"])
        self.assertEqual(len(before["needs_you"]), 1)
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--away-since=1764999999999", "--json")["board"]["away"], empty)

    def test_a_title_after_the_delimiter_is_not_a_flag(self):
        card = self.cli("todo", "add", "--project=nebula", "--json", "--", "--propose")
        self.assertEqual((card["title"], card["proposed"]), ("--propose", False))

    def test_concurrent_notes_all_land_and_each_bumps_the_version(self):
        version = int(self.cli("todo", "board", *SCOPE, "--json")["version"].split(".")[1])
        with ThreadPoolExecutor(24) as pool:
            list(pool.map(lambda i: self.cli("todo", "note", *SCOPE, "--json", "--", "#12", f"note {i}"), range(24)))
        texts = [entry["text"] for entry in self.cli("todo", "show", *SCOPE, "--json", "--", "#12")["log"]]
        self.assertEqual(sorted(t for t in texts if t.startswith("note ")), sorted(f"note {i}" for i in range(24)))
        self.assertEqual(self.cli("todo", "board", *SCOPE, "--json")["version"].split(".")[1], str(version + 24))


if __name__ == "__main__":
    unittest.main()
