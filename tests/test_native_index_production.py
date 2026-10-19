"""Installed index adapter and App receipt on owned synthetic packages.

Explicit candidate host/worker only; no build or model discovery. Normal cases
use cached synthetic references and empty PATH without a Python worker. Fault
launchers run the actual adapter before delaying/mutating transport receipt.
The one real-CCIP case requires three explicit authorised resource paths.
"""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

import numpy as np
from PIL import Image

from tests import test_native_index as index_fixture
from tests import test_native_index_compute as compute
from tests.native_worker_fixture import command, exists as process_exists
from tests.test_native_human_labels import snapshot
from tests.test_read_http import wait_until


class CandidateIndexAdapter:
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not configured:
            raise unittest.SkipTest('explicit candidate host and built worker required')
        cls.binary = Path(configured).resolve(strict=True)
        cls.candidate_worker = Path(os.environ.get('ILLUSTASH_INDEX_ADAPTER_BIN') or
                                    cls.binary.with_name('illustash-worker.exe' if os.name == 'nt' else 'illustash-worker')).resolve(strict=True)
        cls.resources = Path(os.environ.get('ILLUSTASH_NATIVE_ROOT') or Path(__file__).resolve().parents[1]).resolve(strict=True)

    def install(self, root):
        installation = root/'installed with spaces'
        binary = installation/'bin'; binary.mkdir(parents=True)
        adapter = binary/('illustash-index.exe' if os.name == 'nt' else 'illustash-index')
        shutil.copy2(self.candidate_worker, adapter)
        resources = installation/'resources'
        (resources/'web/static').mkdir(parents=True)
        (resources/'web/static/index.html').write_text('synthetic installed resource')
        (resources/'tags').mkdir()
        shutil.copy2(self.resources/'tags/tags_v3.csv', resources/'tags/tags_v3.csv')
        (resources/'models').mkdir()  # No model, WD, runtime or Python calculator is installed.
        if sys.platform == 'win32':
            # The fixed OpenBLAS sits where installers put it; the adapter resolves it itself.
            blas = os.environ.get('ILLUSTASH_RESEARCH_OPENBLAS_DLL')
            if not blas:
                raise AssertionError('explicit fixed OpenBLAS DLL required on Windows; no search')
            (resources/'openblas').mkdir()
            shutil.copy2(blas, resources/'openblas/openblas.dll')
        return adapter, resources

    @staticmethod
    def isolated_env(resources, base=None):
        env = (os.environ if base is None else base) | dict(PATH='', ILLUSTASH_MODELS=str(resources/'models'),
                                ILLUSTASH_ORT_DYLIB=str(resources/'absent-runtime'))
        for key in ('ILLUSTASH_ROOT', 'ILLUSTASH_WORKER_PYTHON', 'ILLUSTASH_INDEX_MODEL', 'ILLUSTASH_INDEX_RUNTIME', 'ILLUSTASH_RESEARCH_OPENBLAS_DLL'):
            env.pop(key, None)
        return env


class IndexTransitionRoutingTest(CandidateIndexAdapter, unittest.TestCase):
    def test_platform_route_with_missing_owned_resources_never_reads_real_models(self):
        with tempfile.TemporaryDirectory(prefix='illustash-index-route-') as tmp:
            root = Path(tmp); adapter, resources = self.install(root)
            request, response = root/'request.json', root/'response.json'
            request.write_text('{"format":"illustash-index/1","identity":{}}')
            result = subprocess.run([str(adapter), str(request), str(response)],
                                    env=self.isolated_env(resources), capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
            self.assertFalse(response.exists())


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class InstalledIndexComputeTest(CandidateIndexAdapter, unittest.TestCase):
    def test_fresh_empty_and_last_drop_clear_anchors_without_model_or_python(self):
        with tempfile.TemporaryDirectory(prefix='illustash-index-empty-') as tmp:
            root = Path(tmp); adapter, resources = self.install(root)
            review = root/'review'; review.mkdir()
            compute.NativeIndexComputeTest().prepare(review)
            for path in (review/'char-index').iterdir(): path.unlink()
            identity = dict(package=str(review), library_id='synthetic', generation=1,
                            session='0'*64, task_id='1'*32)
            request = dict(format='illustash-index/1', identity=identity, before='fixed',
                           action='apply', rows=[], review=str(review), reason=None)
            for action in ('apply', 'drop'):
                if action == 'drop':
                    vectors = np.zeros((6,768),np.float32); vectors[:,0] = 1
                    np.save(review/'char-index/last.npy', vectors)
                    (review/'char-index/last.json').write_text(json.dumps(dict(short_tag='last', tag='last', source='danbooru-patch')))
                    request.update(action='drop', rows=[dict(short_tag='last')], reason='合成最后角色')
                (review/'request.json').write_text(json.dumps(request))
                result = subprocess.run([str(adapter), str(review/'request.json'), str(review/'response.json')],
                                        env=self.isolated_env(resources), capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
                value = json.loads((review/'response.json').read_text())
                self.assertEqual(value['characters'], []); self.assertEqual(value['labels'], [])
                self.assertEqual(np.load(review/'char-anchors.npy').shape, (0,768))
                self.assertEqual((review/'char-anchors.txt').read_text(), '')

    def test_cached_clean_build_scan_exact_old_algorithm_without_python_or_models(self):
        with tempfile.TemporaryDirectory(prefix='illustash-index-installed-') as tmp:
            root = Path(tmp); adapter, resources = self.install(root)
            old, new = root/'old', root/'new'; old.mkdir()
            reference = compute.NativeIndexComputeTest()
            tags, rows = reference.prepare(old); shutil.copytree(old, new)
            results = []
            for folder in (old, new):
                identity = dict(package=str(folder), library_id='synthetic', generation=1,
                                session='0'*64, task_id='1'*32)
                request = dict(format='illustash-index/1', identity=identity, before='fixed',
                               action='apply', rows=rows, review=str(folder), reason=None)
                (folder/'request.json').write_text(json.dumps(request))
                if folder == old:
                    results.append(reference.legacy(old, tags, folder/'request.json'))
                else:
                    # Use fixture tags in the installed root, discovered relative to executable.
                    shutil.copy2(tags, resources/'tags/tags_v3.csv')
                    result = subprocess.run([str(adapter), str(folder/'request.json'), str(folder/'response.json')],
                                            env=self.isolated_env(resources), capture_output=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
                    results.append(json.loads((folder/'response.json').read_text()))
            self.assertEqual(sorted(results[0]['characters'], key=lambda c:c['char_id']),
                             sorted(results[1]['characters'], key=lambda c:c['char_id']))
            for key in ('aliases', 'labels'):
                self.assertEqual(sorted(results[0][key]), sorted(results[1][key]), key)
            for name in ('char-anchors.npy', 'char-index/new.npy', 'char-patch/raw/new.npy', 'char-patch/raw/twin.npy'):
                np.testing.assert_array_equal(np.load(old/name), np.load(new/name))
            self.assertEqual((old/'char-scan.csv').read_bytes(), (new/'char-scan.csv').read_bytes())
            with contextlib.closing(sqlite3.connect(new/'pi.db')) as c, c:
                self.assertEqual(c.execute("SELECT value,source FROM labels WHERE dim='general'").fetchall(), [('solo','human')])

    def test_check_and_invalid_requests_fail_before_python_and_without_response(self):
        with tempfile.TemporaryDirectory(prefix='illustash-index-invalid-') as tmp:
            root = Path(tmp); adapter, resources = self.install(root); env = self.isolated_env(resources)
            result = subprocess.run([str(adapter), '--check'], env=env, capture_output=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
            request, output = root/'request.json', root/'response.json'
            for value in ('{"format":"illustash-index/1","format":"other"}',
                          '{"format":"illustash-index/1","identity":{}}'):
                request.write_text(value)
                result = subprocess.run([str(adapter), str(request), str(output)], env=env, capture_output=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
                self.assertFalse(output.exists())


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class InstalledIndexAppTest(CandidateIndexAdapter, unittest.TestCase):
    package = index_fixture.NativeIndexTest.package
    start = index_fixture.NativeIndexTest.start
    stop = index_fixture.NativeIndexTest.stop
    request = index_fixture.NativeIndexTest.request
    restart = index_fixture.NativeIndexTest.restart
    index = index_fixture.NativeIndexTest.index
    finish = index_fixture.NativeIndexTest.finish
    check = index_fixture.NativeIndexTest.check

    def setUp(self):
        index_fixture.NativeIndexTest.setUp(self)
        self.stop(self.process)
        self.adapter, self.adapter_resources = self.install(self.root)
        # Preserve the fixture's isolated HOME/app support/shared root and strip Python setup.
        self.env = self.isolated_env(self.adapter_resources, self.env)
        self.env.update(PATH='', ILLUSTASH_NO_KEYCHAIN='1', ILLUSTASH_INDEX_WORKER=str(self.adapter))
        self.env.pop('ILLUSTASH_RECOGNITION_WORKER', None)
        self.env.pop('ILLUSTASH_RESEARCH_WORKER', None)
        self.remote_rows = self.remote_rows[:1]
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c:
            for col in ('anchor_p90 REAL','base_tag TEXT','skin TEXT','post_count INTEGER','gender_conf REAL','anchor_p50 REAL','anchor_p75 REAL'):
                c.execute('ALTER TABLE characters ADD COLUMN '+col)
            c.execute('CREATE TABLE embeddings(uuid TEXT,model TEXT,vec BLOB)')
            for col in ('source','model_ver','run_id'):
                c.execute('ALTER TABLE labels ADD COLUMN '+col+' TEXT')
        for cid, axis in [('alice',0), ('bob',2)]:
            rows = np.zeros((6,768),np.float32); rows[:,axis] = 1; rows[:,3] = np.arange(6)*.002
            np.save(self.shared/'char-index'/f'{cid}.npy', rows)
            (self.shared/'char-index'/f'{cid}.json').write_text(json.dumps(dict(short_tag=cid,tag=cid+'_tag',gender={'girl':1},post_count=300)))
        self.reference_rows = np.zeros((6,768),np.float32)
        self.reference_rows[:,1] = 1; self.reference_rows[:,3] = np.arange(6)*.002
        self.features = self.lib.path/'features'; self.features.mkdir(exist_ok=True)
        np.save(self.features/'full-features.npy', self.reference_rows[:3])
        (self.features/'full-manifest.csv').write_text('uuid\na\nb\nc\n')
        self.restart()
        self.human = snapshot(self.lib.db)['actions']
        self.human_assertions = snapshot(self.lib.db)['assertions']

    def seed_reference(self):
        cid = 'carol_example'
        np.save(self.shared/'char-index'/f'{cid}.npy', self.reference_rows)
        meta = dict(self.remote_rows[0], source='danbooru-patch', posts=list(range(len(self.reference_rows))),
                    gender={'girl':.5,'boy':.5}, core_tags=['synthetic_hair'])
        (self.shared/'char-index'/f'{cid}.json').write_text(json.dumps(meta))

    def expected(self):
        old = Path(tempfile.mkdtemp(prefix='independent-old-index-', dir=self.root))
        for name in ('char-index', 'game-chars'):
            if (self.shared/name).exists(): shutil.copytree(self.shared/name, old/name)
            else: (old/name).mkdir()
        (old/'char-patch').mkdir(); (old/'char-patch/plan.json').write_text(json.dumps(self.remote_rows))
        (old/'album-char-map.json').write_text('{"map":{}}')
        for name in ('full-features.npy', 'full-manifest.csv'): shutil.copy2(self.features/name, old/name)
        with contextlib.closing(sqlite3.connect(self.lib.source)) as src, src, contextlib.closing(sqlite3.connect(old/'pi.db')) as dest, dest: src.backup(dest)
        request = dict(format='illustash-index/1', identity={'task_id':'independent-reference'}, before='fixed',
                       action='apply', rows=self.remote_rows, reason=None, review=str(old))
        (old/'request.json').write_text(json.dumps(request))
        return compute.NativeIndexComputeTest().legacy(old, self.adapter_resources/'tags/tags_v3.csv', old/'request.json')

    def apply(self):
        self.check(); self.seed_reference()
        self.expected_result = self.expected()
        self.index({'action':'apply','tags':['carol_example']})
        return self.finish()

    def assert_projection(self, expected):
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c:
            self.assertEqual(sorted(c.execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'").fetchall()),
                             sorted(tuple(r) for r in expected['labels']))
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertEqual(snapshot(self.lib.db)['assertions'], self.human_assertions)
        expected_candidates = sorted((u,conf) for u,cid,conf in expected['labels'] if cid == 'carol_example')
        self.assertTrue(expected_candidates)
        self.assertTrue({'b','c'} <= {u for u,_ in expected_candidates})
        with contextlib.closing(sqlite3.connect(self.lib.db)) as c, c:
            row = c.execute("SELECT entity_id FROM links WHERE system='ccip' AND key='carol_example' AND current=1").fetchone()
            self.assertIsNotNone(row); entity = row[0]
            self.assertEqual(c.execute("SELECT uuid,score FROM candidates WHERE entity=? AND source='CCIP' ORDER BY uuid", [entity]).fetchall(), expected_candidates)
            self.assertEqual(c.execute("SELECT uuid,status FROM memberships WHERE entity=? AND uuid IN ('b','c') ORDER BY uuid", [entity]).fetchall(), [('b','machine'),('c','machine')])
        self.assertFalse(self.sentinel.exists())
        self.assertNotIn('ILLUSTASH_WORKER_PYTHON', self.env)
        self.assertEqual(self.env['PATH'], '')
        return entity

    def launch_through(self, shell, python):
        """Restart the host with a launcher around the installed adapter: `shell` on POSIX, `python` on Windows."""
        launcher = self.root/'controlled-adapter'
        if os.name == 'nt':
            launcher.write_text(python)
            launcher = Path(command(launcher))
        else:
            launcher.write_text('#!/bin/sh\n' + shell)
            launcher.chmod(0o755)
        self.stop(self.process)
        self.env['ILLUSTASH_INDEX_WORKER'] = str(launcher); self.restart()

    def hold_after_actual_compute(self):
        self.ready, self.gate = self.root/'adapter-ready', self.root/'release-adapter'
        self.addCleanup(self.gate.touch)
        self.launch_through(
            shlex.quote(str(self.adapter))+' "$@" || exit $?\n'+
            '/bin/echo "$$" > '+shlex.quote(str(self.ready))+'\n'+
            'while [ ! -f '+shlex.quote(str(self.gate))+' ]; do /bin/sleep 0.02; done\n',
            'import os,sys,time,subprocess\nfrom pathlib import Path\n'
            + 'p=subprocess.run(['+repr(str(self.adapter))+']+sys.argv[1:])\n'
            + 'if p.returncode: sys.exit(p.returncode)\n'
            + 'Path('+repr(str(self.ready))+').write_text(str(os.getpid()))\n'
            + 'while not Path('+repr(str(self.gate))+').exists(): time.sleep(.02)\n')

    def hold_saved_compute(self, timeout_ms):
        """Like hold_after_actual_compute, but the actual calculator's result is made beforehand under the
        default deadline: that run saves it and then fails, so nothing is published and its prepared job
        stays for a retry. The host resumes that job on restart, under `timeout_ms`, and the launcher only
        puts the saved result back in the job folder, so a short deadline need not fit a real compute on a
        busy machine (#219)."""
        self.ready, self.gate = self.root/'adapter-ready', self.root/'release-adapter'
        saved = self.root/'saved-response.json'
        self.addCleanup(self.gate.touch)
        self.env.pop('ILLUSTASH_INDEX_TIMEOUT_MS', None)
        self.launch_through(
            shlex.quote(str(self.adapter))+' "$@" || exit $?\n/bin/cp "$2" '+shlex.quote(str(saved))+' || exit $?\nexit 9\n',
            'import shutil,subprocess,sys\n'
            + 'p=subprocess.run(['+repr(str(self.adapter))+']+sys.argv[1:])\n'
            + 'if p.returncode: sys.exit(p.returncode)\n'
            + 'shutil.copyfile(sys.argv[2],'+repr(str(saved))+')\nsys.exit(9)\n')
        self.check(); self.seed_reference(); self.index({'action':'apply','tags':['carol_example']})
        result = self.finish()
        self.assertEqual(result['state']['status'], 'error', result)
        self.assertTrue(saved.is_file()); self.assertTrue((self.lib.path/'index-job.json').is_file())
        self.env['ILLUSTASH_INDEX_TIMEOUT_MS'] = timeout_ms
        self.launch_through(
            '/bin/cp '+shlex.quote(str(saved))+' "$2" || exit $?\n'+
            '/bin/echo "$$" > '+shlex.quote(str(self.ready))+'\n'+
            'while [ ! -f '+shlex.quote(str(self.gate))+' ]; do /bin/sleep 0.02; done\n',
            'import os,shutil,sys,time\nfrom pathlib import Path\n'
            + 'shutil.copyfile('+repr(str(saved))+',sys.argv[2])\n'
            + 'Path('+repr(str(self.ready))+').write_text(str(os.getpid()))\n'
            + 'while not Path('+repr(str(self.gate))+').exists(): time.sleep(.02)\n')
        wait_until(lambda: self.ready.is_file() and self.ready.read_text().strip().isdigit(), timeout=30)
        job = json.loads((self.lib.path/'index-job.json').read_text())
        self.assertTrue((Path(job['folder'])/'response.json').is_file())

    def start_held(self):
        self.check(); self.seed_reference(); self.index({'action':'apply','tags':['carol_example']})
        # The ready file follows a real index compute, which the parallel full suite can slow well past 5 s.
        wait_until(lambda: self.ready.is_file() and self.ready.read_text().strip().isdigit(), timeout=30)
        job = json.loads((self.lib.path/'index-job.json').read_text())
        result = Path(job['folder'])/'response.json'
        self.assertTrue(result.is_file())
        return result

    def test_cached_installed_compute_publish_drop_preserves_history(self):
        self.check(); self.seed_reference(); expected = self.expected()
        self.index({'action':'apply','tags':['carol_example']}); result = self.finish()
        self.assertEqual(result['state']['status'], 'done', result); entity = self.assert_projection(expected)
        np.testing.assert_array_equal(np.load(self.shared/'char-patch/raw/carol_example.npy'), self.reference_rows)
        self.assertFalse((self.lib.path/'index-job.json').exists())
        self.index({'action':'drop','char':'carol_example','reason':'合成放弃'}); result = self.finish()
        self.assertEqual(result['state']['status'], 'done', result)
        self.assertEqual(result['history'][0]['characters'][0]['status'], 'dropped')
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertEqual(snapshot(self.lib.db)['assertions'], self.human_assertions)
        with contextlib.closing(sqlite3.connect(self.lib.db)) as c, c:
            self.assertEqual(c.execute("SELECT count(*) FROM candidates WHERE entity=? AND source='CCIP'", [entity]).fetchone()[0], 0)
            self.assertEqual(c.execute("SELECT count(*) FROM memberships WHERE entity=? AND status='machine'", [entity]).fetchone()[0], 0)
        self.assertTrue((self.shared/'char-patch/raw/carol_example.npy').exists())
        self.assertNotIn('carol_example', (self.shared/'char-anchors.txt').read_text().splitlines())
        self.assertFalse((self.lib.path/'index-job.json').exists()); self.assertFalse(self.sentinel.exists())

    def test_source_failure_rollback_retry_with_actual_adapter(self):
        self.check(); self.seed_reference(); expected = self.expected()
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c:
            c.execute("CREATE TRIGGER fail_index BEFORE DELETE ON characters BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        before = snapshot(self.lib.source)
        self.index({'action':'apply','tags':['carol_example']}); result = self.finish()
        self.assertEqual(result['state']['status'], 'error', result)
        for table in ('characters','character_aliases','labels'):
            self.assertEqual(snapshot(self.lib.source)[table], before[table])
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c: c.execute('DROP TRIGGER fail_index')
        self.stop(self.process); self.restart(); result = self.finish()
        self.assertEqual(result['state']['status'], 'done', result)
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c:
            self.assertEqual(c.execute("SELECT count(*) FROM characters WHERE char_id='carol_example'").fetchone()[0], 1)
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assert_projection(expected)
        self.assertFalse(self.sentinel.exists())

    def test_source_commit_app_failure_sigkill_idempotent_recovery_with_actual_adapter(self):
        index_fixture.NativeIndexTest.test_source_commit_app_failure_sigkill_restart_idempotent(self)
        self.assert_projection(self.expected_result)
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertFalse(self.sentinel.exists())

    def test_actual_computed_stale_identity_and_invalid_asset_rejected(self):
        for field in ('identity', 'asset'):
            with self.subTest(field=field):
                self.hold_after_actual_compute(); before = snapshot(self.lib.source); output = self.start_held()
                value = json.loads(output.read_text())
                if field == 'identity': value['identity']['generation'] += 1
                else: value['labels'].append(['not-an-asset','carol_example',1])
                output.write_text(json.dumps(value)); self.gate.touch(); result = self.finish()
                self.assertEqual(result['state']['status'], 'error', result)
                for table in ('characters','character_aliases','labels'):
                    self.assertEqual(snapshot(self.lib.source)[table], before[table])
                self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
                self.stop(self.process); (self.lib.path/'index-job.json').unlink()
                self.ready.unlink(); self.gate.unlink()
                for path in (self.shared/'char-index').glob('carol_example.*'): path.unlink()
                self.restart()

    def test_switch_cancels_actual_computed_late_result_and_keeps_same_id_other_package(self):
        self.hold_after_actual_compute(); before = snapshot(self.lib.source); self.start_held()
        worker_pid = int(self.ready.read_text())
        other = self.package('other'); other.config['id'] = self.lib.config['id']
        (other.path/'library.json').write_text(json.dumps(other.config)); other_before = snapshot(other.source)
        result = self.request('/api/libraries/open', {'path':str(other.path)})
        self.assertEqual(result['status'], 200, result['body'])
        wait_until(lambda: not process_exists(worker_pid), timeout=3)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.index()['state']['status'], 'idle')
        self.assertEqual(snapshot(other.source), other_before)
        for table in ('characters','character_aliases','labels'):
            self.assertEqual(snapshot(self.lib.source)[table], before[table])
        self.assertFalse(self.sentinel.exists())

    def test_changed_input_after_actual_compute_rejected_on_restart(self):
        self.hold_after_actual_compute(); self.start_held(); worker_pid = int(self.ready.read_text())
        self.process.kill(); self.process.wait(); self.gate.touch()
        wait_until(lambda: not process_exists(worker_pid), timeout=3)
        with contextlib.closing(sqlite3.connect(self.lib.source)) as c, c: c.execute("UPDATE labels SET conf=.5 WHERE dim='character' AND uuid='b'")
        before = snapshot(self.lib.source)
        self.env['ILLUSTASH_INDEX_WORKER'] = str(self.adapter); self.restart(); result = self.finish()
        self.assertEqual(result['state']['status'], 'error', result)
        self.assertIn('输入已改变', result['state']['message']); self.assertFalse((self.lib.path/'index-job.json').exists())
        for table in ('characters','character_aliases','labels'):
            self.assertEqual(snapshot(self.lib.source)[table], before[table])
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertFalse(self.sentinel.exists())

    def test_timeout_cancels_after_actual_compute_without_formal_writes(self):
        # The deadline covers only putting back a result computed beforehand, never a real compute (#219).
        before = snapshot(self.lib.source); self.hold_saved_compute(timeout_ms='5000')
        worker_pid = int(self.ready.read_text()); result = self.finish()
        self.assertEqual(result['state']['status'], 'error', result)
        self.assertIn('超时', result['state']['message'])
        wait_until(lambda: not process_exists(worker_pid), timeout=3)
        for table in ('characters','character_aliases','labels'):
            self.assertEqual(snapshot(self.lib.source)[table], before[table])
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertFalse(self.sentinel.exists())

    def test_invalid_cached_vectors_fail_without_formal_writes_or_model_loading(self):
        self.check(); self.seed_reference(); before = snapshot(self.lib.source)
        np.save(self.shared/'char-index/carol_example.npy', np.zeros((6,2),np.float32))
        self.index({'action':'apply','tags':['carol_example']}); result = self.finish()
        self.assertEqual(result['state']['status'], 'error', result)
        for table in ('characters','character_aliases','labels'):
            self.assertEqual(snapshot(self.lib.source)[table], before[table])
        self.assertEqual(snapshot(self.lib.db)['actions'], self.human)
        self.assertFalse(self.sentinel.exists())

    def test_actual_ccip_download_to_installed_adapter_without_python(self):
        model = os.environ.get('ILLUSTASH_INDEX_INSTALLED_MODEL')
        runtime = os.environ.get('ILLUSTASH_INDEX_INSTALLED_RUNTIME')
        raw = os.environ.get('ILLUSTASH_INDEX_REFERENCE_RAW')
        if not model or not runtime or not raw:
            self.fail('explicit authorised model/runtime and independent old 8-PNG raw required')
        model, runtime, raw = (Path(p).resolve(strict=True) for p in (model,runtime,raw))
        self.assertEqual(hashlib.sha256(model.read_bytes()).hexdigest(), '4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f')
        self.assertEqual(hashlib.sha256(runtime.read_bytes()).hexdigest(), '5e58353170677b13f05d867f93648c8783b11ac9146f330651d281701180c186' if os.name == 'nt' else 'b46713ea6a25a5227541c21001f6422c1ccdeaecc369aa026a61b752520325c3')
        self.assertEqual(hashlib.sha256(raw.read_bytes()).hexdigest(), '442f260e7dbd7b072ea8c977e31776846d4b5379d4d702b19395dc3710475050' if os.name == 'nt' else '24f00957ff51d32cfb8cea1fc471d12b83e77cf2386cf5cb4f6842753330f87a')
        self.reference_rows = np.load(raw); self.assertEqual(self.reference_rows.shape, (8,768))
        np.save(self.features/'full-features.npy', self.reference_rows[:3])
        self.check(); self.seed_reference()
        meta_path = self.shared/'char-index/carol_example.json'
        meta = json.loads(meta_path.read_text()); meta['gender'] = {'girl':1,'boy':0}
        meta_path.write_text(json.dumps(meta)); expected = self.expected()
        for path in (self.shared/'char-index').glob('carol_example.*'): path.unlink()
        self.stop(self.process); self.env.update(ILLUSTASH_MODELS=str(model.parent.parent), ILLUSTASH_ORT_DYLIB=str(runtime))
        images = []
        for i in range(8):
            y,x = np.indices((64,64))
            images.append(Image.fromarray(np.stack(((x*3+y*2+i)%256,(x+y*3+i)%256,(x*2+y+i)%256),axis=2).astype('u1')))
            with Image.open(raw.parent.parent/'img'/raw.stem/f'{i}.png') as original_image:
                np.testing.assert_array_equal(np.asarray(images[-1]), np.asarray(original_image.convert('RGB')))
        original = self.remote.RequestHandlerClass.do_GET
        def serve(handler):
            if handler.path.startswith('/image/'):
                buffer = io.BytesIO(); images[int(handler.path.rsplit('/',1)[-1])].save(buffer,format='PNG'); data = buffer.getvalue()
                self.calls.append(handler.path); handler.send_response(200); handler.send_header('Content-Type','image/png'); handler.end_headers(); handler.wfile.write(data)
            else: original(handler)
        self.remote.RequestHandlerClass.do_GET = serve
        self.posts = [dict(id=i,tag_string_general='solo 1girl synthetic_hair',media_asset={'variants':[dict(type='720x720',url=f'http://127.0.0.1:{self.remote.server_port}/image/{i}')]}) for i in range(8)]
        self.restart(); self.index({'action':'apply','tags':['carol_example']}); result = self.finish()
        self.assertEqual(result['state']['status'], 'done', result); self.assert_projection(expected)
        self.assertEqual(sum(p.startswith('/image/') for p in self.calls), 8)
        np.testing.assert_array_equal(np.load(self.shared/'char-patch/raw/carol_example.npy'), self.reference_rows)
        self.assertFalse((self.lib.path/'index-job.json').exists())


def load_tests(loader, tests, pattern):
    # The actual-model case runs only when named explicitly; missing inputs then fail.
    explicit = 'test_actual_ccip_download_to_installed_adapter_without_python'
    return unittest.TestSuite(unittest.TestSuite(t for t in group if getattr(t, '_testMethodName', None) != explicit)
                              for group in tests)
