"""测试和验收脚本里「等某件事发生」的统一上限（#508）。

等待上限只防挂死：等宿主就绪、等任务到某个状态、等子进程退出、等套接字回话，都默认用 `WAIT`。
到了上限就当作挂死判失败，所以它放得很宽，在配置弱、多进程并行的共享机器上也不会提前触发。

这里不放三类时限：
- 性能断言（「必须在 X 秒内完成」）：只放在本机按计时规则跑的测试里，不在 CI 和普通全量里跑；
- 「X 时间内不发生」的观察窗口：保持短窗口，放大只会拖慢；
- 测试为了触发产品超时而给产品配的短时限：那是产品行为，不归这里管。

环境变量 `ILLUSTASH_TEST_WAIT_SECONDS` 整体调整上限（秒，正数），例如很慢的机器上调大，排查挂死时调小。
"""
import math
import os

ENV = 'ILLUSTASH_TEST_WAIT_SECONDS'
DEFAULT = 60.0


def _read(value):
    if value is None or value.strip() == '':
        return DEFAULT
    try:
        seconds = float(value)
    except ValueError:
        raise ValueError(f'{ENV} 必须是正数秒数，现在是 {value!r}') from None
    if not math.isfinite(seconds) or seconds <= 0:
        raise ValueError(f'{ENV} 必须是正数秒数，现在是 {value!r}')
    return seconds


WAIT = _read(os.environ.get(ENV))
