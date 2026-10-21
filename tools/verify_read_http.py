"""Strict same-library HTTP comparison for R0–R4. Only temporary synthetic databases are accepted.

Build once: --db /tmp/.../library.sqlite --write-requests /tmp/requests.json
Reuse: --requests /tmp/requests.json --python http://127.0.0.1:INTERNAL
       --rust http://127.0.0.1:PUBLIC --origin http://127.0.0.1:PUBLIC [--require-core R1]
Both requests have the *same* Host/Origin and Accept-Encoding: identity. Status and bytes alone
establish equality. JSON parsing is diagnostic only; the Rust route header separately proves routing.
The URL corpus follows the former verify_terms_characters/names sample selection (removed in #186), never their normalization.
Random /api/ask is excluded; validate it by candidate range and constraints instead.
"""
import argparse
import http.client
import json
import os
from pathlib import Path
import random
import socket
import sqlite3
import struct
import sys
import tempfile
import time
from urllib.parse import urlencode, urlsplit

# No Store constructor, schema upgrade, golden recording, remote lookup, or production DB access.
STAGES = {
    'R1': ('user-tags', 'general-tags', 'artists', 'artist', 'artist-audit'),
    'R2': ('entities', 'works'),
    'R3': ('assets', 'assets/count', 'ids', 'detail', 'home', 'fold', 'similar', 'clusters', 'review-pools'),
    'R4': ('characters', 'character/profile', 'character/activity', 'character/stats', 'tagged',
           'oc/groups', 'local-links', 'identity/count',
           'identity/queue', 'bootstrap'),
}


def temporary_path(path):
    roots = (Path(tempfile.gettempdir()).resolve(), Path('/tmp').resolve())
    path = Path(path).absolute()
    # Refuse an outside path before even resolving its symlinks (including real-library paths).
    lexical_roots = (Path(tempfile.gettempdir()).absolute(), Path('/tmp'), Path('/private/tmp'), *roots)
    if not any(path.is_relative_to(root) for root in lexical_roots):
        raise ValueError('只接受系统临时目录内的合成夹具；真实图库和真实副本不在本次范围')
    path = path.resolve()
    if not any(path.is_relative_to(root) for root in roots):
        raise ValueError('只接受系统临时目录内的合成夹具；真实图库和真实副本不在本次范围')
    return path


def corpus(db, sample=16, query_limit=80):
    if sample < 1 or query_limit < 0:
        raise ValueError('sample 必须大于零，queries 不得为负')
    db = temporary_path(db)
    with sqlite3.connect(db.as_uri() + '?mode=ro', uri=True) as c:
        identities = 'SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE r.merged_into IS NULL'
        ids = [r[0] for r in c.execute(f'{identities} ORDER BY t.key')]
        people = [r[0] for r in c.execute(f"{identities} AND t.kind='character' ORDER BY t.key")]
        works = [r[0] for r in c.execute(f"{identities} AND t.kind='work' ORDER BY t.key")]
        assets = [r[0] for r in c.execute('SELECT uuid FROM assets ORDER BY uuid')]
        artists = [r[0] for r in c.execute("SELECT key FROM terms WHERE kind='artist' ORDER BY key")]
        terms = sorted({r[0] for r in c.execute('''SELECT n.value FROM term_names n JOIN term_tree r ON r.term_id=n.term_id UNION SELECT value FROM term_key_names
                     UNION SELECT value FROM term_key_aliases UNION SELECT x.key FROM term_links x JOIN term_tree r ON r.term_id=x.term_id
                     UNION SELECT work_text FROM term_tree UNION SELECT variant FROM term_tree''') if r[0]})
        pages = {
            'identity/queue': len(assets),
        }
    pick = random.Random(20260929)
    chosen = pick.sample(terms, min(query_limit, len(terms)))
    queries = set(chosen) | {'不存在的身份', '%', '_', 'ALIce', '泳装', 'parent · skin'}
    for value in chosen[:query_limit // 2]:
        i = pick.randrange(len(value))
        queries.update((value.upper(), value[i:], value[:i + 1], value[i:i + 2], value.replace('_', ' ')))
    out = []
    def add(stage, path, **params):
        url = '/api/' + path + ('?' + urlencode(params) if params else '')
        item = {'stage': stage, 'method': 'GET', 'url': url}
        if item not in out:
            out.append(item)
    for stage, paths in STAGES.items():
        for path in paths:
            add(stage, path)
    for nsfw in ('hide', 'only'):
        for stage, path in (('R3', 'assets'), ('R3', 'assets/count'), ('R3', 'home'), ('R4', 'characters'),
                            ('R4', 'character/activity'), ('R4', 'tagged'), ('R4', 'identity/count')):
            add(stage, path, nsfw=nsfw)
    add('R4', 'characters', summary=1)
    for query in sorted(queries):
        add('R2', 'entities', q=query)
        add('R2', 'works', q=query)
        add('R4', 'characters', q=query)
        add('R3', 'assets', q=query, limit=60)
        add('R1', 'user-tags', q=query)
        add('R1', 'general-tags', q=query)
    for ident in ids[::sample]:
        add('R4', 'character/profile', entity=ident)
    for ident in people[::sample]:
        add('R4', 'character/stats', id=ident)
        add('R3', 'assets', entity=ident, limit=60)
    for work in works[::max(1, sample // 4)]:
        add('R4', 'characters', work=work)
        add('R3', 'assets', work=work, limit=60)
    for artist in artists:
        add('R1', 'artist', id=artist)
        add('R3', 'assets', artist=artist, limit=60)
    for asset in assets[::sample]:
        add('R3', 'detail', id=asset)
    for path, count in pages.items():
        # Read all pages including one final empty page; no speculative pair-count query dependency.
        for offset in range(0, count + 200, 200):
            add('R4', path, offset=offset, limit=200)
    for limit in (0, 201, 'bad'):
        add('R3', 'assets', limit=limit)
    return out


# 本机宿主的每个请求都要带会话 cookie（发布前安全自查）：宿主就绪时在 stdout 打印带一次性码的地址，
# 第一次访问它换到 cookie。测试和浏览器走同一条路：enter() 从宿主的输出里读就绪行、换 cookie，
# 之后 fetch() 对同一个端点自动带上。只读 HTTP 对照宿主（read-http）不要 cookie，不调用 enter()。
READY = '图库已启动：'
COOKIES = {}
_ENTERED = set()


def output(log):
    """What the host wrote to `log` so far (a path, or the file it inherited as stdout), without moving the
    position it writes at."""
    if isinstance(log, (str, os.PathLike)):
        return Path(log).read_bytes() if Path(log).exists() else b''
    log.flush()
    if hasattr(os, 'pread'):
        return os.pread(log.fileno(), os.fstat(log.fileno()).st_size, 0)
    # Windows: no pread; read through a second handle. A TemporaryFile is opened delete-on-close, which a second
    # handle may only share when it asks for that too.
    for flags in (0, getattr(os, 'O_TEMPORARY', 0)):
        try:
            fd = os.open(log.name, os.O_RDONLY | getattr(os, 'O_BINARY', 0) | flags)
        except PermissionError:
            continue
        with os.fdopen(fd, 'rb') as copy:
            return copy.read()
    raise PermissionError(f'cannot read {log.name}')


def ready(text, origin):
    """The newest ready address of the host at `origin` in its output, if not entered with yet."""
    lines = text.decode('utf-8', 'replace').splitlines() if isinstance(text, bytes) else text.splitlines()
    for line in reversed(lines):
        if line.startswith(READY) and line[len(READY):].startswith(origin + '/'):
            address = line[len(READY):].strip()
            return None if address in _ENTERED else address
    return None


def enter(origin, log, process=None, timeout=15):
    """Wait for the host at `origin` to print its ready address to `log`, then trade its one-time code for the
    session cookie as a browser would. Returns the address."""
    deadline = time.monotonic() + timeout
    while (address := ready(output(log), origin)) is None:
        if process is not None and process.poll() is not None:
            raise AssertionError(f'host exited ({process.returncode}) before it was ready: '
                                 f'{output(log).decode(errors="replace")[-2000:]}')
        if time.monotonic() > deadline:
            raise AssertionError(f'host not ready within {timeout}s: {output(log).decode(errors="replace")[-2000:]}')
        time.sleep(0.02)
    _ENTERED.add(address)
    COOKIES.pop(origin, None)
    response = fetch(origin, {'url': address[len(origin):]}, origin)
    cookie = response['headers'].get('Set-Cookie', '').split(';')[0]
    if response['status'] != 200 or not cookie:
        raise AssertionError(f'entering {origin} failed: {response["status"]} {response["body"][:200]!r}')
    COOKIES[origin] = cookie
    return address


def cookie(origin):
    """The Cookie header for requests to `origin` that do not go through fetch()."""
    return COOKIES[origin]


# struct linger：Windows 是两个 u_short，其他平台是两个 int。
ABORTIVE_CLOSE = struct.pack('HH' if os.name == 'nt' else 'ii', 1, 0)


def fetch(base, item, origin, timeout=30):
    url, public = urlsplit(base), urlsplit(origin)
    if url.scheme != 'http' or url.hostname != '127.0.0.1' or not url.port or url.path or url.query or url.fragment:
        raise ValueError('对照端点必须是 127.0.0.1 的明确 HTTP origin')
    if public.scheme != 'http' or public.hostname != '127.0.0.1' or not public.port or public.path or public.query or public.fragment:
        raise ValueError('Host / Origin 必须是前门的本机 HTTP origin')
    conn = http.client.HTTPConnection(url.hostname, url.port, timeout=timeout)
    headers = {'Host': public.netloc, 'Origin': origin, 'Accept-Encoding': 'identity'}
    if base in COOKIES:
        headers['Cookie'] = COOKIES[base]
    headers.update(item.get('headers', {}))
    # Corpus callers cannot accidentally turn compression on for JSON comparisons.
    headers['Accept-Encoding'] = 'identity'
    started = time.perf_counter()
    try:
        conn.connect()
        # 关闭时直接复位（RST），不走四次挥手：每个请求一条短连接，客户端先关会在本机留下一个 TIME_WAIT，
        # Windows 上并行跑全套时临时端口会耗尽（#140）。正常路径读完整个响应才关闭，请求与响应都不变。
        conn.sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, ABORTIVE_CLOSE)
        conn.request(item.get('method', 'GET'), item['url'], body=item.get('body'), headers=headers)
        response = conn.getresponse()
        data = response.read()
        return {'status': response.status, 'body': data, 'route': response.getheader('X-Illustash-Route'),
                'headers': dict(response.getheaders()), 'ms': (time.perf_counter() - started) * 1000}
    finally:
        conn.close()


def difference(left, right, radius=32):
    """Success bytes are exact; errors compare only status (user decision 2026-10-01)."""
    status_diff = {'status': [left['status'], right['status']]} if left['status'] != right['status'] else {}
    if left['status'] >= 400 or right['status'] >= 400:
        return status_diff or None
    a, b = left['body'], right['body']
    if a == b:
        return status_diff or None
    offset = next((i for i, (x, y) in enumerate(zip(a, b)) if x != y), min(len(a), len(b)))
    out = {**status_diff, 'offset': offset, 'lengths': [len(a), len(b)],
           'python_context': repr(a[max(0, offset - radius):offset + radius]),
           'rust_context': repr(b[max(0, offset - radius):offset + radius])}
    # Parsing may explain a mismatch, but never decides equality or changes the compared bytes.
    try:
        out['diagnostic_only_json_equal'] = json.loads(a) == json.loads(b)
    except (ValueError, UnicodeError):
        pass
    return out


def compare(python, rust, origin, requests, require_core=()):
    report = {'compared': 0, 'differ': 0, 'core_equal': 0, 'forward_equal': 0, 'unverified_route': 0,
              'stages': {}, 'differences': []}
    for item in requests:
        stage = item.get('stage', 'custom')
        counts = report['stages'].setdefault(stage, {'compared': 0, 'core_equal': 0, 'forward_equal': 0})
        report['compared'] += 1
        counts['compared'] += 1
        try:
            if urlsplit(item['url']).path == '/api/ask':
                raise ValueError('/api/ask 是随机抽样，请按候选范围与约束验收，不能比较两次抽样原始字节')
            left, right = fetch(python, item, origin), fetch(rust, item, origin)
            diff = difference(left, right)
            route = right['route']
            if diff is None and route in ('core', 'forward'):
                report[route + '_equal'] += 1
                counts[route + '_equal'] += 1
            elif diff is None:
                report['unverified_route'] += 1
                diff = {'routing': 'Rust 缺少可信的 core|forward 路由标记'}
            if stage in require_core and route != 'core':
                diff = {**(diff or {}), 'routing': '要求核心直读，实际 ' + str(route)}
            report['differ'] += diff is not None
            if diff and len(report['differences']) < 10:
                report['differences'].append({'request': item, **diff})
        except (OSError, http.client.HTTPException, ValueError) as exc:
            report['differ'] += 1
            if len(report['differences']) < 10:
                report['differences'].append({'request': item, 'error': str(exc)[:200]})
    for stage in require_core:
        if stage not in report['stages']:
            report['differ'] += 1
            report['differences'].append({'stage': stage, 'error': '核心验收阶段没有任何请求'})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--db', type=Path)
    parser.add_argument('--sample', type=int, default=16)
    parser.add_argument('--queries', type=int, default=80)
    parser.add_argument('--write-requests', type=Path)
    parser.add_argument('--requests', type=Path)
    parser.add_argument('--python')
    parser.add_argument('--rust')
    parser.add_argument('--origin')
    parser.add_argument('--require-core', action='append', choices=STAGES, default=[])
    args = parser.parse_args()
    try:
        if args.requests:
            requests = json.loads(temporary_path(args.requests).read_text())
        elif args.db:
            requests = corpus(args.db, args.sample, args.queries)
        else:
            parser.error('需要 --db 或 --requests')
        if args.write_requests:
            temporary_path(args.write_requests).write_text(json.dumps(requests, ensure_ascii=False, indent=2))
        if not args.python and not args.rust and args.write_requests:
            print(f'已保存 {len(requests)} 个请求')
            return 0
        if not all((args.python, args.rust, args.origin)):
            parser.error('对照需要 --python、--rust 和同一个 --origin')
        report = compare(args.python, args.rust, args.origin, requests, args.require_core)
        print(json.dumps(report, ensure_ascii=False, indent=2))
        return 1 if report['differences'] else 0
    except (OSError, ValueError, sqlite3.Error) as exc:
        print(str(exc), file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
