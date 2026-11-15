"""#347 获批的九个数值对照用例；默认继续严格回放冻结值。

异构 CI 显式设置 ILLUSTASH_TEST_NUMERIC_REFERENCE=numpy。同机参考只接收合成输入，
不读取候选输出，不更新冻结值，也不提供生产计算入口。历史录制 CPU 身份仍未知。
"""
from contextlib import contextmanager
import json
import os
import sys
import unittest


ENV = 'ILLUSTASH_TEST_NUMERIC_REFERENCE'
METHODS = frozenset({
    'NativeRelationImagesTest.test_locate_flat_is_rejected_and_synthetic_crop_reference_is_exact',
    'NativeProvenanceMatchTest.test_stored_ratio_float32_boundary_and_flat_rejection_are_exact',
    'InstalledProvenanceMatchTest.test_stored_ratio_float32_boundary_and_flat_rejection_are_exact',
    'NativeIndexComputeTest.test_cached_apply_clean_build_scan_matches_algorithm_reference',
    'NativeIndexComputeTest.test_large_alias_matrix_at_boundary_matches_old_scan',
    'NativeIndexComputeTest.test_merge_percentile_core_and_partition_ties',
    'NativeIndexComputeTest.test_recognition_snapshot_alias_and_claims_on_large_boundary',
    'NativeIndexComputeTest.test_reference_dtype_layout_endian',
    'InstalledIndexComputeTest.test_cached_clean_build_scan_exact_old_algorithm_without_python_or_models',
})


def mode():
    value = os.environ.get(ENV, 'frozen')
    if value not in ('frozen', 'numpy'):
        raise ValueError(f'{ENV} 必须为 frozen 或 numpy，收到 {value!r}')
    if value == 'numpy' and sys.platform not in ('darwin', 'win32'):
        raise RuntimeError('NumPy 参考窗口只支持 macOS 和 Windows，不能跳过后报告通过')
    return value


def identity(test):
    return f'{type(test).__name__}.{test._testMethodName}'


def enabled(test):
    """仅由白名单方法显式选择；错误调用不能扩大参考口径。"""
    if identity(test) not in METHODS:
        raise AssertionError(f'未批准的 NumPy 参考调用：{identity(test)}')
    if mode() == 'frozen':
        return False
    require_dependencies()
    return True


def require_dependencies():
    import numpy as np
    import PIL
    if (np.__version__, PIL.__version__) != ('2.5.3', '12.3.0'):
        raise RuntimeError('NumPy 参考要求 numpy 2.5.3、Pillow 12.3.0；不能回退冻结值')


def defer_missing(cls, error):
    """显式窗口延迟到实例判断，避免把同类其他方法的 skip 改成 error。"""
    if mode() != 'numpy':
        return False
    cls._missing_numeric_candidate = str(error)
    return True


def check_candidate(test):
    message = getattr(test, '_missing_numeric_candidate', None)
    if message:
        if mode() == 'numpy' and identity(test) in METHODS:
            raise RuntimeError(message)
        raise unittest.SkipTest(message)


def references(test):
    """同机参考始终执行；已实测适用的完整身份再追加冻结比较。"""
    if not enabled(test):
        return ('frozen',)
    from tests.numpy_reference_environment import environment, supports_frozen
    return ('numpy', 'frozen') if supports_frozen(environment()) else ('numpy',)


@contextmanager
def comparing(test, reference):
    """记录实际进入的比较及其结果；不捕获失败后改用另一种参考。"""
    if identity(test) not in METHODS or reference not in ('numpy', 'frozen'):
        raise AssertionError('参考比较超出批准范围')
    record = dict(test=test.id(), reference=reference)
    try:
        yield
    except BaseException:
        print('NUMERIC_REFERENCE ' + json.dumps(record | {'status': 'failed'}), file=sys.__stderr__, flush=True)
        raise
    else:
        print('NUMERIC_REFERENCE ' + json.dumps(record | {'status': 'passed'}), file=sys.__stderr__, flush=True)


# 发现测试时即拒绝拼写错误，不能把未知模式当作默认回放。
mode()
