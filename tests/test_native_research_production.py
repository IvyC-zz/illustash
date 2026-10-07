"""#151 installed adapter and existing App lifecycle, on owned synthetic packages only.

No binary builds/discovery: an explicit candidate host supplies its sibling worker (or
ILLUSTASH_RESEARCH_ADAPTER_BIN). macOS direct tests run the installed native adapter with
no Python configuration. Timing tests use the existing #146 fixture launcher/process-tree
checks around responses produced by that actual adapter, never a mocked calculator.
"""
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

import numpy as np
from PIL import Image

from tests import test_native_lifecycle as lifecycle
from tests import test_native_research_compute as compute
from tests import test_native_relations_compute as relations
from tests import test_native_folder as folder_fixture
from tests import test_native_sync as sync_fixture
from tests.native_worker_fixture import command as fixture_command, exists as process_exists
from tests.test_read_http import wait_until
from web import learn
from web.features import Features


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class InstalledResearchComputeTest(compute.NativeResearchComputeTest):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate host and its built worker required')
        cls.core = cls.binary = Path(configured).resolve()
        cls.resources = Path(os.environ.get('ILLUSTASH_NATIVE_ROOT') or Path(__file__).resolve().parents[1]).resolve()
        cls.candidate_worker = Path(os.environ.get('ILLUSTASH_RESEARCH_ADAPTER_BIN') or
                                    cls.core.with_name('illustash-worker.exe' if os.name == 'nt' else 'illustash-worker')).resolve()
        if not cls.core.is_file() or not cls.candidate_worker.is_file():
            raise AssertionError('candidate host/worker missing; do not substitute an older binary')

    def setUp(self):
        compute.NativeResearchComputeTest.setUp(self)
        folder = Path(self.temp.name) / 'installed-bin'
        folder.mkdir()
        self.binary = folder / ('illustash-research.exe' if os.name == 'nt' else 'illustash-research')
        shutil.copy2(self.candidate_worker, self.binary)

    def test_unknown_kind_is_refused_without_partial_native_learning(self):
        before = self.rows(self.store.path)
        result, output, snapshot = self.request('unknown-research-kind', regroup=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('不支持的计算请求', result.stderr.decode(errors='replace'))
        self.assertFalse(output.exists())
        self.assertEqual(self.rows(snapshot / 'library.sqlite'), before)

    def test_duplicate_json_input_is_rejected_before_python_setup(self):
        root = Path(self.temp.name)
        request, response = root / 'duplicate.json', root / 'response.json'
        request.write_text('{"kind":"oc","format":"illustash-research/1","format":"illustash-research/1"}', encoding='utf-8')
        env = os.environ | {'PATH': ''}
        env.pop('ILLUSTASH_WORKER_PYTHON', None)
        result = subprocess.run([str(self.binary), str(request), str(response)], env=env,
                                capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
        self.assertFalse(response.exists())


class RecognitionTransitionRoutingTest(unittest.TestCase):
    setUpClass = InstalledResearchComputeTest.__dict__['setUpClass']

    def test_recognition_platform_gate_without_python_or_model_access(self):
        with tempfile.TemporaryDirectory(prefix='illustash-recognition-gate-') as temp:
            root = Path(temp)
            worker = root / ('illustash-recognition.exe' if os.name == 'nt' else 'illustash-recognition')
            shutil.copy2(self.candidate_worker, worker)
            request, output = root / 'request.json', root / 'response.json'
            request.write_text(json.dumps(dict(format='illustash-recognition/1', identity=dict(synthetic=True),
                                               assets=[], output=str(root / 'result'))), encoding='utf-8')
            # On macOS/Windows the native negative case sees only nonexistent resources
            # below this owned root; no real model or installation is accessed.
            env = os.environ | dict(PATH='', ILLUSTASH_ROOT=str(root), ILLUSTASH_MODELS=str(root / 'absent-models'))
            env.pop('ILLUSTASH_WORKER_PYTHON', None)
            result = subprocess.run([str(worker), str(request), str(output)], env=env,
                                    capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
            self.assertFalse(output.exists())
            self.assertFalse((root / 'result').exists())


class RecognitionTransitionReceiptTest(unittest.TestCase):
    """Actual App receipt of #146 synthetic v1 protocol fixtures, never model inference.

    Only a generated folder/PNG and model fingerprint placeholder bytes are used.
    NativeSyncTest's worker helper is reused; its Photos setup is never invoked.
    """
    setUpClass = InstalledResearchComputeTest.__dict__['setUpClass']
    _fixture_setup = folder_fixture.NativeFolderHTTPTest._fixture_setup
    package = folder_fixture.NativeFolderHTTPTest.package
    start = folder_fixture.NativeFolderHTTPTest.start
    stop = folder_fixture.NativeFolderHTTPTest.stop
    request = folder_fixture.NativeFolderHTTPTest.request
    sync = folder_fixture.NativeFolderHTTPTest.sync
    finish = sync_fixture.NativeSyncTest.finish
    protocol_fixture = sync_fixture.NativeSyncTest.worker
    def rows(self, tables):
        with sqlite3.connect(self.lib.db) as c:
            return {table: c.execute('SELECT * FROM ' + table + ' ORDER BY 1').fetchall() for table in tables}

    def setUp(self):
        folder_fixture.NativeFolderHTTPTest.setUp(self)
        self.stop(self.process)
        self.env.pop('ILLUSTASH_RECOGNITION_WORKER', None)
        self.env.pop('ILLUSTASH_RESEARCH_WORKER', None)
        self.env['ILLUSTASH_MODELS'] = str(self.resources / 'models')
        Image.new('RGB', (20, 30), 'red').save(self.new_folder / 'not-scanned.png')
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        with sqlite3.connect(self.lib.source) as c:
            self.asset = c.execute('SELECT uuid FROM assets ORDER BY uuid').fetchone()[0]
        self.assertEqual(self.request('/api/tag', {'ids': [self.asset], 'tag': '合成人工保留'})['status'], 200)
        self.human = self.rows(['actions', 'assertions', 'asset_tags', 'term_verdicts'])

    def install_protocol_fixture(self, *, bad_sidecar=False):
        self.protocol_fixture()
        self.stop(self.process)
        script = self.root / 'synthetic-worker'
        if bad_sidecar:
            old = "Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=packages,failed=[])))"
            new = "(p/'character-claims.json').write_text('{}')\nPath(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=packages,failed=[],character_claims=[str(p/'character-claims.json')])))"
            source = script.read_text(encoding='utf-8')
            self.assertEqual(source.count(old), 1)
            script.write_text(source.replace(old, new), encoding='utf-8')
        named = self.root / ('illustash-recognition.cmd' if os.name == 'nt' else 'illustash-recognition')
        shutil.copy2(Path(self.env['ILLUSTASH_RECOGNITION_WORKER']), named)
        self.env['ILLUSTASH_RECOGNITION_WORKER'] = str(named)
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def no_recognition_writes(self):
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM labels WHERE source='wd_v3'").fetchone()[0], 0)
            self.assertEqual(c.execute('SELECT count(*) FROM embeddings').fetchone()[0], 0)
            if c.execute("SELECT count(*) FROM sqlite_master WHERE name='recognition_imports'").fetchone()[0]:
                self.assertEqual(c.execute('SELECT count(*) FROM recognition_imports').fetchone()[0], 0)
        self.assertEqual(self.rows(list(self.human)), self.human)

    def test_native_library_new_legacy_reply_platform_gate_and_actual_receipt(self):
        self.install_protocol_fixture()
        state = self.sync()
        if sys.platform in ('darwin', 'win32'):
            self.assertEqual(state['state']['status'], 'error', state)
            self.assertIn('sidecar', state['state']['message'])
            self.no_recognition_writes()
        else:
            self.assertEqual(state['state']['status'], 'done', state)
            self.assertEqual(state['state']['imported'], 1, state)
            with sqlite3.connect(self.lib.source) as c:
                self.assertEqual(c.execute("SELECT status FROM recognition_imports").fetchall(), [('done',)])
                self.assertEqual(c.execute("SELECT model FROM embeddings WHERE uuid=? ORDER BY model", (self.asset,)).fetchall(), [('ccip',), ('wd',)])
                self.assertEqual(c.execute("SELECT value,source FROM labels WHERE uuid=? AND dim='rating'", (self.asset,)).fetchall(), [('general', 'wd_v3')])
            self.assertEqual(self.rows(list(self.human)), self.human)
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_declared_bad_sidecar_refuses_before_formal_import(self):
        self.install_protocol_fixture(bad_sidecar=True)
        state = self.sync()
        self.assertEqual(state['state']['status'], 'error', state)
        self.assertIn('character-claims.json', state['state']['message'])
        self.assertIn('missing field', state['state']['message'])
        self.no_recognition_writes()
        self.assertFalse(self.sentinel.exists())


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class NativeResearchPublicationTest(unittest.TestCase):
    setUpClass = InstalledResearchComputeTest.__dict__['setUpClass']
    package = lifecycle.NativeLifecycleTest.package
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request

    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.stop(self.process)
        self.worker = self.bin / ('illustash-research.exe' if os.name == 'nt' else 'illustash-research')
        shutil.copy2(self.candidate_worker, self.worker)
        self.env.update(ILLUSTASH_RESEARCH_WORKER=str(self.worker), ILLUSTASH_RESEARCH_TIMEOUT_MS='5000')
        self.env.pop('ILLUSTASH_WORKER_PYTHON', None)
        # Keep the #151 base fixture learning-only; #152 extends it below.
        self.store.set_startup({'similar': False})
        with sqlite3.connect(self.lib.source) as c:
            c.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
            for i in range(9):
                ident = ('seed' if i < 6 else 'unmatched') + str(i)
                vector = np.zeros(768, np.float32)
                vector[0 if i < 6 else 1] = 1
                c.execute('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)',
                          (ident, ident + '.png', str(self.lib.path / (ident + '.png')), 120, 150, '2026-10-04', 'orig', 1, 0))
                c.execute("INSERT INTO embeddings VALUES (?,'ccip',?)", (ident, vector.tobytes()))
        self.store.refresh()
        self.entity = self.store.entities('Alice')[0]['id']
        self.store.change(['seed' + str(i) for i in range(6)], 'confirmed', self.entity)
        self.restart()
        self.finished()

    def restart(self):
        self.process, self.log = self.start(self.lib.path)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def state(self):
        return json.loads(self.request('/api/clusters')['body'])['learner']

    def finished(self, status='idle'):
        wait_until(lambda: self.state()['status'] in ('idle', 'error'), timeout=15)
        self.assertEqual(self.state()['status'], status, self.state())

    def rows(self, tables):
        with sqlite3.connect(self.lib.db) as c:
            return {table: c.execute('SELECT * FROM ' + table + ' ORDER BY 1').fetchall() for table in tables}

    def timing_fixture(self):
        # Hold a real native response before App observes worker exit. This permits
        # deterministic cancellation/fingerprint/replay tests without production delay flags.
        self.stop(self.process)
        self.mode, self.ready, self.release = (self.root / name for name in ('mode', 'ready', 'release'))
        self.saved = self.root / 'native-response.json'
        self.pid, self.child = self.root / 'fixture.pid', self.root / 'fixture-child.pid'
        # Opening a package can start its own startup task. Let that real native
        # task finish before holding an explicitly requested regroup, so deleting
        # markers cannot erase the ready signal of an already running startup task.
        self.mode.write_text('pass')
        bridge = self.root / 'research-timing-fixture'
        bridge.write_text('#!' + sys.executable + '\n' + f'''import json,os,sys,subprocess,time
from pathlib import Path
mode=Path({str(self.mode)!r}).read_text()
Path({str(self.pid)!r}).write_text(str(os.getpid()))
if mode=='replay':
    Path(sys.argv[2]).write_bytes(Path({str(self.saved)!r}).read_bytes())
    sys.exit(0)
code=subprocess.call([{str(self.worker)!r},sys.argv[1],sys.argv[2]])
if code: sys.exit(code)
if mode=='pass': sys.exit(0)
Path({str(self.saved)!r}).write_bytes(Path(sys.argv[2]).read_bytes())
child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])
Path({str(self.child)!r}).write_text(str(child.pid))
Path({str(self.ready)!r}).write_text('native calculated')
deadline=time.monotonic()+25
while not Path({str(self.release)!r}).exists() and time.monotonic()<deadline: time.sleep(.01)
child.terminate();child.wait()
''', encoding='utf-8')
        self.env['ILLUSTASH_RESEARCH_WORKER'] = fixture_command(bridge)
        self.restart()
        self.finished()
        self.mode.write_text('hold')

    def held(self):
        for path in (self.ready, self.release, self.pid, self.child):
            path.unlink(missing_ok=True)
        self.assertEqual(self.request('/api/regroup', {})['body'], b'{"started": true}')
        wait_until(self.ready.exists, timeout=5)
        self.assertTrue(self.saved.exists())
        return int(self.pid.read_text()), int(self.child.read_text())

    def no_processes(self, pids):
        def pid_exists(pid):
            if os.name == 'nt':
                return process_exists(pid)
            try:
                os.kill(pid, 0)
                return True
            except ProcessLookupError:
                return False
        for pid in pids:
            wait_until(lambda: not pid_exists(pid), timeout=5)
        # Also retain #146's worker-group check on POSIX.
        self.assertFalse(process_exists(pids[0]))

    def test_installed_startup_regroup_oc_original_body_and_repeat_preserve_history(self):
        before = self.rows(['actions', 'assertions'])
        self.assertGreater(len(self.rows(['prototypes'])['prototypes']), 0)
        self.assertGreater(len(self.rows(['clusters'])['clusters']), 0)
        for _ in range(2):
            self.assertEqual(self.request('/api/regroup', {})['body'], b'{"started": true}')
            self.finished()
        self.assertEqual(self.rows(['actions', 'assertions']), before)
        features = Features(self.lib.source).load()
        expected = learn.oc_groups(self.store, features, self.entity)
        response = self.request('/api/oc/groups?entity=' + self.entity)
        self.assertEqual(response['status'], 200, response)
        self.assertEqual(response['body'], json.dumps(expected, ensure_ascii=False).encode())
        self.assertFalse(self.sentinel.exists())

    def test_inflight_human_change_rejects_old_snapshot_then_retries(self):
        # #194: the stale result is never published; the task retries on a new snapshot by itself.
        self.timing_fixture()
        before = self.rows(['prototypes', 'proto_claims', 'clusters', 'cluster_info'])
        pids = self.held()
        stale = json.loads(self.saved.read_text(encoding='utf-8'))['version']
        self.assertEqual(self.request('/api/tag', {'ids': ['a'], 'tag': '合成旧快照'})['status'], 200)
        human = self.rows(['actions', 'assertions'])
        self.release.write_text('release')
        wait_until(lambda: self.state()['status'] == 'waiting', timeout=10)
        self.assertIn('自动重试', self.state()['message'])
        self.assertEqual(self.rows(list(before)), before)
        self.no_processes(pids)
        self.finished()
        self.assertNotEqual(int(self.pid.read_text()), pids[0])
        self.assertNotEqual(json.loads(self.saved.read_text(encoding='utf-8'))['version'], stale)
        self.assertEqual(self.rows(['actions', 'assertions']), human)

    def test_cancel_timeout_and_retry_use_existing_process_tree_contract(self):
        self.timing_fixture()
        before = self.rows(['prototypes', 'proto_claims', 'clusters', 'actions', 'assertions'])
        pids = self.held()
        with sqlite3.connect(self.lib.db) as c:
            intents = c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        for _ in range(3):
            self.assertEqual(self.request('/api/regroup', {})['body'], b'{"started": true}')
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM research_runs').fetchone()[0], intents)
        self.assertEqual(self.request('/api/regroup', {'action': 'cancel'})['body'], b'{"cancelled": true}')
        self.no_processes(pids)
        self.assertEqual(self.state()['status'], 'idle')
        self.assertEqual(self.rows(list(before)), before)
        self.stop(self.process)
        self.env['ILLUSTASH_RESEARCH_TIMEOUT_MS'] = '1500'
        for path in (self.ready, self.pid, self.child):
            path.unlink(missing_ok=True)
        self.restart()
        wait_until(self.ready.exists, timeout=5)
        self.finished('error')
        self.no_processes((int(self.pid.read_text()), int(self.child.read_text())))
        self.assertEqual(self.rows(list(before)), before)
        self.stop(self.process)
        self.env.update(ILLUSTASH_RESEARCH_WORKER=str(self.worker), ILLUSTASH_RESEARCH_TIMEOUT_MS='5000')
        self.restart()
        self.finished()
        self.assertFalse(self.sentinel.exists())

    def test_switch_same_identity_and_host_exit_kill_held_native_result(self):
        self.timing_fixture()
        before = self.rows(['prototypes', 'proto_claims', 'clusters', 'actions', 'assertions'])
        pids = self.held()
        replacement, _ = self.package('same-id')
        replacement.config['id'] = self.lib.config['id']
        (replacement.path / 'library.json').write_text(json.dumps(replacement.config), encoding='utf-8')
        self.assertEqual(self.request('/api/libraries/open', {'path': str(replacement.path)})['status'], 200)
        self.no_processes(pids)
        self.assertEqual(self.rows(list(before)), before)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.ready.unlink(missing_ok=True)
        self.pid.unlink(missing_ok=True)
        self.child.unlink(missing_ok=True)
        self.assertEqual(self.request('/api/libraries/open', {'path': str(self.lib.path)})['status'], 200)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        # Opening the old package can resume its pending intent. Await/hold that owned task.
        self.request('/api/regroup', {})
        wait_until(self.ready.exists, timeout=5)
        pids = int(self.pid.read_text()), int(self.child.read_text())
        self.stop(self.process)
        self.no_processes(pids)
        self.assertEqual(self.rows(list(before)), before)

    def test_old_real_response_replay_is_rejected_without_republishing(self):
        self.timing_fixture()
        before = self.rows(['prototypes', 'proto_claims', 'clusters', 'actions', 'assertions'])
        pids = self.held()
        self.release.write_text('release')
        self.finished()
        self.no_processes(pids)
        self.mode.write_text('replay')
        self.request('/api/regroup', {})
        self.finished('error')
        self.assertEqual(self.rows(list(before)), before)

    def test_ack_failure_is_atomic_and_actual_native_retry_preserves_human_history(self):
        self.stop(self.process)
        tables = ['prototypes', 'learn_state', 'proto_claims', 'weak_claims',
                  'clusters', 'cluster_info', 'actions', 'assertions', 'memberships']
        before = self.rows(tables)
        # Reembedding all confirmed seeds onto the unmatched direction changes both
        # the actual prototype and its machine claims. Keep the human inputs intact.
        vector = np.zeros(768, np.float32)
        vector[1] = 1
        with sqlite3.connect(self.lib.source) as c:
            c.executemany("UPDATE embeddings SET vec=? WHERE uuid=? AND model='ccip'",
                          [(vector.tobytes(), 'seed' + str(i)) for i in range(6)])
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail_native_ack BEFORE UPDATE ON backend_effects WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'synthetic ack'); END")
            effect = c.execute("INSERT INTO backend_effects(kind) VALUES ('index_changed')").lastrowid
        self.restart()
        wait_until(lambda: self.state()['status'] == 'error', timeout=15)
        self.finished('error')
        self.assertEqual(self.rows(tables), before)
        with sqlite3.connect(self.lib.db) as c:
            status, error = c.execute('SELECT status,error FROM backend_effects WHERE id=?', (effect,)).fetchone()
            self.assertEqual(status, 'pending')
            self.assertIn('synthetic ack', error)
            self.assertGreater(c.execute("SELECT count(*) FROM research_runs WHERE status='pending'").fetchone()[0], 0)
            c.execute('DROP TRIGGER fail_native_ack')
        self.stop(self.process)
        self.restart()
        self.finished()
        after = self.rows(tables)
        self.assertNotEqual(after['prototypes'], before['prototypes'])
        self.assertNotEqual(after['proto_claims'], before['proto_claims'])
        self.assertNotEqual(after['clusters'], before['clusters'])
        for table in ('actions', 'assertions'):
            self.assertEqual(after[table], before[table], table)
        self.assertEqual([row for row in after['memberships'] if row[2] != 'machine'],
                         [row for row in before['memberships'] if row[2] != 'machine'])
        # Successful retry must materialize the three new PROTO machine memberships.
        self.assertEqual({(row[0], row[1], row[3], row[5]) for row in after['memberships']
                          if row[2] == 'machine' and row[4] == 'PROTO'},
                         {(ident, self.entity, 'machine', 1.0)
                          for ident in ('unmatched6', 'unmatched7', 'unmatched8')})
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status,error FROM backend_effects WHERE id=?', (effect,)).fetchone(), ('done', ''))
            self.assertEqual(c.execute("SELECT count(*) FROM research_runs WHERE status='pending'").fetchone()[0], 0)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE status='pending' AND kind='index_changed'").fetchone()[0], 0)


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class InstalledRelationSnapshotTest(relations.NativeSimilarSnapshotTest):
    """Same independent cache/precision reference through the actual installed adapter."""
    setUpClass = InstalledResearchComputeTest.__dict__['setUpClass']

    def setUp(self):
        super().setUp()
        self.binary = self.root / ('illustash-research.exe' if os.name == 'nt' else 'illustash-research')
        shutil.copy2(self.candidate_worker, self.binary)
        # The parent request uses an empty PATH; forbid a configured Python too.
        scoped = unittest.mock.patch.dict(os.environ)
        scoped.start()
        self.addCleanup(scoped.stop)
        os.environ.pop('ILLUSTASH_WORKER_PYTHON', None)


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class NativeRelationPublicationTest(NativeResearchPublicationTest):
    """Mixed startup plus direct similar uses existing App atomic/lifecycle tests."""
    def setUp(self):
        super().setUp()
        self.stop(self.process)
        with sqlite3.connect(self.lib.source) as c:
            for (ident,) in c.execute('SELECT uuid FROM assets').fetchall():
                path = self.root / (ident + '.png')
                # Identical textured images hit original early equality, keeping
                # lifecycle tests quick without substituting a fake calculator.
                y,x=np.mgrid[:59,:87]
                pixels=np.stack(((x*7+y*3)%256,(x*11+y*17)%256,(x*23+y*13)%256),axis=2).astype(np.uint8)
                Image.fromarray(pixels).save(path)
                c.execute('UPDATE assets SET src_path=? WHERE uuid=?',(str(path),ident))
        self.store.refresh()
        self.store.set_startup({'similar': True})
        self.store.set_fold_kinds([])
        self.restart()
        self.finished()
        self.assertGreater(len(self.rows(['image_sigs'])['image_sigs']),0)
        self.assertGreater(len(self.rows(['relations'])['relations']),0)

    def request(self, path, body=None):
        if path == '/api/regroup':
            path = '/api/similar'
        return super().request(path, body)

    def rows(self, tables):
        tables=list(tables)
        if any(t in tables for t in ('prototypes','clusters','image_sigs','relations')):
            tables=list(dict.fromkeys(tables+['image_sigs','relations','similar_paired','similar_retry']))
        return super().rows(tables)

    def timing_fixture(self):
        super().timing_fixture()
        # Force a genuinely changed image result after the startup pass, before
        # each inherited test captures its formal before-state.
        with sqlite3.connect(self.lib.db) as c:
            c.execute("DELETE FROM image_sigs WHERE uuid='seed0'")
            c.execute("DELETE FROM relations WHERE a='seed0' OR b='seed0'")
            c.execute("DELETE FROM similar_paired WHERE uuid='seed0'")

    def held(self):
        before=self.rows(['image_sigs','relations'])
        pids=super().held()
        response=json.loads(self.saved.read_text(encoding='utf-8'))
        self.assertGreater(len(response['tables']['image_sigs']),len(before['image_sigs']))
        self.assertGreater(len(response['tables']['relations']),len(before['relations']))
        return pids

    def test_old_real_response_replay_is_rejected_without_republishing(self):
        self.timing_fixture()
        pids=self.held()
        self.release.write_text('release')
        self.finished()
        self.no_processes(pids)
        published=self.rows(['actions','assertions','prototypes','clusters'])
        self.mode.write_text('replay')
        self.request('/api/similar',{})
        self.finished('error')
        self.assertEqual(self.rows(list(published)),published)

    def test_manual_fold_preferences_and_human_history_survive_repeated_publication(self):
        history=self.rows(['actions','assertions','image_sigs','relations'])
        for kinds in (['identical'], [], ['duplicate','identical']):
            response=self.request('/api/fold',{'kinds': kinds})
            self.assertEqual(response['status'],200,response)
            self.assertEqual(self.request('/api/similar',{})['body'],b'{"started": true}')
            self.finished()
            actual=self.rows(['actions','assertions','image_sigs','relations'])
            for table in ('actions','assertions','image_sigs','relations'):
                self.assertEqual(actual[table],history[table],table)
            with sqlite3.connect(self.lib.db) as c:
                self.assertEqual(c.execute('SELECT count(*) FROM duplicates').fetchone()[0]>0,bool(kinds))
        self.assertFalse(self.sentinel.exists())

    def test_mixed_event_ack_failure_rolls_back_new_image_cache_then_retry_publishes(self):
        self.stop(self.process)
        with sqlite3.connect(self.lib.db) as c:
            c.execute("DELETE FROM image_sigs WHERE uuid='seed0'")
            c.execute("DELETE FROM relations WHERE a='seed0' OR b='seed0'")
            c.execute("DELETE FROM similar_paired WHERE uuid='seed0'")
            c.execute("CREATE TRIGGER fail_image_ack BEFORE UPDATE ON backend_effects WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'synthetic image ack'); END")
            effect=c.execute("INSERT INTO backend_effects(kind) VALUES ('index_changed')").lastrowid
        before=self.rows(['actions','assertions','prototypes','clusters','memberships'])
        self.restart()
        self.finished('error')
        self.assertEqual(self.rows(list(before)),before)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM backend_effects WHERE id=?',(effect,)).fetchone()[0],'pending')
            c.execute('DROP TRIGGER fail_image_ack')
        self.stop(self.process)
        self.restart()
        self.finished()
        after=self.rows(list(before))
        self.assertNotEqual(after['image_sigs'],before['image_sigs'])
        self.assertNotEqual(after['relations'],before['relations'])
        for table in ('actions','assertions'):
            self.assertEqual(after[table],before[table],table)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status,error FROM backend_effects WHERE id=?',(effect,)).fetchone(),('done',''))
        self.assertFalse(self.sentinel.exists())
