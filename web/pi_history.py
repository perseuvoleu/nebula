"""Read-only adapter for Pi's persisted session history (pi-coding-agent 0.84, session v3).

Authoritative source: `<agent_dir>/sessions/--<cwd with / replaced by ->--/<stamp>_<session id>.jsonl`.
The file is a JSONL tree (`id`/`parentId`); Pi itself treats the last entry of the file as the
active leaf, so the displayed conversation is the root→leaf path with compactions marked.
Nothing here attaches to, resumes or writes the session. Hidden context (thinking blocks,
`display: false` custom messages, extension state) is never surfaced.
"""
import json
import os
import re
from pathlib import Path

SESSION_ID = re.compile(r"[0-9A-Za-z][0-9A-Za-z_-]{7,127}")
TEXT_LIMIT = 40000       # characters of one message text part kept in a snapshot
TOOL_LIMIT = 6000        # characters of tool arguments/output kept inline
FULL_TOOL_LIMIT = 200000  # characters served by the dedicated tool-output endpoint
DEFAULT_LIMIT = 150
MAX_LIMIT = 500


class PiHistoryError(Exception):
    """Native history cannot be resolved safely; the message is user-facing."""


def session_dir_for(agent_dir, cwd):
    encoded = "--" + re.sub(r"[/\\:]", "-", re.sub(r"^[/\\]", "", os.path.realpath(cwd))) + "--"
    return Path(agent_dir) / "sessions" / encoded


def locate_session_file(agent_dir, cwd, session_id):
    if not isinstance(session_id, str) or not SESSION_ID.fullmatch(session_id):
        raise PiHistoryError("Native session identity is missing or malformed")
    directory = session_dir_for(agent_dir, cwd)
    if not directory.is_dir():
        raise PiHistoryError("Pi has not persisted a session for this checkout yet")
    matches = [p for p in directory.glob(f"*_{session_id}.jsonl") if p.name.endswith(f"_{session_id}.jsonl")]
    if len(matches) != 1:
        raise PiHistoryError("No persisted history for this session yet" if not matches
                             else "Ambiguous native session files; refusing to guess")
    path = matches[0]
    root = os.path.realpath(directory)
    if path.is_symlink() or os.path.commonpath([root, os.path.realpath(path)]) != root:
        raise PiHistoryError("Native session file is outside the Pi sessions directory")
    with open(path, "rb") as handle:
        first = handle.readline()
    try:
        head = json.loads(first)
    except ValueError:
        raise PiHistoryError("Native session header is unreadable")
    if not isinstance(head, dict) or head.get("type") != "session" or head.get("id") != session_id:
        raise PiHistoryError("Native session header does not match the stored identity")
    return path


def read_entries(path, offset=0):
    """Parse complete JSONL lines from `offset`. Returns (entries, next_offset, has_partial_tail)."""
    with open(path, "rb") as handle:
        handle.seek(offset)
        data = handle.read()
    entries, consumed = [], 0
    for line in data.splitlines(keepends=True):
        if not line.endswith(b"\n"):
            return entries, offset + consumed, True
        consumed += len(line)
        text = line.strip()
        if not text:
            continue
        try:
            item = json.loads(text)
        except ValueError:
            continue  # Pi skips malformed lines as well
        if isinstance(item, dict) and item.get("type") != "session" and isinstance(item.get("id"), str):
            entries.append(item)
    return entries, offset + consumed, False


def _clip(text, limit):
    text = text if isinstance(text, str) else ""
    return (text[:limit], True) if len(text) > limit else (text, False)


def _text_parts(content, limit=TEXT_LIMIT):
    parts = []
    if isinstance(content, str):
        content = [{"type": "text", "text": content}]
    for block in content if isinstance(content, list) else []:
        if not isinstance(block, dict):
            continue
        if block.get("type") == "text":
            text, truncated = _clip(block.get("text"), limit)
            parts.append({"type": "text", "text": text, **({"truncated": True} if truncated else {})})
        elif block.get("type") == "image":
            parts.append({"type": "image", "mimeType": str(block.get("mimeType", ""))})
    return parts


def _tool_part(block):
    args, truncated = _clip(json.dumps(block.get("arguments", {}), ensure_ascii=False, sort_keys=True), TOOL_LIMIT)
    return {"type": "tool", "callId": str(block.get("id", "")), "name": str(block.get("name", "")),
            "args": args, "argsTruncated": truncated, "state": "pending", "output": "", "outputLength": 0,
            "truncated": False}


def tool_result_view(message, limit=TOOL_LIMIT):
    text = "\n".join(b.get("text", "") for b in message.get("content", []) if isinstance(b, dict)
                     and b.get("type") == "text") if isinstance(message.get("content"), list) else ""
    clipped, truncated = _clip(text, limit)
    return {"callId": str(message.get("toolCallId", "")), "name": str(message.get("toolName", "")),
            "state": "error" if message.get("isError") else "done", "output": clipped,
            "outputLength": len(text), "truncated": truncated}


def _message(entry, role, parts, **extra):
    return {"id": entry["id"], "role": role, "timestamp": entry.get("timestamp"), "parts": parts, **extra}


def _convert(entry):
    """One entry → (message | None, tool_result | None, unknown)."""
    kind = entry.get("type")
    if kind == "message":
        message = entry.get("message") or {}
        role = message.get("role")
        if role == "user":
            return _message(entry, "user", _text_parts(message.get("content"))), None, False
        if role == "assistant":
            parts = []
            for block in message.get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "text":
                    parts.extend(_text_parts([block]))
                elif block.get("type") == "toolCall":
                    parts.append(_tool_part(block))
                # thinking blocks are hidden internal reasoning: never surfaced
            extra = {"model": str(message.get("model", "")), "stopReason": str(message.get("stopReason", ""))}
            if message.get("stopReason") == "error" and message.get("errorMessage"):
                extra["error"] = str(message["errorMessage"])[:2000]
            return _message(entry, "assistant", parts, **extra), None, False
        if role == "toolResult":
            return None, tool_result_view(message), False
        if role == "bashExecution":
            output, truncated = _clip(message.get("output"), TOOL_LIMIT)
            part = {"type": "tool", "callId": entry["id"], "name": "bash!", "args": str(message.get("command", ""))[:TOOL_LIMIT],
                    "argsTruncated": False, "state": "error" if message.get("exitCode") not in (0, None) else "done",
                    "output": output, "outputLength": len(message.get("output") or ""),
                    "truncated": truncated or bool(message.get("truncated"))}
            return _message(entry, "tool", [part]), None, False
        if role in {"custom", "hookMessage"}:
            if not message.get("display"):
                return None, None, False
            return _message(entry, "notice", _text_parts(message.get("content")), label=str(message.get("customType", ""))), None, False
        return None, None, True
    if kind == "custom_message":
        if not entry.get("display"):
            return None, None, False
        return _message(entry, "notice", _text_parts(entry.get("content")), label=str(entry.get("customType", ""))), None, False
    if kind == "branch_summary":
        return _message(entry, "notice", _text_parts(str(entry.get("summary", ""))), label="branch"), None, False
    if kind == "compaction":
        return _message(entry, "notice", _text_parts(str(entry.get("summary", ""))), label="compaction"), None, False
    if kind in {"custom", "label", "model_change", "thinking_level_change", "session_info"}:
        return None, None, False
    return None, None, True


def active_path(entries):
    """Root→leaf path; the leaf is the last entry, exactly as Pi's SessionManager loads a file."""
    if not entries:
        return []
    by_id = {e["id"]: e for e in entries}
    path, seen, node = [], set(), entries[-1]
    while node is not None and node["id"] not in seen:
        seen.add(node["id"])
        path.append(node)
        parent = node.get("parentId")
        node = by_id.get(parent) if isinstance(parent, str) else None
    path.reverse()
    return path


def normalize(entries):
    messages, results, unknown = [], [], 0
    for item in active_path(entries):
        message, result, is_unknown = _convert(item)
        unknown += is_unknown
        if message:
            messages.append(message)
        if result:
            results.append(result)
    _merge_results(messages, results)
    name = next((e.get("name") for e in reversed(entries) if e.get("type") == "session_info"), None)
    return {"messages": messages, "unknownEntries": unknown, "name": name if isinstance(name, str) else None}


def _merge_results(messages, results):
    for result in results:
        for message in reversed(messages):
            part = next((p for p in message["parts"] if p.get("type") == "tool" and p["callId"] == result["callId"]), None)
            if part:
                part.update({k: result[k] for k in ("state", "output", "outputLength", "truncated")})
                break


def _cursor(session_id, offset, leaf):
    return {"generation": session_id, "offset": offset, "leaf": leaf}


def snapshot(path, session_id, limit=DEFAULT_LIMIT, before=None):
    entries, offset, _ = read_entries(path)
    result = normalize(entries)
    messages = result["messages"]
    end = len(messages)
    if before is not None:
        index = next((i for i, m in enumerate(messages) if m["id"] == before), None)
        if index is None:
            raise PiHistoryError("Unknown history position; reload the conversation")
        end = index
    limit = max(1, min(int(limit or DEFAULT_LIMIT), MAX_LIMIT))
    start = max(0, end - limit)
    return {"kind": "snapshot", "messages": messages[start:end], "hasMore": start > 0,
            "unknownEntries": result["unknownEntries"], "name": result["name"],
            "cursor": _cursor(session_id, offset, entries[-1]["id"] if entries else None),
            "partial": start > 0 or end < len(messages)}


def apply_cursor(path, session_id, cursor):
    """Continue from `cursor`: {"kind": "none"} | delta of newly appended path entries | full reset."""
    valid = (isinstance(cursor, dict) and cursor.get("generation") == session_id
             and isinstance(cursor.get("offset"), int) and 0 <= cursor["offset"] <= os.path.getsize(path))
    if not valid:
        return snapshot_reset(path, session_id)
    added, offset, _ = read_entries(path, cursor["offset"])
    if offset == cursor["offset"] and not added:
        return {"kind": "none", "cursor": cursor}
    leaf = cursor.get("leaf")
    messages, results = [], []
    for item in added:
        if item.get("parentId") != leaf or item.get("type") == "compaction":
            return snapshot_reset(path, session_id)
        leaf = item["id"]
        message, result, _ = _convert(item)
        if message:
            messages.append(message)
        if result:
            results.append(result)
    _merge_results(messages, results)  # results for calls inside this delta merge locally
    return {"kind": "delta", "messages": messages, "toolResults": results,
            "cursor": _cursor(session_id, offset, leaf)}


def snapshot_reset(path, session_id):
    data = snapshot(path, session_id)
    data["kind"] = "reset"
    return data


def tool_output(path, call_id):
    """Full (bounded) output of one tool result on the active path, by stable call id."""
    entries, _, _ = read_entries(path)
    for item in reversed(active_path(entries)):
        message = item.get("message") if item.get("type") == "message" else None
        if isinstance(message, dict) and message.get("role") == "toolResult" and str(message.get("toolCallId")) == call_id:
            return tool_result_view(message, FULL_TOOL_LIMIT)
        if isinstance(message, dict) and message.get("role") == "bashExecution" and item["id"] == call_id:
            output, truncated = _clip(message.get("output"), FULL_TOOL_LIMIT)
            return {"callId": call_id, "name": "bash!", "state": "done", "output": output,
                    "outputLength": len(message.get("output") or ""), "truncated": truncated}
    raise PiHistoryError("Tool output not found on the active conversation")
