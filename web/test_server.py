import http.client
import json
import os
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import serverless
from server import Gateway, Handler, Problem, PushService, StaticFiles, ThreadingHTTPServer
from test_pi_history import SID_A, SID_B, assistant, user, write_session

TOKEN = "test-token-" * 5
NOTIFY_TOKEN = "notify-token-" * 4
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
                               pi_agent_dir=Path(self.tmp.name) / "pi", usage_service=None)

    def tearDown(self):
        self.tmp.cleanup()

    def test_static_files_survive_build_directory_replacement(self):
        root = make_static(Path(self.tmp.name) / "replaceable")
        static = StaticFiles(root)
        (root / "index.html").unlink()
        self.assertTrue(static.lookup("/")[0].startswith(b"<!doctype"))

    def test_configuration(self):
        Gateway("12345678", "https://example.com", ["p"], usage_service=None)
        for token, origin, projects in [("1234567", "https://example.com", ["p"]),
                                        (TOKEN, "http://example.com", ["p"]),
                                        (TOKEN, "https://example.com/", ["p"]),
                                        (TOKEN, "https://example.com", [])]:
            with self.assertRaises(ValueError):
                Gateway(token, origin, projects, usage_service=None)

    def test_notify_token_equal_to_main_token_is_refused(self):
        with self.assertRaises(ValueError):
            Gateway(TOKEN, "https://example.com", ["p"], notify_token=TOKEN, usage_service=None)

    def test_push_subscriptions_persist_and_notify_only_on_lifecycle_transitions(self):
        sent = []
        push = PushService(None, "https://example.com", Path(self.tmp.name) / "push.json",
                           sender=lambda subscription, data: sent.append((subscription, data)),
                           public_key="B" * 87)
        subscription = {"endpoint": "https://web.push.apple.com/Q", "keys": {"p256dh": "A" * 40, "auth": "b" * 8}}
        push.subscribe(subscription)
        push.subscribe(subscription)  # endpoint identity de-duplicates browser refreshes
        self.assertEqual(push.subscriptions(), [subscription])
        self.gateway.push = push
        self.assertTrue(self.gateway.get("/api/push", {})["available"])
        self.assertTrue(self.gateway.post("/api/push/subscribe", {"subscription": subscription})["ok"])

        states = iter(["running", "needs_feedback", "finished"])
        gateway = SimpleNamespace(projects=["project"], sessions=lambda project: [
            {"id": "one", "name": "Agent", "status": next(states), "archived": False}
        ])
        push.poll(gateway)
        self.assertEqual(sent, [])  # initial snapshot must not alert for old sessions
        push.poll(gateway)
        push.poll(gateway)
        self.assertEqual([item[1]["body"] for item in sent],
                         ["Agent: așteaptă răspunsul tău", "Agent: a terminat"])
        self.assertEqual(sent[-1][1]["url"], "/p/project/s/one")

        push.unsubscribe(subscription["endpoint"])
        self.assertEqual(push.subscriptions(), [])
        for invalid in [{}, {**subscription, "endpoint": "http://127.0.0.1/"},
                        {**subscription, "keys": {"p256dh": "!", "auth": "x"}}]:
            with self.assertRaises(Problem): push.subscribe(invalid)

    def test_host_is_named_as_nebula_names_it(self):
        for env, system, host in [({}, "Gaitanarus-MacBook-Pro.local", "Gaitanarus-MacBook-Pro"),
                                  ({"NEBULA_HOSTNAME": "perseu-mbp"}, "Gaitanarus-MacBook-Pro.local", "perseu-mbp")]:
            with self.subTest(env=env), patch.dict("os.environ", env), patch("server.socket.gethostname", return_value=system):
                gateway = Gateway(TOKEN, "https://example.com", ["p"], usage_service=None)
                self.assertEqual(gateway.get("/api/projects", {})["host"], host)

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
                          binary="/home/andrei/.local/bin/nebula", preview_binary=str(preview), usage_service=None)
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
                Gateway(TOKEN, "http://127.0.0.1:8765", ["project"], preview_binary=binary, usage_service=None)

    def test_preview_fails_closed_for_bad_status_and_never_exposes_diagnostics(self):
        gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["project"], preview_binary="/bin/true", usage_service=None)
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
        self.assertIn("identity", result["capabilities"]["reason"])

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
        write_session(self.gateway.pi_agent_dir, "/srv/project", SID_A, [user("u1", None, "hello")])
        with patch.object(self.gateway, "run", side_effect=[ROWS, {**SHOW, "session_id": SID_A}]):
            retried = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertEqual([m["id"] for m in retried["snapshot"]["messages"]], ["u1"])
        show = {**SHOW, "session_id": SID_A}
        rows = [{**ROWS[0], "session_id": SID_A}]
        with patch.object(self.gateway, "run", side_effect=[rows]):
            self.assertNotIn("session_id", self.gateway.get("/api/sessions", {"project": ["project"]})["sessions"][0])
        with patch.object(self.gateway, "run", side_effect=[rows, show]):
            result = self.gateway.get("/api/conversation", {"project": ["project"], "id": ["one"]})
        self.assertNotIn("session_id", result["session"])

    def test_native_history_cache_per_project(self):
        config = Path(self.tmp.name) / "claude"
        gateway = Gateway(TOKEN, "http://127.0.0.1:8765", ["alpha", "beta"], usage_service=None,
                          static=self.gateway.static, claude_config_dir=config)
        sessions = {"alpha": SID_A, "beta": SID_B}
        for project, sid in sessions.items():
            directory = config / "projects" / f"-srv-{project}"
            directory.mkdir(parents=True)
            (directory / f"{sid}.jsonl").write_text(json.dumps({
                "type": "user", "sessionId": sid, "uuid": "u1",
                "message": {"content": f"{project} transcript"}}) + "\n")
        def nebula(argv, *_, **__):
            project = argv[argv.index("--project") + 1]
            if "show" in argv:
                return cli({**SHOW, "kind": "claude", "session_id": sessions[project]})
            return cli([{**ROWS[0], "kind": "claude", "path": f"/srv/{project}"}])
        for project in sessions:
            with patch("server.subprocess.run", side_effect=nebula):
                result = gateway.get("/api/conversation", {"project": [project], "id": ["one"]})
            self.assertTrue(result["capabilities"]["structured"], result["capabilities"]["reason"])
            self.assertEqual([p["text"] for m in result["snapshot"]["messages"] for p in m["parts"]],
                             [f"{project} transcript"])

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
        cls.server.gateway = Gateway(TOKEN, cls.origin, ["project"], static=StaticFiles(make_static(cls.tmp.name)), usage_service=None)
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
                     "/p/project", "/p/project/s/one", "/serverless", "/board", "/board/p/nebula?view=table",
                     "/board/c/12@leaseweb?peek=12", "/board/c/12%40leaseweb", "/board/decisions/p/nebula"]:
            status, headers, body = self.request(path)
            self.assertEqual(status, 200, path)
            self.assertTrue(body)
            self.assertIn("frame-ancestors 'none'", headers["Content-Security-Policy"])
        self.assertTrue(self.request("/p/project/s/one")[2].startswith(b"<!doctype"))
        self.assertTrue(self.request("/assets/app-abc123.js")[1]["Cache-Control"].startswith("public"))
        self.assertEqual(self.request("/")[1]["Cache-Control"], "no-cache")
        for path in ["/secret.env", "/../server.py", "/assets/../secret.env", "/p/project/s/one/x", "/api", "/server.py", "/board/x/y"]:
            self.assertEqual(self.request(path)[0], 404, path)

    def test_auth_and_no_cache(self):
        self.assertEqual(self.request("/api/projects")[0], 401)
        status, headers, body = self.request("/api/projects", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(json.loads(body)["projects"], ["project"])
        self.assertTrue(json.loads(body)["host"])
        self.assertNotIn("Access-Control-Allow-Origin", headers)

    def test_usage_is_authenticated_global_and_never_cacheable(self):
        self.assertEqual(self.request("/api/usage")[0], 401)
        status, headers, body = self.request("/api/usage?project=not-enabled", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(set(json.loads(body)), {"codex", "claude"})
        self.assertEqual(self.request("/api/usage", headers=self.auth(Origin="https://evil.example"))[0], 403)

    def test_serverless_runs_are_authenticated_and_explicit_when_unconfigured(self):
        self.assertEqual(self.request("/api/serverless/runs")[0], 401)
        status, headers, body = self.request("/api/serverless/runs", headers=self.auth())
        self.assertEqual(status, 200)
        self.assertEqual(headers["Cache-Control"], "no-store")
        self.assertEqual(json.loads(body), {"configured": False, "runs": []})
        with patch.object(self.server.gateway, "serverless", serverless.ServerlessRuns("http://127.0.0.1:1", "cf-secret")):
            status, _, body = self.request("/api/serverless/runs?limit=5", headers=self.auth())
        self.assertEqual((status, json.loads(body)), (502, {"error": "Worker-ul serverless nu poate fi contactat"}))

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



class NotifyTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.sent = []
        self.push = PushService(None, "https://example.com", Path(self.tmp.name) / "push.json",
                                sender=lambda subscription, data: self.sent.append((subscription["endpoint"], data)),
                                public_key="B" * 87)
        for endpoint in ["https://web.push.apple.com/one", "https://fcm.googleapis.com/two"]:
            self.push.subscribe({"endpoint": endpoint, "keys": {"p256dh": "A" * 40, "auth": "b" * 8}})
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.server.gateway = Gateway(TOKEN, self.origin, ["project"], static=StaticFiles(make_static(self.tmp.name)),
                                      push=self.push, notify_token=NOTIFY_TOKEN, usage_service=None)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(); self.tmp.cleanup()

    def notify(self, body, token=NOTIFY_TOKEN, path="/api/notify", **headers):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        data = body if isinstance(body, str) else json.dumps(body)
        connection.request("POST", path, body=data, headers={"Authorization": "Bearer " + token,
                                                             "Content-Type": "application/json", **headers})
        response = connection.getresponse()
        result = response.status, json.loads(response.read())
        connection.close()
        return result

    def test_server_caller_reaches_every_subscribed_device(self):
        self.assertEqual(self.notify({"title": "Teclo", "body": "Descărcare nouă", "tag": "teclo-download"}),
                         (200, {"sent": 2}))
        payload = {"title": "Teclo", "body": "Descărcare nouă", "url": "/", "tag": "teclo-download"}
        self.assertEqual(self.sent, [("https://web.push.apple.com/one", payload),
                                     ("https://fcm.googleapis.com/two", payload)])

    def test_tag_defaults_and_expired_devices_are_not_counted(self):
        class Gone(Exception):
            response = SimpleNamespace(status_code=410)
        def sender(subscription, data):
            if subscription["endpoint"].endswith("/two"):
                raise Gone()
            self.sent.append(data)
        self.push.sender = sender
        self.assertEqual(self.notify({"title": "Teclo", "body": "Descărcare nouă"}), (200, {"sent": 1}))
        self.assertEqual(self.sent, [{"title": "Teclo", "body": "Descărcare nouă", "url": "/", "tag": "nebula-notify"}])
        self.assertEqual([s["endpoint"] for s in self.push.subscriptions()], ["https://web.push.apple.com/one"])

    def test_notify_token_and_main_token_do_not_cross(self):
        valid = {"title": "Teclo", "body": "Descărcare nouă"}
        self.assertEqual(self.notify(valid, token=TOKEN)[0], 401)
        self.assertEqual(self.notify(valid, token=TOKEN, Origin=self.origin)[0], 401)
        for path in ["/api/projects", "/api/push/subscribe", "/api/send"]:
            self.assertEqual(self.notify(valid, path=path)[0], 401, path)
            self.assertEqual(self.notify(valid, path=path, Origin=self.origin)[0], 401, path)
        self.assertEqual(self.sent, [])

    def test_unset_notify_token_refuses_every_caller(self):
        for configured in [None, ""]:
            self.server.gateway.notify_token = configured
            for token in ["", TOKEN, NOTIFY_TOKEN]:
                self.assertEqual(self.notify({"title": "Teclo", "body": "x"}, token=token)[0], 401)
        self.assertEqual(self.sent, [])

    def test_unconfigured_push_is_unavailable(self):
        self.server.gateway.push = None
        self.assertEqual(self.notify({"title": "Teclo", "body": "Descărcare nouă"})[0], 503)

    def test_invalid_payloads_send_nothing(self):
        for body in [{"body": "x"}, {"title": "x"}, {"title": 1, "body": "x"}, {"title": "x", "body": None},
                     {"title": "", "body": "x"}, {"title": "x", "body": ""}, {"title": "t" * 101, "body": "x"},
                     {"title": "x", "body": "b" * 301}, {"title": "x", "body": "y", "tag": 3},
                     {"title": "x", "body": "y", "tag": "g" * 65}, [], "{"]:
            with self.subTest(body=body):
                self.assertEqual(self.notify(body)[0], 400)
        self.assertEqual(self.notify({"title": "x", "body": "y"}, **{"Content-Type": "text/plain"})[0], 415)
        self.assertEqual(self.notify({"title": "x", "body": "y" * 100001})[0], 413)
        self.assertEqual(self.sent, [])
        self.assertEqual(self.notify({"title": "t" * 100, "body": "b" * 300, "tag": "g" * 64}), (200, {"sent": 2}))

    def test_host_and_browser_origin_still_apply(self):
        valid = {"title": "Teclo", "body": "Descărcare nouă"}
        self.assertEqual(self.notify(valid, Host="evil.example")[0], 403)
        self.assertEqual(self.notify(valid, Origin="https://evil.example")[0], 403)
        self.assertEqual(self.sent, [])


FIXTURES = Path(__file__).resolve().parent.parent / "crates" / "nebula-core" / "tests" / "fixtures" / "board"
CARD_ID = "01JD0000000000000000000012"
ARTIFACT_ID = "01JG0000000000000000000001"
OTHER_ID = "01JG0000000000000000000009"


def fixture(name):
    return json.loads((FIXTURES / f"{name}.json").read_text())


def cli(data, code=0, stderr=""):
    return subprocess.CompletedProcess([], code, json.dumps(data) if code == 0 else "", stderr)


class BoardTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        self.artifacts = root / "artifacts"
        (self.artifacts / ARTIFACT_ID / "shots").mkdir(parents=True)
        (self.artifacts / ARTIFACT_ID / "result.json").write_text(json.dumps(fixture("e2e_run_result")))
        (self.artifacts / ARTIFACT_ID / "index.html").write_text("<script>parent.document</script>")
        (self.artifacts / ARTIFACT_ID / "shots" / "drag.png").write_bytes(b"\x89PNG fake")
        (self.artifacts / ARTIFACT_ID / "notes.xyz").write_text("plain")
        (self.artifacts / ARTIFACT_ID / "page.htm").write_text("<p>htm</p>")
        (self.artifacts / ARTIFACT_ID / "page.xhtml").write_text("<p>xhtml</p>")
        (self.artifacts / OTHER_ID).mkdir()
        (self.artifacts / OTHER_ID / "result.json").write_text("{}")
        (root / "secret.txt").write_text("OUTSIDE-SECRET")
        (self.artifacts / ARTIFACT_ID / "link.txt").symlink_to(root / "secret.txt")
        (root / "elsewhere").mkdir()
        (root / "elsewhere" / "index.html").write_text("OUTSIDE-SECRET")
        self.linked_id = "01JG0000000000000000000008"
        (self.artifacts / self.linked_id).symlink_to(root / "elsewhere")
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.origin = f"http://127.0.0.1:{self.server.server_port}"
        self.gateway = Gateway(TOKEN, self.origin, ["project"], static=StaticFiles(make_static(root / "static")),
                               board_projects=["nebula"], artifacts=self.artifacts, usage_service=None)
        self.server.gateway = self.gateway
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown(); self.server.server_close(); self.thread.join(); self.tmp.cleanup()

    def request(self, path, method="GET", body=None, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", self.server.server_port, timeout=5)
        data = body if body is None or isinstance(body, str) else json.dumps(body)
        connection.request(method, path, body=data, headers=headers or {})
        response = connection.getresponse()
        result = response.status, dict(response.getheaders()), response.read()
        connection.close()
        return result

    def auth(self, **extra):
        return {"Authorization": "Bearer " + TOKEN, "Origin": self.origin,
                "Content-Type": "application/json", **extra}

    def argv(self, run):
        return [call.args[0][1:] for call in run.call_args_list]

    def card_src(self):
        with patch("server.subprocess.run", return_value=cli(fixture("card_show"))):
            status, _, body = self.request("/api/cards/%2312", headers=self.auth())
        self.assertEqual(status, 200)
        return json.loads(body)["artifacts"][0]["src"]

    def test_board_poll_unchanged(self):
        unchanged = fixture("board_unchanged")
        with patch("server.subprocess.run", return_value=cli(unchanged)) as run:
            status, _, body = self.request("/api/board?since=" + unchanged["version"] + "&awaySince=1764000000000",
                                           headers=self.auth())
        self.assertEqual((status, json.loads(body)), (200, unchanged))
        self.assertEqual(run.call_count, 1)
        argv = self.argv(run)[0]
        self.assertEqual(argv[:2], ["todo", "board"])
        self.assertIn("--since=" + unchanged["version"], argv)
        self.assertIn("--away-since=1764000000000", argv)
        for query in ["since=--all", "since=a%20b", "awaySince=yesterday"]:
            with self.subTest(query=query), patch("server.subprocess.run") as run:
                self.assertEqual(self.request("/api/board?" + query, headers=self.auth())[0], 400)
                run.assert_not_called()

    def test_board_scope_injected(self):
        reads = [("/api/board?project=secret&scope=all", fixture("board")),
                 ("/api/cards/%2312?project=secret&all=1", fixture("card_show")),
                 ("/api/decisions?query=copy&project=secret", {"items": [fixture("decision")], "next": None})]
        for configured, scope in [(["nebula"], ["--project=nebula", "--include-home"]),
                                  (["nebula", "vela"], ["--project=nebula", "--project=vela", "--include-home"]),
                                  ("*", ["--all", "--include-home"])]:
            self.gateway.board_projects = configured
            for path, output in reads:
                with self.subTest(path=path, configured=configured), \
                        patch("server.subprocess.run", return_value=cli(output)) as run:
                    self.assertEqual(self.request(path, headers=self.auth())[0], 200)
                    argv = self.argv(run)[0]
                    self.assertEqual([a for a in argv if a.startswith(("--project", "--all", "--include-home"))], scope)
                    self.assertFalse(any("secret" in a for a in argv))
        self.gateway.board_projects = ["nebula"]
        with patch("server.subprocess.run", return_value=cli(None, 5, "no card #99 in scope")) as run:
            status, _, _ = self.request("/api/cards/%2399/move", "POST", {"stage": "done"}, self.auth())
        self.assertEqual(status, 404)
        self.assertEqual([a[:2] for a in self.argv(run)], [["todo", "show"]])
        with patch("server.subprocess.run") as run:
            for body in [{"title": "x", "project": "secret"}, {"title": "x"}]:
                self.assertEqual(self.request("/api/cards", "POST", body, self.auth())[0], 403)
            run.assert_not_called()

    def test_decisions_pass_a_page_token_on(self):
        with patch("server.subprocess.run", return_value=cli({"items": [], "next": None})) as run:
            self.assertEqual(self.request("/api/decisions?page=0123.4%402", headers=self.auth())[0], 200)
        self.assertIn("--page=0123.4@2", self.argv(run)[0])
        for page in ["--all", "0123.4", "a%20b"]:
            with self.subTest(page=page), patch("server.subprocess.run") as run:
                self.assertEqual(self.request("/api/decisions?page=" + page, headers=self.auth())[0], 400)
                run.assert_not_called()

    def test_artifact_grant_expiry_tamper(self):
        src = self.card_src()
        _, grant, artifact, entry = src.split("/")[1:]
        self.assertEqual((artifact, entry), (ARTIFACT_ID, "result.json"))
        expiry, mac = grant.split(".")
        self.assertLessEqual(int(expiry) - time.time(), 3600)
        self.assertGreater(int(expiry) - time.time(), 0)
        status, _, body = self.request(src)
        self.assertEqual((status, json.loads(body)), (200, fixture("e2e_run_result")))
        flipped = mac[:-1] + ("A" if mac[-1] != "A" else "B")
        for path in [f"/a/{grant}/{OTHER_ID}/result.json", f"/a/{int(expiry) + 1}.{mac}/{ARTIFACT_ID}/result.json",
                     f"/a/{expiry}.{flipped}/{ARTIFACT_ID}/result.json", f"/a/{expiry}/{ARTIFACT_ID}/result.json",
                     f"/a/x.{mac}/{ARTIFACT_ID}/result.json"]:
            with self.subTest(path=path):
                self.assertEqual(self.request(path)[0], 403)
        with patch("server.time.time", return_value=int(expiry) + 1):
            self.assertEqual(self.request(src)[0], 403)
        with patch("server.time.time", return_value=time.time() - 7200):
            stale = self.card_src()
        self.assertEqual(self.request(stale)[0], 403)

    def test_artifact_expires_is_when_its_src_is_refused(self):
        with patch("server.subprocess.run", return_value=cli(fixture("card_show"))):
            row = json.loads(self.request("/api/cards/%2312", headers=self.auth())[2])["artifacts"][0]
        with patch("server.time.time", return_value=row["expires"] - 1):
            self.assertEqual(self.request(row["src"])[0], 200)
        with patch("server.time.time", return_value=row["expires"]):
            self.assertEqual(self.request(row["src"])[0], 403)

    def test_artifact_traversal(self):
        grant = self.card_src().split("/")[2]
        linked = self.gateway.grant(self.linked_id)
        for path in [f"/a/{grant}/{ARTIFACT_ID}/../{OTHER_ID}/result.json",
                     f"/a/{grant}/{ARTIFACT_ID}/%2e%2e/%2e%2e/secret.txt",
                     f"/a/{grant}/{ARTIFACT_ID}/%2E%2E%2F%2E%2E%2Fsecret.txt",
                     f"/a/{grant}/{ARTIFACT_ID}/shots/..%2f..%2f..%2fsecret.txt",
                     f"/a/{grant}/{ARTIFACT_ID}//etc/passwd",
                     f"/a/{grant}/{ARTIFACT_ID}/%2Fetc%2Fpasswd",
                     f"/a/{grant}/{ARTIFACT_ID}/link.txt",
                     f"/a/{grant}/{ARTIFACT_ID}/shots",
                     f"/a/{grant}/{ARTIFACT_ID}/",
                     f"/a/{linked}/{self.linked_id}/index.html"]:
            with self.subTest(path=path):
                status, _, body = self.request(path)
                self.assertEqual(status, 404)
                self.assertNotIn(b"OUTSIDE-SECRET", body)
        self.assertEqual(self.request(f"/a/{grant}/{ARTIFACT_ID}/shots/drag.png")[0], 200)

    def test_artifact_headers_no_cli(self):
        grant = self.card_src().split("/")[2]
        with patch("server.subprocess.run") as run:
            for entry, mime in [("index.html", "text/html"), ("shots/drag.png", "image/png"),
                                ("result.json", "application/json"), ("notes.xyz", "application/octet-stream"),
                                ("page.htm", "text/html"), ("page.xhtml", "application/xhtml+xml")]:
                with self.subTest(entry=entry):
                    status, headers, body = self.request(f"/a/{grant}/{ARTIFACT_ID}/{entry}")
                    self.assertEqual(status, 200)
                    self.assertEqual(headers["Content-Security-Policy"], "sandbox allow-scripts")
                    self.assertEqual(headers["X-Content-Type-Options"], "nosniff")
                    self.assertEqual(headers["Content-Type"].split(";")[0], mime)
                    self.assertEqual(body, (self.artifacts / ARTIFACT_ID / entry).read_bytes())
            self.assertEqual(self.request(f"/a/{grant}/{ARTIFACT_ID}/index.html", headers={"Host": "evil.example"})[0], 403)
            run.assert_not_called()

    def test_card_verb_exit_codes(self):
        leak = "/home/perseu/.local/share/nebula private detail"
        for code, status in [(3, 503), (4, 504), (5, 404), (6, 409), (1, 502)]:
            for method, path, body in [("GET", "/api/board", None), ("GET", "/api/cards/%2312", None),
                                       ("POST", "/api/cards", {"title": "New", "project": "nebula"})]:
                with self.subTest(code=code, path=path), \
                        patch("server.subprocess.run", return_value=cli(None, code, leak)):
                    got, _, raw = self.request(path, method, body, self.auth())
                    self.assertEqual(got, status)
                    self.assertNotIn(b"perseu", raw)
            with self.subTest(code=code, verb="move"), patch("server.subprocess.run", side_effect=[
                    cli(fixture("card_show")), cli(None, code, leak)]):
                got, _, raw = self.request("/api/cards/%2312/move", "POST", {"stage": "review", "expect": "doing"},
                                           self.auth())
                self.assertEqual(got, status)
                self.assertNotIn(b"perseu", raw)

    def test_card_post_checks_and_publish_limits(self):
        session = {"session": "01JF0000000000000000000002", "host": "leaseweb"}
        verbs = {"move": {"stage": "review"}, "edit": {"title": "Renamed"}, "note": {"text": "--all; rm -rf /", "id": CARD_ID},
                 "archive": {}, "accept": {}, "dismiss": {}, "block": {"by": "#11"}, "unblock": {"by": "#11"},
                 "run": {"role": "worker", "kind": "codex", "host": "leaseweb", "worktree": "new"},
                 "approve": {**session, "expectedStatusAt": 1764000200000}, "open-code": {**session, "on": "perseu-mbp"}}
        posts = [("/api/cards", {"title": "New", "project": "nebula"}), ("/api/decisions", {"what": "Copy files"})]
        posts += [(f"/api/cards/%2312/{verb}", body) for verb, body in verbs.items()]
        with patch("server.subprocess.run") as run:
            for path, body in posts:
                with self.subTest(path=path):
                    self.assertEqual(self.request(path, "POST", body)[0], 401)
                    self.assertEqual(self.request(path, "POST", body, {"Authorization": "Bearer " + TOKEN,
                                                                       "Content-Type": "application/json"})[0], 403)
                    self.assertEqual(self.request(path, "POST", body, self.auth(Origin="https://evil.example"))[0], 403)
                    self.assertEqual(self.request(path, "POST", body, self.auth(**{"Content-Type": "text/plain"}))[0], 415)
                    self.assertEqual(self.request(path, "POST", "{", self.auth())[0], 400)
            self.assertIn(self.request("/api/artifacts", "POST", {"path": "/etc/passwd", "kind": "recap"},
                                       self.auth())[0], {403, 404})
            run.assert_not_called()
        for verb, body in verbs.items():
            with self.subTest(verb=verb), patch("server.subprocess.run", side_effect=[
                    cli(fixture("card_show")), cli(fixture("card_show")), cli({"ok": True})]) as run:
                self.assertEqual(self.request(f"/api/cards/%2312/{verb}", "POST", body, self.auth())[0], 200)
                write = self.argv(run)[-1]
                self.assertIn(CARD_ID if verb != "open-code" else "--session=" + session["session"], write)
                self.assertIn("todo" if verb != "open-code" else "open", write[:1])
        with patch("server.subprocess.run", return_value=cli(fixture("decision"))) as run:
            status, _, _ = self.request("/api/decisions", "POST", {"what": "/etc/passwd", "why": "x", "path": "/etc/passwd",
                                                                    "kind": "recap", "url": "https://x"}, self.auth())
        self.assertEqual(status, 200)
        argv = self.argv(run)[0]
        self.assertEqual(argv[:2], ["decision", "add"])
        self.assertEqual(argv[-2:], ["--", "/etc/passwd"])
        self.assertFalse(any(a.startswith(("--kind", "--path", "--url")) or a == "https://x" for a in argv))
        for body in [{**session, "session": "01JF0000000000000000000009", "on": "perseu-mbp"}, {**session, "host": "elsewhere", "on": "perseu-mbp"}]:
            with self.subTest(body=body), patch("server.subprocess.run", return_value=cli(fixture("card_show"))) as run:
                self.assertEqual(self.request("/api/cards/%2312/open-code", "POST", body, self.auth())[0], 409)
                self.assertEqual(run.call_count, 1)

    def test_card_writes_carry_the_board_scope_and_a_note_id(self):
        writes = {"move": {"stage": "review"}, "edit": {"title": "Renamed"}, "note": {"text": "x", "id": CARD_ID},
                  "archive": {}, "accept": {}, "dismiss": {}, "block": {"by": "#11"}, "unblock": {"by": "#11"},
                  "run": {"role": "worker", "kind": "codex", "host": "leaseweb", "worktree": "new"},
                  "approve": {"session": "01JF0000000000000000000002", "host": "leaseweb", "expectedStatusAt": 1764000200000}}
        for configured, scope in [(["nebula", "vela"], ["--scope=nebula", "--scope=vela", "--include-home"]),
                                  ("*", ["--scope=*", "--include-home"])]:
            self.gateway.board_projects = configured
            for verb, body in writes.items():
                with self.subTest(verb=verb, configured=configured), patch("server.subprocess.run", side_effect=[
                        cli(fixture("card_show")), cli(fixture("card_show")), cli(fixture("card"))]) as run:
                    self.assertEqual(self.request(f"/api/cards/%2312/{verb}", "POST", body, self.auth())[0], 200)
                    write = self.argv(run)[-1]
                    self.assertEqual([a for a in write if a.startswith(("--scope", "--include-home"))], scope)
                    self.assertEqual("--id=" + CARD_ID in write, verb == "note")

    def test_board_writes_need_board_and_explicit_owner(self):
        self.gateway.board_projects = None
        with patch("server.subprocess.run") as run:
            for path, body in [("/api/cards", {"title": "x", "home": True}), ("/api/decisions", {"what": "Copy files"})]:
                with self.subTest(path=path):
                    self.assertEqual(self.request(path, "POST", body, self.auth())[0], 503)
            run.assert_not_called()
        self.gateway.board_projects = ["nebula"]
        with patch("server.subprocess.run", return_value=cli(fixture("decision"))) as run:
            self.assertEqual(self.request("/api/decisions", "POST", {"what": "Copy files"}, self.auth())[0], 200)
        self.assertIn("--home", self.argv(run)[0])

    def test_card_create_retry_outside_scope(self):
        moved = {**fixture("card"), "project": "secret"}
        for body in [{"title": "x", "project": "nebula", "id": CARD_ID}, {"title": "x", "home": True, "id": CARD_ID}]:
            with self.subTest(body=body), patch("server.subprocess.run", return_value=cli(moved)):
                status, _, raw = self.request("/api/cards", "POST", body, self.auth())
                self.assertEqual(status, 404)
                self.assertNotIn(b"Keep verification", raw)
        with patch("server.subprocess.run", return_value=cli({**fixture("card"), "project": None})):
            self.assertEqual(self.request("/api/cards", "POST", {"title": "x", "home": True}, self.auth())[0], 200)

    def test_artifact_parent_swapped_after_check(self):
        grant = self.card_src().split("/")[2]
        outside = Path(self.tmp.name) / "outside"
        outside.mkdir()
        (outside / "drag.png").write_text("OUTSIDE-SECRET")
        shots = self.artifacts / ARTIFACT_ID / "shots"
        swapped = []

        def swap_after(real):
            def call(path, *args, **kwargs):
                result = real(path, *args, **kwargs)
                if str(path).endswith("shots") and not swapped:
                    swapped.append(True)
                    shots.rename(shots.with_name("moved"))
                    shots.symlink_to(outside)
                return result
            return call
        with patch("server.os.lstat", swap_after(os.lstat)), patch("server.os.open", swap_after(os.open)):
            _, _, body = self.request(f"/a/{grant}/{ARTIFACT_ID}/shots/drag.png")
        self.assertEqual(swapped, [True])
        self.assertNotIn(b"OUTSIDE-SECRET", body)

    def test_board_page_continuation(self):
        version = fixture("board_unchanged")["version"]
        with patch("server.subprocess.run", return_value=cli(fixture("board"))) as run:
            status, _, _ = self.request(f"/api/board?since={version}&page={version}@40", headers=self.auth())
        self.assertEqual((status, run.call_count), (200, 1))
        self.assertIn(f"--page={version}@40", self.argv(run)[0])
        with patch("server.subprocess.run") as run:
            for page in ["--all", f"{version}@x", "a%20b"]:
                with self.subTest(page=page):
                    self.assertEqual(self.request("/api/board?page=" + page, headers=self.auth())[0], 400)
            run.assert_not_called()

    def test_artifact_cache_never_outlives_grant(self):
        grant = self.card_src().split("/")[2]
        expiry = int(grant.split(".")[0])
        for remaining, control in [(100, "private, max-age=100"), (3000, "private, max-age=600")]:
            with self.subTest(remaining=remaining), patch("server.time.time", return_value=expiry - remaining):
                status, headers, _ = self.request(f"/a/{grant}/{ARTIFACT_ID}/result.json")
                self.assertEqual((status, headers["Cache-Control"]), (200, control))

    def test_run_accepts_git_branch_names(self):
        base = {"role": "worker", "kind": "codex", "host": "leaseweb"}
        for branch in ["new", "feature/ui+api", "fix/#12@home", "release-1.2"]:
            with self.subTest(branch=branch), patch("server.subprocess.run", side_effect=[
                    cli(fixture("card_show")), cli({"ok": True})]) as run:
                self.assertEqual(self.request("/api/cards/%2312/run", "POST", {**base, "worktree": branch}, self.auth())[0], 200)
                self.assertIn("--worktree=" + branch, self.argv(run)[-1])
        for branch in ["-x", "a..b", "a b", "a~1", "x.lock", "a/x.lock/b", "a/.b", "a//b", "a/", "a.", "@", "a@{1}",
                       "a:b", "a?", "a*", "a[", "a\\b", "/a", ".a", "a^b", "a\x7fb"]:
            with self.subTest(branch=branch), patch("server.subprocess.run", return_value=cli(fixture("card_show"))) as run:
                self.assertEqual(self.request("/api/cards/%2312/run", "POST", {**base, "worktree": branch}, self.auth())[0], 400)
                self.assertEqual(run.call_count, 1)

    def test_note_without_an_id_is_refused(self):
        with patch("server.subprocess.run", return_value=cli(fixture("card_show"))) as run:
            self.assertEqual(self.request("/api/cards/%2312/note", "POST", {"text": "x"}, self.auth())[0], 400)
        self.assertEqual(run.call_count, 1)

    def test_open_code_opens_on_the_chosen_laptop(self):
        session = {"session": "01JF0000000000000000000002", "host": "leaseweb"}
        with patch("server.subprocess.run", side_effect=[cli(fixture("card_show")), cli({"landed": "tui"})]) as run:
            self.assertEqual(self.request("/api/cards/%2312/open-code", "POST", {**session, "on": "perseu-mbp"}, self.auth())[0], 200)
        self.assertEqual(self.argv(run)[-1][:3], ["open", "--session=01JF0000000000000000000002", "--on=perseu-mbp"])
        with patch("server.subprocess.run", return_value=cli(fixture("card_show"))) as run:
            for on in [None, "", "-x", "a b"]:
                with self.subTest(on=on):
                    self.assertEqual(self.request("/api/cards/%2312/open-code", "POST", {**session, "on": on}, self.auth())[0], 400)

    def test_open_code_unreachable_shows_command(self):
        session = {"session": "01JF0000000000000000000002", "host": "leaseweb", "on": "perseu-mbp"}
        with patch("server.subprocess.run", side_effect=[cli(fixture("card_show")), cli(None, 3, "ssh failed /home/perseu")]):
            status, _, raw = self.request("/api/cards/%2312/open-code", "POST", session, self.auth())
        self.assertEqual((status, json.loads(raw)), (409, {"error": "Nebula not reachable on perseu-mbp",
                                                           "command": "nebula open --session=01JF0000000000000000000002"}))


if __name__ == "__main__":
    unittest.main()
