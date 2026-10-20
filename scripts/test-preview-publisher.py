#!/usr/bin/env python3
"""Exercise the real privileged publisher in disposable mount/network namespaces.

No host /etc files or listeners change: /etc is a private tmpfs and loopback
belongs to a new network namespace. The HTTP/2 fixture models the external
Traefik boundary; the separate runtime suite exercises real Nginx auth.
"""
import http.server
import json
import os
from pathlib import Path
import socket
import shutil
import ssl
import subprocess
import sys
import threading
import time


def run(argv, **kwargs):
    return subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)


if len(sys.argv) != 3 or sys.argv[1] != "--inside":
    binary = str(Path(sys.argv[1]).resolve())
    result = subprocess.run(["sudo", "-n", "unshare", "--mount", "--net", "--fork", sys.executable,
                             str(Path(__file__).resolve()), "--inside", binary])
    sys.exit(result.returncode)

BINARY = sys.argv[2]
run(["mount", "--make-rprivate", "/"], check=True)
run(["mount", "-t", "tmpfs", "-o", "mode=755", "preview-test", "/etc"], check=True)
run(["ip", "link", "set", "lo", "up"], check=True)
for directory in ["/etc/nebula", "/etc/dokploy/traefik/dynamic", "/etc/ssl/certs"]:
    Path(directory).mkdir(parents=True, exist_ok=True)
# The privilege entry point refuses a user-writable executable even in tests.
shutil.copyfile(BINARY, "/etc/nebula/candidate")
os.chmod("/etc/nebula/candidate", 0o755)
BINARY = "/etc/nebula/candidate"
route_dir = Path("/etc/dokploy/traefik/dynamic")
route_file = route_dir / "nebula-preview-demo.yaml"
policy = {"namespace": "publisher-test", "state_dir": "/etc/nebula", "bind_address": "127.0.0.1",
          "gateway_image": "nginx:1.28-alpine", "projects": {"demo": {"git_common_dir": "/etc/nebula",
          "domain": "demo.example.test", "ports": [28081, 28082], "auth_file": "/etc/nebula/owner.htpasswd"}}}
for name, data in [("previews.json", json.dumps(policy)), ("owner.htpasswd", "owner:fixture-hash\n")]:
    path = Path("/etc/nebula") / name
    path.write_text(data)
    path.chmod(0o600)
run(["openssl", "req", "-config", "/dev/null", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "/etc/nebula/key.pem",
     "-out", "/etc/ssl/certs/ca-certificates.crt", "-subj", "/CN=demo.example.test",
     "-addext", "subjectAltName=DNS:demo.example.test", "-days", "1"], check=True)
Path("/etc/nebula/key.pem").chmod(0o600)
GENERATIONS = {28081: "a" * 26, 28082: "b" * 26}
MODE = "follow"
HTTP2_COUNT = 0


class Gate(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.headers.get("Host") != "demo.example.test":
            self.send_response(404)
        else:
            self.send_response(401)
            self.send_header("WWW-Authenticate", 'Basic realm="Private preview"')
            self.send_header("X-Nebula-Preview", GENERATIONS[self.server.server_port])
        self.end_headers()

    def log_message(self, *args):
        pass


for port in GENERATIONS:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Gate)
    threading.Thread(target=server.serve_forever, daemon=True).start()


def response_headers():
    if MODE == "unknown":
        return [(":status", "503")]
    if MODE == "hold-a":
        generation = GENERATIONS[28081]
    elif route_file.exists():
        config = json.loads(route_file.read_text())
        generation = next(iter(config["http"]["middlewares"].values()))["headers"]["customResponseHeaders"]["X-Nebula-Route"]
    else:
        return [(":status", "404")]
    return [(":status", "401"), ("www-authenticate", 'Basic realm="Private preview"'),
            ("x-nebula-preview", generation)]


def frame(kind, flags, stream, payload=b""):
    return len(payload).to_bytes(3, "big") + bytes([kind, flags]) + stream.to_bytes(4, "big") + payload


def receive(connection, count):
    data = b""
    while len(data) < count:
        chunk = connection.recv(count - len(data))
        if not chunk:
            raise EOFError()
        data += chunk
    return data


def https_connection(raw, context):
    global HTTP2_COUNT
    try:
        with context.wrap_socket(raw, server_side=True) as connection:
            connection.settimeout(5)
            assert connection.selected_alpn_protocol() == "h2", "fixture requires curl HTTP/2 support"
            assert receive(connection, 24) == b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n"
            connection.sendall(frame(4, 0, 0))
            while True:
                header = receive(connection, 9)
                payload = receive(connection, int.from_bytes(header[:3], "big"))
                if header[3] == 4 and not header[4] & 1:
                    connection.sendall(frame(4, 1, 0))
                if header[3] == 1:
                    stream = int.from_bytes(header[5:], "big")
                    break
            HTTP2_COUNT += 1
            # HPACK literals without indexing; all names/values are <127 bytes.
            block = b""
            for name, value in response_headers():
                name, value = name.encode(), value.encode()
                block += b"\x00" + bytes([len(name)]) + name + bytes([len(value)]) + value
            connection.sendall(frame(1, 5, stream, block))  # END_HEADERS | END_STREAM
            # Let curl acknowledge/close instead of resetting unread TLS records.
            try:
                while connection.recv(4096):
                    pass
            except (TimeoutError, OSError):
                pass
    except (EOFError, OSError):
        raw.close()


def https_server():
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain("/etc/ssl/certs/ca-certificates.crt", "/etc/nebula/key.pem")
    context.set_alpn_protocols(["h2"])
    server = socket.socket()
    server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    server.bind(("127.0.0.1", 443))
    server.listen()
    while True:
        raw, _ = server.accept()
        threading.Thread(target=https_connection, args=(raw, context), daemon=True).start()


threading.Thread(target=https_server, daemon=True).start()
time.sleep(0.2)


def publisher(operation, generation="", ok=True):
    result = run([BINARY, "preview", "publish", "demo", operation, generation])
    assert (result.returncode == 0) == ok, result.stderr.decode()
    return result


publisher("a", GENERATIONS[28081])
assert json.loads(publisher("status").stdout)["slot"] == "a"
assert HTTP2_COUNT > 0
print("PASS: TLS-verified HTTP/2 401 adoption through the real publisher", flush=True)

MODE = "hold-a"
print("Testing failed adoption and publisher concurrency (bounded 15s poll)...", flush=True)
pending = subprocess.Popen([BINARY, "preview", "publish", "demo", "b", GENERATIONS[28082]], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
time.sleep(1)
refused = publisher("status", ok=False)
assert b"in progress" in refused.stderr
assert pending.wait(timeout=35) != 0
assert json.loads(publisher("status").stdout)["slot"] == "a"
print("PASS: failed adoption restores and verifies old route; concurrent publisher refused", flush=True)

MODE = "unknown"
print("Testing uncertain adoption/rollback (two bounded 15s polls)...", flush=True)
failed = publisher("b", GENERATIONS[28082], ok=False)
assert b"uncertain" in failed.stderr
MODE = "follow"
assert json.loads(publisher("status").stdout)["slot"] == "a"
print("PASS: uncertain rollback stays pending until live verification recovers", flush=True)

MODE = "hold-a"
pending = subprocess.Popen([BINARY, "preview", "publish", "demo", "b", GENERATIONS[28082]], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
time.sleep(1)
pending.kill()
pending.wait()
# A file write alone is insufficient: status must not report the unadopted B.
print("Testing publisher crash before adoption (bounded 15s poll)...", flush=True)
publisher("status", ok=False)
MODE = "follow"
assert json.loads(publisher("status").stdout)["slot"] == "b"
publisher("down")
assert json.loads(publisher("status").stdout) is None
print("PASS: crash recovery verifies adoption; scoped withdrawal is verified", flush=True)

# A first publication may roll back to no route, and withdrawal may occur while
# Traefik still returns a 5xx. Neither response acknowledges a withdrawn route.
MODE = "unknown"
print("Testing uncertain first-publication rollback to None...", flush=True)
failed = publisher("a", GENERATIONS[28081], ok=False)
assert b"uncertain" in failed.stderr
publisher("down", ok=False)
MODE = "follow"
publisher("down")
assert json.loads(publisher("status").stdout) is None
print("PASS: 5xx cannot acknowledge rollback-to-None or down; later 404 completes withdrawal", flush=True)
