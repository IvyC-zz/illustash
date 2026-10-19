"""直接验收原生 serve 的事务、并发、超时、崩溃与严格 JSON 边界。"""
import gzip
import json
import queue
import sqlite3
import subprocess
import tempfile
import threading
import time
import unittest
from pathlib import Path

from tests import native_commands as core
from tests.fixed_expectations import upgrade_recorded


class Channel:
    """测试专用逐行通道；超时或退出由用例处理，不重放请求。"""
    def __init__(self, binary, db):
        self.db = str(db)
        self.process = subprocess.Popen([str(binary),'serve'], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self.lines = queue.Queue()
        self.lock = threading.Lock()
        def read():
            for line in self.process.stdout:
                self.lines.put(line)
            self.lines.put(None)
        self.reader = threading.Thread(target=read, daemon=True)
        self.reader.start()

    def request(self, command, timeout=10):
        with self.lock:
            self.process.stdin.write(json.dumps({'db':self.db,'input':command},ensure_ascii=False)+'\n')
            self.process.stdin.flush()
            try:
                line = self.lines.get(timeout=timeout)
            except queue.Empty:
                self.close()
                raise core.Failed('测试请求超时')
            if line is None:
                raise core.Failed('核心进程退出')
            result = json.loads(line)
            if result['code']:
                raise core.Failed(result['error'])
            return result['result']

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait(5)
        self.reader.join(5)
        for stream in (self.process.stdin,self.process.stdout):
            stream.close()


class Service(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = core.binary()
        if cls.binary is None:
            raise unittest.SkipTest('需要显式设置原生核心 ILLUSTASH_TERMS_BIN')
        with gzip.open(Path(__file__).parent/'fixtures/service_seeds.json.gz','rt',encoding='utf-8') as f:
            cls.seeds = json.load(f)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.db = self.root/'core.sqlite'
        seed = self.seeds[self._testMethodName]
        self.alice,self.bob = seed['alice'],seed['bob']
        with sqlite3.connect(self.db) as c:
            c.executescript(seed['library'].replace('@ROOT@',str(self.root)))
        upgrade_recorded(self.db)
        self.start()

    def start(self):
        self.channel = Channel(self.binary,self.db)
        self.addCleanup(self.channel.close)

    def change(self, uuid='a', entity=None):
        return self.channel.request(dict(op='change',asked=[uuid],action='confirmed',entity=entity or self.alice))

    def dump(self):
        with sqlite3.connect(self.db) as c:
            return list(c.iterdump())

    def test_process_is_reused_across_commands_and_refusals(self):
        first = self.change()
        pid = self.channel.process.pid
        before = self.dump()
        with self.assertRaises(core.Failed):
            self.change('missing')
        self.assertEqual(self.dump(),before)
        self.assertEqual(self.channel.process.pid,pid)
        second = self.channel.request(dict(op='answer',entity=self.bob,yes=['b'],no=[],later=[],kind='seed',switch=None))
        self.assertEqual(self.channel.process.pid,pid)
        self.channel.request(dict(op='undo',action=second['action']))
        self.channel.request(dict(op='undo',action=first['action']))
        core.run('verify-characters','--db',self.db)

    def test_sql_failure_rolls_back_and_same_process_can_continue(self):
        with sqlite3.connect(self.db) as c:
            c.execute("CREATE TRIGGER fail_projection BEFORE INSERT ON memberships BEGIN SELECT RAISE(ABORT,'测试事务失败'); END")
        before = self.dump()
        with self.assertRaisesRegex(core.Failed,'测试事务失败'):
            self.change('b')
        self.assertEqual(self.dump(),before)
        pid = self.channel.process.pid
        with sqlite3.connect(self.db) as c:
            c.execute('DROP TRIGGER fail_projection')
        self.change('b')
        self.assertEqual(self.channel.process.pid,pid)

    def test_crash_then_next_request_restarts_without_replaying(self):
        self.change()
        old = self.channel.process.pid
        before = self.dump()
        self.channel.close()
        self.assertEqual(self.dump(),before)
        self.start()
        self.change('b',self.bob)
        self.assertNotEqual(self.channel.process.pid,old)
        with sqlite3.connect(self.db) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM actions').fetchone()[0],2)

    def test_crash_during_a_write_rolls_back_without_replay(self):
        with sqlite3.connect(self.db) as c:
            c.execute("""CREATE TRIGGER pause_projection BEFORE INSERT ON memberships BEGIN
                SELECT sum(x) FROM (WITH RECURSIVE slow(x) AS (SELECT 1 UNION ALL
                    SELECT x+1 FROM slow WHERE x<100000000) SELECT x FROM slow); END""")
        before = self.dump()
        errors = []
        def write():
            try:
                self.change('b')
            except core.Failed as e:
                errors.append(e)
        thread = threading.Thread(target=write,daemon=True)
        thread.start()
        deadline = time.monotonic()+5
        locked = False
        with sqlite3.connect(self.db,timeout=.01) as probe:
            while time.monotonic()<deadline:
                try:
                    probe.execute('BEGIN IMMEDIATE');probe.rollback()
                except sqlite3.OperationalError:
                    locked = True
                    break
                time.sleep(.01)
        self.assertTrue(locked)
        self.channel.process.kill()
        thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(len(errors),1)
        self.assertEqual(self.dump(),before)
        with sqlite3.connect(self.db) as c:
            c.execute('DROP TRIGGER pause_projection')
        self.start();self.change('b')

    def test_blocked_writer_timeout_does_not_leave_partial_writes(self):
        before = self.dump()
        with sqlite3.connect(self.db,timeout=1) as lock:
            lock.execute('BEGIN IMMEDIATE')
            with self.assertRaisesRegex(core.Failed,'超时'):
                self.channel.request(dict(op='change',asked=['a'],action='confirmed',entity=self.alice),timeout=.05)
        self.assertEqual(self.dump(),before)
        self.start();self.change()

    def test_concurrent_requests_are_serialized_and_cli_fallback_still_works(self):
        errors = []
        def write(u):
            try:
                self.channel.request(dict(op='change',asked=[u],action='unknown'))
            except Exception as e:
                errors.append(e)
        threads = [threading.Thread(target=write,args=(u,)) for u in 'abc']
        for thread in threads:thread.start()
        for thread in threads:
            thread.join(10);self.assertFalse(thread.is_alive())
        self.assertEqual(errors,[])
        with sqlite3.connect(self.db) as c:
            rows = c.execute('SELECT id FROM actions WHERE undone=0 ORDER BY id DESC').fetchall()
            self.assertEqual(len(rows),3)
            self.assertEqual(c.execute("SELECT count(*) FROM meta WHERE key='terms_writer'").fetchone()[0],0)
        for action, in rows:
            core.character_command(self.db,dict(op='undo',action=action))
        core.run('verify-characters','--db',self.db)

    def test_cli_and_service_refuse_duplicate_json_keys(self):
        before = self.dump()
        payload = '{"db":'+json.dumps(str(self.db))+',"input":{"op":"change","op":"sweep"}}\n'
        result = subprocess.run([str(self.binary),'serve'],input=payload,capture_output=True,text=True,timeout=5)
        self.assertEqual(json.loads(result.stdout.splitlines()[0])['code'],2)
        result = subprocess.run([str(self.binary),'character-command','--db',str(self.db)],
                                input='{"op":"change","op":"sweep"}',capture_output=True,text=True,timeout=5)
        self.assertEqual(result.returncode,2)
        self.assertEqual(self.dump(),before)
