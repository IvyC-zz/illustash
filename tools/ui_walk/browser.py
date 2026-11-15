"""走查用的无头浏览器（#453）：chrome-headless-shell，经 --remote-debugging-port 控制。

选它而不是 Chrome 的 `--headless=new`：后者在本机主进程会长期占满一个核，挡住别的会话的计时测试，页面里的定时器和
requestAnimationFrame 也会卡住（docs/LOCAL_CHECKS.md「界面走查」）。chrome-headless-shell 空闲时几乎不占 CPU。

查找顺序：环境变量 ILLUSTASH_WALK_CHROME；Playwright 缓存里最新的 chromium_headless_shell-*（macOS 在
~/Library/Caches/ms-playwright，Linux 在 ~/.cache/ms-playwright，也认 PLAYWRIGHT_BROWSERS_PATH）。
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def find_chrome() -> Path:
    if configured := os.environ.get('ILLUSTASH_WALK_CHROME'):
        return Path(configured)
    caches = [os.environ.get('PLAYWRIGHT_BROWSERS_PATH'), '~/Library/Caches/ms-playwright', '~/.cache/ms-playwright']
    names = ('chrome-headless-shell', 'chrome-headless-shell.exe')
    found: list[tuple[int, Path]] = []
    for cache in filter(None, caches):
        base = Path(cache).expanduser()
        for folder in base.glob('chromium_headless_shell-*'):
            revision = int(m.group(1)) if (m := re.search(r'-(\d+)$', folder.name)) else 0
            for name in names:
                found += [(revision, p) for p in folder.glob(f'*/{name}') if os.access(p, os.X_OK)]
    if not found:
        sys.exit('找不到 chrome-headless-shell：设置 ILLUSTASH_WALK_CHROME，或运行 '
                 '`npx playwright install chromium-headless-shell`（只下载浏览器，不往仓库加依赖）。')
    return max(found)[1]


class Browser:
    """一个无头浏览器进程；profile 放在临时目录，退出时删掉。"""

    def __init__(self, chrome: Path, width: int = 1440, height: int = 900):
        self.profile = Path(tempfile.mkdtemp(prefix='illustash-walk-chrome-'))
        args = [str(chrome), '--remote-debugging-port=0', f'--user-data-dir={self.profile}', '--no-first-run',
                '--no-default-browser-check', '--disable-gpu', '--hide-scrollbars', '--mute-audio',
                '--disable-background-timer-throttling', '--disable-renderer-backgrounding',
                '--disable-backgrounding-occluded-windows', '--disable-extensions', '--disable-sync',
                '--force-device-scale-factor=1', f'--window-size={width},{height}', '--font-render-hinting=none',
                'about:blank']
        self.process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        port_file = self.profile / 'DevToolsActivePort'
        deadline = time.monotonic() + 20
        while not port_file.exists() or not port_file.read_text().strip():
            if self.process.poll() is not None or time.monotonic() > deadline:
                self.close()
                sys.exit('无头浏览器没有启动（DevToolsActivePort 没出现）。')
            time.sleep(0.05)
        self.port = int(port_file.read_text().split()[0])
        self.version = self._version(chrome)

    @staticmethod
    def _version(chrome: Path) -> str:
        try:
            out = subprocess.run([str(chrome), '--version'], capture_output=True, text=True, timeout=10).stdout.strip()
        except (OSError, subprocess.TimeoutExpired):
            out = ''
        return out or chrome.parent.parent.name

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()
        shutil.rmtree(self.profile, ignore_errors=True)
