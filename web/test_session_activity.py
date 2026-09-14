"""Public HTTP contract used by the mobile aggregate list; synthetic CLI boundary only."""
import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

from server import Gateway, Handler, StaticFiles, ThreadingHTTPServer


class SessionActivityTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        (root / "index.html").write_text("<!doctype html><title>Test</title>")
        self.state = root / "rows.json"
        self.state.write_text(json.dumps([
            {"id": "same", "name": "Synthetic", "project": project, "status": "finished",
             "status_changed_at": 1234, "archived": archived, "session_id": "private-native-id"}
            for project, archived in [("alpha", False), ("beta", False), ("alpha", True), ("hidden", False)]
        ]))
        binary = root / "cli"
        binary.write_text("#!/usr/bin/env python3\nimport json, sys\nfrom pathlib import Path\n"
                          "project = sys.argv[sys.argv.index('--project') + 1]\n"
                          "rows = json.loads(Path(__file__).with_name('rows.json').read_text())\n"
                          "print(json.dumps([r for r in rows if r['project'] == project]))\n")
        binary.chmod(0o755)
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.server.gateway = Gateway("synthetic-test-token", self.origin, ["alpha", "beta"],
                                      binary=str(binary), static=StaticFiles(root))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()
        self.tmp.cleanup()

    def request(self, path, *, authenticated=True, body=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        headers = {"Origin": self.origin, "Content-Type": "application/json"}
        if authenticated:
            headers["Authorization"] = "Bearer synthetic-test-token"
        connection.request("GET" if body is None else "POST", path,
                           body=None if body is None else json.dumps(body), headers=headers)
        response = connection.getresponse()
        result = response.status, response.read()
        connection.close()
        return result

    def test_scoped_lists_preserve_activity_across_polls_and_after_an_agent_update(self):
        def listed():
            status, body = self.request("/api/sessions?project=alpha")
            self.assertEqual(status, 200)
            return json.loads(body)["sessions"]
        expected = [{"id": "same", "name": "Synthetic", "project": "alpha", "status": "finished",
                     "status_changed_at": 1234, "archived": False}]
        self.assertEqual(listed(), expected)
        self.assertEqual(listed(), expected)
        rows = json.loads(self.state.read_text())
        rows[0]["status_changed_at"] = 5678
        self.state.write_text(json.dumps(rows))
        self.assertEqual(listed()[0]["status_changed_at"], 5678)

    def test_aggregate_discovery_keeps_authentication_allowlist_and_real_project_scoping(self):
        self.assertEqual(self.request("/api/projects", authenticated=False)[0], 401)
        self.assertEqual(self.request("/api/sessions?project=alpha", authenticated=False)[0], 401)
        self.assertEqual(json.loads(self.request("/api/projects")[1])["projects"], ["alpha", "beta"])
        for project in ["alpha", "beta"]:
            rows = json.loads(self.request(f"/api/sessions?project={project}")[1])["sessions"]
            self.assertEqual([(r["project"], r["id"]) for r in rows], [(project, "same")])
        for project in ["All", "hidden"]:
            self.assertEqual(self.request(f"/api/sessions?project={project}")[0], 403)
            self.assertEqual(self.request("/api/new", body={"project": project})[0], 403)
        self.assertEqual(self.request("/?view=all")[0], 200)
        self.assertEqual(self.request("/p/alpha/s/same?view=all")[0], 200)
