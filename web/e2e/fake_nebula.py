#!/usr/bin/env python3
"""Deterministic stand-in for the `nebula` CLI at the gateway's external boundary.

State lives in $FAKE_NEBULA_STATE: agents.json, worktrees.json, sends.log, behaviour and a Pi
agent dir (`pi/`) with synthetic session files. Tests mutate these files to simulate the host.
"""
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
    (STATE / name).write_text(json.dumps(data, indent=1))


def arg(flag, args):
    for index, item in enumerate(args):
        if item == flag:
            return args[index + 1]
        if item.startswith(flag + "="):
            return item.split("=", 1)[1]
    return None


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
        print("\n".join(lines[-int(arg("--lines", argv) or len(lines)):]))
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
            row = {"path": f"/srv/{project}-worktrees/{name}", "branch": name, "is_main": False,
                   "project": project, "created_from": base}
            trees.append(row)
        save("worktrees.json", trees)
        print(json.dumps({**row, "created": True}))
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
