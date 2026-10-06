"""Backend acceptance: fake network, temporary libraries, actual child-process failure modes."""
import hashlib
import json
import os
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock
import numpy as np
from web import effects, library, provenance_job, results
from web.store import Store, Invalid
from web.sync import Syncer
from provenance import accounts, db, evidence
from provenance.sources import pixiv_following, nicknames
from tests.test_web import fixture
from tests.test_provenance_job import FakeRunner, wait


def dump(path):
    with sqlite3.connect(path) as c:
        return list(c.iterdump())


def setUpModule():   # the worker appends every log line to a file under Application Support: keep tests out of the real one
    import tempfile as _tempfile
    from unittest import mock as _mock
    from provenance import run as _run
    folder = _tempfile.TemporaryDirectory()
    patch = _mock.patch.object(_run, 'LOG', __import__('pathlib').Path(folder.name) / 'provenance.log')
    patch.start()
    unittest.addModuleCleanup(patch.stop)
    unittest.addModuleCleanup(folder.cleanup)


class BackendTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.lib = library.create(self.root / 'A.pilib')
        source = fixture(self.root)
        source.rename(self.lib.source)
        with sqlite3.connect(self.lib.source) as c:
            for col in ('source TEXT DEFAULT "legacy"', 'model_ver TEXT', 'run_id TEXT'):
                c.execute('ALTER TABLE labels ADD COLUMN ' + col)
            c.execute('ALTER TABLE assets ADD COLUMN preset TEXT DEFAULT "x"')
            c.execute('ALTER TABLE assets ADD COLUMN source_id TEXT')
        self.store = Store(self.lib.source, self.lib.db)
        self.store.refresh()
        self.specs = {k: dict(version=v, dimension=d, sha256='a' * 64) for k, (v, d, p) in results.MODELS.items()}

    def tearDown(self):
        self.temp.cleanup()

    def package(self, ids=('a', 'b')):
        p = self.root / 'result'
        p.mkdir(exist_ok=True)
        (p / 'labels.jsonl').write_text('\n'.join(json.dumps(dict(asset=u, dim='rating', value='general', confidence=.9)) for u in ids))
        for model in ('wd', 'ccip'):
            np.save(p / (model + '.npy'), np.ones((len(ids), self.specs[model]['dimension']), dtype='<f4'))
        m = dict(format='illustash-recognition/1', library_id=self.lib.config['id'], assets=list(ids), models=self.specs,
                 files={f: dict(bytes=(p / f).stat().st_size, sha256=results.sha((p / f).read_bytes())) for f in results.FILES})
        (p / 'manifest.json').write_text(json.dumps(m))
        return p

    def test_store_open_has_no_initialization_or_source_scan(self):
        before = dump(self.lib.db)
        with mock.patch.object(Store, '_migrate', side_effect=AssertionError), mock.patch.object(Store, '_album_titles', side_effect=AssertionError):
            opened = Store.open(self.lib.source, self.lib.db)
            self.assertEqual(opened.overview()['total'], 3)
        self.assertEqual(before, dump(self.lib.db))
        with self.assertRaises(Invalid):
            Store.open(self.lib.source, self.root / 'missing.sqlite')
        self.assertFalse((self.root / 'missing.sqlite').exists())

    def test_domain_effects_work_without_http_and_retry(self):
        self.store.on_effect = mock.Mock(side_effect=RuntimeError('deferred'))
        bob = self.store.entities('Bob')[0]['id']
        self.store.change(['b'], 'rejected', bob)
        with self.store.connect() as c:
            self.assertEqual(tuple(c.execute('SELECT kind,status,error FROM backend_effects').fetchone()), ('verdict','pending','deferred'))
        handler = mock.Mock()
        effects.drain(self.store, handler)
        handler.assert_called_once_with('verdict')
        effects.drain(self.store, handler)
        handler.assert_called_once()

    def test_concurrent_effect_is_dispatched_without_waiting_for_another_operation(self):
        import threading
        entered, release = threading.Event(), threading.Event()
        handled = []
        def handler(kind):
            handled.append(kind)
            if kind == 'verdict':
                entered.set()
                if not release.wait(5):
                    raise RuntimeError('测试超时')
        with self.store.connect() as c:
            c.execute("INSERT INTO backend_effects(kind) VALUES ('verdict')")
        thread = threading.Thread(target=effects.drain, args=(self.store, handler))
        thread.start()
        try:
            self.assertTrue(entered.wait(5))
            with self.store.connect() as c:
                c.execute("INSERT INTO backend_effects(kind) VALUES ('imported')")
            effects.drain(self.store, handler)
        finally:
            release.set()
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(handled, ['verdict', 'imported'])
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE status='pending'").fetchone()[0], 0)

    def test_import_deduplicates_and_preserves_human_verdict(self):
        self.store.on_effect = mock.Mock()
        bob = self.store.entities('Bob')[0]['id']
        self.store.change(['b'], 'rejected', bob)
        with self.store.connect() as c:
            before = [tuple(x) for x in c.execute('SELECT * FROM assertions')]
        p = self.package()
        with mock.patch('numpy.load', side_effect=AssertionError('unvalidated allocation')):
            first = results.import_package(self.lib, p, self.store, self.specs)
        self.assertEqual(first['status'], 'done')
        state = dump(self.lib.source)
        self.assertTrue(results.import_package(self.lib, p, self.store, self.specs)['duplicate'])
        self.assertEqual(state, dump(self.lib.source))
        with self.store.connect() as c:
            self.assertEqual(before, [tuple(x) for x in c.execute('SELECT * FROM assertions')])
        self.assertIn(mock.call('imported'), self.store.on_effect.call_args_list)

    def test_bad_checksums_ids_models_and_dimensions_never_write(self):
        for corruption in ('checksum','ids','models','shape'):
            p = self.package()
            m = json.loads((p / 'manifest.json').read_text())
            if corruption == 'checksum':
                (p / 'wd.npy').write_bytes(b'bad')
            elif corruption == 'ids':
                p = self.package(('missing',))
                m = json.loads((p / 'manifest.json').read_text())
            elif corruption == 'models':
                m['models']['wd']['version'] = 'unknown'
            else:
                np.save(p / 'ccip.npy', np.ones((2, 7), dtype='<f4'))
                raw = (p / 'ccip.npy').read_bytes()
                m['files']['ccip.npy'] = dict(bytes=len(raw), sha256=results.sha(raw))
            (p / 'manifest.json').write_text(json.dumps(m))
            before = dump(self.lib.source)
            with self.assertRaises((Invalid, ValueError)):
                results.import_package(self.lib, p, self.store, self.specs)
            self.assertEqual(before, dump(self.lib.source), corruption)

    def test_transaction_failure_rolls_back_every_asset(self):
        with sqlite3.connect(self.lib.source) as c:
            c.execute("CREATE TRIGGER reject_import BEFORE INSERT ON labels WHEN NEW.uuid='b' AND NEW.source='wd_v3' BEGIN SELECT RAISE(ABORT,'test failure'); END")
        before = dump(self.lib.source)
        with self.assertRaises(sqlite3.IntegrityError):
            results.import_package(self.lib, self.package(), self.store, self.specs)
        self.assertEqual(before, dump(self.lib.source))

    def test_failed_derived_work_is_recorded_and_resumable(self):
        self.store.on_effect = mock.Mock(side_effect=RuntimeError('learning failed'))
        response = results.import_package(self.lib, self.package(), self.store, self.specs)
        self.assertEqual(response['status'], 'pending')
        self.assertIn('learning failed', response['error'])
        self.store.on_effect = mock.Mock()
        self.assertEqual(results.resume(self.lib, self.store)['status'], 'done')

    def test_sync_import_schedules_new_provenance_without_route(self):
        runner = mock.Mock()
        self.store.on_effect = lambda kind: runner.start('new') if kind == 'imported' else None
        with sqlite3.connect(self.lib.source) as c:
            c.execute("INSERT INTO assets(uuid,filename,src_path,in_scope,trashed,preset) VALUES ('new','new.jpg','',1,0,'x')")
            for u in ('a','b','c','new'):
                c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES (?,'rating','general',1)", (u,))
        syncer = Syncer(self.store, scanner=lambda: {'new': 1, 'returned': 0, 'gone': 0, 'sources': {}})
        syncer.run()
        self.assertEqual(syncer.status()['status'], 'done')
        runner.start.assert_called_once_with('new')

    def test_following_and_nickname_are_separate_from_identity(self):
        net = mock.Mock(authorized=True, headers={'www.pixiv.net': {'Cookie': 'PHPSESSID=123_secret'}})
        net.get.return_value = {'body': {'users': [{'userId': '8', 'userName': 'Same Name'}, {'userId': '9', 'userName': 'Same Name'}], 'total': 2}}
        self.assertEqual(pixiv_following.run(self.lib, net)['candidates'], 2)
        self.assertEqual(pixiv_following.run(self.lib, net)['candidates'], 0)
        window = mock.Mock(authorized=True, headers={}, via={'www.pixiv.net': object()})   # the desktop app's window
        with mock.patch.object(accounts, '_meta', return_value={'pixiv': {'user_id': '123', 'via': 'app'}}):
            self.assertEqual(accounts.pixiv_user_id(window), '123')
            self.assertNotEqual(pixiv_following.run(self.lib, window)['stopped'], '需要绑定 Pixiv 账号')
        with mock.patch.object(accounts, '_meta', return_value={}):
            self.assertEqual(pixiv_following.run(self.lib, window)['stopped'], '需要绑定 Pixiv 账号')
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM artists').fetchone()[0], 2)
            self.assertEqual(c.execute('SELECT count(*) FROM image_artists').fetchone()[0], 0)
        net.get.return_value = {'body': {'name': 'Updated'}}
        self.assertEqual(nicknames.run(self.lib, net)['updated'], 2)
        with db.connect(self.lib) as c:
            self.assertEqual({x[0] for x in c.execute('SELECT name FROM artist_names')}, {'Updated'})

    def test_requests_never_sent_leave_no_back_off(self):
        """An expired login found before the run stops Pixiv; that is not an error of the target (2026-09-28 run 12)."""
        from provenance.net import Net
        with db.connect(self.lib) as c:
            db.artist_for(c, [('pixiv', '8', '')], 'test')
        net = Net(1, {'www.pixiv.net': {'Cookie': 'PHPSESSID=123_secret'}}, opener=lambda *a, **k: self.fail('request sent'))
        net.authorized = True
        net.stopped['www.pixiv.net'] = 'Pixiv 登录已过期'
        self.assertIn('登录已过期', pixiv_following.run(self.lib, net)['stopped'])
        result = nicknames.run(self.lib, net)
        self.assertEqual((result['failed'], result['updated']), (0, 0))
        self.assertIn('登录已过期', result['stopped'])
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM lookups WHERE last_result='error'").fetchone()[0], 0)
            self.assertTrue(evidence.due(c, 'following:123', 'pixiv_following'))

    def test_backing_off_after_an_error_says_so(self):
        from provenance.net import Stopped
        net = mock.Mock(authorized=True, headers={'www.pixiv.net': {'Cookie': 'PHPSESSID=123_secret'}})
        net.get.side_effect = Stopped('www.pixiv.net: HTTP 403')
        self.assertIn('403', pixiv_following.run(self.lib, net)['stopped'])
        net.get.reset_mock()
        again = pixiv_following.run(self.lib, net)              # within the hour: not asked, but not silent either
        net.get.assert_not_called()
        self.assertRegex(again['stopped'], r'Pixiv 关注：上次请求出错，约 (59|60) 分钟后自动重试')
        with db.connect(self.lib) as c:
            c.execute("UPDATE lookups SET last_result='hit'")
        self.assertEqual(pixiv_following.run(self.lib, net)['stopped'], '')   # listed recently: nothing to say

    def test_new_sources_refuse_network_without_authorization(self):
        net = mock.Mock(authorized=False, headers={'www.pixiv.net': {'Cookie': 'PHPSESSID=123_secret'}})
        self.assertIn('未授权', pixiv_following.run(self.lib, net)['stopped'])
        net.get.assert_not_called()


class ProcessTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.lib = library.create(Path(self.temp.name) / 'A.pilib')
        self.runners = []

    def tearDown(self):
        for runner in self.runners:
            runner.close()
        self.temp.cleanup()

    def runner(self, script):
        r = FakeRunner(self.lib, script)
        self.runners.append(r)
        return r

    def test_cross_instance_duplicate_lock_and_crash_recovery(self):
        a = self.runner('import time;time.sleep(60)')
        a.start()
        b = self.runner('pass')
        self.assertFalse(b.start())
        a.close()
        with db.connect(self.lib) as c:
            c.execute("INSERT INTO runs(kind,steps) VALUES ('full','local_match')")
        c = self.runner('pass')
        self.assertEqual(c.status()['status'], 'failed')
        with db.connect(self.lib) as connection:
            self.assertEqual(connection.execute("SELECT count(*) FROM runs WHERE status='running'").fetchone()[0], 0)

    def test_busy_incremental_request_is_durable_and_coalesced(self):
        r = self.runner('import time;time.sleep(.2);print(\'{"done":"done"}\',flush=True)')
        r.start()
        self.assertFalse(r.start('new'))
        self.assertFalse(r.start('new'))
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM pending_runs').fetchone()[0], 1)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            with db.connect(self.lib) as c:
                count = c.execute("SELECT count(*) FROM runs WHERE status='done'").fetchone()[0]
            if count == 2:
                break
            time.sleep(.03)
        self.assertEqual(count, 2)
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM pending_runs').fetchone()[0], 0)

    def test_unauthorized_worker_records_stop_without_reading_credentials(self):
        from provenance import run
        with mock.patch.object(run.accounts, 'headers', side_effect=AssertionError('credentials read')), mock.patch('builtins.print'):
            self.assertEqual(run.main(['--library', str(self.lib.path), '--steps', 'pixel_hash', '--json']), 0)
        with db.connect(self.lib) as c:
            status, reason = c.execute('SELECT status,reason FROM runs').fetchone()
        self.assertEqual(status, 'stopped')
        self.assertIn('未授权', reason)

    def test_timeout_and_force_close_reap_stubborn_worker(self):
        for timeout in (True, False):
            r = self.runner('import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print("ready",flush=True);time.sleep(60)')
            r.timeout = .2 if timeout else 30
            with mock.patch('web.provenance_job.STOP_GRACE', .1):
                r.start()
                time.sleep(.1)
                if timeout:
                    wait(r)
                r.close()
            self.assertIsNotNone(r.proc.returncode)
            self.assertEqual(r.status()['status'], 'failed' if timeout else 'stopped')
            with db.connect(self.lib) as c:
                self.assertEqual(c.execute('SELECT status FROM runs WHERE id=?', (r.run_id,)).fetchone()[0], r.status()['status'])

    def test_force_close_sends_one_hard_kill_before_reaping(self):
        """确定复现二次 killpg 的竞态：第二次强制信号报 EPERM，不能打断进程回收。"""
        r = self.runner('import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print("ready",flush=True);time.sleep(60)')
        original, hard = r._signal, []

        def signal_once(proc, sig):
            if sig == getattr(signal, 'SIGKILL', signal.SIGTERM):
                hard.append(sig)
                if len(hard) > 1:
                    raise PermissionError('模拟已消失的进程组')
            return original(proc, sig)

        with mock.patch.object(r, '_signal', side_effect=signal_once), mock.patch('web.provenance_job.STOP_GRACE', .02):
            self.assertTrue(r.start())
            deadline = time.monotonic() + 5
            while r.status().get('text') != 'ready' and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertEqual(r.status().get('text'), 'ready')
            r.close()
        self.assertEqual(len(hard), 1)
        self.assertIsNotNone(r.proc.returncode)
        self.assertEqual(r.status()['status'], 'stopped')

    def test_long_run_with_progress_is_not_timed_out(self):
        """The timeout counts silence, not total time: a first pass over a big library takes long."""
        r = self.runner('import json,time\nfor i in range(8):\n    print(json.dumps({"log": i}), flush=True); time.sleep(.1)\n'
                        'print(json.dumps({"done": "done"}), flush=True)')
        r.timeout = .4
        r.start()
        wait(r)
        r.close()
        self.assertEqual(r.status()['status'], 'done')
        r = self.runner('import time\nprint("ready", flush=True)\ntime.sleep(60)')
        r.timeout = .4
        with mock.patch('web.provenance_job.STOP_GRACE', .1):
            r.start()
            wait(r)
            r.close()
        self.assertEqual((r.status()['status'], r.status()['reason']), ('failed', provenance_job.TIMED_OUT))

    def test_done_event_does_not_hide_nonzero_exit(self):
        r = self.runner("import json;print(json.dumps(dict(done='done')),flush=True);raise SystemExit(7)")
        r.start()
        wait(r)
        r.close()
        self.assertEqual(r.status()['status'], 'failed')

    @unittest.skipIf(os.name == 'nt', 'POSIX parent-death watchdog')
    def test_parent_crash_kills_worker_tree_and_recovers_record(self):
        marker = Path(self.temp.name) / 'tree.json'
        worker = "\n".join([
            'import os,subprocess,sys,time,json',
            'from pathlib import Path',
            'import tests.reference  # provenance.run lives in the test reference (#186)',
            'from provenance import run',
            'def hang(*args,**kwargs):',
            '    child=subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"])',
            '    Path(' + repr(str(marker)) + ').write_text(json.dumps([os.getpid(),child.pid]))',
            '    time.sleep(60)',
            '    return {}',
            'run.STEPS["local_match"]=hang',
            'run.main(["--library",' + repr(str(self.lib.path)) + ',"--steps","local_match","--run-id","1","--owner-pid",str(os.getppid()),"--kind","cli","--json"])'])
        parent = "\n".join([
            'import time',
            'from web import library',
            'from tests.test_provenance_job import FakeRunner',
            'r=FakeRunner(library.open_library(' + repr(str(self.lib.path)) + '),' + repr(worker) + ')',
            'assert r.start()',
            'time.sleep(60)'])
        p = subprocess.Popen([sys.executable, '-c', parent], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        ids = []
        try:
            deadline = time.monotonic() + 10
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.03)
            self.assertTrue(marker.exists(), 'worker did not start')
            ids = json.loads(marker.read_text())
            p.kill(); p.wait(5)
            for pid in ids:
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    try:
                        os.kill(pid, 0)
                    except ProcessLookupError:
                        break
                    time.sleep(.03)
                else:
                    self.fail('process survived its owner: ' + str(pid))
            recovered = self.runner('pass')
            self.assertEqual(recovered.status()['status'], 'failed')
        finally:
            if p.poll() is None:
                p.kill(); p.wait(5)
            if ids:
                try:
                    os.killpg(ids[0], signal.SIGKILL)
                except ProcessLookupError:
                    pass

    @unittest.skipIf(os.name == 'nt', 'POSIX process-group test')
    def test_grandchild_holding_pipes_does_not_hang_close(self):
        marker = Path(self.temp.name) / 'pid'
        script = 'import subprocess,sys;from pathlib import Path;p=subprocess.Popen([sys.executable,"-c","import time;time.sleep(60)"]);Path(' + repr(str(marker)) + ').write_text(str(p.pid))'
        r = self.runner(script)
        r.start()
        wait(r)
        r.close()
        pid = int(marker.read_text())
        for _ in range(50):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(.02)
        else:
            self.fail('grandchild survived')


if __name__ == '__main__':
    unittest.main()
