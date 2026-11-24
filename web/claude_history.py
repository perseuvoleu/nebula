"""Read-only, paged Claude transcript text, independent of the terminal's screen.

Reads chronological main-session user/assistant text (including retained pre-compaction
history), not the model's reconstructed context. Tool payloads, thinking, system entries,
sidechains and injected metadata are never exposed. Reads from the tail for the initial
page and seeks by byte position for older pages and append polling.
"""
import json
import os
from pathlib import Path
import re
import uuid

DEFAULT_LIMIT = 30
MAX_LIMIT = 100
TEXT_LIMIT = 40000
PAGE_CHARS = 120000
CHUNK_SIZE = 65536
MAX_RECORD_BYTES = 2 * 1024 * 1024


class ClaudeHistoryError(Exception):
    """Persisted history is unavailable or a history position is no longer valid."""


def locate_session_file(config_dir, cwd, session_id):
    """Resolve only the stored native identity under this exact checkout's directory."""
    try:
        if str(uuid.UUID(session_id)) != session_id:
            raise ValueError()
    except (ValueError, TypeError, AttributeError):
        raise ClaudeHistoryError("Claude session identity is missing or malformed")
    root = Path(config_dir).resolve() / "projects"
    directory = root / re.sub(r"[^a-zA-Z0-9]", "-", os.path.realpath(cwd))
    file = directory / f"{session_id}.jsonl"
    if directory.is_symlink() or file.is_symlink() or file.resolve().parent != directory:
        raise ClaudeHistoryError("Claude history path is outside its project directory")
    if not file.is_file():
        raise ClaudeHistoryError("Claude has not persisted history for this session yet")
    with _open(file) as handle:
        for _ in range(128):
            line = _read_line(handle)
            if line == b"":
                break
            identity = (_entry(line) or {}).get("sessionId")
            if identity is not None:
                if identity != session_id:
                    raise ClaudeHistoryError("Claude history does not match the stored session identity")
                return file
    raise ClaudeHistoryError("Claude has not persisted an identifiable session yet")


def _open(path):
    try:
        return open(path, "rb")
    except OSError:
        raise ClaudeHistoryError("Claude history is unavailable; retry or use the terminal view")


def _entry(line):
    if line is None:
        return None
    try:
        entry = json.loads(line)
        return entry if isinstance(entry, dict) else {}
    except (ValueError, UnicodeDecodeError, RecursionError):
        return {}


def _message(entry, position, session_id):
    if entry is None:
        return {"id": f"{position}:oversized", "role": "notice", "timestamp": None,
                "label": "Înregistrare omisă", "parts": [{"type": "text", "text":
                    "Această înregistrare depășește 2 MiB și nu poate fi afișată aici."}]}
    role = entry.get("type")
    if (role not in ("user", "assistant") or entry.get("isMeta") or entry.get("isSidechain")
            or entry.get("teamName") or entry.get("sessionId") != session_id
            or not isinstance(entry.get("uuid"), str)):
        return None
    message = entry.get("message")
    if not isinstance(message, dict):
        return None
    content = message.get("content")
    if isinstance(content, str):
        content = [{"type": "text", "text": content}]
    if not isinstance(content, list):
        return None
    text = "\n".join(block["text"] for block in content if isinstance(block, dict)
                     and block.get("type") == "text" and isinstance(block.get("text"), str))
    if not text.strip():
        return None
    part = {"type": "text", "text": text[:TEXT_LIMIT]}
    if len(text) > TEXT_LIMIT:
        part["truncated"] = True
    return {"id": f"{position}:{entry['uuid']}", "role": "notice" if entry.get("isCompactSummary") else role,
            "timestamp": entry.get("timestamp"), "parts": [part],
            **({"label": "compaction"} if entry.get("isCompactSummary") else {})}


def _complete_end(handle):
    handle.seek(0, os.SEEK_END)
    end = handle.tell()
    while end:
        start = max(0, end - CHUNK_SIZE)
        handle.seek(start)
        block = handle.read(end - start)
        newline = block.rfind(b"\n")
        if newline >= 0:
            return start + newline + 1
        end = start
    return 0


def _read_line(handle):
    line = handle.readline(MAX_RECORD_BYTES + 1)
    if len(line) <= MAX_RECORD_BYTES:
        return line
    while line and not line.endswith(b"\n"):
        line = handle.readline(CHUNK_SIZE)
    return None


def _reverse_lines(handle, end):
    # Skip the last complete newline. Fragments are joined once per record;
    # oversized records are discarded incrementally, in either read direction.
    position = max(0, end - 1)
    fragments, size = [], 0
    while position:
        start = max(0, position - CHUNK_SIZE)
        handle.seek(start)
        block = handle.read(position - start)
        segments = block.split(b"\n")
        boundary = position
        for segment in reversed(segments[1:]):
            boundary -= len(segment)
            size += len(segment)
            if size + 1 <= MAX_RECORD_BYTES:
                fragments.append(segment)
                yield boundary, b"".join(reversed(fragments)) + b"\n"
            else:
                yield boundary, None
            fragments, size = [], 0
            boundary -= 1
        size += len(segments[0])
        if size + 1 <= MAX_RECORD_BYTES:
            fragments.append(segments[0])
        else:
            fragments = []
        position = start
    if end:
        yield 0, b"".join(reversed(fragments)) + b"\n" if size + 1 <= MAX_RECORD_BYTES else None


def _generation(handle, session_id):
    stat = os.fstat(handle.fileno())
    return f"{session_id}:{stat.st_dev}:{stat.st_ino}"


def snapshot(path, session_id, limit=DEFAULT_LIMIT, before=None):
    """Return a bounded recent page; `before` is an opaque message position from a prior page."""
    limit = max(1, min(int(limit or DEFAULT_LIMIT), MAX_LIMIT))
    with _open(path) as handle:
        end = _complete_end(handle)
        cursor = {"generation": _generation(handle, session_id), "offset": end, "leaf": None}
        if before is not None:
            try:
                offset, expected_uuid = before.split(":", 1)
                offset = int(offset)
                if not 0 <= offset < end:
                    raise ValueError()
                handle.seek(offset)
                anchor = _entry(_read_line(handle))
                if anchor is None and expected_uuid == "oversized":
                    anchor = {"uuid": "oversized", "sessionId": session_id}
                anchor = anchor or {}
                if f"{offset}:{anchor.get('uuid')}" != before or anchor.get("sessionId") != session_id:
                    raise ValueError()
                end = offset
            except (ValueError, AttributeError):
                raise ClaudeHistoryError("Unknown history position; reload the conversation")
        messages, chars, has_more = [], 0, False
        for position, line in _reverse_lines(handle, end):
            message = _message(_entry(line), position, session_id)
            if message is None:
                continue
            size = len(message["parts"][0]["text"])
            if len(messages) >= limit or (messages and chars + size > PAGE_CHARS):
                has_more = True
                break
            messages.append(message)
            chars += size
        messages.reverse()
        return {"kind": "snapshot", "messages": messages, "hasMore": has_more,
                "unknownEntries": 0, "name": None, "cursor": cursor,
                "partial": has_more or before is not None}


def apply_cursor(path, session_id, cursor):
    """Read only newly completed records; reset after file replacement, truncation or invalid cursor."""
    with _open(path) as handle:
        end = _complete_end(handle)
        generation = _generation(handle, session_id)
        offset = cursor.get("offset") if isinstance(cursor, dict) else None
        if (not isinstance(offset, int) or not 0 <= offset <= end
                or cursor.get("generation") != generation):
            return {**snapshot(path, session_id), "kind": "reset"}
        if offset:
            handle.seek(offset - 1)
            if handle.read(1) != b"\n":
                return {**snapshot(path, session_id), "kind": "reset"}
        handle.seek(offset)
        messages, chars = [], 0
        while handle.tell() < end:
            position = handle.tell()
            line = _read_line(handle)
            message = _message(_entry(line), position, session_id)
            if message:
                size = len(message["parts"][0]["text"])
                if len(messages) >= DEFAULT_LIMIT or (messages and chars + size > PAGE_CHARS):
                    handle.seek(position)
                    break
                messages.append(message)
                chars += size
        next_cursor = {"generation": generation, "offset": handle.tell(), "leaf": None}
        if not messages:
            return {"kind": "none", "cursor": next_cursor}
        return {"kind": "delta", "messages": messages, "toolResults": [], "cursor": next_cursor}
