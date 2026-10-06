"""Synthetic regression fixtures and opt-in historical reference implementations."""
import gc
import os
import tempfile
import time

from tests import reference
from tests import network_guard

if os.name == 'nt':
    # 参考实现会在引用环里留着 SQLite 连接；Windows 删不掉仍打开的文件。
    # 清理临时目录前先回收这些连接，不放宽清理失败本身。
    _cleanup = tempfile.TemporaryDirectory.cleanup

    def _collect_then_cleanup(self):
        gc.collect()
        # 实时扫描会短暂持有刚写的文件；有上限地重试，持续占用仍然报错。
        for attempt in range(10):
            try:
                return _cleanup(self)
            except PermissionError:
                if attempt == 9:
                    raise
                time.sleep(0.2)

    tempfile.TemporaryDirectory.cleanup = _collect_then_cleanup
