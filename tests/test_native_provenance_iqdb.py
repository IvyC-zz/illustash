"""Native iqdb signature step against fixed historical worker values.

The candidate runs explicitly built illustash-iqdb (ILLUSTASH_IQDB_BINARY) on
generated images; queue order, local paths and signatures match fixed recordings.
"""
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest

import numpy as np
from PIL import Image

from tests import provenance_fixture as db
from tests import test_native_provenance_compute as driver
from tests.test_native_provenance_production import candidate_worker

ROOT = Path(__file__).resolve().parents[1]


def local_errors(hashes):
    # Rust 的系统错误文字由操作系统本地化；只统一缺文件文案，保留错误码与状态。
    for row in hashes or ():
        if row.get('error', '').endswith(' (os error 2)'):
            row['error'] = 'file not found (os error 2)'
    return hashes


class NativeIqdbQueueTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        driver.NativeProvenanceComputeTest.setUpClass.__func__(cls)
        configured = os.environ.get('ILLUSTASH_IQDB_BINARY')
        if not configured:
            raise AssertionError('explicit current candidate IQDB required; no automatic build')
        cls.iqdb = Path(configured).resolve(strict=True)

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.library = self.root / 'library'
        self.images = self.root / 'images'
        self.library.mkdir()
        self.images.mkdir()
        (self.library / 'library.json').write_text(json.dumps({'sources': [{'id': 'files', 'kind': 'folder'}]}))
        rng = np.random.default_rng(177)
        rows = []
        def add(asset, preset, added, path=True, in_scope=1):
            src = None
            if path:
                src = self.images / f'{asset}.png'
                Image.fromarray(rng.integers(0, 256, (24, 32, 3), np.uint8)).save(src)
            rows.append((asset, 'files', None if src is None else str(src), preset, in_scope, 0, added))
        for i in range(8):                                   # one pixiv run of saves
            add(f'p{i}', 'pixiv', f'2025-01-01T10:{i:02d}:00')
        add('p-alone', 'pixiv', '2025-02-01T10:00:00')
        add('p-undated', 'pixiv', None)
        for i in range(6):
            add(f'x{i}', 'x', f'2025-01-01T09:{i:02d}:00')
        add('x-solved', 'x', '2025-01-01T09:30:00')
        add('x-cached', 'x', '2025-01-01T09:31:00')
        add('x-due', 'x', '2025-01-01T09:32:00')
        add('x-nopath', 'x', '2025-01-01T09:33:00', path=False)
        add('x-out', 'x', '2025-01-01T09:34:00', in_scope=0)
        for i in range(4):
            add(f'oc{i}', 'pixiv', f'2025-03-01T10:0{i}:00')
        add('oc-solved-a', 'x', '2025-03-02T10:00:00')
        add('oc-solved-b', 'x', '2025-03-02T10:01:00')
        add('other1', 'other', '2025-01-01T08:00:00')
        with sqlite3.connect(self.library / 'source.sqlite') as c:
            c.execute('CREATE TABLE assets(uuid TEXT,source_id TEXT,src_path TEXT,preset TEXT,in_scope INTEGER,trashed INTEGER,added_at TEXT)')
            c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,?)', rows)
        with sqlite3.connect(self.library / 'library.sqlite') as c:
            c.executescript('''
                CREATE TABLE duplicates(grp TEXT, uuid TEXT); CREATE TABLE similar_groups(gid TEXT, uuid TEXT);
                CREATE TABLE entities(id INTEGER PRIMARY KEY, original INTEGER, label_only INTEGER);
                CREATE TABLE memberships(entity INTEGER, uuid TEXT); CREATE TABLE clusters(cid TEXT, uuid TEXT);
                CREATE TABLE asset_rating(uuid TEXT PRIMARY KEY, rating TEXT);
                INSERT INTO entities VALUES (1,1,0),(2,1,0),(3,0,0);
                INSERT INTO memberships VALUES (1,'oc0'),(1,'oc1'),(1,'oc2'),(1,'oc3'),(2,'oc-solved-a'),(2,'oc-solved-b'),(3,'x0');''')
            c.executemany('INSERT INTO asset_rating VALUES (?,?)', [
                ('x0', 'general'), ('x1', 'questionable'), ('x2', 'explicit'), ('x3', 'sensitive'),
                ('p0', 'general'), ('p2', 'explicit'), ('p4', 'sensitive'), ('p-alone', None), ('oc0', 'questionable')])
        self.cache = self.root / 'cache'
        self.cache.mkdir()
        with db.connect(SimpleNamespace(path=self.cache)) as c:
            artist = db.artist_for(c, [('danbooru', 'someone', '')], 'test')
            c.executemany("INSERT INTO image_artists(asset,artist_id,method,status) VALUES (?,?,'test','auto')",
                          [('x-solved', artist), ('oc-solved-a', artist)])
            c.execute("INSERT INTO lookups(target,backend,attempts,last_result,next_due_at) VALUES ('x-cached','iqdb',1,'miss',datetime('now','+1 day'))")
            c.execute("INSERT INTO lookups(target,backend,attempts,last_result,next_due_at) VALUES ('x-due','iqdb',1,'miss',datetime('now','-1 day'))")

    def request(self, side, only=()):
        mailbox = self.root / f'mailbox-{side}'
        cache = self.root / f'cache-{side}'
        shutil.rmtree(mailbox, ignore_errors=True)
        shutil.rmtree(cache, ignore_errors=True)
        mailbox.mkdir()
        shutil.copytree(self.cache, cache)
        request = dict(format='illustash-provenance/1', identity=dict(task_id='synthetic', generation=3),
                       owner_pid=os.getpid(), library=str(self.library), cache=str(cache), mailbox=str(mailbox),
                       step='iqdb', only=list(only), artists=[], network=True)
        path = self.root / f'request-{side}.json'
        path.write_text(json.dumps(request), encoding='utf-8')
        return path, mailbox

    def run_side(self, side, command, env, only=()):
        path, mailbox = self.request(side, only)
        response = self.root / f'response-{side}.json'
        response.unlink(missing_ok=True)
        run = subprocess.run([*command, str(path), str(response)], env=env, capture_output=True, timeout=120,
                             start_new_session=os.name != 'nt')
        self.assertEqual(run.returncode, 0, run.stderr.decode(errors='replace'))
        result = json.loads(response.read_text())['result']
        queue = [json.loads(line) for line in (mailbox / 'hash-input.jsonl').read_text().splitlines()] \
            if (mailbox / 'hash-input.jsonl').exists() else None
        hashes = json.loads((mailbox / 'hashes.json').read_text()) if (mailbox / 'hashes.json').exists() else None
        return result, queue, local_errors(hashes)

    def expected(self):
        import gzip
        with gzip.open(Path(__file__).parent/'fixtures/iqdb_values.json.gz','rt',encoding='utf-8') as f:
            text = f.read().replace('@ROOT@',str(self.root).replace('\\','\\\\'))
            record = json.loads(text)[self._testMethodName][0]
        # 录制值里的路径段按本机分隔符还原，签名与队列顺序保持固定。
        if record[1] is not None:
            for row in record[1]:
                if row['path'] != 'None':
                    row['path'] = str(Path(row['path']))
        record[2] = local_errors(record[2])
        return tuple(record)

    def native(self, binary, only=(), iqdb=True, side='native'):
        env = {k: v for k, v in os.environ.items() if k not in ('ILLUSTASH_IQDB_BINARY', 'ILLUSTASH_IQDB_BIN', 'ILLUSTASH_WORKER_PYTHON')}
        env['PATH'] = ''
        if iqdb:
            env['ILLUSTASH_IQDB_BINARY'] = str(self.iqdb)
        return self.run_side(side, [str(binary)], env, only)

    def test_automatic_queue_order_paths_and_signatures_match_python(self):
        expected = self.expected()
        got = self.native(self.binary)
        self.assertEqual(got, expected)
        result, queue, hashes = got
        self.assertEqual(result, {'hashes': 'hashes.json'})
        assets = [row['asset'] for row in queue]
        # Solved, not-yet-due, out-of-scope and 'other' pictures are never asked automatically.
        self.assertTrue({'oc0', 'oc1', 'oc2', 'oc3', 'p-undated'} <= set(assets))
        for absent in ('x-solved', 'x-cached', 'x-out', 'other1', 'oc-solved-a'):
            self.assertNotIn(absent, assets)
        self.assertIn('x-due', assets)
        self.assertIn({'asset': 'x-nopath', 'path': 'None'}, queue)
        self.assertGreater(len(hashes), 10)

    def test_manual_selection_matches_python_without_priority(self):
        only = ['other1', 'x-solved', 'x-cached', 'p3', 'x2']
        expected = self.expected()
        self.assertEqual(self.native(self.binary, only), expected)
        # Library order, no priority or due check; pictures with an artist are skipped.
        self.assertEqual([row['asset'] for row in expected[1]], ['p3', 'x2', 'x-cached', 'other1'])

    def test_missing_signature_program_reports_like_python(self):
        expected = self.expected()
        self.assertEqual(expected[0], {'failed': 1, 'stopped': '未配置本地 IQDB 签名计算程序'})
        self.assertEqual(self.native(self.binary, iqdb=False), expected)


@unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')
class InstalledIqdbQueueTest(NativeIqdbQueueTest):
    @classmethod
    def setUpClass(cls):
        NativeIqdbQueueTest.setUpClass.__func__(cls)
        cls.worker = candidate_worker()

    def setUp(self):
        super().setUp()
        folder = self.root / 'installed-bin'
        folder.mkdir()
        self.binary = folder / ('illustash-provenance.exe' if os.name == 'nt' else 'illustash-provenance')
        shutil.copy2(self.worker, self.binary)

    def test_installed_adapter_uses_its_sibling_program(self):
        shutil.copy2(self.iqdb, self.binary.with_name('illustash-iqdb.exe' if os.name == 'nt' else 'illustash-iqdb'))
        expected = self.expected()
        self.assertEqual(self.native(self.binary, iqdb=False, side='sibling'), expected)


if __name__ == '__main__':
    unittest.main()
