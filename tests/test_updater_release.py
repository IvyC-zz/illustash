"""tools/updater_release.py：应用内更新的 latest.json 和 macOS 更新包（#279）。

签名夹具（tests/fixtures/updater/signatures.json）由一把一次性的密钥对生成，私钥生成后即删；它和发布用的密钥无关。
"""
import datetime
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest

from tools import updater_release as release

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = json.loads((ROOT / 'tests/fixtures/updater/signatures.json').read_text(encoding='utf-8'))
MAC = 'Illustash_0.2.0_aarch64.app.tar.gz'
WINDOWS = 'Illustash_0.2.0_x64-setup.exe'


class ManifestTests(unittest.TestCase):
    def make(self, **packages):
        now = datetime.datetime(2026, 11, 14, 4, 5, 6, 789, tzinfo=datetime.timezone.utc)
        return release.manifest(version='0.2.0', packages=packages, pubkey=FIXTURE['pubkey'], notes='n', now=now)

    def test_points_each_platform_at_the_release_of_its_version(self):
        value = self.make(mac=(MAC, FIXTURE['mac']), windows=(WINDOWS, FIXTURE['windows']))
        base = 'https://github.com/shio-vinyl/illustash/releases/download/v0.2.0/'
        self.assertEqual(value, {
            'version': '0.2.0',
            'notes': 'n',
            'pub_date': '2026-11-14T04:05:06Z',
            'platforms': {
                'darwin-aarch64': {'signature': FIXTURE['mac'], 'url': base + MAC},
                'windows-x86_64': {'signature': FIXTURE['windows'], 'url': base + WINDOWS},
            },
        })

    def test_refuses_signatures_that_the_app_would_refuse(self):
        cases = {
            'old_version': '绑定的版本是 0.1.9',
            'no_version': '绑定的版本是 （没有）',
            'other_key': '公钥对应的私钥',
        }
        for key, said in cases.items():
            with self.subTest(key), self.assertRaises(release.ReleaseError) as caught:
                self.make(windows=(WINDOWS, FIXTURE[key]))
            self.assertIn(said, str(caught.exception))
        # 签名是给另一个文件名的：改名之后要重新签。
        with self.assertRaises(release.ReleaseError) as caught:
            self.make(windows=('Illustash_0.2.0_x64-setup (1).exe', FIXTURE['windows']))
        self.assertIn('先改好文件名再签名', str(caught.exception))
        for bad in ['', 'not base64!', 'aGVsbG8=']:
            with self.subTest(bad), self.assertRaises(release.ReleaseError):
                self.make(windows=(WINDOWS, bad))
        with self.assertRaises(release.ReleaseError) as caught:
            release.manifest(version='0.2.0', packages={'mac': (MAC, FIXTURE['mac'])}, pubkey=' ')
        self.assertIn('还没有更新公钥', str(caught.exception))

    def test_configuration_is_read_from_the_repository(self):
        self.assertEqual(release.application_version(), json.loads(
            (ROOT / 'native/terms/application.json').read_text(encoding='utf-8'))['version'])
        config = json.loads((ROOT / 'desktop/tauri.conf.json').read_text(encoding='utf-8'))['plugins']['updater']
        self.assertEqual(release.configured_pubkey(), config['pubkey'])
        # 签名必须绑定版本号：被改过的 latest.json 不能把旧版本的包配上新版本号。
        self.assertIs(config['requireSignedVersion'], True)
        if config['pubkey']:
            release.pubkey_id(config['pubkey'])


class PackMacTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix='illustash-updater-'))
        self.addCleanup(shutil.rmtree, self.root)

    def test_archive_starts_with_the_bundle_and_keeps_links_and_modes(self):
        app = self.root / 'Illustash.app'
        (app / 'Contents/MacOS').mkdir(parents=True)
        (app / 'Contents/Info.plist').write_text('<plist/>')
        exe = app / 'Contents/MacOS/illustash-desktop'
        exe.write_text('#!/bin/sh\n')
        exe.chmod(0o755)
        (app / 'Contents/Frameworks').mkdir()
        (app / 'Contents/Frameworks/current').symlink_to('../MacOS')
        out = self.root / MAC
        release.pack_mac(app, out)
        with tarfile.open(out) as archive:
            members = {m.name: m for m in archive.getmembers()}
        self.assertTrue(all(name == 'Illustash.app' or name.startswith('Illustash.app/') for name in members))
        self.assertFalse(any(Path(name).name.startswith('._') for name in members))
        self.assertEqual(members['Illustash.app/Contents/MacOS/illustash-desktop'].mode & 0o777, 0o755)
        self.assertEqual(members['Illustash.app/Contents/Frameworks/current'].linkname, '../MacOS')
        self.assertEqual({m.uid for m in members.values()}, {0})

    def test_refuses_what_is_not_a_bundle(self):
        (self.root / 'x.app').mkdir()
        with self.assertRaises(release.ReleaseError):
            release.pack_mac(self.root / 'x.app', self.root / MAC)
        with self.assertRaises(release.ReleaseError):
            release.pack_mac(self.root / 'x.app', self.root / 'x.tar.gz')


if __name__ == '__main__':
    unittest.main()
