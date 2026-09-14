#!/usr/bin/env python3
"""Disposable Linux acceptance tests; pass a side-by-side nebula candidate path.

No live ingress is written. Test source, credentials and logs stay under .scratch.
Docker resources use a unique namespace and are removed by exact fixture names.
"""
import base64
import http.client
import json
import os
from pathlib import Path
import secrets
import socket
import time
import subprocess
import sys
import tempfile

BINARY = str(Path(sys.argv[1]).resolve())
SCRATCH = Path(".scratch/private-preview-implementation").resolve()
SCRATCH.mkdir(exist_ok=True)


def command(argv, **kwargs):
    return subprocess.run(argv, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, **kwargs)


def private(path, value):
    path.write_text(value)
    path.chmod(0o600)


with tempfile.TemporaryDirectory(prefix="acceptance-", dir=SCRATCH) as temporary:
    root = Path(temporary)
    checkout = root / "checkout"
    checkout.mkdir()
    command(["git", "init", "-q", str(checkout)])
    command(["git", "-C", str(checkout), "-c", "user.name=Preview Test", "-c", "user.email=preview@example.test", "commit", "--allow-empty", "-qm", "fixture"])
    namespace = "test" + secrets.token_hex(4)
    auth = root / "owner.htpasswd"
    password = secrets.token_urlsafe(32)
    hashed = command(["openssl", "passwd", "-apr1", "-stdin"], input=password.encode()).stdout.decode().strip()
    private(auth, "owner:" + hashed + "\n")
    command(["sudo", "-n", "chown", "root:" + str(os.getgid()), str(auth)])
    command(["sudo", "-n", "chmod", "0640", str(auth)])
    policy = root / "policy.json"
    private(policy, json.dumps({
        "namespace": namespace, "state_dir": str(root / "state"),
        "bind_address": "127.0.0.1", "gateway_image": "nginx:1.28-alpine",
        "projects": {"demo": {"git_common_dir": str(checkout / ".git"),
            "domain": "demo.example.test", "ports": [28171, 28172], "auth_file": str(auth)}}
    }))
    private(checkout / ".nebula-preview.json", json.dumps({
        "services": {"web": {"image": "python:3.13-alpine", "dockerfile": None,
            "command": ["python", "/app/server.py"], "environment": {"TEST_VALUE": "one"},
            "port": 8080, "health": {"kind": "http", "path": "/"}, "user": "1000:1000", "data": {"data": "/app/data"}}},
        "routes": {"/": "web"}, "health_timeout_seconds": 15
    }))
    private(checkout / "server.py", Path("scripts/fixtures/preview_server.py").read_text())
    # Deliberately seeded dotenv must never reach the runtime.
    private(checkout / ".env", "DO_NOT_SERVE=fixture-only\n")

    def cli(*args, ok=True):
        result = subprocess.run([BINARY, "preview", "--offline", "--policy", str(policy), "--project", "demo", *args],
                                cwd=checkout, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if ok and result.returncode:
            raise AssertionError(result.stderr.decode())
        if not ok and not result.returncode:
            raise AssertionError("operation unexpectedly succeeded: " + args[0])
        return result

    docker = ["sudo", "-n", "/usr/bin/docker"]
    prefix = "nebula-preview-offline_" + namespace + "_demo"
    authorization = "Basic " + base64.b64encode(("owner:" + password).encode()).decode()

    def request(path="/", auth=True, port=28171, headers=None):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=4)
        fields = {"Host": "demo.example.test"}
        if auth:
            fields["Authorization"] = authorization
        fields.update(headers or {})
        connection.request("GET", path, headers=fields)
        response = connection.getresponse()
        body = response.read()
        status_code = response.status
        connection.close()
        return status_code, body

    def recipe_env(**environment):
        path = checkout / ".nebula-preview.json"
        recipe = json.loads(path.read_text())
        recipe["services"]["web"]["environment"] = environment
        private(path, json.dumps(recipe))

    def websocket():
        connection = socket.create_connection(("127.0.0.1", 28171), timeout=4)
        connection.sendall(("GET /socket HTTP/1.1\r\nHost: demo.example.test\r\nAuthorization: " + authorization +
            "\r\nOrigin: https://demo.example.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n" +
            "Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n").encode())
        response = b""
        while b"\r\n\r\n" not in response:
            response += connection.recv(4096)
        assert response.split()[1] == b"101", response
        payload, mask = b"hmr", b"abcd"
        connection.sendall(bytes([0x81, 0x80 | len(payload)]) + mask + bytes(value ^ mask[index % 4] for index, value in enumerate(payload)))
        assert connection.recv(5) == b"\x81\x03hmr"
        connection.close()

    canary = namespace + "-unrelated-fixture"
    try:
        command(docker + ["run", "-d", "--label=traefik.enable=false", "--name", canary, "--network", "none", "--cap-drop=ALL", "--user", "1000:1000", "python:3.13-alpine", "python", "-c", "import time; time.sleep(600)"])
        cli("up", "--owner", "agent-one")
        status = json.loads(cli("status").stdout)
        assert status["phase"] == "running", status
        print("PASS: CLI exit leaves a supervised running preview", flush=True)
        for path in ["/", "/api/example", "/socket"]:
            assert request(path, auth=False)[0] == 401
            assert request(path, headers={"Authorization": "Basic invalid"})[0] == 401
        upgrade = {"Connection": "Upgrade", "Upgrade": "websocket", "Origin": "https://demo.example.test"}
        assert request("/socket", auth=False, headers=upgrade)[0] == 401
        assert request(headers={"Host": "code.possu.dev"})[0] == 404
        assert request(headers={"Origin": "https://untrusted.example"})[0] == 403
        code, body = request("/api/example", headers={"X-Forwarded-Proto": "http", "Forwarded": "host=untrusted"})
        observed = json.loads(body)
        assert code == 200 and observed["authorization"] is None
        assert observed["host"] == "demo.example.test" and observed["proto"] == "https"
        assert observed["forwarded"] is None and not observed["env_present"] and not observed["recipe_present"]
        websocket()
        assert b"preview fixture started" in cli("logs", "--service", "web", "--lines", "10").stdout
        print("PASS: direct HTML/API/WS auth, bad credentials, Host/Origin rejection, stripped auth, WS echo, sanitized source", flush=True)

        # Docker is an external process/network boundary. Read only this fixture's
        # assigned backend IP, then verify an outsider cannot reach its app port.
        inspected = json.loads(command(docker + ["inspect", prefix + "_a-web"]).stdout)[0]
        backend_ip = next(iter(inspected["NetworkSettings"]["Networks"].values()))["IPAddress"]
        outsider = subprocess.run(docker + ["run", "--rm", "--label=traefik.enable=false", "--network", "bridge", "--cap-drop=ALL", "--entrypoint", "/usr/bin/curl", "nginx:1.28-alpine", "-q", "--silent", "--noproxy", "*", "--max-time", "2", "http://" + backend_ip + ":8080/"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert outsider.returncode != 0, "outsider reached an unpublished backend"
        assert inspected["NetworkSettings"]["Ports"] == {}, "application port was published"
        print("PASS: outsider-container direct backend access denied; no app ports published", flush=True)

        other = root / "other-checkout"
        command(["git", "-C", str(checkout), "worktree", "add", "-qb", "fixture-other", str(other)])
        for name in ["server.py", ".nebula-preview.json"]:
            private(other / name, (checkout / name).read_text())
        cli("up", "--checkout", str(other), "--owner", "agent-one", ok=False)
        cli("up", "--owner", "agent-two", ok=False)
        cli("down", "--owner", "agent-two", ok=False)
        assert request()[0] == 200
        recipe_env(TEST_VALUE="bad", HEALTH_FAIL="1")
        cli("restart", "--owner", "agent-one", ok=False)
        assert json.loads(request()[1])["value"] == "one"
        assert json.loads(request()[1])["data"] == "one"
        print("PASS: ownership refusal and failed candidate preserve old preview and data", flush=True)

        recipe_env(TEST_VALUE="two")
        (checkout / "raced.txt").symlink_to(auth)
        cli("restart", "--owner", "agent-one", ok=False)
        (checkout / "raced.txt").unlink()
        assert json.loads(request()[1])["value"] == "one"
        print("PASS: source symlink is rejected without switching the active preview", flush=True)

        recipe_env(TEST_VALUE="two", START_DELAY="5")
        pending = subprocess.Popen([BINARY, "preview", "--offline", "--policy", str(policy), "--project", "demo", "restart", "--owner", "agent-one"], cwd=checkout, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        time.sleep(1)
        refused = cli("status", ok=False)
        assert b"in progress" in refused.stderr
        pending.kill()
        pending.wait()
        # A bounded in-flight Docker child retains the operation lock after its
        # parent dies. Wait for that child, without touching its processes.
        for _ in range(30):
            observation = subprocess.run([BINARY, "preview", "--offline", "--policy", str(policy), "--project", "demo", "status"], cwd=checkout, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            if b"in progress" not in observation.stderr:
                break
            time.sleep(0.2)
        cli("down", "--owner", "agent-two", ok=False)
        assert request()[0] == 200
        recipe_env(TEST_VALUE="two")
        cli("restart", "--owner", "agent-two", "--replace")
        assert json.loads(request(port=28172)[1])["value"] == "two"
        assert json.loads(cli("status").stdout)["active"]["owner"] == "agent-two"
        print("PASS: concurrent operation refused; interrupted startup recovered by explicit takeover", flush=True)

        # Simulate loss of a managed process through the external runtime API.
        command(docker + ["stop", prefix + "_b-web"])
        assert json.loads(cli("status").stdout)["phase"] == "stale"
        recipe_env(TEST_VALUE="three")
        cli("restart", "--owner", "agent-two")
        assert json.loads(request()[1])["value"] == "three"
        assert json.loads(request()[1])["data"] == "one"
        cli("down", "--owner", "agent-two")
        assert json.loads(cli("status").stdout)["phase"] == "stopped"
        cli("up", "--owner", "agent-one")
        assert json.loads(request()[1])["data"] == "one"
        print("PASS: stale runtime recovery and preserved test volumes", flush=True)
        # Fault the documented offline publisher artifact (filesystem boundary),
        # not the runtime journal: publication fails after a healthy B starts.
        artifact = root / "state" / "offline" / namespace / "demo" / "published.json"
        artifact.unlink()
        artifact.mkdir(mode=0o700)
        cli("restart", "--owner", "agent-one", ok=False)
        artifact.rmdir()
        private(artifact, "unreadable publisher response")
        uncertain = json.loads(cli("status").stdout)
        assert uncertain["phase"] == "recovering" and uncertain["candidate"] is not None
        assert uncertain["observation_error"]
        # Authorized withdrawal must not first require adoption/status of the
        # broken pending candidate. It can safely replace this artifact with None.
        cli("down", "--owner", "agent-one")
        assert json.loads(cli("status").stdout)["phase"] == "stopped"
        print("PASS: authorized down withdraws a broken pending publication without requiring adoption", flush=True)

        # These valid identifiers formerly produced an identical Docker volume.
        recipe = json.loads((checkout / ".nebula-preview.json").read_text())
        backend = recipe["services"]["web"]
        backend["environment"] = {"TEST_VALUE": "backend"}
        backend["data"] = {"cache-data": "/app/data"}
        backend["health"] = {"kind": "command", "argv": ["python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/')"]}
        frontend = json.loads(json.dumps(backend))
        frontend["environment"] = {"TEST_VALUE": "frontend"}
        frontend["data"] = {"data": "/app/data"}
        frontend["health"] = {"kind": "http", "path": "/"}
        frontend["dockerfile"] = "Dockerfile.preview"
        private(checkout / "Dockerfile.preview", "FROM python:3.13-alpine\nCOPY . /app\nRUN test ! -e /app/.env && echo fixture-built > /app/built-marker\n")
        recipe["services"] = {"api": backend, "api-cache": frontend}
        recipe["routes"] = {"/": "api-cache", "/api/": "api"}
        private(checkout / ".nebula-preview.json", json.dumps(recipe))
        cli("up", "--owner", "agent-one")
        assert json.loads(request()[1])["data"] == "frontend"
        assert json.loads(request()[1])["build_artifact"] == "fixture-built\n"
        assert json.loads(request("/api/example")[1])["data"] == "backend"
        cli("down", "--owner", "agent-one")
        assert json.loads(command(docker + ["inspect", canary]).stdout)[0]["State"]["Running"]
        print("PASS: isolated Dockerfile build, frontend/API routes, command readiness, distinct service data and unrelated process survival", flush=True)
    except Exception:
        for service in ["web", "gate"]:
            logs = subprocess.run([BINARY, "preview", "--offline", "--policy", str(policy), "--project", "demo", "logs", "--candidate", "--service", service], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            print(logs.stdout.decode(), file=sys.stderr)
        raise
    finally:
        subprocess.run(docker + ["rm", "--force", canary], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        # Remove only exact resources belonging to this unique fixture, even if
        # the process died before its runtime journal was written.
        # Lifecycle command first; no wildcards, no production service operations.
        subprocess.run([BINARY, "preview", "--offline", "--policy", str(policy), "--project", "demo", "down", "--owner", "agent-one", "--replace"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)

        for slot in ["a", "b"]:
            for service in ["gate", "web", "api", "api-cache"]:
                subprocess.run(docker + ["rm", "--force", prefix + "_" + slot + "-" + service], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            for suffix in ["back", "edge"]:
                subprocess.run(docker + ["network", "rm", prefix + "_" + slot + "-" + suffix], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            subprocess.run(docker + ["volume", "rm", prefix + "_" + slot + "_web_data"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)

            for service, data_name in [("api", "cache-data"), ("api-cache", "data")]:
                subprocess.run(docker + ["volume", "rm", prefix + "_" + slot + "_" + service + "_" + data_name], stdout=subprocess.PIPE, stderr=subprocess.PIPE)

            images = command(docker + ["image", "ls", "--filter", "label=dev.nebula.preview=" + prefix + "_" + slot, "--format", "{{.ID}}"])
            for image_id in set(images.stdout.decode().split()):
                subprocess.run(docker + ["image", "rm", image_id], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
