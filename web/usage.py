"""Read-only account quotas. Provider diagnostics and credentials never leave this module."""
import datetime
import http.client
from email.utils import parsedate_to_datetime
import math
import json
import os
import selectors
import socket
import subprocess
import time
import threading
from pathlib import Path
import shutil
from concurrent.futures import ThreadPoolExecutor


class UsageError(Exception):
    """A fixed, public reason code, never a provider's raw diagnostic."""

    def __init__(self, reason, retry_after=0):
        self.reason = reason
        self.retry_after = retry_after


def _window(used, reset):
    if (isinstance(used, bool) or not isinstance(used, (int, float)) or not math.isfinite(used)
            or isinstance(reset, bool) or not isinstance(reset, (int, float))
            or not math.isfinite(reset) or not 0 < reset < 253402300800):
        raise UsageError('invalid_data')
    return {'remainingPercent': max(0, min(100, 100 - used)), 'resetAt': reset}


def codex_weekly(data):
    """Select the explicit weekly window; never infer a week from window position."""
    snapshot = data.get('rateLimits') if isinstance(data, dict) else None
    buckets = data.get('rateLimitsByLimitId') if isinstance(data, dict) else None
    if isinstance(buckets, dict) and buckets:
        # 'codex' is the protocol's default account quota, not a model-specific bucket.
        if 'codex' in buckets:
            snapshot = buckets['codex']
        elif len(buckets) > 1:
            raise UsageError('ambiguous_weekly')
        else:
            raise UsageError('missing_weekly')
    if not isinstance(snapshot, dict):
        raise UsageError('missing_weekly')
    if snapshot.get('limitId') not in (None, 'codex'):
        raise UsageError('ambiguous_weekly')
    windows = [w for key in ('primary', 'secondary') if isinstance(w := snapshot.get(key), dict)
               and w.get('windowDurationMins') == 10080]
    if len(windows) != 1:
        raise UsageError('missing_weekly' if not windows else 'ambiguous_weekly')
    return _window(windows[0].get('usedPercent'), windows[0].get('resetsAt'))


def claude_weekly(data):
    """Normalize only Claude's subscription-wide seven_day window."""
    week = data.get('seven_day') if isinstance(data, dict) else None
    if not isinstance(week, dict):
        raise UsageError('missing_weekly')
    try:
        reset = datetime.datetime.fromisoformat(week['resets_at'].replace('Z', '+00:00'))
        if reset.tzinfo is None:
            raise ValueError('timezone required')
        timestamp = reset.timestamp()
    except (KeyError, TypeError, AttributeError, ValueError, OverflowError):
        raise UsageError('invalid_data') from None
    return _window(week.get('utilization'), timestamp)


class CodexSource:
    """Read the existing CLI account over a short-lived, bounded stdio connection."""

    def __init__(self, binary, environment, cwd, timeout):
        self.binary, self.environment, self.cwd, self.timeout = binary, environment, cwd, timeout

    def read(self):
        """Perform initialization and account reads only; never create threads or turns."""
        process = None
        try:
            process = subprocess.Popen([self.binary, 'app-server'], stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                       env=self.environment, cwd=self.cwd)
            deadline = time.monotonic() + self.timeout
            buffer = b''
            total = 0
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)

                def send(message):
                    process.stdin.write(json.dumps(message).encode() + b'\n')
                    process.stdin.flush()

                def request(identifier, method, params):
                    nonlocal buffer, total
                    send({'id': identifier, 'method': method, 'params': params})
                    while True:
                        if time.monotonic() >= deadline:
                            raise UsageError('timeout')
                        if b'\n' not in buffer:
                            if not selector.select(max(0, deadline - time.monotonic())):
                                raise UsageError('timeout')
                            chunk = os.read(process.stdout.fileno(), 65536)
                            total += len(chunk)
                            if not chunk or total > 1048576:
                                raise UsageError('invalid_data')
                            buffer += chunk
                            continue
                        line, buffer = buffer.split(b'\n', 1)
                        response = json.loads(line)
                        if not isinstance(response, dict) or response.get('id') != identifier:
                            continue
                        if 'error' in response:
                            # Do not expose raw errors (which may contain account diagnostics).
                            raw = json.dumps(response['error'])
                            raise UsageError('rate_limited' if '429' in raw else 'provider_error')
                        return response.get('result')

                request(1, 'initialize', {'clientInfo': {'name': 'nebula_mobile_usage', 'version': '1.0'}})
                send({'method': 'initialized', 'params': {}})
                account = request(2, 'account/read', {'refreshToken': False})
                account = account.get('account') if isinstance(account, dict) else None
                if not isinstance(account, dict) or account.get('type') not in ('chatgpt', 'chatgptAuthTokens'):
                    raise UsageError('account_unavailable')
                return codex_weekly(request(3, 'account/rateLimits/read', {}))
        except FileNotFoundError:
            raise UsageError('cli_missing') from None
        except (OSError, ValueError):
            raise UsageError('provider_error') from None
        finally:
            if process is not None:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=1)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
                process.stdin.close()
                process.stdout.close()


class ClaudeSource:
    """Read existing Linux Claude OAuth credentials in memory; never refresh or persist them."""

    def __init__(self, credentials_path, clock, timeout):
        self.credentials_path, self.clock, self.timeout = credentials_path, clock, timeout

    def read(self):
        """Poll the undocumented subscription endpoint with redirects/proxies disabled."""
        try:
            credentials = json.loads(self.credentials_path.read_text())
            oauth = credentials.get('claudeAiOauth', {})
            token = oauth.get('accessToken')
            expiry = oauth.get('expiresAt')
            if not isinstance(token, str) or not token:
                raise UsageError('account_unavailable')
            if not isinstance(expiry, (int, float)) or expiry / 1000 <= self.clock():
                raise UsageError('credentials_expired')
        except (OSError, ValueError, AttributeError):
            raise UsageError('account_unavailable') from None
        connection = http.client.HTTPSConnection('api.anthropic.com', timeout=self.timeout)
        deadline = time.monotonic() + self.timeout
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

            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise UsageError('timeout')
            # Interrupt headers AND body reads, including a peer that continually trickles bytes.
            timer = threading.Timer(remaining, expire)
            timer.daemon = True
            timer.start()
            connection.request('GET', '/api/oauth/usage', headers={
                'Authorization': 'Bearer ' + token, 'anthropic-beta': 'oauth-2025-04-20',
                'Accept': 'application/json', 'User-Agent': 'nebula-mobile-usage/1.0'})
            response = connection.getresponse()
            if response.status == 429:
                retry = response.getheader('Retry-After', '')
                try:
                    delay = float(retry) if retry.isdigit() else parsedate_to_datetime(retry).timestamp() - self.clock()
                except (TypeError, ValueError, OverflowError):
                    delay = 900
                raise UsageError('rate_limited', max(900, min(86400, delay)))
            if response.status in (401, 403):
                raise UsageError('account_unavailable')
            if response.status != 200:
                raise UsageError('provider_error')
            data = response.read(1048577)
            if expired.is_set():
                raise UsageError('timeout')
            if len(data) > 1048576:
                raise UsageError('invalid_data')
            return claude_weekly(json.loads(data))
        except TimeoutError:
            raise UsageError('timeout') from None
        except (OSError, ValueError, http.client.HTTPException):
            raise UsageError('timeout' if expired.is_set() else 'provider_error') from None
        finally:
            if timer is not None:
                timer.cancel()
            connection.close()


class UsageService:
    """Two account-level cache entries, single flight across callers, independent retries."""

    def __init__(self, sources, clock, monotonic):
        self.sources, self.clock, self.monotonic = sources, clock, monotonic
        self.lock = threading.Lock()
        self.values = {name: {'remainingPercent': None, 'resetAt': None, 'updatedAt': None,
                              'status': 'loading', 'reason': None} for name in sources}
        self.next_read = {name: 0 for name in sources}
        self.failures = {name: 0 for name in sources}

    def _refresh(self, name):
        try:
            value = self.sources[name].read()
            if value['resetAt'] <= self.clock():
                raise UsageError('expired_window')
            self.values[name] = {**value, 'updatedAt': self.clock(), 'status': 'ok', 'reason': None}
            self.failures[name] = 0
            delay = 300
        except Exception as error:
            # Never serialize exception text: all public diagnostics come from this allowlist.
            reason = error.reason if isinstance(error, UsageError) else 'provider_error'
            self.failures[name] += 1
            delay = 300
            if reason == 'rate_limited':
                delay = max(min(3600, 900 * 2 ** min(self.failures[name] - 1, 2)), error.retry_after)
            old = self.values[name]
            self.values[name] = {**old, 'status': 'stale' if old['updatedAt'] is not None else 'unavailable', 'reason': reason}
        self.next_read[name] = self.monotonic() + delay

    def get(self):
        """Return safe snapshots; overlapping callers reuse values while one refresh runs."""
        if self.lock.acquire(blocking=False):
            try:
                due = [name for name in self.sources if self.monotonic() >= self.next_read[name]]
                if due:
                    with ThreadPoolExecutor(max_workers=len(due), thread_name_prefix='account-usage') as pool:
                        list(pool.map(self._refresh, due))
            finally:
                self.lock.release()
        result = {}
        for name, value in self.values.items():
            value = dict(value)
            if value['status'] == 'ok' and (self.clock() - value['updatedAt'] >= 300 or value['resetAt'] <= self.clock()):
                value.update(status='stale', reason='expired_window' if value['resetAt'] <= self.clock() else 'refresh_pending')
            result[name] = value
        return result


def unavailable():
    """Explicitly disabled sources for embedded gateways and synthetic previews."""
    return {name: {'remainingPercent': None, 'resetAt': None, 'updatedAt': None,
                   'status': 'unavailable', 'reason': 'disabled'} for name in ('codex', 'claude')}


def from_environment():
    """Wire host CLI credentials without inheriting agent identity or gateway secrets."""
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('NEBULA_', 'MC_', 'PI_', 'CODEX_THREAD', 'ANTHROPIC_', 'OPENAI_API_KEY'))}
    home = Path.home()
    config = Path(os.environ.get('CLAUDE_CONFIG_DIR', str(home / '.claude')))
    return UsageService({
        'codex': CodexSource(shutil.which('codex') or 'codex', environment, str(home), 12),
        'claude': ClaudeSource(config / '.credentials.json', time.time, 10),
    }, time.time, time.monotonic)
