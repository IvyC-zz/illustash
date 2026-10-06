"""The desktop shell's Pixiv channel (desktop/src/bridge.rs).

The person logs in to Pixiv themselves in the shell's Pixiv window (any bot check is theirs to pass). Requests to
www.pixiv.net/ajax then go out from that real WebView, one read-only GET at a time, and come back here unchanged:
Python-side requests that carried a copied PHPSESSID were stopped by Cloudflare's bot check (docs/PROVENANCE.md §8),
and we neither disguise ourselves nor get around the check. i.pximg.net thumbnails still come straight from Python.

Only available when the server was started by the shell, which passes the channel's address and a per-launch token.
"""
import json
import os
import urllib.error
import urllib.parse
import urllib.request

HOST = 'www.pixiv.net'
_open = urllib.request.build_opener(urllib.request.ProxyHandler({})).open   # 127.0.0.1: never through a proxy


def available():
    return bool(os.environ.get('ILLUSTASH_DESKTOP_BRIDGE') and os.environ.get('ILLUSTASH_DESKTOP_TOKEN'))


def _call(method, route, params=None, timeout=120):
    if not available():
        raise ConnectionError('不是从 Illustash 桌面 app 启动的，用不了 Pixiv 登录窗口')
    url = os.environ['ILLUSTASH_DESKTOP_BRIDGE'].rstrip('/') + route
    if params:
        url += '?' + urllib.parse.urlencode(params)
    request = urllib.request.Request(url, method=method, data=b'' if method == 'POST' else None,
                                     headers={'X-Illustash-Token': os.environ['ILLUSTASH_DESKTOP_TOKEN']})
    try:
        with _open(request, timeout=timeout) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as exc:
        try:
            why = json.loads(exc.read()).get('error')
        except ValueError:
            why = None
        raise ConnectionError(f'桌面 app：{why or f"HTTP {exc.code}"}') from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise ConnectionError(f'桌面 app 没有响应：{exc}') from exc


def session():
    """{loggedIn, userId, loginOpen}: whether the Pixiv window holds a login (read locally from its cookies, no request)."""
    return _call('GET', '/pixiv/session', timeout=30)


def login(force=False):
    """Show Pixiv; force a fresh login after expiry. The person closes the window when finished."""
    return _call('POST', '/pixiv/login', {'force': '1'} if force else None, timeout=30)


def logout():
    """Forget the login: clears the Pixiv window's own website data."""
    return _call('POST', '/pixiv/logout', timeout=30)


def fetch(url):
    """GET one www.pixiv.net/ajax URL from the Pixiv window: (HTTP status, body bytes). Status 0 means it never got an answer."""
    parts = urllib.parse.urlsplit(url)
    if parts.hostname != HOST:
        raise ValueError(f'Pixiv 窗口只发 {HOST} 的请求')
    reply = _call('GET', '/pixiv/fetch', {'path': parts.path + (f'?{parts.query}' if parts.query else '')})
    if not reply.get('status'):
        raise ConnectionError(f"Pixiv 窗口里的请求失败：{reply.get('error') or '没有响应'}")
    return reply['status'], (reply.get('body') or '').encode('utf-8')


def via():
    """Routes for provenance.net.Net: www.pixiv.net through the window, when the shell is there."""
    return {HOST: fetch} if available() else {}
