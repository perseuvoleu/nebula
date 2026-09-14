"""Push public API regressions; synthetic subscriptions never leave this process."""
import tempfile
import unittest
from pathlib import Path

from server import PushService, Problem

SUBSCRIPTION = {"endpoint": "https://web.push.apple.com/synthetic", "keys": {"p256dh": "A" * 40, "auth": "b" * 8}}


class PushTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.push = PushService(None, "https://example.com", Path(self.tmp.name) / "private" / "push.json",
                                sender=lambda subscription, data: None, public_key="B" * 87)

    def test_browser_json_subscription_survives_reload(self):
        self.push.subscribe({**SUBSCRIPTION, "expirationTime": None})
        restored = PushService(None, "https://example.com", self.push.storage,
                               sender=lambda subscription, data: None, public_key="B" * 87)
        self.assertEqual(restored.subscriptions(), [SUBSCRIPTION])

    def test_subscription_rejects_destinations_outside_supported_push_services(self):
        for endpoint in ["https://127.0.0.1/x", "https://[::1]/x", "https://internal.example/x",
                         "https://web.push.apple.com.evil.test/x", "https://web.push.apple.com:8443/x",
                         "https://web.push.apple.com:bad/x", "https://web.push.apple.com\n/x",
                         "https://user@web.push.apple.com/x"]:
            with self.subTest(endpoint=endpoint), self.assertRaises(Problem):
                self.push.subscribe({**SUBSCRIPTION, "endpoint": endpoint})

    def test_subscription_storage_is_private_even_with_permissive_umask(self):
        import os
        old = os.umask(0)
        try:
            self.push.subscribe(SUBSCRIPTION)
        finally:
            os.umask(old)
        self.assertEqual((self.push.storage.parent.stat().st_mode & 0o777,
                          self.push.storage.stat().st_mode & 0o777), (0o700, 0o600))

    def test_delivery_does_not_follow_redirects_or_use_environment_credentials(self):
        import base64
        import os
        from unittest.mock import patch
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ec
        from requests import Response
        key = ec.generate_private_key(ec.SECP256R1())
        private = Path(self.tmp.name) / "key.pem"
        private.write_bytes(key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                             serialization.NoEncryption()))
        raw = key.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
        subscription = {**SUBSCRIPTION, "keys": {"p256dh": base64.urlsafe_b64encode(raw).decode().rstrip("="),
                                               "auth": base64.urlsafe_b64encode(b"a" * 16).decode().rstrip("=")}}
        netrc = Path(self.tmp.name) / "netrc"
        netrc.write_text("machine web.push.apple.com login private-user password private-password\n")
        requests = []
        def respond(adapter, request, **kwargs):
            requests.append((request, kwargs))
            response = Response()
            response.status_code = 307 if len(requests) == 1 else 201
            response.headers["Location"] = "https://127.0.0.1/private"
            response._content = b""
            response.request = request
            return response
        with patch.dict(os.environ, {"NETRC": str(netrc), "HTTPS_PROXY": "http://127.0.0.1:1"}), \
                patch("requests.adapters.HTTPAdapter.send", new=respond):
            push = PushService(str(private), "https://example.com", self.push.storage)
            push.subscribe(subscription)
            push.notify("project", {"id": "one", "name": "Agent", "status": "finished"})
        self.assertEqual([(r.url, r.headers.get("Authorization", "missing").split()[0], kw["proxies"]) for r, kw in requests],
                         [(SUBSCRIPTION["endpoint"], "vapid", {})])

