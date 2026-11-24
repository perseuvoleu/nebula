#!/usr/bin/env python3
"""Deterministic stand-in for the `nebula` CLI at the gateway's external boundary.

State lives in $FAKE_NEBULA_STATE: agents.json, worktrees.json, sends.log, behaviour, a Pi
agent dir (`pi/`) with synthetic session files, and board.json (cards, their sessions,
artifacts and log, decisions) seeded from the golden board fixtures. Tests mutate these files
to simulate the host.
"""
import fcntl
import json
import os
import sys
import time
import uuid
from pathlib import Path

STATE = Path(os.environ["FAKE_NEBULA_STATE"])


def load(name):
    return json.loads((STATE / name).read_text())


def save(name, data):
    temporary = STATE / f".{name}.{os.getpid()}"
    temporary.write_text(json.dumps(data, indent=1))
    os.replace(temporary, STATE / name)


def arg(flag, args):
    for index, item in enumerate(args):
        if item == flag:
            return args[index + 1]
        if item.startswith(flag + "="):
            return item.split("=", 1)[1]
    return None


def options(args):
    return args[:args.index("--")] if "--" in args else args


def positionals(args):
    if "--" in args:
        return args[args.index("--") + 1:]
    return [a for a in args if not a.startswith("--")]


def session_file(agent):
    from test_pi_history import session_dir_for  # synthetic fixture helpers, no private data
    directory = session_dir_for(STATE / "pi", agent["path"])
    hits = list(directory.glob(f"*_{agent['session_id']}.jsonl")) if agent.get("session_id") else []
    return hits[0] if hits else None


def append(agent, entry):
    path = session_file(agent)
    entries = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
    entry = {"type": "message", "id": uuid.uuid4().hex[:8], "parentId": entries[-1]["id"] if len(entries) > 1 else None,
             "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime()), "message": entry}
    with open(path, "a") as handle:
        handle.write(json.dumps(entry) + "\n")


def scoped(card, args):
    projects = [a.split("=", 1)[1] for a in args if a.startswith("--project=")]
    if card["project"] is None:
        return "--include-home" in args
    return "--all" in args or card["project"] in projects


def find_card(board, ref, args):
    number = ref.lstrip("#").split("@")[0]
    card = next((c for c in board["cards"] if c["id"] == ref or (number.isdigit() and c["number"] == int(number))), None)
    return card if card and scoped(card, args) else None


def away(board, cards, args):
    since = arg("--away-since", args)
    if since is None:
        return {"finished": [], "needs_you": [], "decisions": []}
    after = lambda event: event["at"] > int(since)
    ids = {c["id"] for c in cards}
    return {"finished": [e["id"] for e in board["away"]["finished"] if after(e) and e["id"] in ids],
            "needs_you": [{"host": e["host"], "id": e["id"]} for e in board["away"]["needs_you"] if after(e)],
            "decisions": [d["id"] for d in board["decisions"] if d["created_at"] > int(since) and scoped(d, args)]}


def board_command(verb, args, board):
    """One `todo`/`decision` verb against board.json; returns (exit code, output)."""
    flags = lambda name: arg(name, options(args))
    values = positionals(args)
    args = options(args)
    version = f"{board['board_id']}.{board['version']}"
    if verb == ("todo", "board"):
        if (STATE / "behaviour").exists() and (STATE / "behaviour").read_text().strip() == "fail":
            return 3, "board host unreachable"
        if flags("--since") == version:
            return 0, {"version": version, "unchanged": True}
        cards = [c for c in board["cards"] if scoped(c, args) and not c["archived"]]
        hosts = [{**h, "checkouts": [c for c in h["checkouts"] if scoped(c, args)]} for h in board["hosts"]]
        return 0, {"version": version, "board": {"cards": cards, "hosts": hosts, "away": away(board, cards, args)}, "next": None}
    if verb == ("decision", "list"):
        query = (flags("--query") or "").lower()
        return 0, {"items": [d for d in board["decisions"] if scoped(d, args) and query in (d["title"] + d["content"]["body"]).lower()],
                   "next": None}
    if verb == ("decision", "add"):
        decision = {"id": f"01JG{len(board['decisions']) + 100:022d}", "kind": "decision", "title": values[0],
                    "card": flags("--card"), "project": flags("--project"), "session": None, "checkout": None,
                    "content": {"body": flags("--why") or ""}, "meta": {}, "created_at": int(time.time() * 1000)}
        board["decisions"].insert(0, decision)
        return 0, decision
    if verb == ("todo", "add"):
        number = max((c["number"] for c in board["cards"]), default=0) + 1
        card = {"id": flags("--id") or f"01JD{number:022d}", "number": number, "ref": f"#{number}",
                "title": values[0], "stage": flags("--stage") or "backlog", "project": flags("--project"),
                "proposed": "--propose" in args, "archived": False,
                "created_by": {"host": "fake", "agent": None, "name": "web"}, "checkout": None, "blocked_by": []}
        existing = next((c for c in board["cards"] if c["id"] == card["id"]), None)
        if existing:
            return 0, existing
        board["cards"].append(card)
        return 0, card
    card = find_card(board, values[0] if values else "", ["--all", "--include-home"])
    if card is None:
        return 5, "no such card"
    if verb == ("todo", "show"):
        if not scoped(card, args):
            return 5, "no such card"
        return 0, {**card, **board["details"].get(card["id"], {"sessions": [], "artifacts": [], "log": []})}
    if verb == ("todo", "move"):
        if flags("--expect") not in (None, card["stage"]):
            return 6, "card moved meanwhile"
        card["stage"] = values[1]
    elif verb == ("todo", "edit"):
        card["title"], card["project"] = flags("--title") or card["title"], flags("--project") or card["project"]
    elif verb == ("todo", "note"):
        board["details"].setdefault(card["id"], {"sessions": [], "artifacts": [], "log": []})["log"].append(
            {"id": f"01JH{time.time_ns():022d}"[:26], "card": card["id"], "text": values[1],
             "author": {"host": "fake", "agent": None, "name": "web"}, "created_at": int(time.time() * 1000)})
    elif verb[1] in {"archive", "dismiss"}:
        card["archived"] = True
    elif verb == ("todo", "accept"):
        card["proposed"] = False
    elif verb[1] in {"block", "unblock"}:
        by = find_card(board, flags("--by"), ["--all", "--include-home"])
        if by is None or by["id"] == card["id"]:
            return 6, "cannot block on that card"
        card["blocked_by"] = [r for r in card["blocked_by"] if r != by["ref"]] + ([by["ref"]] if verb[1] == "block" else [])
    elif verb == ("todo", "run"):
        card["stage"] = "doing"
        return 0, {"dispatch": flags("--dispatch") or f"01JE{time.time_ns():022d}"[:26], "state": "started",
                   "session": {"host": flags("--host"), "id": "01JF0000000000000000000002"}, "card": card["id"]}
    elif verb == ("todo", "approve"):
        return 0, {"ok": True}
    else:
        return 2, f"unsupported fake board verb: {verb}"
    return 0, card


def main(argv):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    command = tuple(argv[:2])
    project = arg("--project", argv)
    agents = load("agents.json")
    behaviour = (STATE / "behaviour").read_text().strip() if (STATE / "behaviour").exists() else "ok"
    if argv[0] == "-C":  # read-only git checks through the harness-only PATH shim
        trees = load("worktrees.json")
        primary = next(w for w in trees if w["path"] == argv[1])
        if "for-each-ref" in argv:
            print("\n".join(w["branch"] for w in trees if w["project"] == primary["project"]))
        elif "status" in argv:
            print("")
        else:
            return 2
    elif command == ("agent", "list"):
        print(json.dumps([a for a in agents if a["project"] == project]))  # rows carry session_id, as on this branch
    elif command == ("agent", "show"):
        name = positionals(argv[2:])[0]
        hits = [a for a in agents if a["project"] == project and a["name"] == name and not a["archived"]]
        if len(hits) != 1:
            print("no such session", file=sys.stderr)
            return 1
        print(json.dumps(hits[0]))  # includes session_id, like `agent show` on this branch
    elif command == ("agent", "read"):
        name = positionals(argv[2:])[0]
        count = int((STATE / "output-lines").read_text()) if (STATE / "output-lines").exists() else 40
        lines = [f"$ terminal snapshot for {name}"] + [f"line {i}: build step {i} ok" for i in range(1, count)]
        if (STATE / "output.txt").exists():
            lines = (STATE / "output.txt").read_text().splitlines()
        print("\n".join(lines[-int(arg("--lines", argv) or len(lines)):]))
    elif command in {("agent", "key"), ("agent", "delete-id")}:
        values = positionals(argv[2:])
        hits = [a for a in agents if a["id"] == values[0] and a["project"] == project and not a["archived"]]
        with open(STATE / "sends.log", "a") as handle:
            handle.write(json.dumps({"action": command[1], "id": values[0], "key": values[1] if len(values) > 1 else ""}) + "\n")
        if not hits or behaviour == "fail":
            print("Session unavailable or PTY closed", file=sys.stderr)
            return 1
        if behaviour == "hang": time.sleep(30)
        if command[1] == "delete-id": save("agents.json", [a for a in agents if a["id"] != values[0]])
        print(json.dumps({"ok": True}))
    elif command == ("worktree", "delete-safe"):
        id, branch, path = positionals(argv[2:])
        trees = load("worktrees.json")
        tree = next((w for w in trees if w.get("id") == id and w["project"] == project), None)
        reason = None
        if not tree or tree["branch"] != branch or tree["path"] != path: reason = "Checkout changed"
        elif tree["is_main"] or branch in {"main", "master"}: reason = "Protected checkout"
        elif any(a["path"] == path for a in agents): reason = "Sessions still exist"
        elif tree.get("dirty"): reason = "Dirty checkout"
        elif tree.get("unmerged"): reason = "Unmerged commits"
        elif behaviour == "fail": reason = "Checkout unavailable"
        if reason:
            print(reason, file=sys.stderr)
            return 1
        if "--check" not in argv:
            save("worktrees.json", [w for w in trees if w.get("id") != id])
            with open(STATE / "sends.log", "a") as handle: handle.write(json.dumps({"action": "delete-safe", "id": id}) + "\n")
        print(json.dumps({"ok": True}))
    elif command == ("agent", "send"):
        name, text = positionals(argv[2:])[:2]
        with open(STATE / "sends.log", "a") as handle:
            handle.write(json.dumps({"name": name, "text": text}) + "\n")
        if behaviour == "fail":
            print("agent send failed: PTY closed", file=sys.stderr)
            return 1
        if behaviour == "hang":
            time.sleep(30)
        agent = next(a for a in agents if a["project"] == project and a["name"] == name)
        # "silent": the PTY accepted the text but Pi has not persisted a user entry (yet).
        if agent["kind"] == "pi" and session_file(agent) and behaviour != "silent":
            append(agent, {"role": "user", "content": text, "timestamp": int(time.time() * 1000)})
        agent["status"] = "running"
        save("agents.json", agents)
        print(json.dumps({"ok": True}))
    elif command == ("agent", "new"):
        name, kind, prompt = arg("--name", argv), arg("--kind", argv), arg("--prompt", argv)
        selector = arg("--worktree", argv)
        trees = [w for w in load("worktrees.json") if w["project"] == project]
        tree = next(w for w in trees if w["branch"] == selector or Path(w["path"]).name == selector
                    or (selector in {"root", "primary"} and w["is_main"]))
        sid = str(uuid.uuid4())
        row = {"id": f"new-{uuid.uuid4().hex[:6]}", "name": name, "kind": kind, "status": "running", "project": project,
               "worktree": tree["branch"], "path": tree["path"], "archived": False, "session_id": sid if kind == "pi" else None}
        agents.append(row)
        save("agents.json", agents)
        if kind == "pi":
            from test_pi_history import assistant, user, write_session
            write_session(STATE / "pi", tree["path"], sid,
                          [user("n1", None, prompt), assistant("n2", "n1", [{"type": "text", "text": "Am început sarcina."}])])
        with open(STATE / "sends.log", "a") as handle:
            handle.write(json.dumps({"new": name, "kind": kind, "text": prompt}) + "\n")
        print(json.dumps({"ok": True}))
    elif command == ("worktree", "new") or argv[0] == "switch":
        name, base = positionals(argv)[-1], arg("--from", argv)
        trees = load("worktrees.json")
        primary = next(w for w in trees if w["project"] == project and w["is_main"])
        if argv[0] == "switch":
            primary["branch"] = name
            row = primary
        else:
            row = {"id": f"wt-{uuid.uuid4().hex[:8]}", "path": f"/srv/{project}-worktrees/{name}", "branch": name, "is_main": False,
                   "project": project, "created_from": base}
            trees.append(row)
        save("worktrees.json", trees)
        print(json.dumps({**row, "created": True}))
    elif argv[0] == "open":
        print(json.dumps({"landed": "tui", "tui_pid": 1, "id": arg("--session", argv), "name": "worker",
                          "project": "nebula", "host": arg("--on", argv), "focused": True}))
    elif argv[0] in {"todo", "decision"}:
        with open(STATE / "board.lock", "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            board = load("board.json")
            before = json.dumps(board)
            code, output = board_command(command, argv[2:], board)
            if json.dumps(board) != before:
                board["version"] += 1
                save("board.json", board)
        if code:
            print(output, file=sys.stderr)
            return code
        print(json.dumps(output))
    elif command == ("worktree", "list"):
        trees = [w for w in load("worktrees.json") if w["project"] == project]
        for tree in trees:
            tree["sessions"] = [{"name": a["name"], "kind": a["kind"], "status": a["status"], "alive": True}
                                for a in agents if a["path"] == tree["path"] and not a["archived"]]
        print(json.dumps(trees))
    else:
        print(f"unsupported fake command: {argv}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
