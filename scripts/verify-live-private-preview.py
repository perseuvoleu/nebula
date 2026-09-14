#!/usr/bin/env python3
"""Verify the deployed nebula.possu.dev synthetic fixture without logging credentials.

Run with sudo on findl. Reads only the dedicated preview credential into memory;
no CLI secret arguments, curl config, proxies, redirects or TLS bypass. This is
server verification, not a substitute for desktop/physical-mobile browser tests.
"""
import base64
import http.client
import json
import os
from pathlib import Path
import socket
import ssl
import stat

DOMAIN = "nebula.possu.dev"
BRIDGE = "172.18.0.1"
PORT = 28171  # This deployment verifier targets the initial slot A fixture.


def request(path, headers, direct=False):
    """Return status, headers and bounded body through the public HTTP boundary."""
    connection = (http.client.HTTPConnection(BRIDGE, PORT, timeout=10) if direct
                  else http.client.HTTPSConnection(DOMAIN, timeout=10, context=ssl.create_default_context()))
    try:
        connection.request("GET", path, headers={"Host": DOMAIN, **headers})
        response = connection.getresponse()
        return response.status, dict(response.getheaders()), response.read(65536)
    finally:
        connection.close()


for direct in (False, True):
    for path in ("/", "/api/status", "/ws"):
        headers = {} if path != "/ws" else {"Connection": "Upgrade", "Upgrade": "websocket",
                  "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ=="}
        status, response_headers, _ = request(path, headers, direct)
        assert status == 401, f"anonymous denial failed: direct={direct} path={path} status={status}"
        assert any(k.lower() == "www-authenticate" and v.startswith("Basic ")
                   for k, v in response_headers.items()), "missing Basic challenge"
        status, _, _ = request(path, {**headers, "Authorization": "Basic aW52YWxpZDppbnZhbGlk"}, direct)
        assert status == 401, f"invalid credential accepted: direct={direct} path={path}"
print("PASS verified TLS and anonymous/invalid-credential HTML/API/WebSocket denial; direct gate also protected")

path = Path("/etc/nebula/preview-owner.initial-password")
fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
with os.fdopen(fd) as credential:
    metadata = os.fstat(credential.fileno())
    assert stat.S_ISREG(metadata.st_mode) and metadata.st_uid == 0 and stat.S_IMODE(metadata.st_mode) == 0o600
    password = credential.read(4096).strip()
assert len(password) >= 32
owner = {"Authorization": "Basic " + base64.b64encode(("owner:" + password).encode()).decode()}
del password
status, _, body = request("/", owner)
assert status == 200 and b"Nebula private preview fixture" in body
for direct in (False, True):
    status, _, body = request("/api/status", owner, direct)
    observation = json.loads(body)
    assert status == 200 and observation["authorization"] is None
    assert observation["host"] == DOMAIN and observation["proto"] == "https"
    assert not observation["env_present"] and not observation["recipe_present"]
    assert observation["value"] == "Nebula synthetic private preview — test data only"
    assert request("/", {**owner, "Origin": "https://evil.example"}, direct)[0] == 403
    assert request("/", {**owner, "Host": "code.possu.dev"}, True)[0] == 404
print("PASS owner HTML/API, stripped Authorization, preserved Host/scheme, sanitized source and Host/Origin denial")

with socket.create_connection((DOMAIN, 443), timeout=10) as tcp:
    with ssl.create_default_context().wrap_socket(tcp, server_hostname=DOMAIN) as connection:
        headers = {"Host": DOMAIN, **owner, "Origin": "https://" + DOMAIN,
                   "Connection": "Upgrade", "Upgrade": "websocket", "Sec-WebSocket-Version": "13",
                   "Sec-WebSocket-Key": "dGhlIHNhbXBsZSBub25jZQ=="}
        message = "GET /ws HTTP/1.1\r\n" + "".join(k + ": " + v + "\r\n" for k, v in headers.items()) + "\r\n"
        connection.sendall(message.encode())
        response = b""
        while not response.endswith(b"\r\n\r\n"):
            chunk = connection.recv(1)
            assert chunk and len(response) < 16384, "invalid WebSocket handshake"
            response += chunk
        assert response.split(b"\r\n", 1)[0].split()[1] == b"101"
        assert b"s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" in response
        payload = b"server-preview-check"
        mask = os.urandom(4)
        connection.sendall(bytes([0x81, 0x80 | len(payload)]) + mask +
                           bytes(value ^ mask[i % 4] for i, value in enumerate(payload)))
        expected = bytes([0x81, len(payload)]) + payload
        echoed = b""
        while len(echoed) < len(expected):
            chunk = connection.recv(len(expected) - len(echoed))
            assert chunk, "WebSocket closed before echo"
            echoed += chunk
        assert echoed == expected
print("PASS authenticated same-origin WebSocket upgrade and echo over verified TLS")
