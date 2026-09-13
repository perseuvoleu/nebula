#!/usr/bin/env python3
"""Single-user Nebula mobile gateway. Bind only to loopback behind HTTPS."""
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

STATIC = Path(__file__).parent / "static"
FILES = {"/": ("index.html", "text/html"), "/app.js": ("app.js", "text/javascript"),
         "/style.css": ("style.css", "text/css"), "/sw.js": ("sw.js", "text/javascript"),
         "/manifest.webmanifest": ("manifest.webmanifest", "application/manifest+json"),
         "/icon.svg": ("icon.svg", "image/svg+xml"),
         "/icon-192.png": ("icon-192.png", "image/png"),
         "/icon-512.png": ("icon-512.png", "image/png")}
KINDS = {"pi", "claude", "codex", "cursor"}


class Problem(Exception):
    def __init__(self, status, message):
        self.status, self.message = status, message


class Gateway:
    def __init__(self, token, origin, projects, binary="nebula"):
        if len(token) < 32:
            raise ValueError("NEBULA_WEB_TOKEN must contain at least 32 characters")
        parsed = urlsplit(origin)
        if (parsed.scheme not in {"http", "https"} or not parsed.netloc
                or parsed.path or parsed.query or parsed.fragment or parsed.username):
            raise ValueError("NEBULA_WEB_ORIGIN must be an exact origin without a trailing slash")
        if parsed.scheme != "https" and parsed.hostname not in {"localhost", "127.0.0.1"}:
            raise ValueError("Non-local access requires HTTPS")
        if not projects:
            raise ValueError("NEBULA_WEB_PROJECTS must list at least one project")
        self.token, self.origin, self.host = token, origin, parsed.netloc
        self.projects, self.binary = projects, binary
        self.slots = threading.BoundedSemaphore(4)

    def run(self, *args, json_output=True):
        if not self.slots.acquire(blocking=False):
            raise Problem(503, "Gateway busy; try again shortly")
        try:
            # Never inherit the parent agent's identity or pass gateway credentials to CLIs.
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(("NEBULA_", "MC_", "PI_"))}
            result = subprocess.run([self.binary, *args], capture_output=True, text=True,
                                    errors="replace", timeout=20, env=env)
            if result.returncode:
                raise Problem(502, result.stderr.strip()[:2000] or "Nebula command failed")
            return json.loads(result.stdout) if json_output else result.stdout
        except subprocess.TimeoutExpired:
            raise Problem(504, "Nebula timed out. Check sessions before retrying an action.")
        except (OSError, ValueError):
            raise Problem(502, "Cannot read Nebula response; check the gateway installation")
        finally:
            self.slots.release()

    def project(self, value):
        if not isinstance(value, str) or value not in self.projects:
            raise Problem(403, "Project is not enabled")
        return value

    def sessions(self, project):
        return self.run("agent", "list", "--project", project)

    def session_name(self, project, session_id):
        rows = self.sessions(project)
        row = next((r for r in rows if r["id"] == session_id and not r["archived"]), None)
        if not row:
            raise Problem(404, "Session no longer available")
        # The current CLI selects by name, not id. Refuse ambiguous names.
        if sum(r["name"] == row["name"] for r in rows) != 1:
            raise Problem(409, "Duplicate session name; rename it in Nebula first")
        return row["name"]

    def get(self, path, query):
        if path == "/api/projects":
            return {"projects": self.projects}
        project = self.project(query.get("project", [None])[0])
        if path == "/api/sessions":
            return {"sessions": [r for r in self.sessions(project) if not r["archived"]]}
        if path == "/api/worktrees":
            return {"worktrees": self.run("worktree", "list", "--project", project)}
        if path == "/api/output":
            name = self.session_name(project, query.get("id", [None])[0])
            return {"output": self.run("agent", "read", "--project", project,
                                       "--lines", "250", "--", name, json_output=False)}
        raise Problem(404, "Unknown endpoint")

    def post(self, path, body):
        project = self.project(body.get("project"))
        text = body.get("text")
        if not isinstance(text, str) or not text.strip() or len(text) > 16000 or "\0" in text:
            raise Problem(400, "Message must contain 1–16000 characters without NUL")
        if path == "/api/send":
            name = self.session_name(project, body.get("id"))
            self.run("agent", "send", "--project", project, "--", name, text, json_output=False)
            return {"ok": True}
        if path == "/api/new":
            kind, worktree = body.get("kind"), body.get("worktree")
            if not isinstance(kind, str) or kind not in KINDS:
                raise Problem(400, "Unknown agent kind")
            trees = self.run("worktree", "list", "--project", project)
            selected = next((w for w in trees if w["path"] == worktree), None)
            if not isinstance(worktree, str) or selected is None:
                raise Problem(400, "Choose an existing checkout")
            selector = None
            for candidate in [selected["branch"], Path(worktree).name, "primary"]:
                hits = [w for w in trees if w["branch"] == candidate
                        or Path(w["path"]).name == candidate
                        or (candidate in {"root", "primary"} and w["is_main"])]
                if candidate and len(hits) == 1 and hits[0]["path"] == worktree:
                    selector = candidate
                    break
            if selector is None:
                raise Problem(409, "Checkout has no unambiguous CLI selector")
            # Unique names also avoid CLI name-resolution ambiguity on creation.
            name = "Mobile " + secrets.token_hex(6)
            self.run("agent", "new", "--project", project, f"--worktree={selector}",
                     f"--kind={kind}", f"--name={name}", f"--prompt={text}", json_output=False)
            return {"ok": True, "name": name}
        raise Problem(404, "Unknown endpoint")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, format, *args):
        # No request paths, tokens, message content, or Nebula output in access logs.
        pass

    def reply(self, status, data, content_type="application/json"):
        if not isinstance(data, bytes):
            data = json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type + "; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")
        self.end_headers()
        self.wfile.write(data)

    def handle_request(self, method):
        try:
            gateway = self.server.gateway
            if self.headers.get("Host") != gateway.host:
                raise Problem(403, "Invalid host")
            path = urlsplit(self.path).path
            if method == "GET" and path in FILES:
                filename, mime = FILES[path]
                return self.reply(200, (STATIC / filename).read_bytes(), mime)
            if not path.startswith("/api/"):
                raise Problem(404, "Not found")
            if not hmac.compare_digest(self.headers.get("Authorization", "").encode(),
                                       ("Bearer " + gateway.token).encode()):
                raise Problem(401, "Authentication required")
            origin = self.headers.get("Origin")
            if origin is not None and origin != gateway.origin:
                raise Problem(403, "Invalid origin")
            if method == "GET":
                result = gateway.get(path, parse_qs(urlsplit(self.path).query))
            else:
                if origin != gateway.origin:
                    raise Problem(403, "Origin required")
                if self.headers.get("Content-Type") != "application/json":
                    raise Problem(415, "Expected application/json")
                raw_length = self.headers.get("Content-Length", "")
                if not re.fullmatch(r"[0-9]{1,6}", raw_length) or not 0 < int(raw_length) <= 100000:
                    raise Problem(413, "Invalid request size")
                try:
                    body = json.loads(self.rfile.read(int(raw_length)))
                except (ValueError, UnicodeError):
                    raise Problem(400, "Invalid JSON")
                if not isinstance(body, dict):
                    raise Problem(400, "Expected a JSON object")
                result = gateway.post(path, body)
            self.reply(200, result)
        except Problem as error:
            self.reply(error.status, {"error": error.message})
        except (BrokenPipeError, ConnectionResetError, TimeoutError):
            pass
        except Exception:
            self.reply(500, {"error": "Gateway error; check server configuration"})

    def do_GET(self):
        self.handle_request("GET")

    def do_POST(self):
        self.handle_request("POST")


def main():
    port = int(os.environ.get("NEBULA_WEB_PORT", "8765"))
    gateway = Gateway(os.environ.get("NEBULA_WEB_TOKEN", ""),
                      os.environ.get("NEBULA_WEB_ORIGIN", f"http://127.0.0.1:{port}"),
                      [p.strip() for p in os.environ.get("NEBULA_WEB_PROJECTS", "").split(",") if p.strip()],
                      os.environ.get("NEBULA_WEB_BINARY", "nebula"))
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.gateway = gateway
    print(f"Nebula web listening on 127.0.0.1:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
