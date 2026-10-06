"""Run provenance for the open library and follow its progress (docs/PROVENANCE.md section 8, 接进 App).

The core never imports the worker: it starts the reference `provenance.run --json` as a child process, reads one JSON
event per line, and reads what the worker wrote from provenance.sqlite (tests/reference/web/artists.py). Once the library's artists
live in the terms core (docs/TERMS_ARTISTS.md), every finished run is handed over as an evidence package instead
(`deliver`: tests/reference/provenance/export.py writes it, the core imports it). Stopping sends SIGTERM (the
worker records the run as stopped), then kills the process group if it has not gone within STOP_GRACE seconds.
A run has no total time limit (a first pass over a large library takes long); it is stopped as timed out when the
worker prints nothing for IDLE_TIMEOUT seconds. Every long step reports progress well within that.
"""
import collections
from provenance import db, export
from provenance.lifecycle import Lease, recover, finish
import json
import os
import shutil
import signal
import sqlite3
import subprocess
import sys
import threading
import time
from pathlib import Path

from web import terms_core

ROOT = Path(__file__).resolve().parents[3]
RUN = 'import runpy, tests.reference; runpy.run_module("provenance.run", run_name="__main__", alter_sys=True)'
# What each kind of run does, and its traffic cap (MB). A manual run may use 1 GB (the user's choice, 2026-09-29): each run
# is authorised on its own in the settings page, which states the cap before anything goes out.
KINDS = {
# In the background only the cheap, picture-by-picture steps (2026-09-29, the user's choice): verifying an artist's works
# is hundreds of thumbnails for a few pictures, so it runs on demand (「找 TA 的其他图」), for the artist the person asked about.
    'full': ('local_match,pixel_hash,iqdb,x_following,pixiv_following,groups', 1024),   # 设置页「查找画师」
    'new': ('local_match,pixel_hash,iqdb,groups', 50),                                       # after a sync brought new pictures
    'pick': ('pixel_hash,iqdb,groups', 20),     # 按需查找: the selected pictures, a few seconds (the click authorises it)
    'artist': ('verify,groups', 200),           # 找 TA 的其他图: one artist's works against the library
}
STEP_NAMES = {'ocr': '识别图上的签名', 'groups': '分组推荐', 'local_match': '本地重比新图', 'pixel_hash': '像素哈希查 Danbooru', 'iqdb': 'IQDB 查相似图', 'x_following': '对应 X 关注',
              'pixiv_following': '读取 Pixiv 关注', 'verify': '本地比对画师作品', 'pixiv_related': 'Pixiv 相关画师',
              'nicknames': '更新画师昵称', 'report': '统计覆盖'}
STOP_GRACE = 10
IDLE_TIMEOUT = 600
TIMED_OUT = f'运行超时（{IDLE_TIMEOUT // 60} 分钟没有进展）'


class Runner:
    def __init__(self, library, python=sys.executable, on_finish=None, timeout=IDLE_TIMEOUT, allow_network=False):
        self.library = library
        self.python = python
        self.on_finish = on_finish   # e.g. drop cached counts once new artists landed
        self.lock = threading.RLock()
        self.proc = None
        self.state = {'status': 'idle'}
        self.timeout, self.allow_network = timeout, allow_network   # timeout: seconds without any output from the worker
        self.last_output = 0.0
        self.thread = None
        self.closed = False
        self.pending_new = False
        self.run_id = None
        self.delivery = None             # the last hand-over to the terms core: {'status': 'done'|'failed', ...}
        self.deliver_lock = threading.Lock()
        self.lease = Lease(library)
        if self.lease.acquire():
            try:
                recover(library)
                with db.connect(library) as c:
                    row = c.execute('SELECT status,kind,reason,finished_at FROM runs ORDER BY id DESC LIMIT 1').fetchone()
                    if row:
                        self.state = dict(row)
            finally:
                self.lease.close()

    def status(self):
        with self.lock:
            return dict(self.state)

    def running(self):
        with self.lock:
            return self.state.get('status') in ('running', 'stopping')

    def _queue_new(self):
        self.pending_new = True
        with db.connect(self.library) as c:
            c.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')")

    def resume_pending(self):
        with db.connect(self.library) as c:
            pending = c.execute("SELECT 1 FROM pending_runs WHERE kind='new'").fetchone()
        return self.start('new') if pending else False

    def start(self, kind='full', *, allow_network=None, only=None, artists=None):
        """False when busy; incremental requests are coalesced durably instead of lost.
        `only` / `artists` narrow a pick / artist run (按需查找) to what the person chose."""
        if kind not in KINDS:
            raise ValueError('未知的运行方式')
        steps, budget = KINDS[kind]
        with self.lock:
            if self.closed:
                return False
            if (self.thread and self.thread.is_alive()) or self.state.get('status') in ('running', 'stopping'):
                if kind == 'new':
                    self._queue_new()
                return False
            if not self.lease.acquire():
                if kind == 'new':
                    self._queue_new()
                return False
            try:
                recover(self.library)
                with db.connect(self.library) as c:
                    self.run_id = c.execute('INSERT INTO runs(kind,steps) VALUES (?,?)', (kind, steps)).lastrowid
                    if kind == 'new':
                        c.execute("DELETE FROM pending_runs WHERE kind='new'")
                self._run_allow_network = self.allow_network if allow_network is None else allow_network is True
                self.holding = False     # a lookup is starting: the run it made way for comes back after it
                self._narrow = [*(['--only', ','.join(only)] if only else []), *(['--artists', ','.join(artists)] if artists else [])]
                self.state = {'status': 'running', 'kind': kind, 'started_at': time.strftime('%Y-%m-%d %H:%M:%S'),
                              'budget_mb': budget, 'step': '', 'index': 0, 'total': len(steps.split(',')), 'text': '正在启动…'}
                self.proc = subprocess.Popen(self.command(kind, steps, budget), cwd=ROOT, stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE, text=True, encoding='utf-8', start_new_session=True,
                    **({'pass_fds': (self.lease.file.fileno(),)} if os.name != 'nt' else {}))
            except Exception as exc:
                self.state = {'status': 'failed', 'kind': kind, 'reason': f'无法启动溯源进程：{exc}'}
                try:
                    if self.run_id is not None:
                        finish(self.library, self.run_id, 'failed', self.state['reason'])
                finally:
                    self.lease.close()
                return False
            self.thread = threading.Thread(target=self._follow, args=(self.proc,), daemon=True)
            self.thread.start()
        return True

    def deliver(self):
        """Hand provenance's results to the terms core as one evidence package (a full snapshot; the core drops what it
        already has). Only once the core owns this library's artists; None otherwise. A failure is recorded and printed,
        never raised: the next run or start hands over the snapshot again."""
        lib_db = Path(self.library.path) / 'library.sqlite'
        if not lib_db.exists() or not db.path_of(self.library).exists():
            return None
        with sqlite3.connect(lib_db.resolve().as_uri() + '?mode=ro', uri=True, timeout=10) as c:
            if not terms_core.artists_in_core(c):
                return None
        with self.deliver_lock:
            out = Path(self.library.path) / '.evidence' / f'provenance-{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}-{threading.get_ident()}'
            try:
                export.write(self.library, out)
                self.delivery = {'status': 'done', **terms_core.import_package(lib_db, self.library.config['id'], out)}
            except Exception as exc:  # noqa: BLE001 - recorded for the settings page, retried on the next hand-over
                self.delivery = {'status': 'failed', 'error': str(exc)}
                print(f'溯源结果交给词条核心失败：{exc}', file=sys.stderr, flush=True)
            finally:
                shutil.rmtree(out, ignore_errors=True)
            return self.delivery

    def command(self, kind, steps, budget):
        # provenance.run lives in tests/reference (#186); the child enables that package path before running it.
        return [self.python, '-u', '-c', RUN, '--library', str(self.library.path), '--steps', steps,
                '--budget-mb', str(budget), '--kind', kind, '--json', '--run-id', str(self.run_id),
                '--owner-pid', str(os.getpid()), *(['--allow-network'] if self._run_allow_network else []), *getattr(self, '_narrow', [])]

    def preempt(self, timeout=20):
        """按需查找 goes before a background run: stop that run (it picks up where it stopped next time) and wait for it.
        A run queued after a sync comes back by itself when the lookup is done."""
        with self.lock:
            kind, thread = self.state.get('kind'), self.thread
            busy = thread and thread.is_alive()
        if not busy or kind in ('pick', 'artist'):
            return not busy
        with self.lock:
            self.holding = True   # the stopped run must not start again before the lookup (see _follow)
        if kind == 'new':
            self._queue_new()
        self.stop()
        thread.join(timeout)
        return not thread.is_alive()

    def stop(self):
        with self.lock:
            if not self.proc or self.state.get('status') != 'running':
                return False
            self.state.update(status='stopping', text='正在停止…', reason='已手动停止')
            proc = self.proc
        self._signal(proc, signal.SIGTERM)
        return True

    @staticmethod
    def _signal(proc, sig):
        try:
            if os.name == 'nt':
                subprocess.run(['taskkill', '/PID', str(proc.pid), '/T', '/F'], capture_output=True)
            else:
                os.killpg(proc.pid, sig)
        except ProcessLookupError:
            pass

    def close(self):
        """A library switch cannot proceed until the worker and its descendants are reaped."""
        with self.lock:
            self.closed = True
        self.stop()
        thread = self.thread
        if thread and thread is not threading.current_thread():
            thread.join(self.timeout + STOP_GRACE + 10)
            if thread.is_alive():
                raise RuntimeError('溯源进程尚未清理，已阻止切换图库')

    def _event(self, line):
        try:
            e = json.loads(line)
        except ValueError:
            e = {'log': line}
        if not isinstance(e, dict):
            return
        with self.lock:
            s = self.state
            if 'step_start' in e:
                s.update(step=e['step_start'], index=e.get('index', 0), total=e.get('total', s.get('total')),
                         text=STEP_NAMES.get(e['step_start'], e['step_start']) + '…')
                s.pop('progress', None)
            elif isinstance(e.get('side'), dict):   # a step running beside the main one (tests/reference/provenance/run.py BESIDE)
                side = s.setdefault('side', {})
                side['step'] = str(e['side'].get('step', ''))
                side['name'] = STEP_NAMES.get(side['step'], side['step'])
                if 'text' in e['side']:
                    side['text'] = str(e['side']['text'])[:200]
                if isinstance(e['side'].get('progress'), dict):
                    side['progress'] = {k: v for k, v in e['side']['progress'].items() if isinstance(v, (int, float, str))}
            elif 'side_done' in e:
                s.pop('side', None)
                s.setdefault('results', []).append({'step': e['side_done'], 'result': e.get('result')})
            elif isinstance(e.get('progress'), dict):   # numbers for the progress bars (the text line stays as is)
                s['progress'] = {k: v for k, v in e['progress'].items() if isinstance(v, (int, float, str))}
            elif 'log' in e:
                s['text'] = str(e['log'])[:200]
            elif 'step' in e:
                s.setdefault('results', []).append({'step': e['step'], 'result': e.get('result')})
            elif 'done' in e:
                s.update(outcome=e['done'], reason=e.get('reason', ''), found=e.get('found', 0), traffic_mb=e.get('traffic_mb', 0))

    def _follow(self, proc):
        tail = collections.deque(maxlen=40)
        def read_events():
            for line in proc.stdout:
                self.last_output = time.monotonic()
                if line.strip():
                    self._event(line.strip())
        readers = [threading.Thread(target=read_events, daemon=True),
                   threading.Thread(target=lambda: tail.extend(proc.stderr), daemon=True)]
        for reader in readers:
            reader.start()
        self.last_output, stopping_at = time.monotonic(), None
        while proc.poll() is None:
            with self.lock:
                if self.state['status'] == 'stopping' and stopping_at is None:
                    stopping_at = time.monotonic()
                if time.monotonic() - self.last_output >= self.timeout and stopping_at is None:
                    self.state.update(status='stopping', reason=TIMED_OUT, timed_out=True)
                    stopping_at = time.monotonic()
                    self._signal(proc, signal.SIGTERM)
            if stopping_at is not None and time.monotonic() - stopping_at >= STOP_GRACE:
                # 强制信号统一在循环后发送一次；连续 killpg 会与进程组消失竞争，阻断 wait 收尾。
                break
            time.sleep(0.05)
        # A child can exit while a grandchild keeps stdout/stderr open. Kill the group BEFORE draining.
        self._signal(proc, getattr(signal, 'SIGKILL', signal.SIGTERM))
        code = proc.wait()
        for reader in readers:
            reader.join(5)
        err = ''.join(tail)
        with self.lock:
            s = dict(self.state)
            outcome = s.pop('outcome', None)
            if s.get('timed_out'):
                s.update(status='failed', reason=TIMED_OUT)
            elif s.get('status') == 'stopping':
                s.update(status='stopped', reason='已手动停止')
            elif code != 0 and outcome == 'failed' and s.get('reason'):
                s['status'] = 'failed'
            elif code != 0 or outcome not in ('done', 'stopped', 'failed'):
                s.update(status='failed', reason=(err.strip().splitlines() or [f'进程退出（{code}）'])[-1][:300])
            else:
                s['status'] = outcome
            s['finished_at'] = time.strftime('%Y-%m-%d %H:%M:%S')
            s.pop('text', None)
            found = s.get('found', 0)
            status, reason = s['status'], s.get('reason', '')
        try:
            # Parent owns final process outcome, including a crash after the worker's done event.
            with db.connect(self.library) as c:
                c.execute("UPDATE runs SET status=?,reason=?,finished_at=datetime('now') WHERE id=?",
                          (status, reason, self.run_id))
        finally:
            proc.stdout.close()
            proc.stderr.close()
            self.lease.close()
        try:
            self.deliver()   # stopped and failed runs may have found artists too
            if self.on_finish and found:
                self.on_finish()
        finally:
            with self.lock:
                with db.connect(self.library) as c:
                    queued = c.execute("SELECT 1 FROM pending_runs WHERE kind='new'").fetchone()
                pending = (self.pending_new or queued) and not self.closed and not getattr(self, 'holding', False)
                self.pending_new = False
                self.thread = None
                self.state = s  # Publish terminal status only AFTER persistence, callbacks and all cleanup.
                if pending:
                    self.start('new')
