"""让合成的 urllib 测试保持离线，同时允许回环地址上的 HTTP 夹具。"""
import ipaddress
import urllib.request
from urllib.parse import urlsplit


def is_local_http(url):
    parts = urlsplit(url)
    if parts.scheme not in ('http', 'https') or parts.username or parts.password:
        return False
    host = parts.hostname
    if host == 'localhost':
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


blocked_requests = []
local_requests = 0
_open = urllib.request.OpenerDirector.open


def guarded_open(self, fullurl, *args, **kwargs):
    global local_requests
    url = fullurl.full_url if isinstance(fullurl, urllib.request.Request) else fullurl
    if not is_local_http(url):
        blocked_requests.append(url)
        raise AssertionError(f'Synthetic tests must mock external urllib requests: {url}')
    local_requests += 1
    return _open(self, fullurl, *args, **kwargs)


urllib.request.OpenerDirector.open = guarded_open
