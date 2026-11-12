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

    def run_cli(self, command, *args, code=0, stdin=None):
        result = subprocess.run([str(self.binary), command, *args], env=self.env, capture_output=True, timeout=30,
                                input=stdin.encode() if stdin is not None else None)
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
        return {table: sorted(self.rows(db, f'SELECT * FROM {table}'), key=repr) for table in names}

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


    def test_pending_batch_skips_invalidated_asset_then_redelivers_it(self):
        self.stop(self.process)
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END")
        self.assertEqual(self.import_package(code=2)['status'], 'pending')
        image(self.pics / 'a.png', 'yellow', (51, 31))
        # 源扫描已提交而应用尚未恢复：由正式 folder-scan 建立这个崩溃边界。
        self.run_cli('folder-scan', '--db', str(self.lib.source), stdin=json.dumps(self.lib.config['sources'][0]))
        with sqlite3.connect(self.lib.db) as c:
            c.execute('DROP TRIGGER fail')
        self.scan()
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (self.ids['a.png'],)), [])
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (self.ids['b.png'],)), [('general',)])
        self.assertEqual(self.rows(self.lib.source, "SELECT status FROM recognition_imports"), [('done',)])
        image(self.pics / 'a.png', 'red')
        self.scan()
        self.assertFalse(self.import_package()['duplicate'])
        self.assertTrue(self.import_package()['duplicate'])
        self.assertEqual(len(self.rows(self.lib.db, 'SELECT * FROM asset_rating')), 2)

    def test_stale_pending_sync_rehashes_identical_size_and_mtime(self):
        self.stop(self.process)
        path = self.pics / 'a.png'
        image(path, 'blue')
        self.scan()
        self.manifest(self.package, tuple(self.ids.values()))
        old = path.stat()
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END")
        self.assertEqual(self.import_package(code=2)['status'], 'pending')
        image(path, 'red')
        path.write_bytes(path.read_bytes().ljust(old.st_size, b'\0'))
        self.assertEqual(path.stat().st_size, old.st_size)
        os.utime(path, ns=(old.st_atime_ns, old.st_mtime_ns))
        with sqlite3.connect(self.lib.db) as c:
            c.execute('DROP TRIGGER fail')
        # 公开恢复严格拒绝；同步会撤回未完成交付、强制重算内容元数据，然后允许生成新包。
        denied = self.run_cli('recognition-import', '--library', str(self.lib.path), '--root', str(self.resources), '--resume', code=2)
        self.assertEqual(denied['status'], 'pending')
        self.scan()
        asset = self.ids['a.png']
        self.assertEqual(self.rows(self.lib.source, 'SELECT stable_hash FROM assets WHERE uuid=?', (asset,)),
                         [(hashlib.md5(path.read_bytes()).hexdigest(),)])
        self.assertEqual(self.rows(self.lib.source, 'SELECT status FROM recognition_imports'), [('superseded',)])
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating'), [])
        self.manifest(self.package, tuple(self.ids.values()))
        self.assertEqual(self.import_package()['status'], 'done')
        self.assertEqual(len(self.rows(self.lib.db, 'SELECT * FROM asset_rating')), 2)

    def test_legacy_pending_sync_retires_machine_results_preserving_humans(self):
        asset = self.ids['a.png']
        self.assertEqual(self.request('/api/tag', {'ids': [asset], 'tag': '合成人工'})['status'], 200)
        human = self.tables(self.lib.db, ('asset_tags', 'term_verdicts', 'actions'))
        self.stop(self.process)
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON assets BEGIN SELECT RAISE(ABORT,'synthetic app failure'); END")
        self.assertEqual(self.import_package(code=2)['status'], 'pending')
        with sqlite3.connect(self.lib.source) as c:
            manifest = json.loads(c.execute('SELECT manifest FROM recognition_imports').fetchone()[0])
            manifest['format'] = 'illustash-recognition/1'
            manifest.pop('inputs')
            c.execute('UPDATE recognition_imports SET manifest=?', (json.dumps(manifest),))
        with sqlite3.connect(self.lib.db) as c:
            c.execute('DROP TRIGGER fail')
        denied = self.run_cli('recognition-import', '--library', str(self.lib.path), '--root', str(self.resources), '--resume', code=2)
        self.assertEqual(denied['status'], 'pending')
        self.scan()
        self.assertEqual(self.rows(self.lib.source, 'SELECT * FROM recognition_assets'), [])
        self.assertEqual(self.rows(self.lib.db, 'SELECT * FROM asset_rating'), [])
        self.assertEqual(self.tables(self.lib.db, human), human)
        self.assertEqual(self.import_package()['status'], 'done')
        self.assertEqual(self.tables(self.lib.db, human), human)

    def test_offline_pending_does_not_block_other_receipts_or_leak_through_refresh(self):
        asset = self.ids['a.png']
        self.assertEqual(self.request('/api/tag', {'ids': [asset], 'tag': '离线人工保留'})['status'], 200)
        human = self.tables(self.lib.db, ('asset_tags', 'term_verdicts', 'actions'))
        self.stop(self.process)
        second = self.root / 'online'
        image(second / 'c.png', 'green')
        self.lib.config['sources'].append(dict(self.lib.config['sources'][0], id='online', path=str(second)))
        (self.lib.path / 'library.json').write_text(json.dumps(self.lib.config))
        self.scan()
        online = self.rows(self.lib.source, "SELECT uuid FROM assets WHERE filename='c.png'")[0][0]
        tags = self.resources / 'tags/tags_v3.csv'
        tags.write_text(tags.read_text() + 'realistic,0\n')
        self.specs['wd']['tags_sha256'] = hashlib.sha256(tags.read_bytes()).hexdigest()
        offline_package = self.incoming((asset,), name='offline-result')
        labels = offline_package / 'labels.jsonl'
        labels.write_text(labels.read_text() + '\n' + json.dumps(dict(asset=asset, dim='general', value='realistic', confidence=.9)))
        self.manifest(offline_package, (asset,))
        online_package = self.incoming((online,), name='online-result')
        # 源提交后、首个 WD 证据提交前中断；离线回执不能借另一张图的刷新发布任何机器证据。
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail BEFORE INSERT ON term_packages BEGIN SELECT RAISE(ABORT,'synthetic evidence failure'); END")
        for package in (offline_package, online_package):
            self.package = package
            self.assertEqual(self.import_package(code=2)['status'], 'pending')
        with sqlite3.connect(self.lib.db) as c:
            c.execute('DROP TRIGGER fail')
        disconnected = self.root / 'disconnected'
        self.pics.rename(disconnected)
        result = self.run_cli('recognition-import', '--library', str(self.lib.path), '--root', str(self.resources), '--resume', code=2)
        self.assertEqual(result['unavailable_inputs'], [asset])
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (online,)), [('general',)])
        self.assertEqual(self.rows(self.lib.db, "SELECT * FROM auto_rules WHERE uuid=? AND reason='photo'", (asset,)), [])
        image(second / 'd.png', 'yellow')
        self.scan()
        self.assertEqual(self.rows(self.lib.source, "SELECT status FROM recognition_imports ORDER BY rowid"), [('pending',), ('done',)])
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (asset,)), [])
        self.assertEqual(self.rows(self.lib.db, "SELECT * FROM auto_rules WHERE uuid=? AND reason='photo'", (asset,)), [])
        self.assertEqual(self.rows(self.lib.db, "SELECT count(*) FROM term_evidence e JOIN term_assets a ON a.id=e.asset_id WHERE a.namespace='library' AND a.key=? AND e.source='wd'", (asset,)), [(0,)])
        self.assertEqual(self.tables(self.lib.db, human), human)
        disconnected.rename(self.pics)
        self.assertEqual(self.resume()['status'], 'done')
        self.assertEqual(self.rows(self.lib.db, 'SELECT rating FROM asset_rating WHERE uuid=?', (asset,)), [('general',)])
        self.assertEqual(self.rows(self.lib.db, "SELECT reason FROM auto_rules WHERE uuid=? AND reason='photo'", (asset,)), [('photo',)])
        self.assertEqual(self.tables(self.lib.db, human), human)
