"""Explicit #153 candidate build only; call inside an assigned build window.

LCMS overrides are removed only from child environments. Uses this worktree's
own target, never the fixed acceptance cache. Final linked CMM must be 2.19.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def build_environment(inherited):
    child = dict(inherited)
    for name in ('LCMS2_LIB_DIR', 'LCMS2_INCLUDE_DIR', 'LCMS2_STATIC', 'CARGO_TARGET_DIR'):
        child.pop(name, None)
    return child


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--jobs', type=int, choices=(1, 2), default=2)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    target = root / 'native/terms/target'
    child = build_environment(os.environ)
    subprocess.run(['cargo', 'build', '--manifest-path', str(root / 'native/terms/Cargo.toml'),
                    '--locked', '--offline', f'-j{args.jobs}', '--target-dir', str(target),
                    '--example', 'provenance-compute'], cwd=root, env=child, check=True)
    binary = target / 'debug/examples' / ('provenance-compute.exe' if os.name == 'nt' else 'provenance-compute')
    with tempfile.TemporaryDirectory(prefix='illustash-lcms-version-') as tmp:
        incoming, output = Path(tmp) / 'request.json', Path(tmp) / 'output.json'
        incoming.write_text(json.dumps(dict(op='lcms_version')))
        subprocess.run([str(binary), str(incoming), str(output)], cwd=root,
                       env=child | {'PATH': ''}, check=True, timeout=10)
        if json.loads(output.read_text()) != 2190:
            raise SystemExit('candidate linked LittleCMS is not 2.19')
    print('candidate linked LittleCMS2.19; no global environment changes')


if __name__ == '__main__':
    main()
