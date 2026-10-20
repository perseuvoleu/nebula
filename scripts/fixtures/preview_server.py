"""Test-only preview backend: HTTP observations and a one-message WS echo."""
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import time

time.sleep(float(os.environ.get("START_DELAY", "0")))
data = Path("/app/data/value")
if not data.exists():
    data.write_text(os.environ["TEST_VALUE"])
print("preview fixture started", flush=True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_GET(self):
        if os.environ.get("HEALTH_FAIL"):
            self.send_response(503)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if os.environ.get("HEALTH_REDIRECT"):
            self.send_response(302)
            self.send_header("Location", "http://127.0.0.1:8080/")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if self.headers.get("Upgrade", "").lower() == "websocket":
            key = self.headers["Sec-WebSocket-Key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
            self.send_response(101)
            self.send_header("Upgrade", "websocket")
            self.send_header("Connection", "Upgrade")
            self.send_header("Sec-WebSocket-Accept", base64.b64encode(hashlib.sha1(key.encode()).digest()).decode())
            self.end_headers()
            frame = self.rfile.read(2)
            if len(frame) != 2:
                return
            length = frame[1] & 127
            mask = self.rfile.read(4)
            payload = self.rfile.read(length)
            payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
            self.wfile.write(bytes([0x81, len(payload)]) + payload)
            self.wfile.flush()
            self.close_connection = True
            return
        body = json.dumps({"value": os.environ["TEST_VALUE"], "data": data.read_text(),
            "authorization": self.headers.get("Authorization"), "host": self.headers.get("Host"),
            "proto": self.headers.get("X-Forwarded-Proto"), "forwarded": self.headers.get("Forwarded"),
            "env_present": Path("/app/.env").exists(), "recipe_present": Path("/app/.nebula-preview.json").exists(),
            "build_artifact": Path("/app/built-marker").read_text() if Path("/app/built-marker").exists() else None,
            "leak": Path("/app/raced.txt").read_text() if Path("/app/raced.txt").is_file() else None}).encode()
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


ThreadingHTTPServer(("0.0.0.0", 8080), Handler).serve_forever()
