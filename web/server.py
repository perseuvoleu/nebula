#!/usr/bin/env python3
"""Single-user Nebula mobile gateway. Bind only to loopback behind HTTPS.

Owns authentication, the enabled-project scope, argument-safe Nebula CLI control and the
read-only Pi conversation contract (see pi_history.py). Serves the built React app from
`web/static` through a startup allowlist: no directory listing, no arbitrary file reads.
"""
import base64
import datetime
import hmac
import json
import os
from pathlib import Path
import re
import secrets
import socket
import sqlite3
import subprocess
import sys
import threading
import tempfile
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, quote, urlsplit

import pi_history

STATIC = Path(__file__).parent / "static"
KINDS = {"pi", "claude", "codex", "cursor"}
ATTEMPT = re.compile(r"[A-Za-z0-9_-]{4,64}")
ROUTE = re.compile(r"/(?:p/[A-Za-z0-9._@-]{1,80}(?:/s/[A-Za-z0-9._-]{1,80})?)?/?")
ASSET_TYPES = {".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml",
               ".png": "image/png", ".ico": "image/x-icon", ".webmanifest": "application/manifest+json",
               ".json": "application/json", ".woff2": "font/woff2", ".woff": "font/woff", ".txt": "text/plain",
               ".map": "application/json"}
IDENTITY_TTL = 5.0
TRANSIENT = {502, 503, 504}  # gateway/CLI trouble: reported as an error, not as a capability
PUSH_STATES = {"finished", "needs_feedback"}
CSP = ("default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; "
       "worker-src 'self'; manifest-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'")


class Problem(Exception):
    def __init__(self, status, message):
        self.status, self.message = status, message


class PushService:
    """Persistent Web Push subscriptions and transition-based agent notifications."""

    def __init__(self, private_key, subject, storage, *, sender=None, public_key=None):
        self.storage = Path(storage)
        self.lock = threading.Lock()
        self.statuses = {}
        if sender is None:
            try:
                from cryptography.hazmat.primitives import serialization
                from py_vapid import Vapid
                from pywebpush import webpush
                from requests import Session
            except ImportError as error:
                raise ValueError("Web Push dependencies missing; install web/requirements.txt") from error
            key = Path(private_key)
            if not key.is_absolute() or not key.is_file():
                raise ValueError("NEBULA_WEB_VAPID_PRIVATE_KEY must be an absolute existing file")
            vapid = Vapid.from_file(str(key))
            raw = vapid.public_key.public_bytes(serialization.Encoding.X962,
                                                serialization.PublicFormat.UncompressedPoint)
            public_key = base64.urlsafe_b64encode(raw).decode().rstrip("=")
            class PushSession(Session):
                """Send only to the validated provider, without netrc, proxies or redirects."""

                def request(self, method, url, **kwargs):
                    """Prevent a provider redirect from forwarding encrypted payloads elsewhere."""
                    kwargs["allow_redirects"] = False
                    return super().request(method, url, **kwargs)

            transport = PushSession()
            transport.trust_env = False
            sender = lambda subscription, data: webpush(
                subscription_info=subscription, data=json.dumps(data), vapid_private_key=vapid,
                vapid_claims={"sub": subject}, ttl=300, timeout=10, requests_session=transport)
        self.sender, self.public_key = sender, public_key
        if not isinstance(public_key, str) or len(public_key) < 80:
            raise ValueError("Invalid VAPID public key")

    @staticmethod
    def validate(subscription):
        if not isinstance(subscription, dict) or set(subscription) not in ({"endpoint", "keys"}, {"endpoint", "keys", "expirationTime"}):
            raise Problem(400, "Invalid push subscription")
        endpoint, keys = subscription["endpoint"], subscription["keys"]
        try:
            parsed = urlsplit(endpoint) if isinstance(endpoint, str) else None
            hostname = parsed.hostname if parsed else None
            port = parsed.port if parsed else None
        except ValueError:
            parsed, hostname = None, None
        if (parsed is None or parsed.scheme != "https" or not hostname or parsed.username
                or parsed.password or parsed.fragment or port not in (None, 443)
                or hostname not in {"web.push.apple.com", "fcm.googleapis.com", "updates.push.services.mozilla.com"}
                or re.search(r"[\x00-\x20\x7f\\]", endpoint) or len(endpoint) > 4096 or not isinstance(keys, dict)
                or set(keys) != {"p256dh", "auth"}):
            raise Problem(400, "Invalid push subscription")
        for name, minimum in (("p256dh", 40), ("auth", 8)):
            value = keys.get(name)
            if not isinstance(value, str) or not minimum <= len(value) <= 256 or not re.fullmatch(r"[A-Za-z0-9_-]+", value):
                raise Problem(400, "Invalid push subscription")
        return {"endpoint": endpoint, "keys": keys}

    def subscriptions(self):
        try:
            value = json.loads(self.storage.read_text())
        except (OSError, ValueError):
            return []
        if not isinstance(value, list):
            return []
        valid = []
        for subscription in value:
            try:
                valid.append(self.validate(subscription))
            except Problem:
                pass
        return valid

    def save(self, subscriptions):
        self.storage.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, name = tempfile.mkstemp(prefix=".push-", dir=self.storage.parent)
        temporary = Path(name)
        try:
            with os.fdopen(descriptor, "w") as file:
                json.dump(subscriptions, file)
            temporary.replace(self.storage)
        finally:
            temporary.unlink(missing_ok=True)

    def subscribe(self, subscription):
        subscription = self.validate(subscription)
        with self.lock:
            subscriptions = [s for s in self.subscriptions()
                             if isinstance(s, dict) and s.get("endpoint") != subscription["endpoint"]]
            subscriptions.append(subscription)
            self.save(subscriptions[-8:])

    def unsubscribe(self, endpoint):
        if not isinstance(endpoint, str) or len(endpoint) > 4096:
            raise Problem(400, "Invalid push endpoint")
        with self.lock:
            subscriptions = [s for s in self.subscriptions()
                             if isinstance(s, dict) and s.get("endpoint") != endpoint]
            self.save(subscriptions)

    def notify(self, project, row):
        status = row["status"]
        data = {
            "title": "Nebula",
            "body": f"{row['name']}: " + ("a terminat" if status == "finished" else "așteaptă răspunsul tău"),
            "url": f"/p/{quote(project, safe='')}/s/{quote(row['id'], safe='')}",
            "tag": f"nebula-{row['id']}",
        }
        with self.lock:
            subscriptions = self.subscriptions()
        expired = set()
        for subscription in subscriptions:
            try:
                self.sender(subscription, data)
            except Exception as error:
                response = getattr(error, "response", None)
                if getattr(response, "status_code", None) in {404, 410}:
                    expired.add(subscription["endpoint"])
                else:
                    print("Nebula Web Push delivery failed", file=sys.stderr, flush=True)
        if expired:
            with self.lock:
                self.save([s for s in self.subscriptions() if s.get("endpoint") not in expired])

    def poll(self, gateway):
        if not self.subscriptions():
            self.statuses.clear()
            return
        for project in gateway.projects:
            try:
                rows = [row for row in gateway.sessions(project) if not row["archived"]]
            except Problem:
                continue
            present = set()
            for row in rows:
                key = (project, row["id"])
                present.add(key)
                previous = self.statuses.get(key)
                self.statuses[key] = row["status"]
                if previous is not None and previous != row["status"] and row["status"] in PUSH_STATES:
                    self.notify(project, row)
            for key in [key for key in self.statuses if key[0] == project and key not in present]:
                del self.statuses[key]


class StaticFiles:
    """Startup allowlist of regular files under the build directory (never symlinks, never re-scanned)."""

    def __init__(self, root):
        self.root = Path(root)
        self.files = {}
        if self.root.is_dir():
            for path in sorted(self.root.rglob("*")):
                relative = "/" + path.relative_to(self.root).as_posix()
                if (path.is_file() and not path.is_symlink() and path.suffix in ASSET_TYPES
                        and not any(part.startswith(".") for part in path.relative_to(self.root).parts)):
                    self.files[relative] = (path.read_bytes(), path.suffix)

    def lookup(self, path):
        """(file, mime, cache) for an asset, the app shell for client routes, else None."""
        if path == "/" or (path not in self.files and ROUTE.fullmatch(path)):
            shell = self.files.get("/index.html")
            return (shell[0], "text/html", "no-cache") if shell else None
        file = self.files.get(path)
        if not file:
            return None
        immutable = path.startswith("/assets/") and re.search(r"-[A-Za-z0-9_-]{6,}\.\w+$", path)
        return file[0], ASSET_TYPES[file[1]], "public, max-age=31536000, immutable" if immutable else "no-cache"


class Gateway:
    def __init__(self, token, origin, projects, binary="nebula", static=None, pi_agent_dir=None, db_path=None, *, preview_binary=None, push=None):
        if len(token) < 8:
            raise ValueError("NEBULA_WEB_TOKEN must contain at least 8 characters")
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
        # None explicitly disables optional preview discovery; never fall back to the main CLI.
        if preview_binary is not None and (not Path(preview_binary).is_absolute()
                or not Path(preview_binary).is_file() or not os.access(preview_binary, os.X_OK)):
            raise ValueError("NEBULA_WEB_PREVIEW_BINARY must be an absolute executable file")
        self.preview_binary = preview_binary
        self.static = static or StaticFiles(STATIC)
        self.pi_agent_dir = Path(pi_agent_dir) if pi_agent_dir else default_pi_agent_dir()
        self.db_path = Path(db_path) if db_path else None
        self.hostname = socket.gethostname()
        self.slots = threading.BoundedSemaphore(4)
        self.identities = {}  # agent id -> (native session id | None, reason, expires)
        self.lock = threading.Lock()
        self.checkout_lock = threading.Lock()
        self.push = push

    def run(self, *args, json_output=True, binary=None):
        if not self.slots.acquire(blocking=False):
            raise Problem(503, "Gateway busy; try again shortly")
        try:
            # Never inherit the parent agent's identity or pass gateway credentials to CLIs.
            env = {k: v for k, v in os.environ.items()
                   if not k.startswith(("NEBULA_", "MC_", "PI_"))}
            executable = binary or self.binary
            if binary is None and args[:1] == ("preview",):
                if (len(args) != 4 or args[1] != "--project" or args[2] not in self.projects
                        or args[3] != "status" or self.preview_binary is None):
                    raise Problem(503, "Preview status unavailable")
                executable = self.preview_binary
            result = subprocess.run([executable, *args], capture_output=True, text=True,
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

    def session_row(self, project, session_id, *, by_name=True):
        rows = self.sessions(project)
        row = next((r for r in rows if r["id"] == session_id and not r["archived"]), None)
        if not row:
            raise Problem(404, "Session no longer available")
        # Legacy read/show/send select by name; ID-only controls do not.
        if by_name and sum(r["name"] == row["name"] for r in rows) != 1:
            raise Problem(409, "Duplicate session name; rename it in Nebula first")
        return row

    def session_name(self, project, session_id):
        return self.session_row(project, session_id)["name"]

    # --- native identity -------------------------------------------------------------------

    def native_session_id(self, project, row, detail=None):
        """Stored native Pi session id for a Nebula agent, cached briefly.

        Source order: the `agent show` JSON field `session_id` (CLI ≥ this branch), else a
        read-only lookup of the daemon's SQLite row when NEBULA_WEB_DB is configured.
        """
        with self.lock:
            cached = self.identities.get(row["id"])
            if cached and cached[2] > time.monotonic():
                if cached[1]:
                    raise Problem(409, cached[1])
                return cached[0]
        try:
            detail = detail if detail is not None else self.run("agent", "show", "--project", project, "--", row["name"])
            if detail.get("id") not in (None, row["id"]):
                raise Problem(409, "Session name resolved to a different agent; retry")
            if "session_id" in detail:
                value = detail["session_id"]
            elif self.db_path:
                value = self.db_session_id(row["id"])
            else:
                value = None
            if not value:
                raise Problem(409, "Nebula has not recorded a native Pi session identity for this session yet")
        except Problem as error:
            if error.status in TRANSIENT:
                raise  # busy/timeout/CLI failure: not an identity fact, never cached or "unsupported"
            with self.lock:
                self.identities[row["id"]] = (None, error.message, time.monotonic() + IDENTITY_TTL)
            raise
        with self.lock:
            self.identities[row["id"]] = (value, None, time.monotonic() + IDENTITY_TTL)
        return value

    def db_session_id(self, agent_id):
        try:
            con = sqlite3.connect(f"file:{self.db_path}?mode=ro", uri=True, timeout=1)
            try:
                columns = {r[1] for r in con.execute("PRAGMA table_info(agents)")}
                column = next((c for c in ("session_id", "claude_session_id") if c in columns), None)
                if column is None or "id" not in columns:
                    raise Problem(500, "Nebula database schema is not recognised; upgrade the gateway")
                found = con.execute(f"select {column} from agents where id = ?", (agent_id,)).fetchone()
            finally:
                con.close()
        except sqlite3.Error:
            raise Problem(502, "Cannot read the Nebula database")
        if found is None:
            raise Problem(404, "Session no longer available")
        return found[0]

    def capabilities(self, row, structured, reason=None):
        return {"structured": structured, "reason": reason, "liveText": False,
                "liveTextReason": "Nebula's managed Pi extension reports lifecycle only; partial text needs a new "
                                  "extension and a daemon restart, so replies appear once each message completes",
                "tools": structured, "send": True, "terminal": True, "kind": row["kind"]}

    def conversation_source(self, project, session_id):
        row = self.session_row(project, session_id)
        if row["kind"] != "pi":
            return row, None, None, f"Structured history is only implemented for Pi; {row['kind']} sessions use terminal output"
        try:
            native = self.native_session_id(project, row)
            path = pi_history.locate_session_file(self.pi_agent_dir, row["path"], native)
        except Problem as error:
            if error.status in TRANSIENT:
                raise
            return row, None, None, error.message
        except pi_history.PiHistoryError as error:
            return row, None, None, str(error)
        return row, native, path, None

    # --- HTTP verbs ------------------------------------------------------------------------

    def get(self, path, query):
        if path == "/api/projects":
            return {"projects": self.projects, "host": self.hostname}
        if path == "/api/push":
            return {"available": self.push is not None,
                    "publicKey": self.push.public_key if self.push else None}
        project = self.project(query.get("project", [None])[0])
        first = {k: v[0] for k, v in query.items()}
        if path == "/api/sessions":
            return {"sessions": [public(r) for r in self.sessions(project) if not r["archived"]]}
        if path == "/api/preview":
            try:
                status = self.run("preview", "--project", project, "status")
            except Problem:
                # CLI stderr may contain private diagnostic details. Never return it here.
                return {"url": None, "phase": None}
            domain = status.get("domain") if isinstance(status, dict) else None
            phase = status.get("phase") if isinstance(status, dict) else None
            # Match the preview policy's DNS-label rules, including its reserved control domain.
            if (not isinstance(phase, str) or phase not in
                    {"running", "stopped", "stale", "recovering", "orphaned_route", "unknown"}
                    or not isinstance(domain, str) or len(domain) > 253 or "." not in domain
                    or domain == "code.possu.dev" or not all(
                        re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
                        for label in domain.split("."))):
                return {"url": None, "phase": None}
            return {"url": f"https://{domain}" if phase == "running" else None, "phase": phase}
        if path == "/api/worktrees":
            return {"worktrees": self.run("worktree", "list", "--project", project)}
        if path == "/api/output":
            name = self.session_name(project, first.get("id"))
            lines = first.get("lines", "250")
            if not isinstance(lines, str) or not lines.isdigit() or not 250 <= int(lines) <= 10000:
                raise Problem(400, "Output limit must be between 250 and 10000 lines")
            output = self.run("agent", "read", "--project", project, "--lines", lines, "--", name, json_output=False)
            return {"output": output, "hasMore": len(output.splitlines()) >= int(lines) and int(lines) < 10000}
        if path in {"/api/conversation", "/api/conversation/updates", "/api/conversation/tool"}:
            row, native, file, reason = self.conversation_source(project, first.get("id"))
            row = public(row)
            if reason:
                return {"session": row, "capabilities": self.capabilities(row, False, reason)}
            try:
                if path == "/api/conversation/tool":
                    call = first.get("call", "")
                    if not re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", call):
                        raise Problem(400, "Invalid tool call id")
                    return {"session": row, "tool": pi_history.tool_output(file, call)}
                if path == "/api/conversation":
                    limit = first.get("limit", "")
                    snapshot = pi_history.snapshot(file, native, int(limit) if limit.isdigit() else None,
                                                   first.get("before"))
                    return {"session": row, "capabilities": self.capabilities(row, True), "snapshot": snapshot}
                cursor = {"generation": first.get("generation"), "leaf": first.get("leaf") or None,
                          "offset": int(first["offset"]) if first.get("offset", "").isdigit() else -1}
                return {"session": row, **pi_history.apply_cursor(file, native, cursor)}
            except pi_history.PiHistoryError as error:
                raise Problem(409, str(error))
        raise Problem(404, "Unknown endpoint")

    def create_checkout(self, project, body):
        name, mode = body.get("name"), body.get("mode")
        if (not isinstance(name, str) or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", name)
                or len(name) > 80 or name in {"main", "master"}):
            raise Problem(400, "Folosește un nume nou: litere mici, cifre și cratime (maxim 80).")
        if not isinstance(mode, str) or mode not in {"branch", "worktree"}:
            raise Problem(400, "Alege Branch sau Worktree")
        # Serialize only creation, not ordinary reads/sends. No mutation retries.
        if not self.checkout_lock.acquire(blocking=False):
            raise Problem(503, "O creare este deja în curs; verifică lista înainte de a reîncerca")
        try:
            trees = self.run("worktree", "list", "--project", project)
            source = next((w for w in trees if w["path"] == body.get("source")), None)
            primary = next((w for w in trees if w["is_main"]), None)
            if not source or not primary:
                raise Problem(400, "Alege un checkout existent din acest proiect")
            if source["branch"] != body.get("base"):
                raise Problem(409, "Branch-ul de bază s-a schimbat; reîncarcă lista")
            if mode == "branch" and any(s.get("alive") for s in primary.get("sessions", [])):
                raise Problem(409, "Checkout-ul principal are sesiuni deschise. Alege Worktree pentru a nu le schimba fișierele.")
            # Read-only Git checks against the daemon-resolved project path, never a user path.
            refs = self.run("-C", primary["path"], "for-each-ref", "--format=%(refname:short)",
                            "refs/heads/", binary="git", json_output=False).splitlines()
            if name in refs:
                raise Problem(409, "Branch-ul există deja; alege un nume nou")
            if mode == "branch" and self.run("-C", primary["path"], "status", "--porcelain",
                                              binary="git", json_output=False).strip():
                raise Problem(409, "Checkout-ul principal are modificări necomise. Alege Worktree.")
            args = ("worktree", "new") if mode == "worktree" else ("switch",)
            created = self.run(*args, "--project", project, f"--from={source['branch']}", "--", name)
            if not isinstance(created, dict) or not isinstance(created.get("path"), str) or not created["path"] or created.get("branch") != name:
                raise Problem(504, "Crearea poate să fi reușit; verifică lista înainte de a reîncerca")
            return {"worktree": {"path": created["path"], "branch": name, "project": project,
                                 "is_main": mode == "branch", "sessions": []}}
        finally:
            self.checkout_lock.release()

    def post(self, path, body):
        if path == "/api/push/subscribe":
            if not self.push:
                raise Problem(503, "Push notifications are not configured")
            self.push.subscribe(body.get("subscription"))
            return {"ok": True}
        if path == "/api/push/unsubscribe":
            if self.push:
                self.push.unsubscribe(body.get("endpoint"))
            return {"ok": True}
        project = self.project(body.get("project"))
        if path == "/api/worktrees":
            return self.create_checkout(project, body)
        if path in {"/api/worktrees/check-delete", "/api/worktrees/delete"}:
            trees = self.run("worktree", "list", "--project", project)
            tree = next((w for w in trees if w.get("id") and w["id"] == body.get("id")), None)
            if not tree:
                raise Problem(409, "Checkout unavailable or CLI too old; refresh and check installation")
            if tree["is_main"] or tree["branch"] in {"main", "master"}:
                raise Problem(409, "Primary checkout and main/master are protected")
            if tree["path"] != body.get("path") or tree["branch"] != body.get("branch"):
                raise Problem(409, "Checkout changed; refresh before confirming")
            args = ("--check",) if path.endswith("check-delete") else ()
            self.run("worktree", "delete-safe", "--project", project, *args,
                     "--", tree["id"], tree["branch"], tree["path"])
            return {"ok": True}
        if path == "/api/sessions/delete":
            row = self.session_row(project, body.get("id"), by_name=False)
            self.run("agent", "delete-id", "--project", project, "--", row["id"])
            return {"ok": True}
        if path == "/api/key":
            key = body.get("key")
            if not isinstance(key, str) or key not in {"Up", "Down", "Left", "Right", "Enter", "Escape", "Tab"}:
                raise Problem(400, "Unsupported terminal key")
            row = self.session_row(project, body.get("id"), by_name=False)
            self.run("agent", "key", "--project", project, "--", row["id"], key)
            return {"ok": True}
        text = body.get("text")
        if not isinstance(text, str) or not text.strip() or len(text) > 16000 or "\0" in text:
            raise Problem(400, "Message must contain 1–16000 characters without NUL")
        if path == "/api/send":
            attempt = body.get("attempt", "")
            if not isinstance(attempt, str) or not ATTEMPT.fullmatch(attempt):
                raise Problem(400, "Delivery attempt id is required")
            name = self.session_name(project, body.get("id"))
            self.run("agent", "send", "--project", project, "--", name, text, json_output=False)
            return {"ok": True, "attempt": attempt,
                    "acceptedAt": datetime.datetime.now(datetime.timezone.utc).isoformat()}
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
            for attempt in range(3):
                matches = [r for r in self.sessions(project) if r["name"] == name and not r["archived"]]
                if len(matches) == 1:
                    return {"ok": True, "name": name, "id": matches[0]["id"]}
                time.sleep(0.3 * (attempt + 1))
            return {"ok": True, "name": name, "id": None}
        raise Problem(404, "Unknown endpoint")


def public(row):
    """Session row for the browser: native ids stay server-side."""
    return {k: v for k, v in row.items() if k != "session_id"}


def default_pi_agent_dir():
    configured = os.environ.get("NEBULA_WEB_PI_AGENT_DIR") or os.environ.get("PI_CODING_AGENT_DIR")
    return Path(configured) if configured else Path.home() / ".pi" / "agent"


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def setup(self):
        super().setup()
        self.connection.settimeout(10)

    def log_message(self, format, *args):
        # No request paths, tokens, message content, or Nebula output in access logs.
        pass

    def reply(self, status, data, content_type="application/json", cache="no-store"):
        if not isinstance(data, bytes):
            data = json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type + "; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", cache)
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", CSP)
        self.end_headers()
        self.wfile.write(data)

    def handle_request(self, method):
        try:
            gateway = self.server.gateway
            if self.headers.get("Host") != gateway.host:
                raise Problem(403, "Invalid host")
            path = urlsplit(self.path).path
            if method == "GET" and not path.startswith("/api/"):
                asset = gateway.static.lookup(path)
                if asset is None:
                    raise Problem(404, "Not found")
                file, mime, cache = asset
                return self.reply(200, file, mime, cache)
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
    origin = os.environ.get("NEBULA_WEB_ORIGIN", f"http://127.0.0.1:{port}")
    private_key = os.environ.get("NEBULA_WEB_VAPID_PRIVATE_KEY")
    push = PushService(private_key, os.environ.get("NEBULA_WEB_VAPID_SUBJECT", origin),
                       os.environ.get("NEBULA_WEB_PUSH_STORE", str(Path.home() / ".local/share/nebula/web-push.json"))) if private_key else None
    gateway = Gateway(os.environ.get("NEBULA_WEB_TOKEN", ""), origin,
                      [p.strip() for p in os.environ.get("NEBULA_WEB_PROJECTS", "").split(",") if p.strip()],
                      os.environ.get("NEBULA_WEB_BINARY", "nebula"),
                      db_path=os.environ.get("NEBULA_WEB_DB") or None,
                      preview_binary=os.environ.get("NEBULA_WEB_PREVIEW_BINARY"), push=push)
    if not gateway.static.files.get("/index.html"):
        raise SystemExit(f"No built app in {gateway.static.root}; run `pnpm --dir web/app build` first")
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.gateway = gateway
    if push:
        def monitor():
            while True:
                try:
                    push.poll(gateway)
                except Exception:
                    print("Nebula Web Push monitor failed", file=sys.stderr, flush=True)
                time.sleep(5)
        threading.Thread(target=monitor, name="web-push", daemon=True).start()
    print(f"Nebula web listening on 127.0.0.1:{port}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
