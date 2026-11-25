#!/usr/bin/env python3
"""Start the real gateway on a loopback port against the fake CLI and synthetic Pi files.

State dir: .scratch/mobile-chat-e2e/state (recreated on every start). Token is synthetic.
The board (project nebula) is seeded from the golden fixtures in crates/nebula-core.
`--real` serves the real lane instead: an isolated daemon at $REAL_NEBULA_STATE running
$REAL_NEBULA_BIN (both required; playwright.config.ts sets them for --project real).
"""
import base64
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent
sys.path.insert(0, str(WEB))
from test_pi_history import assistant, entry, tool_result, user, write_session  # noqa: E402

STATE = WEB.parent / ".scratch" / "mobile-chat-e2e" / "state"
FIXTURES = WEB.parent / "crates" / "nebula-core" / "tests" / "fixtures" / "board"
TOKEN = "e2e-synthetic-token-not-a-secret-0123456789"
PORT = int(os.environ.get("E2E_PORT", "8790"))
PNG = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=")
GATEWAY_ENV = ("PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR",
               "REAL_NEBULA_STATE", "REAL_NEBULA_BIN", "E2E_STATIC_DIR")
SID = {"pi-1": "aaaaaaaa-1111-4111-8111-aaaaaaaaaaaa", "pi-2": "bbbbbbbb-2222-4222-8222-bbbbbbbbbbbb",
       "pi-b": "cccccccc-3333-4333-8333-cccccccccccc"}

LONG_ANSWER = """Am terminat refactorizarea modulului de autentificare.

## Ce s-a schimbat

- `auth/session.py` folosește acum **token-uri semnate** cu rotație
- testele rulează în ~2 s (înainte 9 s)
- am eliminat dependența de `legacy_crypto`

```python
def verify(token: str, *, now: float | None = None) -> Claims:
    header, payload, signature = token.split(".")
    if not hmac.compare_digest(sign(header + "." + payload), signature):
        raise InvalidToken("signature mismatch")
    claims = Claims.from_json(base64url_decode(payload))
    if claims.expires_at < (now or time.time()):
        raise InvalidToken("expired")
    return claims
```

Link util: [documentația JWT](https://example.com/jwt). Un link nesigur: [click](javascript:alert(1)).

<script>alert("nu se execută")</script>

| Fișier | Linii |
| --- | --- |
| auth/session.py | +84 −120 |
| tests/test_session.py | +40 −8 |
"""


def fixture(name):
    return json.loads((FIXTURES / f"{name}.json").read_text())


def seed_board():
    board, show, artifact = fixture("board"), fixture("card_show"), fixture("artifact")
    board_id, version = board["version"].split(".")
    away = board["board"]["away"]
    hosts = board["board"]["hosts"]
    hosts[0]["checkouts"].append({"project": "vela", "branch": "main", "path": "/home/perseu/vela", "worktree": False})
    blocker = {**fixture("card"), "id": "01JD0000000000000000000011", "number": 11, "ref": "#11@leaseweb",
               "title": "Pin the board contract", "stage": "done", "checkout": None, "blocked_by": []}
    (STATE / "board.json").write_text(json.dumps({
        "board_id": board_id, "version": int(version), "cards": [blocker, *board["board"]["cards"]],
        "hosts": hosts, "decisions": [fixture("decision")],
        "away": {"finished": [{"id": i, "at": 1764000500000} for i in away["finished"]],
                 "needs_you": [{**s, "at": 1764000500000} for s in away["needs_you"]]},
        "details": {show["id"]: {key: show[key] for key in ("sessions", "artifacts", "log")}},
    }, indent=1))
    run = STATE / "artifacts" / artifact["id"]
    (run / "shots").mkdir(parents=True)
    result = fixture("e2e_run_result")
    (run / "result.json").write_text(json.dumps(result))
    for step in result["steps"]:
        (run / step["screenshot"]).write_bytes(PNG)


def seed():
    if STATE.exists():
        shutil.rmtree(STATE)
    STATE.mkdir(parents=True)
    (STATE / "pi").mkdir()
    (STATE / "bin").mkdir()
    (STATE / "bin" / "git").symlink_to(HERE / "fake_nebula.py")
    (STATE / "sends.log").write_text("")
    (STATE / "behaviour").write_text("ok")
    agents = [
        {"id": "pi-1", "name": "Refactor auth", "kind": "pi", "status": "finished", "project": "alpha",
         "worktree": "main", "path": "/srv/alpha", "archived": False, "session_id": SID["pi-1"]},
        {"id": "pi-2", "name": "A doua sesiune Pi", "kind": "pi", "status": "running", "project": "alpha",
         "worktree": "main", "path": "/srv/alpha", "archived": False, "session_id": SID["pi-2"]},
        {"id": "claude-1", "name": "Claude pe feature", "kind": "claude", "status": "running", "project": "alpha",
         "worktree": "feature/homepage", "path": "/srv/alpha-wt", "archived": False, "session_id": None},
        {"id": "pi-3", "name": "Pi fără identitate", "kind": "pi", "status": "fresh", "project": "alpha",
         "worktree": "main", "path": "/srv/alpha", "archived": False, "session_id": None},
        {"id": "old-1", "name": "Arhivată", "kind": "pi", "status": "finished", "project": "alpha",
         "worktree": "main", "path": "/srv/alpha", "archived": True, "session_id": None},
        {"id": "pi-b", "name": "Beta migration", "kind": "pi", "status": "finished", "project": "beta",
         "worktree": "main", "path": "/srv/beta", "archived": False, "session_id": SID["pi-b"]},
    ]
    (STATE / "agents.json").write_text(json.dumps(agents, indent=1))
    seed_board()
    (STATE / "worktrees.json").write_text(json.dumps([
        {"id": "wt-alpha", "path": "/srv/alpha", "branch": "main", "is_main": True, "project": "alpha", "pinned": False, "created_from": None},
        {"id": "wt-feature", "path": "/srv/alpha-wt", "branch": "feature/homepage", "is_main": False, "project": "alpha", "pinned": False, "created_from": "main"},
        {"id": "wt-beta", "path": "/srv/beta", "branch": "main", "is_main": True, "project": "beta", "pinned": False, "created_from": None},
    ]))
    big = "\n".join(f"[{i:04d}] compiling module {i} … ok" for i in range(400))
    write_session(STATE / "pi", "/srv/alpha", SID["pi-1"], [
        user("u1", None, "Refactorizează modulul de autentificare și rulează testele."),
        assistant("a1", "u1", [{"type": "thinking", "thinking": "hidden reasoning"},
                               {"type": "text", "text": "Mă uit întâi la structura actuală."},
                               {"type": "toolCall", "id": "call-read", "name": "read", "arguments": {"path": "auth/session.py"}}],
                  stop="toolUse"),
        tool_result("t1", "a1", "call-read", "import hmac\n\ndef verify(token):\n    ...\n"),
        assistant("a2", "t1", [{"type": "toolCall", "id": "call-bash", "name": "bash", "arguments": {"command": "make test"}}], stop="toolUse"),
        tool_result("t2", "a2", "call-bash", big, error=True),
        entry("custom_message", "c1", "t2", customType="nebula-context", content="HIDDEN CONTEXT", display=False),
        assistant("a3", "c1", [{"type": "text", "text": LONG_ANSWER}]),
        user("u2", "a3", "Mulțumesc. Poți adăuga și un test pentru token expirat?"),
        assistant("a4", "u2", [{"type": "text", "text": "Da — am adăugat `test_expired_token` și trece."}]),
    ])
    write_session(STATE / "pi", "/srv/alpha", SID["pi-2"], [
        user("x1", None, "Ce face a doua sesiune?"),
        assistant("x2", "x1", [{"type": "text", "text": "A doua sesiune Pi lucrează la altceva pe același checkout."}]),
    ], name="2026-09-13T12-00-00-000Z")
    write_session(STATE / "pi", "/srv/beta", SID["pi-b"], [
        user("b1", None, "Pregătește migrarea bazei de date beta."),
        assistant("b2", "b1", [{"type": "text", "text": "Migrarea **beta** este pregătită: 3 tabele, 0 conflicte."}]),
    ])


def main():
    seed()
    os.chmod(HERE / "fake_nebula.py", 0o755)
    env = {**os.environ, "NEBULA_WEB_TOKEN": TOKEN, "NEBULA_WEB_PROJECTS": "alpha,beta",
           "NEBULA_WEB_CLAUDE_DIR": str(STATE / "claude"), "NEBULA_WEB_USAGE": "0", "NEBULA_WEB_PORT": str(PORT), "NEBULA_WEB_BOARD_PROJECTS": "nebula,vela",
           "NEBULA_WEB_ARTIFACTS": str(STATE / "artifacts"), "NEBULA_WEB_ORIGIN": f"http://127.0.0.1:{PORT}",
           "NEBULA_WEB_BINARY": str(HERE / "fake_nebula.py"), "NEBULA_WEB_PI_AGENT_DIR": str(STATE / "pi"),
           "FAKE_NEBULA_STATE": str(STATE), "PYTHONDONTWRITEBYTECODE": "1",
           "PYTHONPATH": str(WEB), "PATH": str(STATE / "bin") + os.pathsep + os.environ["PATH"]}
    # Let browser tests serve an isolated build without replacing live web/static.
    script = "import os; from pathlib import Path; import server; " \
             "server.STATIC = Path(os.environ.get('E2E_STATIC_DIR', str(server.STATIC))); server.main()"
    os.execve(sys.executable, [sys.executable, "-c", script], env)


def serve_real():
    """The real lane: an isolated daemon at $REAL_NEBULA_STATE behind the real gateway, via real_nebula.sh."""
    state, binary = Path(os.environ["REAL_NEBULA_STATE"]), Path(os.environ["REAL_NEBULA_BIN"])
    if not os.access(binary, os.X_OK):
        raise SystemExit(f"No nebula at {binary}; run `heavy web/e2e/build_real_nebula.sh` first")
    if state.exists() and not (state / ".real-e2e").exists():
        raise SystemExit(f"{state} was not made by the real lane; refusing to delete it")
    shutil.rmtree(state, ignore_errors=True)
    for sub in ("rt", "data/artifacts", "repo", "codex", "pi", "claude"):
        (state / sub).mkdir(parents=True)
    (state / ".real-e2e").touch()
    (state / "rt").chmod(0o700)
    (state / "data" / "config.json").write_text(json.dumps({"board": "local", "auto_archive_after": "off"}))
    repo = state / "repo"
    subprocess.run(["git", "init", "-q", "-b", "main", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.email=e2e@nebula.dev", "-c", "user.name=real-e2e",
                    "commit", "-q", "--allow-empty", "-m", "init"], check=True)
    launcher = str(HERE / "real_nebula.sh")
    replay = WEB.parent / "crates" / "nebula" / "tests" / "replay"
    env = {**os.environ, "SHELL": "/bin/sh", "CODEX_HOME": str(state / "codex"), "PI_CODING_AGENT_DIR": str(state / "pi"),
           "NEBULA_AGENT_CMD": f"{replay / 'agent'} {replay / 'turn.replay'}", "NEBULA_STATUS_TICK_MS": "100"}
    daemon = subprocess.Popen([launcher, "daemon", "--foreground"], cwd=repo, env=env,
                              stdout=open(state / "daemon.log", "wb"), stderr=subprocess.STDOUT)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    try:
        deadline = time.monotonic() + 15
        while subprocess.run([launcher, "agent", "list", "--all"], env=env, capture_output=True).returncode:
            if time.monotonic() > deadline or daemon.poll() is not None:
                raise SystemExit(f"The real lane's daemon never answered; see {state / 'daemon.log'}")
            time.sleep(0.1)
        subprocess.run([launcher, "add", str(repo)], cwd=repo, env=env, check=True, capture_output=True)
        gateway = {**{k: os.environ[k] for k in GATEWAY_ENV if k in os.environ}, "NEBULA_WEB_TOKEN": TOKEN, "NEBULA_WEB_PROJECTS": "repo", "NEBULA_WEB_BOARD_PROJECTS": "repo",
                   "NEBULA_WEB_ARTIFACTS": str(state / "data" / "artifacts"), "NEBULA_WEB_BINARY": launcher,
                   "NEBULA_WEB_USAGE": "0", "NEBULA_WEB_PORT": str(PORT), "NEBULA_WEB_ORIGIN": f"http://127.0.0.1:{PORT}",
                   "NEBULA_WEB_CLAUDE_DIR": str(state / "claude"), "NEBULA_WEB_PI_AGENT_DIR": str(state / "pi"),
                   "PYTHONDONTWRITEBYTECODE": "1", "PYTHONPATH": str(WEB)}
        script = "import os; from pathlib import Path; import server; " \
                 "server.STATIC = Path(os.environ.get('E2E_STATIC_DIR', str(server.STATIC))); server.main()"
        subprocess.run([sys.executable, "-c", script], env=gateway)
    finally:
        daemon.terminate()
        try:
            daemon.wait(10)
        except subprocess.TimeoutExpired:
            daemon.kill()


if __name__ == "__main__":
    serve_real() if sys.argv[1:] == ["--real"] else main()
