"""Read-only list of build/test runs recorded by the Cloudflare serverless Worker.

The Worker token never leaves this module: it is not logged, echoed or included in errors.
"""
import http.client
import json
import math
import os
import socket
import threading
from urllib.parse import urlsplit

STATUSES = {"running", "passed", "failed", "error"}


class ServerlessError(Exception):
    """A fixed public message; never upstream text, which may echo request details."""

    def __init__(self, status, message):
        self.status, self.message = status, message


def _limit(raw):
    if not isinstance(raw, str) or not raw.isdigit():
        return 50
    return max(1, min(500, int(raw)))


def _number(value):
    return value if isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) else None


def _text(value):
    return value if isinstance(value, str) else None


def _steps(raw):
    try:
        steps = json.loads(raw) if isinstance(raw, str) else []
    except ValueError:
        return []
    if not isinstance(steps, list):
        return []
    return [{"name": step["name"], "ms": _number(step.get("ms")), "exitCode": _number(step.get("exitCode"))}
            for step in steps if isinstance(step, dict) and isinstance(step.get("name"), str)]


def _run(row):
    if (not isinstance(row, dict) or not isinstance(row.get("id"), str)
            or _number(row.get("started_at")) is None or not isinstance(row.get("status"), str)
            or row["status"] not in STATUSES):
        return None
    return {"id": row["id"], "repo": _text(row.get("repo")), "ref": _text(row.get("ref")),
            "cache_mode": _text(row.get("cache_mode")), "instance": _text(row.get("instance")),
            "started_at": row["started_at"], "ended_at": _number(row.get("ended_at")), "status": row["status"],
            "cache": _text(row.get("cache")), "max_cost_usd": _number(row.get("max_cost_usd")),
            "steps": _steps(row.get("steps"))}


class ServerlessRuns:
    def __init__(self, url, token, timeout=8):
        parsed = urlsplit(url.rstrip("/"))
        if parsed.scheme not in {"http", "https"} or not parsed.hostname or parsed.query or parsed.username:
            raise ValueError("NEBULA_SERVERLESS_URL must be an http(s) URL")
        if parsed.scheme != "https" and parsed.hostname not in {"localhost", "127.0.0.1"}:
            raise ValueError("NEBULA_SERVERLESS_URL requires HTTPS off loopback")
        self.parsed, self.token, self.timeout = parsed, token, timeout

    def get(self, limit):
        """Newest-first runs; one bounded request, no redirects or proxies."""
        kind = http.client.HTTPSConnection if self.parsed.scheme == "https" else http.client.HTTPConnection
        connection = kind(self.parsed.hostname, self.parsed.port, timeout=self.timeout)
        expired = threading.Event()
        timer = None
        try:
            connection.connect()
            transport = connection.sock

            def expire():
                expired.set()
                try:
                    transport.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

            # The socket timeout bounds each read; this bounds the whole exchange.
            timer = threading.Timer(self.timeout, expire)
            timer.daemon = True
            timer.start()
            connection.request("GET", f"{self.parsed.path}/runs?limit={_limit(limit)}", headers={
                "Authorization": "Bearer " + self.token, "Accept": "application/json",
                "User-Agent": "nebula-mobile-serverless/1.0"})
            response = connection.getresponse()
            if response.status in (401, 403):
                raise ServerlessError(502, "Worker-ul serverless a refuzat tokenul; verifică NEBULA_SERVERLESS_TOKEN pe server")
            if response.status != 200:
                raise ServerlessError(502, f"Worker-ul serverless a răspuns cu HTTP {response.status}")
            data = response.read(4194305)
            if expired.is_set():
                raise ServerlessError(504, "Worker-ul serverless nu a răspuns la timp")
            if len(data) > 4194304:
                raise ServerlessError(502, "Răspuns prea mare de la Worker-ul serverless")
            rows = json.loads(data)
            if not isinstance(rows, list):
                raise ValueError("expected a list")
        except ServerlessError:
            raise
        except TimeoutError:
            raise ServerlessError(504, "Worker-ul serverless nu a răspuns la timp") from None
        except ValueError:
            raise ServerlessError(502, "Răspuns invalid de la Worker-ul serverless") from None
        except (OSError, http.client.HTTPException):
            if expired.is_set():
                raise ServerlessError(504, "Worker-ul serverless nu a răspuns la timp") from None
            raise ServerlessError(502, "Worker-ul serverless nu poate fi contactat") from None
        finally:
            if timer is not None:
                timer.cancel()
            connection.close()
        return {"configured": True, "runs": [run for run in map(_run, rows) if run]}


def unconfigured():
    return {"configured": False, "runs": []}


def from_environment(environment=os.environ):
    url, token = environment.get("NEBULA_SERVERLESS_URL"), environment.get("NEBULA_SERVERLESS_TOKEN")
    return ServerlessRuns(url, token) if url and token else None
