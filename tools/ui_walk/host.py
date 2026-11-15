"""走查用的宿主进程（#453）：与 tools/dev_preview.py 相同的隔离环境，在空闲端口上起 `illustash-terms http`。

走查时关掉自动同步、名字查询、来源查询和索引检查：合成图库在建夹具时已经同步好，这些后台任务只会让两次运行的
时序不同，也不该联网。
"""
from __future__ import annotations

import collections
import subprocess
import threading
import time
from pathlib import Path

from tools import dev_preview

QUIET = ['--no-auto-sync', '--no-provenance', '--no-index-check']


class Host:
    def __init__(self, core: Path, root: Path, library: Path, work: Path, extra: list[str] | None = None,
                 env: dict[str, str] | None = None):
        self.core, self.root, self.library, self.work = core, root, library, work
        self.extra = QUIET if extra is None else extra
        self.env = env or {}
        self.lines: collections.deque[str] = collections.deque(maxlen=200)
        self.process: subprocess.Popen[str] | None = None
        self.url = ''

    def __enter__(self) -> 'Host':
        env = {**dev_preview.isolated_env(self.work), **self.env}
        self.process = subprocess.Popen(
            [str(self.core), 'http', '--root', str(self.root), '--library', str(self.library),
             '--port', str(dev_preview.free_port()), '--no-open', *self.extra],
            env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, bufsize=1)
        ready = threading.Event()

        def pump() -> None:
            assert self.process and self.process.stdout
            for line in self.process.stdout:
                self.lines.append(line.rstrip('\n'))
                if not self.url and (m := dev_preview.READY.search(line)):
                    self.url = m.group(1)
                    ready.set()
            ready.set()

        threading.Thread(target=pump, daemon=True).start()
        if not ready.wait(60) or not self.url:
            self.__exit__()
            raise RuntimeError('宿主没有启动：\n' + self.log_tail())
        return self

    def log_tail(self, n: int = 40) -> str:
        return '\n'.join(list(self.lines)[-n:])

    def __exit__(self, *_: object) -> None:
        if self.process and self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        time.sleep(0.05)
