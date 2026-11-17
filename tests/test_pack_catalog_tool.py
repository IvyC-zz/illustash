"""tools/pack_catalog.py（#602）：条目从包文件读字段、组目录时序号加一、发布时拒绝应用会跳过的条目、核对签名、打离线包。

夹具在 tests/fixtures/pack_catalog/（全部合成）。签名的完整核对要装 cryptography；没装时只核对密钥编号，测试照样跑。
"""
import json
from pathlib import Path
import tempfile
import unittest
import zipfile

from tools import pack_catalog

FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'pack_catalog'


class PackCatalogToolTest(unittest.TestCase):
    def test_entry_reads_format_heads_tiers_and_base_from_the_package(self):
        wd = pack_catalog.entry(kind='wd-extension', revision=3, path=FIXTURES / 'wd-extension-v3.bin', min_app='0.3.0',
                                license_text='Apache-2.0')
        self.assertEqual((wd['version'], wd['format'], wd['heads'], wd['candidate_only']),
                         ('wd-extension-v3', 'illustash-wd-extension/2', 3, 1))
        self.assertEqual(wd['file']['url'], pack_catalog.RELEASE_PREFIX + 'wd-extension-v3/wd-extension-v3.bin')
        self.assertEqual(set(wd['base']), {'wd_model_sha256', 'wd_tags_sha256'})
        ccip = pack_catalog.entry(kind='ccip-longtail', revision=2, path=FIXTURES / 'ccip-longtail-v2.zip', min_app='0.3.0',
                                  license_text='MIT', name='A-longtail-p75.zip')
        self.assertEqual((ccip['heads'], ccip['file']['name']), (2, 'A-longtail-p75.zip'))
        self.assertTrue(ccip['file']['url'].endswith('/ccip-longtail-v2/A-longtail-p75.zip'))
        with self.assertRaises(pack_catalog.CatalogError):
            pack_catalog.entry(kind='wd-extension', revision=2, path=FIXTURES / 'wd-extension-v2.bin', min_app='0.3',
                               license_text='Apache-2.0')
        with self.assertRaises(pack_catalog.CatalogError):
            pack_catalog.entry(kind='wd-extension', revision=2, path=FIXTURES / 'wd-extension-v2.bin', min_app='0.3.0',
                               license_text='Apache-2.0', note_zh='只有中文')

    def test_build_merges_by_version_and_bumps_the_serial(self):
        v2 = pack_catalog.entry(kind='wd-extension', revision=2, path=FIXTURES / 'wd-extension-v2.bin', min_app='0.3.0',
                                license_text='Apache-2.0')
        first = pack_catalog.build([v2])
        self.assertEqual((first['serial'], [p['version'] for p in first['packs']]), (1, ['wd-extension-v2']))
        v3 = pack_catalog.entry(kind='wd-extension', revision=3, path=FIXTURES / 'wd-extension-v3.bin', min_app='0.3.0',
                                license_text='Apache-2.0')
        second = pack_catalog.build([v3, {**v2, 'license': 'Apache-2.0 AND MIT'}], first)
        self.assertEqual(second['serial'], 2)
        self.assertEqual([(p['version'], p['license']) for p in second['packs']],
                         [('wd-extension-v2', 'Apache-2.0 AND MIT'), ('wd-extension-v3', 'Apache-2.0')])
        # 同一个修订号或文件不能出现两次。
        with self.assertRaises(pack_catalog.CatalogError):
            pack_catalog.build([{**v3, 'version': 'wd-extension-v3b'}], second)
        # 发布时拒绝应用会跳过的条目；测试夹具用 strict=False 才能写进去。
        future = {**v2, 'version': 'future-v1', 'kind': 'future-pack', 'revision': 9, 'format': 'x/1',
                  'file': {**v2['file'], 'sha256': '0' * 64, 'url': pack_catalog.RELEASE_PREFIX + 'future-v1/' + v2['file']['name']}}
        with self.assertRaises(pack_catalog.CatalogError):
            pack_catalog.build([future])
        self.assertEqual(len(pack_catalog.build([future], strict=False)['packs']), 1)

    def test_verify_checks_the_key_and_the_bytes(self):
        data = (FIXTURES / 'packs.json').read_bytes()
        a = (FIXTURES / 'key-a.pub').read_text()
        self.assertIn(pack_catalog.verify(data, (FIXTURES / 'packs.json.sig').read_text(), a), (True, None))
        self.assertIn(pack_catalog.verify(data, (FIXTURES / 'packs.json.tauri.sig').read_text(),
                                          (FIXTURES / 'tauri-key.pub').read_text()), (True, None))
        with self.assertRaisesRegex(pack_catalog.CatalogError, '不是用'):
            pack_catalog.verify(data, (FIXTURES / 'packs.json.sig-b').read_text(), a)
        try:
            import cryptography  # noqa: F401
        except ImportError:
            return
        with self.assertRaisesRegex(pack_catalog.CatalogError, '对不上'):
            pack_catalog.verify(data + b'\n', (FIXTURES / 'packs.json.sig').read_text(), a)

    def test_offline_bundle_has_catalog_signature_package_and_extras(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / 'wd-extension-v2-offline.zip'
            notice = Path(temp) / 'NOTICE.txt'
            notice.write_text('synthetic notice\n')
            pack_catalog.offline(FIXTURES / 'packs.json', FIXTURES / 'wd-extension-v2.bin', output, extra=[notice])
            with zipfile.ZipFile(output) as archive:
                self.assertEqual(archive.namelist(), ['packs.json', 'packs.json.sig', 'wd-extension-v2.bin', 'NOTICE.txt'])
                self.assertEqual(archive.read('packs.json'), (FIXTURES / 'packs.json').read_bytes())
            with self.assertRaises(pack_catalog.CatalogError):
                pack_catalog.offline(FIXTURES / 'packs.json', notice, output)

    def test_pubkey_file_placeholder_and_write(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / 'pack-key.pub'
            path.write_text((Path(pack_catalog.PUBKEY_FILE)).read_text(encoding='utf-8'), encoding='utf-8')
            self.assertIsNone(pack_catalog.configured_pubkey(path))
            key = (FIXTURES / 'key-a.pub').read_text().strip()
            pack_catalog.write_pubkey(key, path)
            self.assertEqual(pack_catalog.configured_pubkey(path), key)
            self.assertNotIn('占位', path.read_text(encoding='utf-8'))
            with self.assertRaises(pack_catalog.CatalogError):
                pack_catalog.write_pubkey('not a key', path)


if __name__ == '__main__':
    unittest.main()
