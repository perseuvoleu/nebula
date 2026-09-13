#!/usr/bin/env python3
"""Start the real gateway on a loopback port against the fake CLI and synthetic Pi files.

State dir: .scratch/mobile-chat-e2e/state (recreated on every start). Token is synthetic.
"""
import json
import os
import shutil
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
WEB = HERE.parent
sys.path.insert(0, str(WEB))
from test_pi_history import assistant, entry, tool_result, user, write_session  # noqa: E402

STATE = WEB.parent / ".scratch" / "mobile-chat-e2e" / "state"
TOKEN = "e2e-synthetic-token-not-a-secret-0123456789"
PORT = int(os.environ.get("E2E_PORT", "8790"))
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
           "NEBULA_WEB_PORT": str(PORT), "NEBULA_WEB_ORIGIN": f"http://127.0.0.1:{PORT}",
           "NEBULA_WEB_BINARY": str(HERE / "fake_nebula.py"), "NEBULA_WEB_PI_AGENT_DIR": str(STATE / "pi"),
           "FAKE_NEBULA_STATE": str(STATE), "PYTHONDONTWRITEBYTECODE": "1",
           "PYTHONPATH": str(WEB), "PATH": str(STATE / "bin") + os.pathsep + os.environ["PATH"]}
    os.execve(sys.executable, [sys.executable, str(WEB / "server.py")], env)


if __name__ == "__main__":
    main()
