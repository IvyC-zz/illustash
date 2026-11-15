"""独立参考自身的边界：授权范围、失败行为和非浮点业务契约。"""
import os
import unittest
from unittest.mock import patch

import numpy as np

from tests import numeric_reference_mode as mode
from tests import numpy_index_reference as index


class NumericReferenceContractTest(unittest.TestCase):
    def fixture(self, name):
        cls, method = name.split('.')
        return type(cls, (), {'_testMethodName': method})()

    def test_default_stays_frozen_and_only_nine_methods_are_allowed(self):
        self.assertEqual(len(mode.METHODS), 9)
        with patch.dict(os.environ, clear=False):
            os.environ.pop(mode.ENV, None)
            for name in mode.METHODS:
                self.assertFalse(mode.enabled(self.fixture(name)))
            with self.assertRaisesRegex(AssertionError, '未批准'):
                mode.enabled(self.fixture('NativeIndexComputeTest.test_scan_exact_threshold_and_adjacent_float_inputs_match_old_csv'))

    def test_explicit_window_never_falls_back_or_skips_missing_capabilities(self):
        fixture = self.fixture(next(iter(mode.METHODS)))
        with patch.dict(os.environ, {mode.ENV: 'numpyy'}):
            with self.assertRaisesRegex(ValueError, '必须为'):
                mode.enabled(fixture)
        with patch.dict(os.environ, {mode.ENV: 'numpy'}), patch.object(mode.sys, 'platform', 'win32'):
            with self.assertRaisesRegex(RuntimeError, 'candidate missing'):
                mode.missing_candidate('candidate missing')
            with patch.object(np, '__version__', 'unsupported'):
                with self.assertRaisesRegex(RuntimeError, '不能回退'):
                    mode.enabled(fixture)
        with patch.dict(os.environ, {mode.ENV: 'numpy'}), patch.object(mode.sys, 'platform', 'linux'):
            with self.assertRaisesRegex(RuntimeError, '不能跳过'):
                mode.enabled(fixture)

    def test_scalar_sort_keeps_fixed_partition_order_and_rejects_invalid_input(self):
        values = np.ones(10, np.float32)
        self.assertEqual(index.scalar_order(values, partition=True).tolist(), [6, 4, 8, 7, 0, 5, 3, 1, 2])
        for values in (np.array([np.nan], np.float32), np.ones(2, np.float64), np.ones((2, 2), np.float32)):
            with self.assertRaises(ValueError):
                index.scalar_order(values)

    def test_alias_hard_boundary_metadata_priority_and_uphill_walk(self):
        # 用手写矩阵独立约束 <= .01，避免用参考算法自己产生阈值期望。
        edge = np.float32(.01)
        above = np.nextafter(edge, np.float32(np.inf))
        distances = np.array([[0, edge, above], [edge, 0, edge], [above, edge, 0]], np.float32)
        ids = ['a', 'b', 'c']
        def meta(skin, posts, gender):
            return (None, None, None, None, skin, posts, gender, None, None)
        quality = {'a': meta(None, 10000, .89), 'b': meta('skin', 100, .9), 'c': meta(None, 1, .9)}
        with patch.object(index, 'cross', return_value=distances):
            self.assertEqual(index.anchor_alias(np.zeros((3, 768), np.float32), ids, quality), ['c', 'c', 'c'])
        # 将 b→c 移至阈值之外，a 的高热度不能越过人物质量和本体优先级。
        distances[1, 2] = distances[2, 1] = above
        with patch.object(index, 'cross', return_value=distances):
            self.assertEqual(index.anchor_alias(np.zeros((3, 768), np.float32), ids, quality), ['b', 'b', 'c'])


if __name__ == '__main__':
    unittest.main()
