"""#347 测试环境身份；只读取加载器和依赖信息，不读取原生计算结果。"""
import ctypes
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path
import platform
import sys


# 实测适用登记，依据诊断 run 43659840636 的 auto 组九方法全部通过。
# 此项不能用来倒推历史录制机器的 CPU 身份；不按 SkylakeX 名字单独选参考。
FROZEN_ENVIRONMENT = {
    'platform': 'win32', 'machine': 'AMD64', 'numpy': '2.5.3', 'pillow': '12.3.0',
    'dll_sha256': 'ed4f167a5330424524f45258e7ca2c8d7f03c0cba1f30ee4985553ac1f88ecf8',
    'core': 'SkylakeX', 'numpy_blas_threads': 4,
    'config': 'OpenBLAS 0.3.34.106.0  USE64BITINT DYNAMIC_ARCH NO_AFFINITY SkylakeX MAX_THREADS=24',
    'numpy_dispatch': {'AVX2': True, 'FMA3': True, 'AVX512F': True, 'X86_V3': True, 'X86_V4': True},
    'numpy_disable_cpu_features': '',
}


def supports_frozen(value):
    return all(value.get(key) == expected for key, expected in FROZEN_ENVIRONMENT.items())


@lru_cache(None)
def environment():
    import numpy as np
    import PIL
    result = dict(platform=sys.platform, machine=platform.machine(), python=platform.python_version(),
                  numpy=np.__version__, pillow=PIL.__version__, cpu=platform.processor(),
                  identity_process='numpy_reference', candidate_blas_threads='not_measured',
                  numpy_disable_cpu_features=os.environ.get('NPY_DISABLE_CPU_FEATURES', '').strip())
    if sys.platform != 'win32':
        return result
    path = Path(os.environ['ILLUSTASH_RESEARCH_OPENBLAS_DLL']).resolve(strict=True)
    digest = hashlib.sha256(path.read_bytes()).hexdigest()
    registry = json.loads((Path(__file__).resolve().parents[1] / 'crates/platform/src/research_openblas.json').read_text())
    if registry.get(digest, {}).get('numpy') != np.__version__:
        raise RuntimeError('NumPy 与已登记 OpenBLAS DLL 身份不匹配')
    # NumPy 先导入；再核对其 wheel 内的 DLL，防止只检查另行加载的一份同名库。
    libraries = Path(np.__file__).resolve().parent.parent / 'numpy.libs'
    numpy_dlls = [p for p in libraries.glob('*.dll') if 'openblas' in p.name.lower()]
    if len(numpy_dlls) != 1 or not os.path.samefile(numpy_dlls[0], path):
        raise RuntimeError('候选 DLL 必须是当前 NumPy wheel 实际使用的同一文件')
    dll = ctypes.CDLL(str(path), winmode=0x900)
    get_path = ctypes.WinDLL('kernel32', use_last_error=True).GetModuleFileNameW
    get_path.argtypes = [ctypes.c_void_p, ctypes.c_wchar_p, ctypes.c_uint32]
    get_path.restype = ctypes.c_uint32
    buffer = ctypes.create_unicode_buffer(32768)
    length = get_path(dll._handle, buffer, len(buffer))
    if not 0 < length < len(buffer) or not os.path.samefile(buffer.value, path):
        raise RuntimeError('实际加载的 OpenBLAS 与核验的文件不一致')

    def exported(name, result_type):
        function = getattr(dll, 'scipy_openblas_' + name + '64_')
        function.argtypes = []
        function.restype = result_type
        value = function()
        return value.decode('ascii') if isinstance(value, bytes) else value

    from numpy._core import _multiarray_umath
    result.update(dll_sha256=digest, core=exported('get_corename', ctypes.c_char_p),
                  config=exported('get_config', ctypes.c_char_p), numpy_blas_threads=exported('get_num_threads', ctypes.c_int),
                  cpu_features=_multiarray_umath.__cpu_features__,
                  numpy_dispatch={key: _multiarray_umath.__cpu_features__.get(key)
                                  for key in FROZEN_ENVIRONMENT['numpy_dispatch']})
    return result


if __name__ == '__main__':
    from tests.numeric_reference_mode import mode, require_dependencies
    if mode() == 'numpy':
        require_dependencies()
    value = environment()
    print(json.dumps(value | {'mode': mode(), 'additional_frozen': supports_frozen(value)}, indent=2))
