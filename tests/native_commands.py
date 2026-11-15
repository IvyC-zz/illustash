"""测试直接调用公开原生命令；无自动构建、发现或旧实现回退。"""
import json
import os
from pathlib import Path
import subprocess

from tools.wait_limit import WAIT


class Failed(RuntimeError):
    pass


def binary():
    configured = os.environ.get('ILLUSTASH_TERMS_BIN')
    return Path(configured).resolve() if configured and Path(configured).is_file() else None


def run(*args, stdin=None, timeout=WAIT):
    executable = binary()
    if executable is None:
        raise Failed('测试需要显式设置 ILLUSTASH_TERMS_BIN')
    result = subprocess.run([str(executable), *map(str, args)], input=stdin,
                            text=True, capture_output=True, timeout=timeout)
    if result.returncode:
        raise Failed(result.stderr.strip())
    return json.loads(result.stdout)


def character_command(db, command):
    return run('character-command', '--db', db, stdin=json.dumps(command, ensure_ascii=False, allow_nan=False))
