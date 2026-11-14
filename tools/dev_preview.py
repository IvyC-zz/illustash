#!/usr/bin/env python3
"""浏览器走查用的隔离宿主：`.claude/launch.json` 的 `web-isolated` 配置调用它。

不碰真实图库和设置：HOME、App Support（`ILLUSTASH_APP_SUPPORT`）、共享资源（`ILLUSTASH_SHARED`）都指向
一个临时目录，`ILLUSTASH_NO_KEYCHAIN=1` 让凭据写进这个目录而不是钥匙串；图库是在里面新建的，只含脚本
生成的渐变图。默认退出时删掉临时目录；`--work DIR` 改用指定目录并保留，下次用同一目录接着用。

宿主的地址带一次性码（docs/DEVELOPMENT.md「快速开始」），launch.json 只能写固定地址，所以脚本在
`--port`（或环境变量 PORT）上另开一个只做跳转的小服务：宿主就绪后才监听，`GET /` 跳到宿主打印的地址。
宿主本身在另一个空闲端口上，就绪行和日志原样转到本脚本的 stdout。

用法：python3 tools/dev_preview.py [--port 8777] [--work DIR] [--images N] [-- 宿主额外参数…]
"""
from __future__ import annotations

import argparse
import http.server
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
READY = re.compile(r'图库已启动：(http://127\.0\.0\.1:\d+/\?k=\S+)')


def binary() -> Path:
    """与 start-web.command 相同：ILLUSTASH_TERMS_BIN 优先，否则取 target 里较新的 release/debug 构建。"""
    if configured := os.environ.get('ILLUSTASH_TERMS_BIN'):
        return Path(configured)
    found = [p for p in (ROOT / 'target' / kind / 'illustash-terms' for kind in ('release', 'debug'))
             if os.access(p, os.X_OK)]
    if not found:
        sys.exit('缺少词条核心程序，请先构建 native/terms 或设置 ILLUSTASH_TERMS_BIN。')
    return max(found, key=lambda p: p.stat().st_mtime)


def png(path: Path, width: int, height: int, seed: int) -> None:
    """写一张合成渐变图（RGB PNG），颜色由 seed 决定。"""
    a = ((seed * 67) % 256, (seed * 151 + 40) % 256, (seed * 29 + 90) % 256)
    b = ((seed * 13 + 200) % 256, (seed * 97 + 10) % 256, (seed * 181 + 60) % 256)
    # 沿对角线渐变：颜色只取决于 x + y，先算好每条对角线的颜色，每行切一段。
    span = width + height - 1
    colors = [bytes(int(a[c] + (b[c] - a[c]) * s / (span - 1)) for c in range(3)) for s in range(span)]
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        rows += b''.join(colors[y:y + width])

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))

    header = struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0)
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(bytes(rows), 6))
                     + chunk(b'IEND', b''))


def free_port() -> int:
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def redirector(port: int, target: str) -> http.server.ThreadingHTTPServer:
    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802（标准库要求的方法名）
            self.send_response(302)
            self.send_header('Location', target)
            self.send_header('Cache-Control', 'no-store')
            self.end_headers()

        do_HEAD = do_GET

        def log_message(self, *_: object) -> None:
            pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def main() -> int:
    parser = argparse.ArgumentParser(description='启动只含合成图片的隔离宿主，供浏览器走查')
    parser.add_argument('--port', type=int, default=8777, help='跳转服务端口（环境变量 PORT 优先）')
    parser.add_argument('--work', type=Path, help='隔离目录；指定时保留，不指定则用临时目录并在退出时删除')
    parser.add_argument('--images', type=int, default=24, help='新建图库时生成的合成图片张数')
    parser.add_argument('host_args', nargs='*', help='在 -- 之后，原样传给 illustash-terms http')
    args = parser.parse_args()
    port = int(os.environ.get('PORT') or args.port)
    core = binary()

    work = args.work.resolve() if args.work else Path(tempfile.mkdtemp(prefix='illustash-preview-'))
    keep = args.work is not None
    env = dict(os.environ)
    env.update({
        'HOME': str(work / 'home'),
        'ILLUSTASH_APP_SUPPORT': str(work / 'support'),
        'ILLUSTASH_SHARED': str(work / 'shared'),
        'ILLUSTASH_NO_KEYCHAIN': '1',
    })
    for name in ('home', 'support', 'shared'):
        (work / name).mkdir(parents=True, exist_ok=True)
    library = work / '合成图库.pilib'
    host = None
    jump = None

    def stop(*_: object) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    try:
        if not library.exists():
            images = work / 'images'
            images.mkdir(exist_ok=True)
            for i in range(args.images):
                png(images / f'synthetic-{i + 1:03d}.png', 320 + (i % 3) * 80, 240 + (i % 4) * 60, i + 1)
            subprocess.run([str(core), 'library-create', '--dest', str(library), '--folder', str(images),
                            '--name', '合成图库'],
                           env=env, check=True, stdout=subprocess.DEVNULL)
        print(f'隔离目录：{work}（{"保留" if keep else "退出时删除"}）', flush=True)
        host = subprocess.Popen(
            [str(core), 'http', '--root', str(ROOT), '--library', str(library), '--port', str(free_port()),
             '--no-open', *args.host_args],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        assert host.stdout is not None
        for line in host.stdout:
            print(line, end='', flush=True)
            if jump is None and (match := READY.search(line)):
                jump = redirector(port, match.group(1))
                print(f'跳转入口：http://127.0.0.1:{port}/（第一次打开时换会话 cookie；浏览器丢了 cookie 就重启本脚本拿新码）', flush=True)
        return host.wait()
    except KeyboardInterrupt:
        return 0
    finally:
        if jump is not None:
            jump.shutdown()
        if host is not None and host.poll() is None:
            host.terminate()
            try:
                host.wait(timeout=10)
            except subprocess.TimeoutExpired:
                host.kill()
        if not keep:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == '__main__':
    sys.exit(main())
