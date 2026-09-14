import http.client
import json
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from pathlib import Path
from unittest.mock import patch

from server import Gateway, Handler, Problem, StaticFiles, ThreadingHTTPServer
from test_pi_history import SID_A, SID_B, assistant, user, write_session

TOKEN = "test-token-" * 5
ROWS = [{"id": "one", "name": "An agent", "kind": "pi", "status": "finished", "archived": False,
         "worktree": "main", "path": "/srv/project"}]
TREES = [{"path": "/srv/project", "branch": "main", "is_main": True, "sessions": []}]
SHOW = {"id": "one", "name": "An agent", "kind": "pi", "status": "finished", "alive": True}


def make_static(root):
    root = Path(root)
    (root / "assets").mkdir(parents=True)
    (root / "index.html").write_text("<!doctype html><title>Nebula</title>")
    (root / "assets" / "app-abc123.js").write_text("console.log(1)")
    (root / "manifest.webmanifest").write_text(json.dumps({"icons": [{"src": "/icon.svg"}]}))
    (root / "icon.svg").write_text("<svg/>")
    (root / "sw.js").write_text("// sw")
    (root / "secret.env").write_text("TOKEN=x")
    return root


class GatewayTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["project"],
                               static=StaticFiles(make_static(Path(self.tmp.name) / "static")),
                               pi_agent_dir=Path(self.tmp.name) / "pi")

    def tearDown(self):
        self.tmp.cleanup()

    def test_static_files_survive_build_directory_replacement(self):
        root = make_static(Path(self.tmp.name) / "replaceable")
        static = StaticFiles(root)
        (root / "index.html").unlink()
        self.assertTrue(static.lookup("/")[0].startswith(b"<!doctype"))

    def test_configuration(self):
        Gateway("12345678", "https://example.com", ["p"])
        for token, origin, projects in [("1234567", "https://example.com", ["p"]),
                                        (TOKEN, "http://example.com", ["p"]),
                                        (TOKEN, "https://example.com/", ["p"]),
                                        (TOKEN, "https://example.com", [])]:
            with self.assertRaises(ValueError):
                Gateway(token, origin, projects)

    def test_project_allowlist(self):
        for path in ["/api/sessions", "/api/conversation", "/api/conversation/updates", "/api/preview"]:
            with self.assertRaises(Problem) as ctx:
                self.gateway.get(path, {"project": ["secret"], "id": ["one"]})
            self.assertEqual(ctx.exception.status, 403)

    def test_preview_uses_dedicated_binary_and_returns_only_running_url(self):
        preview = Path(self.tmp.name) / "preview binary"
        preview.write_text("#!/bin/sh\nexit 0\n")
        preview.chmod(0o700)
        gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["project"],
                          binary="/home/andrei/.local/bin/nebula", preview_binary=str(preview))
        with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0,
                   '{"domain":"demo.example.test","phase":"running","credentials":"never expose"}', "")) as run:
            self.assertEqual(gateway.get("/api/preview", {"project": ["project"]}),
                             {"url": "https://demo.example.test", "phase": "running"})
            self.assertEqual(run.call_args.args[0], [str(preview), "preview", "--project", "project", "status"])
        for path, command in [("/api/sessions", "agent"), ("/api/worktrees", "worktree")]:
            with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0, '[]', "")) as run:
                gateway.get(path, {"project": ["project"]})
                self.assertEqual(run.call_args.args[0], ["/home/andrei/.local/bin/nebula", command, "list", "--project", "project"])

    def test_preview_binary_must_be_an_absolute_executable_file(self):
        plain = Path(self.tmp.name) / "not executable"
        plain.write_text("not executable")
        for binary in ["", "nebula", "./preview", str(plain), self.tmp.name, str(plain) + "-missing"]:
            with self.subTest(binary=binary), self.assertRaises(ValueError):
                Gateway(TOKEN, "http://127.0.0.1:8765", ["project"], preview_binary=binary)

    def test_preview_fails_closed_for_bad_status_and_never_exposes_diagnostics(self):
        gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["project"], preview_binary="/bin/true")
        invalid = [None, [], "running", {}, {"domain": "demo.example.test", "phase": []}]
        invalid += [{"domain": domain, "phase": "running"} for domain in [
            None, [], "", "localhost", "evil.test/path", "user@evil.test", "evil.test:443", "a..test",
            "-a.test", "a-.test", "a.test.", "A.test", "a" * 64 + ".test", "code.possu.dev",
            ".".join(["a" * 63] * 4), "evil.test\n",
        ]]
        for status in invalid:
            with self.subTest(status=status), patch("server.subprocess.run", return_value=
                    subprocess.CompletedProcess([], 0, json.dumps(status), "")):
                self.assertEqual(gateway.get("/api/preview", {"project": ["project"]}), {"url": None, "phase": None})
        for phase in ["stopped", "stale", "recovering", "orphaned_route", "unknown"]:
            with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0,
                       json.dumps({"domain": "demo.example.test", "phase": phase}), "")):
                self.assertEqual(gateway.get("/api/preview", {"project": ["project"]}), {"url": None, "phase": phase})
        for result in [subprocess.CompletedProcess([], 0, "not JSON", ""),
                       subprocess.CompletedProcess([], 1, "", "private diagnostic")]:
            with patch("server.subprocess.run", return_value=result):
                self.assertEqual(gateway.get("/api/preview", {"project": ["project"]}), {"url": None, "phase": None})
        with patch("server.subprocess.run") as run:
            self.assertEqual(self.gateway.get("/api/preview", {"project": ["project"]}), {"url": None, "phase": None})
            with self.assertRaises(Problem): gateway.get("/api/preview", {"project": ["secret"]})
            run.assert_not_called()

    def test_terminal_keys_dispatch_once_by_id_without_prompt(self):
        for key in ["Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"]:
            with self.subTest(key=key), patch("server.subprocess.run", side_effect=[
                subprocess.CompletedProcess([], 0, json.dumps(ROWS), ""),
                subprocess.CompletedProcess([], 0, '{"ok":true}', ""),
            ]) as run:
                self.assertTrue(self.gateway.post("/api/key", {"project": "project", "id": "one", "key": key})["ok"])
                self.assertEqual(run.call_args.args[0], ["nebula", "agent", "key", "--project", "project", "--", "one", key])
                self.assertEqual(run.call_count, 2)
        for changed in [{"key": "Space"}, {"key": []}, {"project": "secret"}, {"id": "deleted"}]:
            with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0, json.dumps(ROWS), "")) as run:
                with self.assertRaises(Problem):
                    self.gateway.post("/api/key", {"project": "project", "id": "one", "key": "Enter", **changed})
                self.assertLessEqual(run.call_count, 1)
        with patch("server.subprocess.run", side_effect=[
            subprocess.CompletedProcess([], 0, json.dumps(ROWS), ""),
            subprocess.CompletedProcess([], 1, "", "PTY disconnected"),
        ]) as run:
            with self.assertRaises(Problem) as error:
                self.gateway.post("/api/key", {"project": "project", "id": "one", "key": "Enter"})
            self.assertEqual(error.exception.message, "PTY disconnected")
            self.assertEqual(run.call_count, 2)

    def test_delete_session_is_project_scoped_and_id_only(self):
        body = {"project": "project", "id": "one"}
        renamed = [{**ROWS[0], "name": "Renamed"}]
        with patch("server.subprocess.run", side_effect=[
            subprocess.CompletedProcess([], 0, json.dumps(renamed), ""),
            subprocess.CompletedProcess([], 0, '{"ok":true}', ""),
        ]) as run:
            self.assertTrue(self.gateway.post("/api/sessions/delete", body)["ok"])
            self.assertEqual(run.call_args.args[0], ["nebula", "agent", "delete-id", "--project", "project", "--", "one"])
        for changed in [{"id": "gone"}, {"project": "secret"}]:
            with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0, json.dumps(ROWS), "")) as run:
                with self.assertRaises(Problem): self.gateway.post("/api/sessions/delete", {**body, **changed})
                self.assertLessEqual(run.call_count, 1)

    def test_safe_checkout_delete_checks_identity_and_never_forces_or_retries(self):
        tree = {"id": "wt-one", "path": "/srv/feature", "branch": "feature", "is_main": False, "sessions": []}
        body = {"project": "project", "id": "wt-one", "path": "/srv/feature", "branch": "feature"}
        for path in ["/api/worktrees/check-delete", "/api/worktrees/delete"]:
            for changed in [{"is_main": True}, {"branch": "main"}, {"branch": "master"}, {"id": "replacement"}]:
                with patch("server.subprocess.run", return_value=subprocess.CompletedProcess([], 0, json.dumps([{**tree, **changed}]), "")) as run:
                    with self.assertRaises(Problem): self.gateway.post(path, body)
                    self.assertEqual(run.call_count, 1)
            for failure in ["Dirty checkout", "Unmerged commits", "Sessions still exist"]:
                with patch("server.subprocess.run", side_effect=[
                    subprocess.CompletedProcess([], 0, json.dumps([tree]), ""),
                    subprocess.CompletedProcess([], 1, "", failure),
                ]) as run:
                    with self.assertRaises(Problem) as error: self.gateway.post(path, body)
                    self.assertEqual(error.exception.message, failure)
                    self.assertEqual(run.call_count, 2)
            with patch("server.subprocess.run", side_effect=[
                subprocess.CompletedProcess([], 0, json.dumps([tree]), ""),
                subprocess.CompletedProcess([], 0, '{"ok":true}', ""),
            ]) as run:
                self.assertTrue(self.gateway.post(path, body)["ok"])
                expected = ["nebula", "worktree", "delete-safe", "--project", "project"]
                if path.endswith("check-delete"): expected += ["--check"]
                self.assertEqual(run.call_args.args[0], expected + ["--", "wt-one", "feature", "/srv/feature"])

    def test_send_treats_input_as_positional_not_flags(self):
        text = '--project=secret; $(touch /tmp/never)'
        with patch.object(self.gateway, "run", side_effect=[ROWS, "ok"]) as run:
            result = self.gateway.post("/api/send", {"project": "project", "id": "one", "text": text,
                                                     "attempt": "att-0001"})
        self.assertEqual(run.call_args.args, ("agent", "send", "--project", "project", "--", "An agent", text))
        self.assertEqual(result["attempt"], "att-0001")
        self.assertIn("acceptedAt", result)
        with self.assertRaises(Problem):
            self.gateway.post("/api/send", {"project": "project", "id": "one", "text": "hi", "attempt": "bad id!"})

    def test_new_uses_existing_checkout_selector_and_resolves_identity(self):
        def fake_run(*args, **kwargs):
            if args[:2] == ("worktree", "list"):
                return TREES
            if args[:2] == ("agent", "new"):
                fake_run.name = next(a for a in args if a.startswith("--name=")).split("=", 1)[1]
                return "ok"
            return [{**ROWS[0], "id": "fresh-id", "name": fake_run.name}]
        with patch.object(self.gateway, "run", side_effect=fake_run) as run:
            result = self.gateway.post("/api/new", {"project": "project", "worktree": "/srv/project", "kind": "pi", "text": "--help"})
        new_call = next(c for c in run.call_args_list if c.args[:2] == ("agent", "new"))
        self.assertIn("--worktree=main", new_call.args)
        self.assertIn("--prompt=--help", new_call.args)
        self.assertEqual(result["id"], "fresh-id")

    def test_create_worktree_uses_selected_checkout_branch_not_primary(self):
        trees = TREES + [{**TREES[0], "path": "/srv/feature", "branch": "feature-base", "is_main": False}]
        created = {"project": "project", "branch": "fresh-task", "path": "/srv/new-wt"}
        with patch.object(self.gateway, "run", side_effect=[trees, "main\nfeature-base\n", created]) as run:
            result = self.gateway.post("/api/worktrees", {"project": "project", "source": "/srv/feature",
                "base": "feature-base", "name": "fresh-task", "mode": "worktree"})
        self.assertEqual(run.call_args.args, ("worktree", "new", "--project", "project", "--from=feature-base", "--", "fresh-task"))
        self.assertEqual(result["worktree"]["path"], "/srv/new-wt")
        self.assertFalse(result["worktree"]["is_main"])

    def test_branch_uses_primary_and_refuses_live_sessions_or_dirty_work(self):
        body = {"project": "project", "source": "/srv/project", "base": "main", "name": "fresh-task", "mode": "branch"}
        busy = [{**TREES[0], "sessions": [{"alive": True, "status": "finished"}]}]
        with patch.object(self.gateway, "run", return_value=busy) as run:
            with self.assertRaises(Problem) as ctx:
                self.gateway.post("/api/worktrees", body)
            self.assertEqual(ctx.exception.status, 409)
            self.assertEqual(run.call_count, 1)
        with patch.object(self.gateway, "run", side_effect=[TREES, "main\n", " M README.md\n"]):
            with self.assertRaises(Problem) as ctx: self.gateway.post("/api/worktrees", body)
            self.assertEqual(ctx.exception.status, 409)
        created = {"project": "project", "branch": "fresh-task", "path": "/srv/project", "created": True}
        with patch.object(self.gateway, "run", side_effect=[TREES, "main\n", "", created]) as run:
            result = self.gateway.post("/api/worktrees", body)
        self.assertEqual(run.call_args.args, ("switch", "--project", "project", "--from=main", "--", "fresh-task"))
        self.assertTrue(result["worktree"]["is_main"])

    def test_checkout_creation_scope_names_and_stale_base_fail_closed(self):
        body = {"project": "project", "source": "/srv/project", "base": "main", "name": "fresh-task", "mode": "worktree"}
        for changed in [{"project": "secret"}, {"name": "--help"}, {"name": "a;touch x"}, {"name": "main"},
                        {"name": "../escape"}, {"mode": "bogus"}, {"source": "/etc"}, {"base": "stale"}]:
            with patch.object(self.gateway, "run", return_value=TREES) as run:
                with self.assertRaises(Problem): self.gateway.post("/api/worktrees", {**body, **changed})
                self.assertLessEqual(run.call_count, 1)
        with patch.object(self.gateway, "run", side_effect=[TREES, "main\nfresh-task\n"]) as run:
            with self.assertRaises(Problem) as ctx: self.gateway.post("/api/worktrees", body)
            self.assertEqual(ctx.exception.status, 409)
            self.assertEqual(run.call_count, 2)

    def test_new_rejects_unknown_checkout(self):
        with patch.object(self.gateway, "run", return_value=TREES) as run:
            with self.assertRaises(Problem):
                self.gateway.post("/api/new", {"project": "project", "worktree": "/etc", "kind": "pi", "text": "hi"})
        self.assertEqual(run.call_count, 1)

    def test_session_resolution_fails_closed(self):
        for rows in [[], [{**ROWS[0], "archived": True}], ROWS + [{**ROWS[0], "id": "two"}]]:
            with patch.object(self.gateway, "run", return_value=rows):
                with self.assertRaises(Problem):
                    self.gateway.session_row("project", "one")

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

    def test_conversation_without_native_identity_is_unavailable_not_empty(self):
        with patch.object(self.gateway, "run", side_effect=[ROWS, SHOW]):
            result = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertFalse(result["capabilities"]["structured"])
        self.assertIn("identity", result["capabilities"]["reason"])
        self.assertNotIn("snapshot", result)
        self.assertTrue(result["capabilities"]["terminal"])
        claude = [{**ROWS[0], "kind": "claude"}]
        with patch.object(self.gateway, "run", side_effect=[claude]):
            result = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertFalse(result["capabilities"]["structured"])
        self.assertIn("terminal", result["capabilities"]["reason"])

    def test_conversation_reads_native_history_by_stored_identity(self):
        write_session(self.gateway.pi_agent_dir, "/srv/project", SID_B, [user("x1", None, "other session")])
        write_session(self.gateway.pi_agent_dir, "/srv/project", SID_A,
                      [user("u1", None, "hello"), assistant("a1", "u1", [{"type": "text", "text": "hi there"}])],
                      name="2026-09-13T11-00-00-000Z")
        show = {**SHOW, "session_id": SID_A}
        with patch.object(self.gateway, "run", side_effect=[ROWS, show]):
            result = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertTrue(result["capabilities"]["structured"])
        self.assertFalse(result["capabilities"]["liveText"])
        self.assertEqual([m["id"] for m in result["snapshot"]["messages"]], ["u1", "a1"])
        self.assertEqual(result["session"]["status"], "finished")
        cursor = result["snapshot"]["cursor"]
        query = {"project": ["project"], "id": ["one"], "generation": [cursor["generation"]],
                 "offset": [str(cursor["offset"])], "leaf": [cursor["leaf"]]}
        with patch.object(self.gateway, "run", side_effect=[ROWS]):  # identity cached, no second show
            self.assertEqual(self.gateway.get("/api/conversation/updates", query)["kind"], "none")
        with open(self.gateway.pi_agent_dir / "sessions" / "--srv-project--" / f"2026-09-13T11-00-00-000Z_{SID_A}.jsonl", "a") as handle:
            handle.write(json.dumps(user("u2", "a1", "next")) + "\n")
        with patch.object(self.gateway, "run", side_effect=[ROWS]):
            delta = self.gateway.get("/api/conversation/updates", query)
        self.assertEqual((delta["kind"], [m["id"] for m in delta["messages"]]), ("delta", ["u2"]))
        with patch.object(self.gateway, "run", side_effect=[ROWS]):
            reset = self.gateway.get("/api/conversation/updates", {**query, "generation": ["stale"]})
        self.assertEqual(reset["kind"], "reset")

    def test_transient_gateway_failures_are_errors_not_unsupported(self):
        with patch.object(self.gateway, "run", side_effect=[ROWS, Problem(503, "Gateway busy; try again shortly")]):
            with self.assertRaises(Problem) as ctx:
                self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertEqual(ctx.exception.status, 503)
        self.assertNotIn("one", self.gateway.identities)  # nothing cached from a transient failure
        show = {**SHOW, "session_id": SID_A}
        rows = [{**ROWS[0], "session_id": SID_A}]
        with patch.object(self.gateway, "run", side_effect=[rows]):
            self.assertNotIn("session_id", self.gateway.get("/api/sessions", {"project": ["project"]})["sessions"][0])
        with patch.object(self.gateway, "run", side_effect=[rows, show]):
            result = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertNotIn("session_id", result["session"])

    def test_native_identity_db_fallback_is_read_only_and_schema_checked(self):
        db = Path(self.tmp.name) / "nebula.db"
        con = sqlite3.connect(db)
        con.execute("create table agents (id text primary key, kind text, claude_session_id text)")
        con.execute("insert into agents values ('one', 'pi', ?)", (SID_A,))
        con.commit(); con.close()
        self.gateway.db_path = db
        self.assertEqual(self.gateway.native_session_id("project", ROWS[0], SHOW), SID_A)
        with self.assertRaises(Problem):
            self.gateway.native_session_id("project", {**ROWS[0], "id": "missing"}, SHOW)
        bad = Path(self.tmp.name) / "bad.db"
        sqlite3.connect(bad).execute("create table agents (id text)").connection.close()
        self.gateway.db_path = bad
        self.gateway.identities.clear()
        with self.assertRaises(Problem):
            self.gateway.native_session_id("project", ROWS[0], SHOW)


class HTTPTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.port = cls.server.server_port
        cls.origin = f"http://127.0.0.1:{cls.port}"
        cls.server.gateway = Gateway(TOKEN, cls.origin, ["project"], static=StaticFiles(make_static(cls.tmp.name)))
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown(); cls.server.server_close(); cls.thread.join(); cls.tmp.cleanup()

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

    def test_shell_assets_and_route_fallback(self):
        for path in ["/", "/assets/app-abc123.js", "/manifest.webmanifest", "/icon.svg", "/sw.js",
                     "/p/project", "/p/project/s/one"]:
            status, headers, body = self.request(path)
            self.assertEqual(status, 200, path)
            self.assertTrue(body)
            self.assertIn("frame-ancestors 'none'", headers["Content-Security-Policy"])
        self.assertTrue(self.request("/p/project/s/one")[2].startswith(b"<!doctype"))
        self.assertTrue(self.request("/assets/app-abc123.js")[1]["Cache-Control"].startswith("public"))
        self.assertEqual(self.request("/")[1]["Cache-Control"], "no-cache")
        for path in ["/secret.env", "/../server.py", "/assets/../secret.env", "/p/project/s/one/x", "/api", "/server.py"]:
            self.assertEqual(self.request(path)[0], 404, path)

    def test_auth_and_no_cache(self):
        self.assertEqual(self.request("/api/projects")[0], 401)
        status, headers, body = self.request("/api/projects", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(json.loads(body)["projects"], ["project"])
        self.assertTrue(json.loads(body)["host"])
        self.assertNotIn("Access-Control-Allow-Origin", headers)

    def test_wrong_host_and_origin(self):
        self.assertEqual(self.request("/", headers={"Host": "evil.example"})[0], 403)
        self.assertEqual(self.request("/api/projects", headers=self.auth(Origin="https://evil.example"))[0], 403)
        for path in ["/api/send", "/api/key", "/api/sessions/delete", "/api/worktrees/delete", "/api/worktrees/check-delete"]:
            self.assertEqual(self.request(path, "POST", "{}")[0], 401)
            self.assertEqual(self.request(path, "POST", "{}", {"Authorization": "Bearer " + TOKEN})[0], 403)
            self.assertEqual(self.request(path, "POST", "{}", self.auth(Origin="https://evil.example"))[0], 403)

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
        self.assertEqual(self.request("/api/sessions?project=secret", headers=self.auth())[0], 403)
        self.assertEqual(self.request("/api/nothing?project=project", headers=self.auth())[0], 404)


if __name__ == "__main__":
    unittest.main()
