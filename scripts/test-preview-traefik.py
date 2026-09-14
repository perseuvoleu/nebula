#!/usr/bin/env python3
"""Test the publisher with real Traefik in private mount/network namespaces.

Arguments: candidate binary, extracted Traefik v3.6.7 binary. No Docker socket,
production configuration, ACME server or host listener is used inside the fixture.
"""
import http.server
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import threading
import time


def run(argv, **kwargs):
    return subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)


if sys.argv[1] != "--inside":
    result = subprocess.run(["sudo", "-n", "unshare", "--mount", "--net", "--fork",
                             sys.executable, str(Path(__file__).resolve()), "--inside",
                             str(Path(sys.argv[1]).resolve()), str(Path(sys.argv[2]).resolve())])
    sys.exit(result.returncode)

run(["mount", "--make-rprivate", "/"], check=True)
run(["mount", "-t", "tmpfs", "-o", "mode=755", "preview-traefik-test", "/etc"], check=True)
run(["ip", "link", "set", "lo", "up"], check=True)
root = Path("/etc/nebula")
root.mkdir()
routes = Path("/etc/dokploy/traefik/dynamic")
routes.mkdir(parents=True)
Path("/etc/ssl/certs").mkdir(parents=True)
for source, name in [(sys.argv[2], "candidate"), (sys.argv[3], "traefik")]:
    shutil.copyfile(source, root / name)
    (root / name).chmod(0o755)
policy = {"namespace": "real-traefik", "state_dir": str(root), "bind_address": "127.0.0.1",
          "gateway_image": "nginx:1.28-alpine", "projects": {"demo": {"git_common_dir": str(root),
          "domain": "demo.example.test", "ports": [28081, 28082], "auth_file": str(root / "owner.htpasswd")}}}
for name, value in [("previews.json", json.dumps(policy)), ("owner.htpasswd", "owner:fixture-only\n")]:
    (root / name).write_text(value)
    (root / name).chmod(0o600)
run(["openssl", "req", "-config", "/dev/null", "-x509", "-newkey", "rsa:2048", "-nodes",
     "-keyout", str(root / "key.pem"), "-out", "/etc/ssl/certs/ca-certificates.crt",
     "-subj", "/CN=demo.example.test", "-addext", "subjectAltName=DNS:demo.example.test", "-days", "1"], check=True)
(root / "key.pem").chmod(0o600)
certificate = {"tls": {"certificates": [{"certFile": "/etc/ssl/certs/ca-certificates.crt", "keyFile": str(root / "key.pem")}]}}
(routes / "test-certificate.yaml").write_text(json.dumps(certificate))


class Gate(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(401 if self.headers.get("Host") == "demo.example.test" else 404)
        self.send_header("WWW-Authenticate", 'Basic realm="Private preview"')
        self.send_header("X-Nebula-Preview", ("a" if self.server.server_port == 28081 else "b") * 26)
        self.end_headers()

    def log_message(self, *args):
        pass


for port in (28081, 28082):
    server = http.server.ThreadingHTTPServer(("127.0.0.1", port), Gate)
    threading.Thread(target=server.serve_forever, daemon=True).start()

static = {"entryPoints": {"websecure": {"address": "127.0.0.1:443"}},
          "providers": {"file": {"directory": str(routes), "watch": True}},
          "certificatesResolvers": {"letsencrypt": {"acme": {"email": "fixture@example.test",
              "storage": str(root / "acme.json"), "caServer": "http://127.0.0.1:9/directory",
              "httpChallenge": {"entryPoint": "websecure"}}}}}
(root / "traefik.yaml").write_text(json.dumps(static))
with (root / "traefik.log").open("w") as log:
    traefik = subprocess.Popen([str(root / "traefik"), "--configFile=" + str(root / "traefik.yaml")], stdout=log, stderr=log)
    pending = None
    try:
        time.sleep(1)
        legacy = routes / "nebula-preview-demo.json"
        legacy.write_text("{}")
        legacy.chmod(0o600)
        refused = run([str(root / "candidate"), "preview", "publish", "demo", "a", "a" * 26])
        assert refused.returncode != 0 and b"legacy" in refused.stderr and legacy.read_text() == "{}"
        legacy.unlink()
        for slot in ("a", "b"):
            pending = subprocess.Popen([str(root / "candidate"), "preview", "publish", "demo", slot, slot * 26], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            # A missing/ignored route fails quickly rather than waiting for ACME's
            # bounded initial-adoption deadline. No public CA exists in this fixture.
            try:
                output, error = pending.communicate(timeout=12)
            except subprocess.TimeoutExpired:
                raise AssertionError("real Traefik did not adopt the publisher's route within 12 seconds") from None
            assert pending.returncode == 0, error.decode()
            status = run([str(root / "candidate"), "preview", "publish", "demo", "status", ""], check=True)
            assert json.loads(status.stdout)["slot"] == slot
        result = run([str(root / "candidate"), "preview", "publish", "demo", "down", ""], check=True)
        status = run([str(root / "candidate"), "preview", "publish", "demo", "status", ""], check=True)
        assert json.loads(status.stdout) is None
        print("PASS: actual Traefik v3.6.7 loads publisher route, atomically adopts A/B and verifies withdrawal over TLS")
    finally:
        if pending and pending.poll() is None:
            pending.kill()
            pending.wait()
        traefik.terminate()
        traefik.wait(timeout=10)
