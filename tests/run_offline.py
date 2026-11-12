"""运行合成 Python 测试集；被拦下的请求即使被捕获了也判为失败。"""
import sys
import unittest

from tests import network_guard


if __name__ == '__main__':
    program = unittest.main(module=None, argv=[sys.argv[0], 'discover', '-s', 'tests',
                                               '-t', '.', *sys.argv[1:]], exit=False)
    print(f'urllib guard: {len(network_guard.blocked_requests)} external attempts, '
          f'{network_guard.local_requests} loopback opens')
    for url in network_guard.blocked_requests:
        print(f'Blocked: {url}')
    sys.exit(0 if program.result.wasSuccessful() and not network_guard.blocked_requests else 1)
