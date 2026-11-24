"""Account usage behavior at the normalization, provider and cache seams."""
import unittest

import usage


class WeeklyTests(unittest.TestCase):
    def test_codex_selects_week_by_duration_even_when_primary(self):
        result = usage.codex_weekly({'rateLimits': {'primary': {
            'usedPercent': 31, 'windowDurationMins': 10080, 'resetsAt': 2000000000},
            'secondary': {'usedPercent': 90, 'windowDurationMins': 300}}})
        self.assertEqual(result, {'remainingPercent': 69, 'resetAt': 2000000000})

    def test_codex_selects_named_account_bucket_and_rejects_ambiguous_models(self):
        week = {'primary': {'usedPercent': 12, 'windowDurationMins': 10080, 'resetsAt': 2000000000}}
        self.assertEqual(usage.codex_weekly({'rateLimitsByLimitId': {'codex': week, 'model-x': {}}})['remainingPercent'], 88)
        with self.assertRaises(usage.UsageError) as error:
            usage.codex_weekly({'rateLimitsByLimitId': {'model-a': week, 'model-b': week}})
        self.assertEqual(error.exception.reason, 'ambiguous_weekly')

    def test_claude_week_is_used_percentage_with_iso_reset(self):
        self.assertEqual(usage.claude_weekly({'seven_day': {'utilization': 41.2, 'resets_at': '2033-05-18T03:33:20+00:00'}}),
                         {'remainingPercent': 58.8, 'resetAt': 2000000000})

    def test_invalid_and_missing_windows_never_become_full_quota(self):
        for used in [None, True, '10', float('nan'), float('inf')]:
            with self.subTest(used=used), self.assertRaises(usage.UsageError):
                usage.codex_weekly({'rateLimits': {'secondary': {'usedPercent': used, 'windowDurationMins': 10080, 'resetsAt': 2000000000}}})
        for data in [{}, {'seven_day': None}, {'seven_day': {'utilization': 10, 'resets_at': 'bad'}}]:
            with self.subTest(data=data), self.assertRaises(usage.UsageError):
                usage.claude_weekly(data)
        self.assertEqual(usage.claude_weekly({'seven_day': {'utilization': 101, 'resets_at': '2033-05-18T03:33:20Z'}})['remainingPercent'], 0)


class ProviderTests(unittest.TestCase):
    def test_codex_real_stdio_handshake_reads_account_without_starting_a_turn(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as root:
            cli = Path(root) / 'codex'
            cli.write_text('''#!/usr/bin/env python3
import json, sys
for method, result in [('initialize', {}), ('initialized', None), ('account/read', {'account': {'type': 'chatgpt'}}), ('account/rateLimits/read', {'rateLimits': {'primary': {'usedPercent': 23, 'windowDurationMins': 10080, 'resetsAt': 2000000000}}})]:
    request = json.loads(sys.stdin.readline())
    assert request['method'] == method
    if method == 'account/read': assert request['params']['refreshToken'] is False
    if result is not None: print(json.dumps({'id': request['id'], 'result': result}), flush=True)
''')
            cli.chmod(0o700)
            self.assertEqual(usage.CodexSource(str(cli), {}, root, 2).read()['remainingPercent'], 77)

    def test_claude_reads_existing_oauth_without_refresh_and_backs_off_on_429(self):
        import json
        import tempfile
        from pathlib import Path
        from unittest.mock import patch, MagicMock
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'credentials.json'
            path.write_text(json.dumps({'claudeAiOauth': {'accessToken': 'synthetic-secret', 'expiresAt': 2000000000000}}))
            connection = MagicMock()
            response = connection.getresponse.return_value
            response.status = 429
            response.getheader.return_value = '1200'
            with patch('http.client.HTTPSConnection', return_value=connection), self.assertRaises(usage.UsageError) as error:
                usage.ClaudeSource(path, lambda: 1900000000, 10).read()
            self.assertEqual((error.exception.reason, error.exception.retry_after), ('rate_limited', 1200))
            self.assertEqual(json.loads(path.read_text())['claudeAiOauth']['accessToken'], 'synthetic-secret')
            request = connection.request.call_args
            self.assertEqual(request.args, ('GET', '/api/oauth/usage'))
            self.assertEqual(request.kwargs['headers']['anthropic-beta'], 'oauth-2025-04-20')


class CacheTests(unittest.TestCase):
    def test_global_cache_keeps_last_good_on_failure_and_never_resets_to_full(self):
        from types import SimpleNamespace
        now = [1900000000]
        outcomes = iter([{'remainingPercent': 24, 'resetAt': 1900000350}, usage.UsageError('rate_limited', 1200)])
        def read():
            value = next(outcomes)
            if isinstance(value, Exception): raise value
            return value
        service = usage.UsageService({'codex': SimpleNamespace(read=read)}, lambda: now[0], lambda: now[0])
        self.assertEqual(service.get()['codex']['remainingPercent'], 24)
        now[0] += 300
        stale = service.get()['codex']
        self.assertEqual((stale['remainingPercent'], stale['updatedAt'], stale['status'], stale['reason']),
                         (24, 1900000000, 'stale', 'rate_limited'))
        now[0] += 100
        expired = service.get()['codex']
        self.assertEqual((expired['remainingPercent'], expired['status'], expired['resetAt']), (24, 'stale', 1900000350))

    def test_concurrent_callers_share_one_read_and_provider_failures_are_independent(self):
        from concurrent.futures import ThreadPoolExecutor
        from types import SimpleNamespace
        import threading
        entered, release = threading.Event(), threading.Event()
        reads = []
        def read():
            reads.append(1)
            entered.set()
            if not release.wait(2): raise TimeoutError()
            return {'remainingPercent': 17, 'resetAt': 2000000000}
        def failed(): raise usage.UsageError('account_unavailable')
        service = usage.UsageService({'codex': SimpleNamespace(read=read), 'claude': SimpleNamespace(read=failed)},
                                     lambda: 1900000000, lambda: 1)
        with ThreadPoolExecutor(max_workers=2) as pool:
            pending = pool.submit(service.get)
            self.assertTrue(entered.wait(2))
            try:
                self.assertEqual(service.get()['codex']['status'], 'loading')
            finally:
                release.set()
            result = pending.result(2)
        self.assertEqual((len(reads), result['codex']['remainingPercent'], result['claude']['status']), (1, 17, 'unavailable'))


class FailureTests(unittest.TestCase):
    def test_codex_missing_account_api_key_and_timeout_are_safe(self):
        import tempfile
        from pathlib import Path
        with tempfile.TemporaryDirectory() as root:
            cli = Path(root) / 'codex'
            for account in [None, {'type': 'apiKey'}]:
                cli.write_text('''#!/usr/bin/env python3
import json, sys
request=json.loads(sys.stdin.readline()); print(json.dumps({'id':request['id'],'result':{}}),flush=True)
sys.stdin.readline()
request=json.loads(sys.stdin.readline()); print(json.dumps({'id':request['id'],'result':{'account':%s}}),flush=True)
''' % repr(account))
                cli.chmod(0o700)
                with self.subTest(account=account), self.assertRaises(usage.UsageError) as error:
                    usage.CodexSource(str(cli), {}, root, 2).read()
                self.assertEqual(error.exception.reason, 'account_unavailable')
            cli.write_text('#!/usr/bin/env python3\nimport time\ntime.sleep(10)\n')
            with self.assertRaises(usage.UsageError) as error:
                usage.CodexSource(str(cli), {}, root, 0.1).read()
            self.assertEqual(error.exception.reason, 'timeout')

    def test_claude_expired_credentials_and_redirect_never_send_refresh_or_follow(self):
        import json
        import tempfile
        from pathlib import Path
        from unittest.mock import patch, MagicMock
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / 'credentials.json'
            for expiry, status, reason in [(1000, 200, 'credentials_expired'), (2000000000000, 302, 'provider_error')]:
                path.write_text(json.dumps({'claudeAiOauth': {'accessToken': 'synthetic', 'expiresAt': expiry}}))
                connection = MagicMock()
                connection.getresponse.return_value.status = status
                with patch('http.client.HTTPSConnection', return_value=connection), self.assertRaises(usage.UsageError) as error:
                    usage.ClaudeSource(path, lambda: 1900000000, 10).read()
                self.assertEqual(error.exception.reason, reason)
                self.assertEqual(connection.request.call_count, 0 if expiry == 1000 else 1)

    def test_claude_slow_response_cannot_extend_request_deadline(self):
        import http.client
        import json
        import tempfile
        import threading
        import time
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        from pathlib import Path
        from unittest.mock import patch
        class SlowHandler(BaseHTTPRequestHandler):
            def log_message(self, *args): pass
            def do_GET(self):
                self.send_response(200)
                self.send_header('Content-Length', '30')
                self.end_headers()
                try:
                    for _ in range(30):
                        self.wfile.write(b' ')
                        self.wfile.flush()
                        time.sleep(0.03)
                except (BrokenPipeError, ConnectionResetError): pass
        server = ThreadingHTTPServer(('127.0.0.1', 0), SlowHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with tempfile.TemporaryDirectory() as root:
                path = Path(root) / 'credentials.json'
                path.write_text(json.dumps({'claudeAiOauth': {'accessToken': 'synthetic', 'expiresAt': 2000000000000}}))
                connection = http.client.HTTPConnection('127.0.0.1', server.server_port, timeout=0.1)
                with patch('http.client.HTTPSConnection', return_value=connection), self.assertRaises(usage.UsageError) as error:
                    usage.ClaudeSource(path, lambda: 1900000000, 0.1).read()
                self.assertEqual(error.exception.reason, 'timeout')
        finally:
            server.shutdown(); server.server_close(); thread.join()
