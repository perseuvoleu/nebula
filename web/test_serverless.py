"""Serverless run listing against a fake Cloudflare Worker on loopback."""
import json
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import serverless

SECRET = "cf-synthetic-secret-token"
FINISHED = {"id": "e564497e", "repo": "https://github.com/unjs/h3", "ref": None, "cache_mode": "modules",
            "instance": "standard-2", "started_at": 1792323464686, "ended_at": 1792323466574,
            "status": "passed", "cache": "modules hit (331 MB)", "max_cost_usd": 0.0000377,
            "steps": json.dumps([{"name": "boot", "ms": 184, "exitCode": 0}, {"name": "test", "ms": 1087, "exitCode": 1}])}
RUNNING = {"id": "f00d", "repo": "https://github.com/vitejs/vite", "ref": "main", "cache_mode": "off",
           "instance": "standard-4", "started_at": 1792323470000, "ended_at": None, "status": "running",
           "cache": None, "max_cost_usd": None, "steps": None}


class FakeWorker(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        self.server.seen.append((self.path, self.headers.get("Authorization")))
        status, body, delay = self.server.answer
        time.sleep(delay)
        data = body if isinstance(body, bytes) else json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


class ServerlessTests(unittest.TestCase):
    def setUp(self):
        self.worker = ThreadingHTTPServer(("127.0.0.1", 0), FakeWorker)
        self.worker.seen, self.worker.answer = [], (200, [RUNNING, FINISHED], 0)
        threading.Thread(target=self.worker.serve_forever, daemon=True).start()
        self.runs = serverless.ServerlessRuns(f"http://127.0.0.1:{self.worker.server_port}", SECRET, timeout=0.5)

    def tearDown(self):
        self.worker.shutdown()
        self.worker.server_close()

    def test_lists_runs_with_bearer_token_and_parsed_steps(self):
        result = self.runs.get("120")
        self.assertEqual(self.worker.seen, [("/runs?limit=120", "Bearer " + SECRET)])
        self.assertTrue(result["configured"])
        running, finished = result["runs"]
        self.assertEqual(running["status"], "running")
        self.assertIsNone(running["ended_at"])
        self.assertEqual(running["steps"], [])
        self.assertEqual(finished["steps"], [{"name": "boot", "ms": 184, "exitCode": 0},
                                             {"name": "test", "ms": 1087, "exitCode": 1}])
        self.assertEqual(finished["max_cost_usd"], 0.0000377)
        self.assertNotIn(SECRET, json.dumps(result))

    def test_limit_is_clamped(self):
        for raw, sent in [(None, "50"), ("", "50"), ("abc", "50"), ("0", "1"), ("9999", "500"), ("-3", "50")]:
            self.runs.get(raw)
            self.assertEqual(self.worker.seen[-1][0], f"/runs?limit={sent}", raw)

    def test_malformed_steps_and_rows_never_break_the_list(self):
        broken = {**FINISHED, "steps": "not json"}
        self.worker.answer = (200, [broken, "junk", {"id": 3}, {"id": "x", "started_at": 1, "status": []}], 0)
        result = self.runs.get(None)
        self.assertEqual([run["id"] for run in result["runs"]], ["e564497e"])
        self.assertEqual(result["runs"][0]["steps"], [])

    def test_upstream_failures_are_gateway_errors_without_the_token(self):
        for answer, status in [((401, {"error": SECRET}, 0), 502), ((500, b"boom", 0), 502),
                               ((200, b"<html>", 0), 502), ((200, [], 2), 504)]:
            self.worker.answer = answer
            with self.subTest(answer=answer[0]), self.assertRaises(serverless.ServerlessError) as error:
                self.runs.get(None)
            self.assertEqual(error.exception.status, status)
            self.assertNotIn(SECRET, error.exception.message)

    def test_missing_configuration_is_an_explicit_empty_answer(self):
        for env in [{}, {"NEBULA_SERVERLESS_URL": "https://w.example"}, {"NEBULA_SERVERLESS_TOKEN": SECRET}]:
            self.assertIsNone(serverless.from_environment(env))
        self.assertEqual(serverless.unconfigured(), {"configured": False, "runs": []})
        self.assertIsNotNone(serverless.from_environment({"NEBULA_SERVERLESS_URL": "https://w.example",
                                                          "NEBULA_SERVERLESS_TOKEN": SECRET}))

    def test_plain_http_is_refused_off_loopback(self):
        with self.assertRaises(ValueError):
            serverless.ServerlessRuns("http://worker.example", SECRET)


if __name__ == "__main__":
    unittest.main()
