"""检查声明的 crate 依赖方向，不解析 Rust，也不引入 workspace。"""
from pathlib import Path
import argparse
import tomllib

ROOT = Path(__file__).resolve().parents[1]
MANIFESTS = {'core': 'crates/core/Cargo.toml', 'platform': 'crates/platform/Cargo.toml',
             'app': 'native/terms/Cargo.toml', 'desktop': 'desktop/Cargo.toml'}
OWNERS = {'illustash-core': 'core', 'illustash-platform': 'platform',
          'illustash-terms': 'app', 'illustash-desktop': 'desktop'}
ALLOWED = {'core': set(), 'platform': {'core'}, 'app': {'core', 'platform'},
           'desktop': {'app', 'platform'}}


def dependencies(table):
    """包括普通、build、dev 和特定 target 的依赖声明；识别改了名的包。"""
    for key, value in table.items():
        if key in ('dependencies', 'dev-dependencies', 'build-dependencies'):
            for alias, spec in value.items():
                yield alias, spec.get('package', alias) if isinstance(spec, dict) else alias, spec
        elif isinstance(value, dict):
            yield from dependencies(value)


def check(root=ROOT):
    errors = []
    for owner, relative in MANIFESTS.items():
        manifest = root / relative
        data = tomllib.loads(manifest.read_text())
        for alias, package, spec in dependencies(data):
            target = OWNERS.get(package)
            if target and target not in ALLOWED[owner]:
                errors.append(f'{relative}: forbidden {owner} -> {target} ({alias})')
            if owner in {'core', 'platform'} and (package == 'tiny_http' or package.startswith('tauri')):
                errors.append(f'{relative}: transport/UI dependency {package}')
            if isinstance(spec, dict) and 'path' in spec:
                dest = (manifest.parent / spec['path']).resolve()
                expected = next((root / Path(p).parent for o, p in MANIFESTS.items() if o == target), None)
                if expected is None or dest != expected.resolve():
                    errors.append(f'{relative}: unregistered path dependency {alias}')
            if isinstance(spec, dict) and spec.get('workspace'):
                errors.append(f'{relative}: workspace dependency is outside the explicit manifest boundary')
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    errors = check(args.root)
    for error in errors:
        print(error)
    if not errors:
        print('crate dependency direction valid: desktop -> app/platform -> core')
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(main())
