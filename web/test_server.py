import http.client
import json
import subprocess
import threading
import unittest
from unittest.mock import patch

from server import FILES, STATIC, Gateway, Handler, Problem, ThreadingHTTPServer

TOKEN = "test-token-" * 5
ROWS = [{"id": "one", "name": "An agent", "archived": False}]
TREES = [{"path": "/srv/project", "branch": "main", "is_main": True, "sessions": []}]


class GatewayTests(unittest.TestCase):
    def setUp(self):
        self.gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["project"])

    def test_configuration(self):
        for token, origin, projects in [("short", "https://example.com", ["p"]),
                                        (TOKEN, "http://example.com", ["p"]),
                                        (TOKEN, "https://example.com/", ["p"]),
                                        (TOKEN, "https://example.com", [])]:
            with self.assertRaises(ValueError):
                Gateway(token, origin, projects)

    def test_project_allowlist(self):
        with self.assertRaises(Problem) as ctx:
            self.gateway.get("/api/sessions", {"project": ["secret"]})
        self.assertEqual(ctx.exception.status, 403)

    def test_send_treats_input_as_positional_not_flags(self):
        text = '--project=secret; $(touch /tmp/never)'
        with patch.object(self.gateway, "run", side_effect=[ROWS, "ok"]) as run:
            self.gateway.post("/api/send", {"project": "project", "id": "one", "text": text})
        self.assertEqual(run.call_args.args, ("agent", "send", "--project", "project", "--", "An agent", text))

    def test_new_uses_existing_checkout_selector(self):
        with patch.object(self.gateway, "run", side_effect=[TREES, "ok"]) as run:
            self.gateway.post("/api/new", {"project": "project", "worktree": "/srv/project", "kind": "pi", "text": "--help"})
        self.assertIn("--worktree=main", run.call_args.args)
        self.assertIn("--prompt=--help", run.call_args.args)

    def test_new_rejects_unknown_checkout(self):
        with patch.object(self.gateway, "run", return_value=TREES) as run:
            with self.assertRaises(Problem):
                self.gateway.post("/api/new", {"project": "project", "worktree": "/etc", "kind": "pi", "text": "hi"})
        self.assertEqual(run.call_count, 1)

    def test_session_resolution_fails_closed(self):
        for rows in [[], [{**ROWS[0], "archived": True}], ROWS + [{**ROWS[0], "id": "two"}]]:
            with patch.object(self.gateway, "run", return_value=rows):
                with self.assertRaises(Problem):
                    self.gateway.session_name("project", "one")

    def test_invalid_messages_and_kind(self):
        for text in [None, [], "", " ", "\0", "a" * 16001]:
            with self.assertRaises(Problem):
                self.gateway.post("/api/send", {"project": "project", "text": text})
        with self.assertRaises(Problem):
            self.gateway.post("/api/new", {"project": "project", "text": "hi", "kind": []})

    def test_subprocess_environment_and_timeout(self):
        with patch.dict("os.environ", {"NEBULA_WEB_TOKEN": TOKEN, "NEBULA_AGENT_ID": "parent", "PATH": "/bin"}):
            with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0, "[]", "")) as run:
                self.assertEqual(self.gateway.run("agent", "list"), [])
            self.assertNotIn("NEBULA_WEB_TOKEN", run.call_args.kwargs["env"])
            self.assertNotIn("NEBULA_AGENT_ID", run.call_args.kwargs["env"])
            self.assertEqual(run.call_args.kwargs["env"]["PATH"], "/bin")
        with patch("server.subprocess.run", side_effect=subprocess.TimeoutExpired("nebula", 20)):
            with self.assertRaises(Problem) as ctx:
                self.gateway.run("agent", "list")
            self.assertEqual(ctx.exception.status, 504)
        self.assertTrue(self.gateway.slots.acquire(blocking=False))
        self.gateway.slots.release()


class HTTPTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.port = cls.server.server_port
        cls.origin = f"http://127.0.0.1:{cls.port}"
        cls.server.gateway = Gateway(TOKEN, cls.origin, ["project"])
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown(); cls.server.server_close(); cls.thread.join()

    def request(self, path, method="GET", body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=5)
        connection.request(method, path, body=body, headers=headers or {})
        response = connection.getresponse()
        result = response.status, dict(response.getheaders()), response.read()
        connection.close()
        return result

    def auth(self, **extra):
        return {"Authorization": "Bearer " + TOKEN, "Origin": self.origin,
                "Content-Type": "application/json", **extra}

    def test_shell_and_manifest_assets(self):
        for path in FILES:
            status, headers, body = self.request(path)
            self.assertEqual(status, 200, path)
            self.assertTrue(body)
            self.assertIn("frame-ancestors 'none'", headers["Content-Security-Policy"])
        manifest = json.loads((STATIC / "manifest.webmanifest").read_text())
        for icon in manifest["icons"]:
            self.assertIn(icon["src"], FILES)

    def test_auth_and_no_cache(self):
        self.assertEqual(self.request("/api/projects")[0], 401)
        status, headers, body = self.request("/api/projects", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(json.loads(body), {"projects": ["project"]})
        self.assertNotIn("Access-Control-Allow-Origin", headers)

    def test_wrong_host_and_origin(self):
        self.assertEqual(self.request("/", headers={"Host": "evil.example"})[0], 403)
        self.assertEqual(self.request("/api/projects", headers=self.auth(Origin="https://evil.example"))[0], 403)
        self.assertEqual(self.request("/api/send", "POST", "{}", {"Authorization": "Bearer " + TOKEN})[0], 403)

    def test_malformed_body(self):
        for body in ["{", "[]", '"hello"']:
            self.assertEqual(self.request("/api/send", "POST", body, self.auth())[0], 400)
        self.assertEqual(self.request("/api/send", "POST", "x" * 100001, self.auth())[0], 413)
        headers = self.auth(); headers["Content-Type"] = "text/plain"
        self.assertEqual(self.request("/api/send", "POST", "{}", headers)[0], 415)

    def test_output_is_json_not_html(self):
        with patch.object(self.server.gateway, "run", side_effect=[ROWS, "<script>alert(1)</script>"]):
            status, headers, body = self.request("/api/output?project=project&id=one", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertTrue(headers["Content-Type"].startswith("application/json"))
        self.assertEqual(json.loads(body)["output"], "<script>alert(1)</script>")

    def test_missing_and_disallowed_routes(self):
        self.assertEqual(self.request("/server.py")[0], 404)
        self.assertEqual(self.request("/../server.py")[0], 404)
        self.assertEqual(self.request("/api/sessions?project=secret", headers=self.auth())[0], 403)


if __name__ == "__main__":
    unittest.main()
