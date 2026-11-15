"""#347 获批的九个数值对照用例；默认继续严格回放冻结值。

异构 CI 显式设置 ILLUSTASH_TEST_NUMERIC_REFERENCE=numpy。同机参考只接收合成输入，
不读取候选输出，不更新冻结值，也不提供生产计算入口。历史录制 CPU 身份仍未知。
"""
import os
import sys


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


def enabled(test):
    """仅由白名单方法显式选择；错误调用不能扩大参考口径。"""
    identity = f'{type(test).__name__}.{test._testMethodName}'
    if identity not in METHODS:
        raise AssertionError(f'未批准的 NumPy 参考调用：{identity}')
    if mode() == 'frozen':
        return False
    import numpy as np
    import PIL
    if (np.__version__, PIL.__version__) != ('2.5.3', '12.3.0'):
        raise RuntimeError('NumPy 参考要求 numpy 2.5.3、Pillow 12.3.0；不能回退冻结值')
    return True


def missing_candidate(message):
    """显式参考窗口不能因缺少候选而变成跳过。"""
    if mode() == 'numpy':
        raise RuntimeError(message)


# 发现测试时即拒绝拼写错误，不能把未知模式当作默认回放。
mode()
