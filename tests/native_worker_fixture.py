"""显式的 Python 夹具启动器；生产环境的 worker 仍是外部可执行文件。"""
import os
from pathlib import Path
import sys


def command(script):
    script = Path(script)
    if os.name != 'nt':
        script.chmod(0o755)
        return str(script)
    wrapper = script.with_name(script.name + '.cmd')
    # stderr 追加写进 `<脚本>.stderr`，多次启动的记录都留着，失败时可以按次数对照（#501）。
    wrapper.write_text('@echo off\n"' + sys.executable + '" "' + str(script) + '" %* 2>>"' + str(script) + '.stderr"\n', encoding='utf-8')
    return str(wrapper)


def source(executable):
    path = Path(executable)
    return path.with_suffix('') if path.suffix == '.cmd' else path


def exists(pid):
    if os.name != 'nt':
        try:
            os.killpg(pid, 0)
            return True
        except ProcessLookupError:
            return False
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    kernel.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    kernel.OpenProcess.restype = wintypes.HANDLE
    kernel.WaitForSingleObject.argtypes = [wintypes.HANDLE, wintypes.DWORD]
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    handle = kernel.OpenProcess(0x100000, False, pid)
    if not handle:
        if ctypes.get_last_error() == 87:
            return False
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        return kernel.WaitForSingleObject(handle, 0) == 258
    finally:
        kernel.CloseHandle(handle)
