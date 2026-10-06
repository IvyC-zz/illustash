"""Outside requests: per-host pacing, a traffic budget for the whole run, and a hard stop on refusals.

A 403, 429, a challenge page or a site in maintenance stops that host for the rest of the run: we never retry our way
past a refusal or a bot check. Transient network errors are retried a couple of times and never recorded as misses.
"""
import base64
import http.client
import io
import json
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

UA = 'Illustash/0.1 (+local library; provenance lookups)'
PACE = {'danbooru.donmai.us': 0.35, 'www.pixiv.net': 2.0,   # Pixiv's Cloudflare answered 403 at 1 s (2026-09-28)
        'i.pximg.net': 0.25, 'cdn.donmai.us': 0.25}   # with reused connections: ~3/s, what the old handshakes allowed (2026-09-28)


class Stopped(Exception):
    """This host refused or is unavailable; stop asking it for this run."""


class NotAsked(Stopped):
    """The host was already stopped for this run (an earlier refusal, or an expired login found before the run), so
    this request never went out. It says nothing new about the target: callers must not back off on it."""


class LoginRejected(Stopped):
    """The host answered 401 to a request that carried the user's session: the bound login has expired."""


class BudgetExceeded(Exception):
    pass


class KeepAlive:
    """urlopen that keeps one reusable connection per host (and proxy), through the same proxies urllib would use.
    Without it every thumbnail paid a new TCP + TLS (+ proxy CONNECT) handshake: 0.64 s → 0.17 s per 150 px thumbnail
    through the local proxy (2026-09-28). Requests stay one at a time per host at the same pace; only the handshakes go."""
    STALE = (http.client.RemoteDisconnected, http.client.CannotSendRequest, http.client.BadStatusLine,
             ConnectionResetError, BrokenPipeError)

    def __init__(self):
        self.proxies = urllib.request.getproxies()
        self.bypass = {}
        self.idle = {}                        # (scheme, host, port, proxy) -> [connection]
        self.lock = threading.Lock()

    def _proxy(self, scheme, host):
        if host not in self.bypass:
            self.bypass[host] = bool(urllib.request.proxy_bypass(host))
        proxy = None if self.bypass[host] else self.proxies.get(scheme)
        return urllib.parse.urlsplit(proxy if '://' in proxy else 'http://' + proxy) if proxy else None

    def _connect(self, scheme, host, port, proxy, timeout):
        if not proxy:
            kind = http.client.HTTPSConnection if scheme == 'https' else http.client.HTTPConnection
            return kind(host, port, timeout=timeout)
        if scheme == 'https':                 # CONNECT through the proxy, TLS with the real host
            conn = http.client.HTTPSConnection(proxy.hostname, proxy.port or 80, timeout=timeout)
            conn.set_tunnel(host, port, self._auth(proxy))
            return conn
        return http.client.HTTPConnection(proxy.hostname, proxy.port or 80, timeout=timeout)

    @staticmethod
    def _auth(proxy):
        if not proxy or not proxy.username:
            return {}
        user = f'{urllib.parse.unquote(proxy.username)}:{urllib.parse.unquote(proxy.password or "")}'
        return {'Proxy-Authorization': 'Basic ' + base64.b64encode(user.encode()).decode()}

    def _fetch(self, url, headers, timeout):
        parts = urllib.parse.urlsplit(url)
        scheme, host = parts.scheme, parts.hostname
        port = parts.port or (443 if scheme == 'https' else 80)
        proxy = self._proxy(scheme, host)
        key = (scheme, host, port, proxy.geturl() if proxy else None)
        target = parts.path or '/'
        if parts.query:
            target += '?' + parts.query
        if proxy and scheme == 'http':        # plain HTTP through a proxy: absolute URL, credentials per request
            target, headers = url, {**headers, **self._auth(proxy)}
        for attempt in range(2):
            with self.lock:
                conn = self.idle.get(key, []).pop() if self.idle.get(key) and not attempt else None
            reused = conn is not None
            if not reused:
                conn = self._connect(scheme, host, port, proxy, timeout)
            elif conn.sock:
                conn.sock.settimeout(timeout)
            try:
                conn.request('GET', target, headers=headers)
                response = conn.getresponse()
                body = response.read()
            except Exception as exc:
                conn.close()
                if reused and isinstance(exc, self.STALE):   # the server dropped an idle connection: once more on a new one
                    continue
                raise
            if response.will_close:
                conn.close()
            else:
                with self.lock:
                    self.idle.setdefault(key, []).append(conn)
            return response.status, response.reason, response.headers, body
        raise ConnectionError(f'{host}: 连接被关闭')

    def __call__(self, request, timeout=30):
        url, headers = request.full_url, dict(request.header_items())
        for _ in range(5):
            code, reason, response_headers, body = self._fetch(url, headers, timeout)
            if code in (301, 302, 303, 307, 308) and response_headers.get('Location'):
                url = urllib.parse.urljoin(url, response_headers['Location'])
                continue
            break
        if code >= 400:
            raise urllib.error.HTTPError(url, code, reason, response_headers, io.BytesIO(body))
        return _Answer(body)

    def close(self):
        with self.lock:
            for conns in self.idle.values():
                for conn in conns:
                    conn.close()
            self.idle.clear()


class _Answer(io.BytesIO):
    """What urlopen's response gives Net.get: a context manager with read()."""


class Net:
    def __init__(self, budget_mb=300, headers=None, opener=None, via=None):
        self.budget = budget_mb * 2 ** 20
        self.used = 0
        self.headers = headers or {}          # host -> extra headers (e.g. a user's session cookie)
        self.via = via or {}                  # host -> fetch(url) -> (status, body): the desktop app's logged-in Pixiv window
        self.stopped = {}                     # host -> reason
        self.rejected = set()                 # hosts that refused the bound session (401 with a Cookie)
        self.last = {}
        self.lock = threading.Lock()
        self.open = opener or KeepAlive()

    def _wait(self, host):
        with self.lock:
            gap = PACE.get(host, 0.2) - (time.monotonic() - self.last.get(host, 0))
            self.last[host] = time.monotonic() + max(gap, 0)
        if gap > 0:
            time.sleep(gap)

    def get(self, url, params=None, raw=False, extra=None):
        if params:
            url += ('&' if '?' in url else '?') + urllib.parse.urlencode(params)
        host = urllib.parse.urlsplit(url).hostname
        if host in self.stopped:
            raise NotAsked(f'{host}: {self.stopped[host]}')
        if self.used >= self.budget:
            raise BudgetExceeded(f'已用 {self.used / 2 ** 20:.0f} MB，达到本次上限')
        headers = {'User-Agent': UA, **self.headers.get(host, {}), **(extra or {})}
        via = self.via.get(host)
        session = bool(via) or 'Cookie' in headers     # the window always carries the login the person made there
        for attempt in range(3):
            self._wait(host)
            try:
                if via:
                    code, body = via(url)
                else:
                    with self.open(urllib.request.Request(url, headers=headers), timeout=30) as r:
                        code, body = 200, r.read()
            except urllib.error.HTTPError as exc:
                code, body = exc.code, exc.read(4096)
            except (OSError, http.client.HTTPException) as exc:   # URLError, timeouts, TLS and dropped connections
                error = exc
                time.sleep(2 * (attempt + 1))
                continue
            if 200 <= code < 300:
                with self.lock:   # steps beside each other share one Net (tests/reference/provenance/run.py BESIDE)
                    self.used += len(body)
                return body if raw else json.loads(body)
            if code in (403, 429) or (code == 503 and b'maintenance' in body[:4096].lower()):
                self.stopped[host] = f'HTTP {code}'
                raise Stopped(f'{host}: HTTP {code}')
            if code == 401 and session:     # our session was sent and refused: never fall back to logged out
                self.rejected.add(host)
                self.stopped[host] = '登录已失效（HTTP 401）'
                raise LoginRejected(f'{host}: {self.stopped[host]}')
            if code in (401, 404):   # not logged in / not there: an answer, not a failure
                return None
            error = ConnectionError(f'HTTP {code}')
            time.sleep(2 * (attempt + 1))
        raise ConnectionError(f'{host}: {error}')
