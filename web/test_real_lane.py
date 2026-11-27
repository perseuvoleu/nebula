import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from e2e import serve


class Launched(Exception):
    pass


class RealLaneStartTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)

    def start(self, state):
        env = {"REAL_NEBULA_STATE": str(state), "REAL_NEBULA_BIN": shutil.which("true")}
        with patch.dict(os.environ, env), patch.object(serve.subprocess, "run", side_effect=Launched):
            serve.serve_real()

    def test_a_nested_state_path_gets_its_parents(self):
        state = self.root / "fresh" / "nested" / "state"
        with self.assertRaises(Launched):
            self.start(state)
        self.assertTrue((state / ".real-e2e").exists())

    def test_the_lock_file_keeps_its_contents(self):
        state = self.root / "state"
        state.mkdir()
        lock = self.root / "state.lock"
        lock.write_text("another tool's data")
        with self.assertRaisesRegex(SystemExit, "was not made by the real lane"):
            self.start(state)
        self.assertEqual(lock.read_text(), "another tool's data")


if __name__ == "__main__":
    unittest.main()
