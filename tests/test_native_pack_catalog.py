"""数据包的签名目录（#602）走原生 HTTP：跟着检查更新读目录、下载新版本、换包、换回、撤回，离线包导入。

GitHub 的版本列表和 Release 下载都由本机替身服务答复（ILLUSTASH_UPDATE_BASE），目录用测试密钥签名
（ILLUSTASH_TEST_PACKS_PUBKEY，只在指向本机测试服务时才认）。包、图片和模型身份都是合成的，夹具见
tests/fixtures/pack_catalog/。
"""
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hashlib
import json
from pathlib import Path
import sqlite3
import struct
import subprocess
import tempfile
from threading import Thread
import unittest
import zipfile

from tests.folder_fixture import image
from tests.fixed_expectations import Library
from tests import test_native_lifecycle as lifecycle
from tests.test_read_http import free_port, wait_until
from tools.dev_preview import isolated_env
from tools.wait_limit import WAIT

FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'pack_catalog'
DOWNLOAD = '/shio-vinyl/illustash/releases/download/'
WD_MODEL = 'ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff'
WD_TAGS = '298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217'
RELEASES = '/repositories/1407079068/releases?per_page=30'


def sha(data):
    return hashlib.sha256(data).hexdigest()


@contextmanager
def github(files):
    """`files` 是「路径 → 字节」，随时可以改；`seen` 记下每个请求的路径。"""
    seen = []

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self):
            seen.append(self.path)
            body = b'[]' if self.path == RELEASES else files.get(self.path)
            self.send_response(200 if body is not None else 404)
            body = body if body is not None else b'{"message": "Not Found"}'
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_):
            pass

    with ThreadingHTTPServer(('127.0.0.1', 0), Handler) as server:
        thread = Thread(target=server.serve_forever)
        thread.start()
        try:
            yield f'http://127.0.0.1:{server.server_port}', seen
        finally:
            server.shutdown()
            thread.join()


def builtin_v1(directory, origin):
    """内置包的走查替身（ILLUSTASH_TEST_WD_EXTENSION）：一个头看第 0 维，和目录里的新版本同一个角色标签。"""
    header = {'format': 'illustash-wd-extension/1', 'wd_model_sha256': WD_MODEL, 'wd_tags_sha256': WD_TAGS,
              'threshold': 0.8008725047111511, 'labels': ['packone_(synthetic)']}
    weights = struct.pack('<1024f', 2., *([0.] * 1023))
    contents = json.dumps(header).encode() + b'\n' + weights + struct.pack('<f', 0.)
    spec = {'revision': 1, 'format': 'illustash-wd-extension/1', 'heads': 1, 'license': 'Apache-2.0',
            'base_sha256': WD_MODEL,
            'file': {'path': 'wd-extension-v1.bin', 'size': len(contents), 'sha256': sha(contents),
                     'url': f'{origin}/builtin/wd-extension-v1.bin'}}
    directory.mkdir(parents=True, exist_ok=True)
    manifest = directory / 'wd-extension.json'
    manifest.write_text(json.dumps(spec), encoding='utf-8')
    return manifest, contents


class NativePackCatalogTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-pack-catalog-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        checkout = self.resources
        self.resources = self.root / 'resources'
        (self.resources / 'tags').mkdir(parents=True)
        (self.resources / 'tags' / 'tags_v3.csv').write_bytes((checkout / 'tags' / 'tags_v3.csv').read_bytes())
        self.env = {**isolated_env(self.root), 'USERPROFILE': str(self.root / 'home'),
                    'ILLUSTASH_TEST_PACKS_PUBKEY': (FIXTURES / 'key-a.pub').read_text().strip()}
        folder = self.root / 'images'
        folder.mkdir()
        for name, color in (('a.png', 'blue'), ('b.png', 'orange'), ('c.png', 'green')):
            image(folder / name, color)
        path = self.root / 'first.pilib'
        self.cli('library-create', '--dest', str(path), '--folder', str(folder))
        self.lib = Library(path)
        self.cli('library-scan', '--library', str(self.lib.path))
        # 第 i 张图（按 uuid 排）的向量只有第 i 维是 1：合成包的第 i 个头只认它。
        with sqlite3.connect(self.lib.source) as source:
            source.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
            self.ids = [u for u, in source.execute('SELECT uuid FROM assets WHERE in_scope=1 ORDER BY uuid')]
            for i, uuid in enumerate(self.ids):
                vector = [0.] * 1024
                vector[i] = 1.
                source.execute("INSERT INTO embeddings VALUES (?,'wd',?)", (uuid, struct.pack('<1024f', *vector)))
        self.assertEqual(len(self.ids), 3)
        self.port = free_port()
        self.origin = f'http://127.0.0.1:{self.port}'

    def cli(self, *arguments):
        result = subprocess.run([str(self.binary), *arguments], env=self.env, capture_output=True, timeout=WAIT)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))

    def restart(self):
        if hasattr(self, 'process'):
            self.stop(self.process)
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def get(self, route):
        response = self.request(route)
        self.assertEqual(response['status'], 200, response['body'])
        return json.loads(response['body'])

    def post(self, route, body, status=200):
        response = self.request(route, body)
        self.assertEqual(response['status'], status, response['body'])
        return json.loads(response['body'])

    def finish(self, route, kind, key='task'):
        observed = None

        def ended():
            nonlocal observed
            observed = self.get(route)
            job = observed[key]
            return job and job['kind'] == kind and job['status'] != 'running'

        wait_until(ended)
        return observed

    def members(self):
        with sqlite3.connect(self.lib.db) as app:
            assigned = [u for u, in app.execute("SELECT uuid FROM memberships WHERE sources='WD_EXT' ORDER BY uuid")]
            candidates = [u for u, in app.execute("SELECT DISTINCT uuid FROM candidates WHERE source='WD_EXT' ORDER BY uuid")]
        return assigned, candidates

    def serve(self, origin):
        files = {DOWNLOAD + 'packs/packs.json': (FIXTURES / 'packs.json').read_bytes(),
                 DOWNLOAD + 'packs/packs.json.sig': (FIXTURES / 'packs.json.sig').read_bytes()}
        for version, name in (('wd-extension-v2', 'wd-extension-v2.bin'), ('wd-extension-v3', 'wd-extension-v3.bin'),
                              ('ccip-longtail-v2', 'ccip-longtail-v2.zip')):
            files[f'{DOWNLOAD}{version}/{name}'] = (FIXTURES / name).read_bytes()
        manifest, contents = builtin_v1(self.root / 'builtin', origin)
        files['/builtin/wd-extension-v1.bin'] = contents
        self.env['ILLUSTASH_TEST_WD_EXTENSION'] = str(manifest)
        self.env['ILLUSTASH_UPDATE_BASE'] = origin + '/'
        return files

    def test_catalog_follows_update_check_then_download_switch_back_and_withdraw(self):
        files = {}
        with github(files) as (origin, seen):
            files.update(self.serve(origin))
            self.restart()
            packs = self.get('/api/packs')
            self.assertEqual(packs['catalog']['configured'], True)
            self.assertEqual(packs['catalog']['serial'], None)
            self.assertEqual((packs['wd']['current']['version'], packs['wd']['offer']), ('wd-extension-v1', None))
            # 没允许自动检查时，页面的自动检查什么都不读，也不读目录。
            self.post('/api/updates', {'action': 'check', 'auto': True})
            self.assertEqual(seen, [])
            self.assertIsNone(self.get('/api/packs')['catalog']['serial'])

            # 先照旧下载并启用内置包。
            self.post('/api/wd-extension/package', {'action': 'download'})
            self.assertEqual(self.finish('/api/wd-extension', 'download', 'download')['package']['status'], 'ready')
            self.post('/api/wd-extension/enable', {})
            enabled = self.finish('/api/wd-extension', 'enable')
            self.assertEqual(enabled['task']['status'], 'done', enabled)
            self.assertEqual(self.members(), (self.ids[:1], self.ids[:1]))
            before = self.request('/api/wd-extension')['body']
            del seen[:]

            # 手动检查更新时顺带读目录；不下载任何包。
            self.post('/api/updates', {'action': 'check'})
            self.assertEqual(seen, [RELEASES, DOWNLOAD + 'packs/packs.json', DOWNLOAD + 'packs/packs.json.sig'])
            packs = self.get('/api/packs')
            self.assertEqual((packs['catalog']['serial'], packs['catalog']['error']), (1, None))
            self.assertEqual(packs['wd']['current']['version'], 'wd-extension-v1')
            offer = packs['wd']['offer']
            self.assertEqual((offer['version'], offer['heads'], offer['candidate_only'], offer['status']),
                             ('wd-extension-v3', 3, 1, 'missing'))
            self.assertEqual(offer['note'], {'zh-CN': '合成测试包 3', 'en': 'Synthetic test pack 3'})
            # CCIP 长尾：本图库没启用、本机什么都没有，卡片上直接是最新的版本，没有「新版本」。
            self.assertEqual((packs['ccip']['current']['version'], packs['ccip']['offer']), ('ccip-longtail-v2', None))
            # 读到目录以后，用户点之前，原来的接口逐字节不变。
            self.assertEqual(self.request('/api/wd-extension')['body'], before)

            # 目录里没有的版本、格式不对的请求都不认。
            self.post('/api/wd-extension/enable', {'version': 'wd-extension-v4'}, 409)
            self.post('/api/wd-extension/enable', {'version': 3}, 400)
            self.post('/api/wd-extension/withdraw', {'version': 'wd-extension-v3'}, 400)
            self.post('/api/wd-extension/package', {'action': 'download', 'version': 'wd-extension-v9'}, 409)
            self.post('/api/wd-extension/package', {'action': 'delete', 'version': 'wd-extension-v3'}, 400)
            self.post('/api/wd-extension/enable', {'version': 'wd-extension-v3'}, 409)  # 还没下载
            self.assertEqual(self.members(), (self.ids[:1], self.ids[:1]))

            # 下载新版本：进度算在「新版本」一行，卡片上仍是已启用的 v1。
            self.post('/api/wd-extension/package', {'action': 'download', 'version': 'wd-extension-v3'})
            done = self.finish('/api/wd-extension', 'download', 'download')
            self.assertEqual(done['download']['status'], 'done', done)
            self.assertEqual(done['package']['version'], 'wd-extension-v1')
            self.assertEqual(self.get('/api/packs')['wd']['offer']['status'], 'ready')
            self.assertTrue((self.root / 'shared/extensions/wd-extension-v3/wd-extension-v3.bin').is_file())

            # 换成新版本：二级头（第三张图）只出候选，不补归属。
            self.post('/api/wd-extension/enable', {'version': 'wd-extension-v3'})
            switched = self.finish('/api/wd-extension', 'enable')
            self.assertEqual(switched['task']['status'], 'done', switched)
            v3 = (FIXTURES / 'wd-extension-v3.bin').read_bytes()
            self.assertEqual(switched['library']['package_sha256'], sha(v3))
            self.assertEqual((switched['package']['version'], switched['package']['heads']), ('wd-extension-v3', 3))
            self.assertEqual(self.members(), (self.ids[:2], self.ids))
            self.assertEqual((switched['library']['images'], switched['library']['assigned']), (3, 2))
            self.assertIsNone(self.get('/api/packs')['wd']['offer'])
            alternatives = json.loads(self.request('/api/detail?id=' + self.ids[2])['body'])['alternatives']
            self.assertIn(['WD_EXT'], [alt['evidence']['sources'] for alt in alternatives])

            # 重启以后照样记得用的是哪个包。
            self.restart()
            self.assertEqual(self.get('/api/wd-extension')['library']['package_sha256'], sha(v3))

            # 换回 v1，再撤回。
            self.post('/api/wd-extension/enable', {'version': 'wd-extension-v1'})
            back = self.finish('/api/wd-extension', 'enable')
            self.assertEqual(back['task']['status'], 'done', back)
            self.assertEqual(back['package']['version'], 'wd-extension-v1')
            self.assertEqual(self.members(), (self.ids[:1], self.ids[:1]))
            self.assertEqual(self.get('/api/packs')['wd']['offer']['status'], 'ready')
            self.post('/api/wd-extension/withdraw', {})
            withdrawn = self.finish('/api/wd-extension', 'withdraw')
            self.assertEqual(withdrawn['task']['status'], 'done', withdrawn)
            self.assertEqual(self.members(), ([], []))
            # 没启用时卡片上是本机已有的最新版本。
            self.assertEqual(self.get('/api/wd-extension')['package']['version'], 'wd-extension-v3')

            # 目录读不到时只记一句话，已有的缓存照用。
            files.pop(DOWNLOAD + 'packs/packs.json.sig')
            self.post('/api/updates', {'action': 'check'})
            packs = self.get('/api/packs')
            self.assertEqual(packs['catalog']['serial'], 1)
            self.assertIn('HTTP 404', packs['catalog']['error'])
            # 签名对不上的目录整份不用。
            files[DOWNLOAD + 'packs/packs.json'] = (FIXTURES / 'packs-2.json').read_bytes()
            files[DOWNLOAD + 'packs/packs.json.sig'] = (FIXTURES / 'packs.json.sig').read_bytes()
            self.post('/api/updates', {'action': 'check'})
            packs = self.get('/api/packs')
            self.assertEqual(packs['catalog']['serial'], 1)
            self.assertIn('签名', packs['catalog']['error'])

    def test_offline_bundle_carries_the_signed_catalog(self):
        files = {}
        with github(files) as (origin, seen):
            files.update(self.serve(origin))
            self.restart()
            # 签名不对的离线包：目录不采用，包文件也就不认。
            bad = self.root / 'wd-extension-v2-offline.zip'
            with zipfile.ZipFile(bad, 'w') as archive:
                archive.writestr('packs.json', (FIXTURES / 'packs.json').read_bytes())
                archive.writestr('packs.json.sig', (FIXTURES / 'packs.json.sig-b').read_bytes())
                archive.writestr('wd-extension-v2.bin', (FIXTURES / 'wd-extension-v2.bin').read_bytes())
            self.post('/api/wd-extension/package', {'action': 'import', 'paths': [str(bad)]})
            results = self.finish('/api/wd-extension', 'import', 'download')['download']['results']
            self.assertEqual({r['name']: r['result'] for r in results}, {
                'wd-extension-v2-offline.zip/packs.json': 'catalog_invalid',
                'wd-extension-v2-offline.zip/packs.json.sig': 'catalog_invalid',
                'wd-extension-v2-offline.zip/wd-extension-v2.bin': 'unknown'})
            self.assertIsNone(self.get('/api/packs')['catalog']['serial'])

            good = FIXTURES / 'wd-extension-v2-offline.zip'
            self.post('/api/wd-extension/package', {'action': 'import', 'paths': [str(good)]})
            results = self.finish('/api/wd-extension', 'import', 'download')['download']['results']
            self.assertEqual({r['name']: r['result'] for r in results}, {
                'wd-extension-v2-offline.zip/packs.json': 'catalog',
                'wd-extension-v2-offline.zip/packs.json.sig': 'catalog',
                'wd-extension-v2-offline.zip/wd-extension-v2.bin': 'accepted'})
            packs = self.get('/api/packs')
            self.assertEqual(packs['catalog']['serial'], 1)
            # 没启用：卡片上是本机已有的 v2，目录里的 v3 是新版本。
            self.assertEqual((packs['wd']['current']['version'], packs['wd']['offer']['version']),
                             ('wd-extension-v2', 'wd-extension-v3'))
            self.assertEqual(self.get('/api/wd-extension')['package']['status'], 'ready')
            self.post('/api/wd-extension/enable', {})
            enabled = self.finish('/api/wd-extension', 'enable')
            self.assertEqual(enabled['task']['status'], 'done', enabled)
            self.assertEqual(self.members(), (self.ids[:2], self.ids[:2]))

            # CCIP 长尾的离线包：参考包本身是 ZIP，夹在离线包里。
            bundle = FIXTURES / 'ccip-longtail-v2-offline.zip'
            self.request('/api/ccip-longtail/package', {'action': 'import', 'paths': [str(bundle)]})

            def imported():
                d = self.get('/api/ccip-longtail')['download']
                return d and d['kind'] == 'import' and d['status'] != 'running' and d
            wait_until(imported)
            results = {r['name']: r['result'] for r in imported()['results']}
            self.assertEqual(results['ccip-longtail-v2-offline.zip/ccip-longtail-v2.zip'], 'accepted', results)
            self.assertEqual(results['ccip-longtail-v2-offline.zip/packs.json'], 'catalog')
            self.assertTrue((self.root / 'shared/extensions/ccip-longtail-v2/ccip-longtail-v2.zip').is_file())
            ccip = self.get('/api/ccip-longtail')['package']
            self.assertEqual((ccip['version'], ccip['heads'], ccip['status']), ('ccip-longtail-v2', 2, 'ready'))
            # 整个过程不联网。
            self.assertEqual(seen, [])


if __name__ == '__main__':
    unittest.main()
