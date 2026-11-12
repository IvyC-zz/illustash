"""#364：真实扫描/识别导入入口的跨阶段回归，只创建合成图库和模型指纹。"""
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
from types import SimpleNamespace
import unittest

from tests.folder_fixture import image
from tests.synthetic_source import synthetic_reference
from tests import test_native_lifecycle as lifecycle, test_native_recognition as recognition
from tests.test_read_http import free_port, wait_until


class RecognitionRecoveryTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    incoming = recognition.NativeRecognitionTest.incoming
    manifest = recognition.NativeRecognitionTest.manifest

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-recognition-recovery-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.resources = self.root / 'resources'
        self.shared = self.root / 'shared'
        self.shared.mkdir()
        reference = synthetic_reference(self.shared)
        self.pics = self.root / 'images'
        image(self.pics / 'a.png', 'red')
        image(self.pics / 'b.png', 'blue')
        inherited = {key: value for key, value in os.environ.items()
                     if not key.startswith('ILLUSTASH_') or key.startswith('ILLUSTASH_TEST_')}
        self.env = {**inherited, 'HOME': str(self.root / 'home'), 'USERPROFILE': str(self.root / 'home'),
                    'ILLUSTASH_APP_SUPPORT': str(self.root / 'support'), 'ILLUSTASH_SHARED': str(self.shared)}
        for model, path in (('WD', 'models/wd_swinv2_v3_emb.onnx'), ('CCIP', 'models/ccip/model_feat.onnx')):
            target = self.resources / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(f'synthetic {model}'.encode())
        (self.resources / 'tags').mkdir()
        tags = self.resources / 'tags/tags_v3.csv'
        tags.write_text('name,category\ngeneral,9\nblue_hair,0\nalice,4\n')
        self.specs = {
            'wd': dict(version='swinv2_448', dimension=1024, sha256=hashlib.sha256(b'synthetic WD').hexdigest(),
                       tags_sha256=hashlib.sha256(tags.read_bytes()).hexdigest()),
            'ccip': dict(version='ccip-model_feat', dimension=768, sha256=hashlib.sha256(b'synthetic CCIP').hexdigest())}
        path = self.root / 'test.pilib'
        self.run_cli('library-create', '--dest', str(path), '--folder', str(self.pics), '--reference', str(reference))
        self.lib = SimpleNamespace(path=path, source=path / 'source.sqlite', db=path / 'library.sqlite',
                                   config=json.loads((path / 'library.json').read_text()))
        self.scan()
        self.ids = dict(self.rows(self.lib.source, 'SELECT filename,uuid FROM assets'))
        self.package = self.incoming(tuple(self.ids.values()))
        self.port = free_port()
        self.origin = f'http://127.0.0.1:{self.port}'
        self.process, self.log = self.start(path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def run_cli(self, command, *args, code=0):
        result = subprocess.run([str(self.binary), command, *args], env=self.env, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, code, result.stderr.decode(errors='replace'))
        return json.loads(result.stdout)

    def scan(self):
        return self.run_cli('library-scan', '--library', str(self.lib.path))

    def import_package(self, code=0):
        return self.run_cli('recognition-import', '--library', str(self.lib.path), '--root', str(self.resources),
                            '--package', str(self.package), code=code)

    def resume(self):
        return self.run_cli('recognition-import', '--library', str(self.lib.path), '--root', str(self.resources), '--resume')

    @staticmethod
    def rows(db, sql, args=()):
        with sqlite3.connect(db) as connection:
            return connection.execute(sql, args).fetchall()

    def tables(self, db, names):
        return {table: self.rows(db, f'SELECT * FROM {table} ORDER BY rowid') for table in names}

    def test_mtime_preserves_results_and_duplicate_is_a_noop(self):
        self.assertFalse(self.import_package()['duplicate'])
        source = self.tables(self.lib.source, ('labels', 'embeddings', 'recognition_assets', 'recognition_imports'))
        app = self.tables(self.lib.db, ('general_hits', 'asset_rating', 'actions'))
        path = self.pics / 'a.png'
        before = path.stat()
        os.utime(path, ns=(before.st_atime_ns, before.st_mtime_ns + 7_000_000_000))
        self.scan()
        self.assertEqual(self.tables(self.lib.source, source), source)
        self.assertEqual(self.tables(self.lib.db, app), app)
        self.assertTrue(self.import_package()['duplicate'])
        self.assertEqual(self.tables(self.lib.source, source), source)
        self.assertEqual(self.tables(self.lib.db, app), app)

    def test_partial_batch_roundtrip_retry_restart_and_human_history(self):
        original = self.import_package()
        asset = self.ids['a.png']
        self.assertEqual(self.request('/api/tag', {'ids': [asset], 'tag': '合成人工'})['status'], 200)
        human = self.tables(self.lib.db, ('asset_tags', 'term_verdicts', 'actions'))
        other = self.rows(self.lib.source, 'SELECT * FROM recognition_assets WHERE asset=?', (self.ids['b.png'],))
        # 用公开同步入口撤销 A 的旧结果，未变化的 B 保留。
        image(self.pics / 'a.png', 'yellow', (51, 31))
        self.assertEqual(self.request('/api/sync', {})['status'], 200)
        wait_until(lambda: json.loads(self.request('/api/sync')['body'])['state']['status']
                   not in ('scanning', 'importing'), timeout=30)
        self.assertEqual(self.rows(self.lib.source, 'SELECT * FROM recognition_assets WHERE asset=?', (asset,)), [])
        self.assertEqual(self.rows(self.lib.db, 'SELECT * FROM asset_rating WHERE uuid=?', (asset,)), [])
        self.stop(self.process)
        image(self.pics / 'a.png', 'red')
        self.scan()
        # 在 source 已提交、应用投影失败的位置中断；新进程 --resume 必须恢复新回执。
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic projection failure'); END")
        result = self.import_package(code=2)
        self.assertFalse(result['duplicate'])
        self.assertEqual(result['digest'], original['digest'])
        receipts = self.rows(self.lib.source, 'SELECT digest,assets,status FROM recognition_imports ORDER BY rowid')
        self.assertEqual(len(receipts), 2)
        self.assertEqual(receipts[0][2], 'done')
        self.assertEqual((json.loads(receipts[1][1]), receipts[1][2]), ([asset], 'pending'))
        self.assertTrue(receipts[1][0].startswith(original['digest'] + ':redelivery:'))
        self.assertEqual(self.rows(self.lib.source, 'SELECT * FROM recognition_assets WHERE asset=?', (self.ids['b.png'],)), other)
        with sqlite3.connect(self.lib.db) as c:
            c.execute('DROP TRIGGER fail')
        self.assertEqual(self.resume()['status'], 'done')
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (asset,)), [('general',)])
        self.assertEqual(self.tables(self.lib.db, human), human)
        app = self.tables(self.lib.db, ('general_hits', 'asset_rating', 'term_verdicts', 'actions', 'backend_effects'))
        self.assertTrue(self.import_package()['duplicate'])
        self.assertEqual(self.tables(self.lib.db, app), app)
        self.assertEqual(len(self.rows(self.lib.db, "SELECT key FROM meta WHERE key LIKE 'recognition_projection:%'")), 2)

