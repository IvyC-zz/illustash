"""App 里运行溯源：子进程的 JSON 进度、停止、出错，以及 provenance.run 的运行记录。只用临时目录，不联网。"""
import json
import sqlite3
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

from web import library
from web.provenance_job import Runner
from provenance import db, run as prun


def setUpModule():   # the worker appends every log line to a file under Application Support: keep tests out of the real one
    import tempfile as _tempfile
    from unittest import mock as _mock
    from provenance import run as _run
    folder = _tempfile.TemporaryDirectory()
    patch = _mock.patch.object(_run, 'LOG', __import__('pathlib').Path(folder.name) / 'provenance.log')
    patch.start()
    unittest.addModuleCleanup(patch.stop)
    unittest.addModuleCleanup(folder.cleanup)


class FakeRunner(Runner):
    def __init__(self, library, script):
        super().__init__(library, on_finish=self.finished)
        self.script, self.done = script, threading.Event()

    def command(self, kind, steps, budget):
        return [sys.executable, '-c', self.script]

    def finished(self):
        self.done.set()


def wait(runner, timeout=10):
    end = time.time() + timeout
    while runner.running() and time.time() < end:
        time.sleep(0.05)
    return runner.status()


def subprocess_module():
    import web.provenance_job as job
    return job.subprocess


class FakePopen:
    """Stands in for the worker process: exits at once with a done event."""
    def __init__(self, cmd):
        import io
        self.cmd, self.pid, self.returncode = cmd, 999999, None
        self.stdout, self.stderr = io.StringIO('{"done": "done"}\n'), io.StringIO('')

    def poll(self):
        self.returncode = 0
        return 0

    def wait(self, timeout=None):
        return 0


class RunnerTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.lib = library.create(Path(self.temp.name).resolve() / 'A.pilib')

    def tearDown(self):
        self.temp.cleanup()

    def test_progress_and_outcome_from_json_lines(self):
        script = ('import json\n'
                  'for e in [{"step_start": "pixel_hash", "index": 0, "total": 2}, {"log": "像素哈希：已查 50/100"},'
                  ' {"step": "pixel_hash", "result": {"hits": 3}}, {"done": "done", "reason": "", "found": 3, "traffic_mb": 0.2}]:\n'
                  '    print(json.dumps(e, ensure_ascii=False), flush=True)\n')
        r = FakeRunner(self.lib, script)
        self.assertTrue(r.start('full'))
        s = wait(r)
        self.assertEqual((s['status'], s['found'], s['kind']), ('done', 3, 'full'))
        self.assertEqual(s['results'], [{'step': 'pixel_hash', 'result': {'hits': 3}}])
        self.assertTrue(r.done.wait(5))                     # found something: the core is told
        with self.assertRaises(ValueError):
            r.start('everything')

    def test_progress_numbers_are_kept_for_the_bars(self):
        r = FakeRunner(self.lib, '')
        r.state = {'status': 'running'}
        r._event(json.dumps({'step_start': 'verify', 'index': 4, 'total': 8}))
        r._event(json.dumps({'progress': {'label': '画师', 'done': 3, 'total': 40, 'found': 12, 'mb': 7.5, 'bad': [1]}}))
        r._event(json.dumps({'log': '本地比对：第 4/40 位画师'}))
        s = r.status()
        self.assertEqual(s['progress'], {'label': '画师', 'done': 3, 'total': 40, 'found': 12, 'mb': 7.5})
        self.assertEqual(s['text'], '本地比对：第 4/40 位画师')             # the text line is unchanged
        r._event(json.dumps({'step_start': 'nicknames', 'index': 7, 'total': 8}))
        self.assertNotIn('progress', r.status())                           # a new step starts without old numbers
        r._event(json.dumps({'side': {'step': 'iqdb', 'text': 'IQDB：已查 50/3000'}}))
        r._event(json.dumps({'side': {'step': 'iqdb', 'progress': {'label': '图片', 'done': 50, 'total': 3000}}}))
        self.assertEqual(r.status()['side'], {'step': 'iqdb', 'name': 'IQDB 查相似图', 'text': 'IQDB：已查 50/3000',
                                              'progress': {'label': '图片', 'done': 50, 'total': 3000}})
        self.assertEqual(r.status()['text'], '更新画师昵称…')               # the main line is left alone
        r._event(json.dumps({'side_done': 'iqdb', 'result': {'asked': 3000}}))
        self.assertNotIn('side', r.status())

    def test_a_lookup_goes_before_the_background_run(self):
        """按需查找: the background run stops (a sync's run is queued again); the lookup carries what was picked."""
        for kind in ('full', 'new'):
            r = FakeRunner(self.lib, 'import time\nprint("{\\"log\\": \\"working\\"}", flush=True)\ntime.sleep(30)\n')
            with mock.patch('web.provenance_job.STOP_GRACE', .2):
                self.assertTrue(r.start(kind))
                time.sleep(.2)
                self.assertFalse(r.start('pick', allow_network=True, only=['a']))   # busy
                self.assertTrue(r.preempt())
                with db.connect(self.lib) as c:                          # a sync's run waits for the lookup, then comes back
                    self.assertEqual(bool(c.execute("SELECT 1 FROM pending_runs WHERE kind='new'").fetchone()), kind == 'new')
                    c.execute('DELETE FROM pending_runs')
                r.command = Runner.command.__get__(r)
                with mock.patch.object(subprocess_module(), 'Popen', side_effect=lambda cmd, **k: FakePopen(cmd)) as popen:
                    self.assertTrue(r.start('pick', allow_network=True, only=['a', 'b']))
                cmd = popen.call_args[0][0]
                self.assertEqual(cmd[cmd.index('--only') + 1], 'a,b')
                self.assertIn('--allow-network', cmd)
                self.assertEqual(cmd[cmd.index('--steps') + 1], 'pixel_hash,iqdb,groups')
                r.close()

    def test_without_authorisation_local_steps_after_network_ones_still_run(self):
        ran = []
        def step(name):
            def run(lib, net, limit=None, log=print, **_):
                ran.append(name)
                return {'stopped': ''}
            return run
        with mock.patch.dict(prun.STEPS, {'local_match': step('local_match'), 'pixel_hash': step('pixel_hash'), 'groups': step('groups')}), mock.patch('builtins.print'):
            prun.main(['--library', str(self.lib.path), '--steps', 'local_match,pixel_hash,groups', '--json'])
        self.assertEqual(ran, ['local_match', 'groups'])
        with db.connect(self.lib) as c:
            self.assertEqual(tuple(c.execute('SELECT status, reason FROM runs').fetchone()), ('stopped', '外部请求未授权'))

    def test_a_pick_run_narrows_its_steps(self):
        from provenance.net import Net as RealNet
        seen = {}
        def step(name):
            def run(lib, net, limit=None, log=print, **narrow):
                seen[name] = narrow
                return {'stopped': ''}
            return run
        with mock.patch.dict(prun.STEPS, {'pixel_hash': step('pixel_hash'), 'iqdb': step('iqdb'), 'verify': step('verify')}), \
             mock.patch.object(prun, 'net_for', lambda mb: RealNet(mb)), mock.patch('builtins.print'):
            prun.main(['--library', str(self.lib.path), '--steps', 'pixel_hash,iqdb', '--only', 'a,b', '--allow-network', '--json'])
            prun.main(['--library', str(self.lib.path), '--steps', 'verify', '--artists', 'X', '--allow-network', '--json'])
        self.assertEqual(seen, {'pixel_hash': {'only': ['a', 'b']}, 'iqdb': {'only': ['a', 'b']}, 'verify': {'artists': ['X']}})

    def test_a_step_runs_beside_the_next_ones(self):
        """IQDB (danbooru.donmai.us) runs while later steps use other hosts; the run waits for it and records it."""
        import io, threading
        from provenance.net import Net as RealNet
        started, release, lines = threading.Event(), threading.Event(), []
        def slow(lib, net, limit=None, log=print, stop=None):
            started.set()
            log('IQDB：已查 1/1')
            release.wait(5)
            return {'asked': 1, 'hits': 1, 'stopped': ''}
        def next_step(lib, net, limit=None, log=print):
            self.assertTrue(started.wait(5))                              # already going when this one starts
            release.set()
            return {'candidates': 0, 'stopped': ''}
        with mock.patch.dict(prun.STEPS, {'iqdb': slow, 'x_following': next_step}), \
             mock.patch.object(prun, 'net_for', lambda mb: RealNet(mb)), \
             mock.patch('builtins.print', lambda text, **k: lines.append(text)):
            self.assertEqual(prun.main(['--library', str(self.lib.path), '--steps', 'iqdb,x_following', '--allow-network', '--json']), 0)
        events = [json.loads(l) for l in lines]
        self.assertIn({'side': {'step': 'iqdb', 'text': 'IQDB：已查 1/1'}}, events)
        self.assertFalse(any(e.get('step_start') == 'iqdb' for e in events))
        self.assertTrue(any(e.get('side_done') == 'iqdb' for e in events))
        with db.connect(self.lib) as c:
            results = json.loads(c.execute('SELECT results FROM runs').fetchone()[0])
        self.assertEqual(set(results), {'1.iqdb', '2.x_following'})

    def test_waiting_for_iqdb_replaces_finished_pixiv_text(self):
        from provenance.net import Net as RealNet
        release = threading.Event()
        runner = FakeRunner(self.lib, '')
        events = []

        def slow(lib, net, **kwargs):
            if not release.wait(5):
                return {'failed': 1}
            return {'asked': 1}

        def receive(text, **kwargs):
            event = json.loads(text)
            events.append(event)
            runner._event(text)
            if event.get('log') == '等待 IQDB 查相似图完成，再进行分组推荐…':
                self.assertEqual(runner.status()['text'], event['log'])
                release.set()

        def groups(lib, net, **kwargs):
            self.assertTrue(release.is_set())
            self.assertEqual(runner.status()['text'], '分组推荐…')
            return {}

        with mock.patch.dict(prun.STEPS, {'iqdb': slow, 'pixiv_following': lambda *a, **k: {}, 'groups': groups}), \
             mock.patch.object(prun, 'net_for', lambda mb: RealNet(mb)), \
             mock.patch.object(prun.accounts, 'pixiv_bound', return_value=False), \
             mock.patch('builtins.print', receive):
            self.assertEqual(prun.main(['--library', str(self.lib.path), '--steps',
                                       'iqdb,pixiv_following,groups', '--allow-network', '--json']), 0)
        self.assertTrue(release.is_set())
        waiting = next(i for i, e in enumerate(events) if e.get('log', '').startswith('等待 IQDB'))
        pixiv_done = next(i for i, e in enumerate(events) if e.get('step') == 'pixiv_following')
        groups_start = next(i for i, e in enumerate(events) if e.get('step_start') == 'groups')
        self.assertLess(pixiv_done, waiting)
        self.assertLess(waiting, groups_start)

    def test_one_run_at_a_time_and_stop(self):
        r = FakeRunner(self.lib, 'import time\nprint("{\\"log\\": \\"working\\"}", flush=True)\ntime.sleep(30)\n')
        self.assertTrue(r.start('new'))
        self.assertFalse(r.start('full'))
        time.sleep(0.3)
        self.assertTrue(r.stop())
        s = wait(r)
        self.assertEqual((s['status'], s['reason']), ('stopped', '已手动停止'))

    def test_a_crash_is_reported(self):
        r = FakeRunner(self.lib, 'raise SystemExit("坏了")')
        r.start('full')
        s = wait(r)
        self.assertEqual((s['status'], s['reason']), ('failed', '坏了'))


class RunRecordTest(unittest.TestCase):
    def test_every_run_is_recorded_with_coverage(self):
        with tempfile.TemporaryDirectory() as temp:
            lib = library.create(Path(temp).resolve() / 'A.pilib')
            out = []
            args = ['--library', str(lib.path), '--steps', 'report', '--kind', 'full', '--json']
            with mock.patch('builtins.print', lambda line, **k: out.append(line)), mock.patch.object(prun.accounts, 'headers', lambda: {}):
                with self.assertRaises(sqlite3.OperationalError):       # no source database yet: recorded, then raised
                    prun.main(args)
                with db.connect(lib) as c:
                    self.assertEqual(c.execute('SELECT status FROM runs').fetchone()[0], 'failed')
                    c.execute('DELETE FROM runs')
                with sqlite3.connect(lib.source) as c:
                    c.execute('CREATE TABLE assets(uuid TEXT, preset TEXT, source_id TEXT, src_path TEXT, in_scope INTEGER, trashed INTEGER)')
                out.clear()
                self.assertEqual(prun.main(args), 0)
            events = [json.loads(line) for line in out]
            self.assertEqual(events[0], {'step_start': 'report', 'index': 0, 'total': 1})
            self.assertEqual(events[-1]['done'], 'done')
            with db.connect(lib) as c:
                row = c.execute('SELECT kind,status,found,report FROM runs').fetchone()
            self.assertEqual((row['kind'], row['status'], row['found']), ('full', 'done', 0))
            self.assertIn('numbers', json.loads(row['report']))


if __name__ == '__main__':
    unittest.main()
