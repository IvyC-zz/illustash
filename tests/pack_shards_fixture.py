"""分片走查用的两份合成 WD 文件。第二份只替换最后一个头的偏置，仍和旧包等长。"""
from pathlib import Path
import struct
from unittest.mock import patch

from tools import pack_catalog

FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'pack_catalog'


def shard_fixture(directory):
    first = directory / 'wd-extension-shards-first.bin'
    second = directory / 'wd-extension-shards-second.bin'
    data = (FIXTURES / 'wd-extension-v3.bin').read_bytes()
    first.write_bytes(data)
    second.write_bytes(data[:-4] + struct.pack('<f', 0.25))
    ccip = FIXTURES / 'ccip-longtail-v1.zip'
    options = dict(ccip=ccip, min_app='0.3.0', ccip_tag='ccip-longtail-v1', ccip_name='A-longtail-p75.zip')
    with patch.object(pack_catalog, 'WD_SHARD_HEADS', 2):
        entry_a = pack_catalog.entry(version='2.0', revision=10, wd=first, wd_shards=directory / 'first', **options)
        old = pack_catalog.build([entry_a])
        entry_b = pack_catalog.entry(version='2.1', revision=11, wd=second, wd_shards=directory / 'second', previous=old, **options)
        new = pack_catalog.build([entry_b], old)
    return (first, second), (old, new)
