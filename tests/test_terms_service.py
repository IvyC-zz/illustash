"""常驻核心的事务、并发、超时与崩溃边界；只用临时库。"""
import json
import os
import sqlite3
import sys
import threading
import time
import unittest
from unittest import mock

from tests.terms_commands_fixture import CoreCommands
from web import terms_core


class Service(CoreCommands):
    policy = 'hybrid'
    labels = True

    def setUp(self):
        super().setUp()
        terms_core.close_service()
        self.addCleanup(terms_core.close_service)
        self.env = mock.patch.dict(os.environ, {'ILLUSTASH_TERMS_DAEMON': '1'})
        self.env.start()
        self.addCleanup(self.env.stop)

    def test_process_is_reused_across_commands_and_refusals(self):
        self.both(lambda: self.api.change(['a'], 'confirmed', self.alice), 'change',
                  asked=['a'], action='confirmed', entity=self.alice)
        pid = terms_core._service.process.pid
        self.refused(lambda: self.api.change(['missing'], 'confirmed', self.alice), 'change',
                     asked=['missing'], action='confirmed', entity=self.alice)
        self.assertEqual(terms_core._service.process.pid, pid)
        self.both(lambda: self.api.answer(self.bob, yes=['b']), 'answer',
                  entity=self.bob, yes=['b'], no=[], later=[], kind='seed', switch=None)
        self.assertEqual(terms_core._service.process.pid, pid)
        self.undo_all()

    def test_sql_failure_rolls_back_and_same_process_can_continue(self):
        with sqlite3.connect(self.new.path) as c:
            c.execute("CREATE TRIGGER fail_projection BEFORE INSERT ON memberships BEGIN SELECT RAISE(ABORT,'测试事务失败'); END")
        before = self.dump()
        with self.assertRaisesRegex(RuntimeError, '测试事务失败'):
            terms_core.character_command(self.new.path, dict(op='change', asked=['b'], action='confirmed', entity=self.alice))
        self.assertEqual(self.dump(), before)
        pid = terms_core._service.process.pid
        with sqlite3.connect(self.new.path) as c:
            c.execute('DROP TRIGGER fail_projection')
        self.both(lambda: self.api.change(['b'], 'confirmed', self.alice), 'change',
                  asked=['b'], action='confirmed', entity=self.alice)
        self.assertEqual(terms_core._service.process.pid, pid)

    def test_crash_then_next_request_restarts_without_replaying(self):
        self.both(lambda: self.api.change(['a'], 'confirmed', self.alice), 'change',
                  asked=['a'], action='confirmed', entity=self.alice)
        process = terms_core._service.process
        before = self.dump()
        process.kill()
        process.wait()
        self.assertEqual(self.dump(), before)
        self.both(lambda: self.api.answer(self.bob, yes=['b']), 'answer',
                  entity=self.bob, yes=['b'], no=[], later=[], kind='seed', switch=None)
        self.assertNotEqual(terms_core._service.process.pid, process.pid)

    def test_crash_during_a_write_rolls_back_without_replay(self):
        with sqlite3.connect(self.new.path) as c:
            c.execute("""CREATE TRIGGER pause_projection BEFORE INSERT ON memberships BEGIN
                SELECT sum(x) FROM (WITH RECURSIVE slow(x) AS (SELECT 1 UNION ALL
                    SELECT x+1 FROM slow WHERE x<100000000) SELECT x FROM slow); END""")
        before = self.dump()
        errors = []
        def write():
            try:
                terms_core.character_command(self.new.path, dict(op='change', asked=['b'], action='confirmed', entity=self.alice))
            except Exception as exc:
                errors.append(exc)
        thread = threading.Thread(target=write, daemon=True)
        thread.start()
        deadline = time.monotonic() + 5
        locked = False
        with sqlite3.connect(self.new.path, timeout=.01) as probe:
            while time.monotonic() < deadline:
                try:
                    probe.execute('BEGIN IMMEDIATE')
                    probe.rollback()
                except sqlite3.OperationalError:
                    locked = True
                    break
                time.sleep(.01)
        self.assertTrue(locked)
        terms_core._service.process.kill()
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], terms_core.Failed)
        self.assertEqual(self.dump(), before)
        with sqlite3.connect(self.new.path) as c:
            c.execute('DROP TRIGGER pause_projection')
        self.both(lambda: self.api.change(['b'], 'confirmed', self.alice), 'change',
                  asked=['b'], action='confirmed', entity=self.alice)

    def test_blocked_writer_timeout_does_not_leave_partial_writes(self):
        before = self.dump()
        with sqlite3.connect(self.new.path, timeout=1) as lock:
            lock.execute('BEGIN IMMEDIATE')
            with self.assertRaisesRegex(terms_core.Failed, '超时'):
                terms_core._resident(self.new.path, dict(op='change', asked=['a'], action='confirmed', entity=self.alice), timeout=.05)
        self.assertEqual(self.dump(), before)
        self.assertIsNone(terms_core._service)
        self.both(lambda: self.api.change(['a'], 'confirmed', self.alice), 'change',
                  asked=['a'], action='confirmed', entity=self.alice)

    def test_concurrent_requests_are_serialized_and_cli_fallback_still_works(self):
        errors = []
        def write(u):
            try:
                terms_core.character_command(self.new.path, dict(op='change', asked=[u], action='unknown'))
            except Exception as exc:
                errors.append(exc)
        threads = [threading.Thread(target=write, args=(u,)) for u in ('a', 'b', 'c')]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(10)
            self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        with self.new.connect() as c:
            rows = list(c.execute('SELECT id,before_json FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 3'))
            self.assertEqual(len(rows), 3)
            self.assertEqual(c.execute("SELECT count(*) FROM meta WHERE key=?", ('terms_writer',)).fetchone()[0], 0)
        for row in rows:
            with mock.patch.dict(os.environ, {'ILLUSTASH_TERMS_DAEMON': '0'}):
                terms_core.character_command(self.new.path, dict(op='undo', action=row['id']))
        terms_core.run('verify-characters', '--db', self.new.path)

    def test_cli_and_service_refuse_duplicate_json_keys(self):
        before = self.dump()
        process = __import__('subprocess').Popen([str(terms_core.binary()), 'serve'], stdin=-1, stdout=-1, stderr=-1, text=True)
        payload = '{"db":' + json.dumps(str(self.new.path)) + ',"input":{"op":"change","op":"sweep"}}\n'
        out, _ = process.communicate(payload, timeout=5)
        self.assertEqual(json.loads(out.splitlines()[0])['code'], 2)
        self.assertEqual(self.dump(), before)

    def test_idle_transport_releases_previous_request(self):
        terms_core.character_command(self.new.path, dict(op='default_tags', padding='x' * 1000000))
        ident = terms_core._service.thread.ident
        deadline = time.monotonic() + 2
        while time.monotonic() < deadline:
            frame = sys._current_frames()[ident]
            if frame.f_code.co_name == 'wait':
                while frame is not None and frame.f_code.co_name != '_transport':
                    frame = frame.f_back
                self.assertIsNotNone(frame)
                self.assertTrue({'job', 'payload', 'reply', 'line'}.isdisjoint(frame.f_locals))
                break
            time.sleep(.01)
        else:
            self.fail('通道没有返回空闲队列等待')
