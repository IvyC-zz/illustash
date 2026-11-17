"""从 #557 冻结的四个成员复封产品认领的 CCIP 长尾参考包。"""

import argparse
import hashlib
import json
from pathlib import Path
from tempfile import TemporaryDirectory
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo


ARCHIVE = 'A-longtail-p75.zip'
PACKAGE_SHA256 = 'd36eeab8768e30cc36e2ba5b2788f00a5f608ecad0df264a2830a44c14407fb9'
PACKAGE_BYTES = 1_273_692
MANIFEST = 'A-provenance.json'
MEMBERS = (MANIFEST, 'A-metadata.json', 'A-references.npz', 'A-anchors.f32')


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def build(source: Path, output: Path) -> None:
    data = {name: (source / name).read_bytes() for name in MEMBERS}
    manifest = json.loads(data[MANIFEST])
    hashes = manifest['files']
    if set(hashes) != set(MEMBERS[1:]) or any(digest(data[name]) != sha for name, sha in hashes.items()):
        raise ValueError('参考或锚点输入与冻结清单不一致')
    target = output / ARCHIVE
    if target.exists():
        raise FileExistsError(target)
    output.mkdir(parents=True, exist_ok=True)
    with TemporaryDirectory(prefix='ccip-repack-', dir=output) as scratch:
        temporary = Path(scratch) / ARCHIVE
        with ZipFile(temporary, 'w', ZIP_DEFLATED, compresslevel=6) as archive:
            for name in MEMBERS:
                entry = ZipInfo(name, (2026, 11, 16, 0, 0, 0))
                archive.writestr(entry, data[name], compress_type=ZIP_DEFLATED, compresslevel=6)
        if temporary.stat().st_size != PACKAGE_BYTES or digest(temporary.read_bytes()) != PACKAGE_SHA256:
            raise ValueError('复封产物与产品支持的冻结 ZIP 不同')
        temporary.rename(target)
    print(f'{ARCHIVE} {PACKAGE_BYTES} B {PACKAGE_SHA256}')


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path, help='冻结成员所在目录')
    parser.add_argument('output', type=Path, help='产物目录')
    args = parser.parse_args()
    build(args.source, args.output)


if __name__ == '__main__':
    main()
