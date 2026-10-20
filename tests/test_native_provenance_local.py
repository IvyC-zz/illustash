"""Independent legacy queue/group parity. Synthetic databases only; no automatic build."""
import json
import os
from pathlib import Path
import sqlite3
from types import SimpleNamespace
import unittest
from PIL import Image
from tests import provenance_fixture as db
from tests.provenance_expectations import native_worker
from tests.provenance_expectations import groups
from tests import test_native_provenance_compute as driver


class IndependentLocalProvenanceInputs(unittest.TestCase):
    setUpClass = classmethod(driver.NativeProvenanceComputeTest.setUpClass.__func__)
    def test_handwritten_group_counts_conflict_and_300_second_boundary(self):
        for partial, expected in ((False, dict(auto=1, candidates=3, stopped='')),
                                  (True, dict(auto=1, candidates=1, stopped=''))):
            with self.subTest(partial=partial):
                fixture = NativeLocalProvenanceTest()
                fixture.binary = self.binary
                fixture.setUp()
                try:
                    request, reference = fixture.group_fixture(partial)
                    self.assertEqual(fixture.probe(request)['result'], expected)
                    with sqlite3.connect(Path(request['cache']) / 'provenance.sqlite') as c:
                        self.assertEqual(c.execute("SELECT evidence FROM image_artists WHERE asset='dup'").fetchone()[0],
                                         '{"group": "duplicate", "count": 1, "from": ["known"]}')
                        self.assertEqual(c.execute("SELECT count(*) FROM image_artists WHERE asset IN ('untouched','weak','after','old')").fetchone()[0], 0)
                finally:
                    fixture.doCleanups()

    def test_fixed_old_worker_missing_path_is_fatal(self):
        fixture = NativeLocalProvenanceTest()
        fixture.binary = self.binary
        fixture.setUp()
        try:
            request, reference = fixture.fixture_local('pixel_hash')
            with sqlite3.connect(reference.source) as c:
                c.execute('INSERT INTO assets VALUES (?, ?, NULL, ?, 1, 0, NULL)', ('absent', 'files', 'x'))
            fixture.probe(request, success=False)
        finally:
            fixture.doCleanups()


class NativeLocalProvenanceTest(unittest.TestCase):
    setUpClass = classmethod(driver.NativeProvenanceComputeTest.setUpClass.__func__)
    setUp = driver.NativeProvenanceComputeTest.setUp
    probe = driver.NativeProvenanceComputeTest.probe
    def fixture_local(self, step):
        library, candidate, reference = [self.root / p for p in ('library', 'candidate', 'reference')]
        for p in (library, candidate, reference):
            p.mkdir()
        (library / 'library.json').write_text(json.dumps({'sources': [{'id': 'files', 'kind': 'folder'}]}))
        with sqlite3.connect(library / 'source.sqlite') as c:
            c.execute('CREATE TABLE assets(uuid TEXT,source_id TEXT,src_path TEXT,preset TEXT,in_scope INTEGER,trashed INTEGER,added_at TEXT)')
        for p in (candidate, reference):
            with db.connect(SimpleNamespace(path=p)):
                pass
        request = dict(format='illustash-provenance/1', identity=dict(task_id='synthetic', generation=9),
                       owner_pid=os.getpid(), library=str(library), cache=str(candidate), mailbox=str(candidate),
                       step=step, only=[], artists=[], network=False)
        lib = SimpleNamespace(path=reference, db=library / 'library.sqlite', source=library / 'source.sqlite')
        return request, lib

    def test_pixel_worker_queue_filter_errors_and_no_lookup_writes(self):
        request, reference = self.fixture_local('pixel_hash')
        library = Path(request['library'])
        image = library / 'generated.png'
        Image.new('RGBA', (2, 1), (12, 34, 56, 0)).save(image)
        bad = library / 'bad.png'
        bad.write_bytes(b'synthetic unreadable')
        with sqlite3.connect(reference.source) as c:
            c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,NULL)',
                          [(u, 'files', p, preset, scope, trash) for u, p, preset, scope, trash in
                           [('good', str(image), 'x', 1, 0), ('bad', str(bad), 'pixiv', 1, 0),
                            ('missing', str(library / 'missing.png'), 'pixiv', 1, 0), ('done', str(image), 'x', 1, 0),
                            ('other', str(image), 'other', 1, 0), ('hidden', str(image), 'x', 0, 0),
                            ('trash', str(image), 'x', 1, 1)]])
        for p in (reference.path, Path(request['cache'])):
            with db.connect(SimpleNamespace(path=p)) as c:
                c.execute("INSERT INTO lookups(target,backend,last_result) VALUES ('done','pixel_hash','hit')")
        old_request = request | dict(cache=str(reference.path), mailbox=str(reference.path))
        expected = native_worker.execute(old_request)
        source_before = reference.source.read_bytes()
        for only in ([], ['good', 'done', 'other']):
            request['only'] = only
            expected = native_worker.execute(old_request | dict(only=only))
            actual = self.probe(request)
            self.assertEqual(actual['identity'], request['identity'])
            self.assertEqual(actual['result'], {'hashes': 'hashes.json'})
            self.assertEqual(json.loads((Path(request['mailbox']) / 'hashes.json').read_text()), expected)
            with sqlite3.connect(Path(request['cache']) / 'provenance.sqlite') as c:
                self.assertEqual(c.execute('SELECT target,backend,last_result FROM lookups').fetchall(),
                                 [('done', 'pixel_hash', 'hit')])
        self.assertEqual(reference.source.read_bytes(), source_before)

    def group_fixture(self, partial=False):
        request, reference = self.fixture_local('groups')
        with sqlite3.connect(reference.db) as c:
            c.execute('CREATE TABLE duplicates(grp,uuid TEXT)')
            c.executemany('INSERT INTO duplicates VALUES (?,?)', [(1, 'known'), (1, 'dup'), (2, 'conflict'), (2, 'untouched')])
            if not partial:
                c.executescript('CREATE TABLE similar_groups(gid,uuid TEXT); CREATE TABLE memberships(entity,uuid TEXT); CREATE TABLE terms(id INTEGER PRIMARY KEY, key, kind TEXT); CREATE TABLE term_tree(term_id INTEGER PRIMARY KEY, parent_id, merged_into, work_id, work_text, variant, artist, label_only INTEGER, mascot INTEGER, mascot_origin, original INTEGER, seq INTEGER); CREATE TABLE clusters(cid,uuid TEXT);')
                c.executescript("INSERT INTO terms VALUES (1,1,'character'),(2,2,'character');"
                                "INSERT INTO term_tree(term_id,original,label_only,seq) VALUES (1,1,0,1),(2,0,0,2);")
                c.executemany('INSERT INTO memberships VALUES (?,?)', [(1,'known'),(1,'oc'),(2,'ordinary')])
                c.executemany('INSERT INTO clusters VALUES (?,?)', [(1,'known'),(1,'known2'),(1,'cluster'),(2,'known'),(2,'weak')])
        with sqlite3.connect(reference.source) as c:
            c.executemany('INSERT INTO assets VALUES (?, ?, NULL, ?, 1, 0, ?)',
                          [(u,'files','pixiv',t) for u,t in [('known','2026-01-01T00:00:00'),
                            ('known2','2026-01-01T00:05:00'),('run','2026-01-01T00:10:00'),
                            ('after','2026-01-01T00:15:01')]])
        for p in (reference.path, Path(request['cache'])):
            with db.connect(SimpleNamespace(path=p)) as c:
                c.execute("INSERT INTO artists(id) VALUES ('A'),('B')")
                c.executemany('INSERT INTO image_artists(asset,artist_id,method,status) VALUES (?,?,?,?)',
                              [('known','A','pixel_hash','auto'),('known2','A','pixel_hash','auto'),
                               ('conflict','A','pixel_hash','auto'),('conflict','B','pixel_hash','auto'),
                               ('old','A','group_run','candidate'),('oc','B','ocr','candidate')])
        return request, reference

    def test_pixel_originals_generated_photos_layout_and_sorted_choice(self):
        request, reference = self.fixture_local('pixel_hash')
        library = Path(request['library'])
        originals = library / 'generated originals' / 'originals' / 'a'
        originals.mkdir(parents=True)
        Image.new('RGB', (1, 1), (1, 2, 3)).save(originals / 'asset.z.png')
        Image.new('RGB', (1, 1), (4, 5, 6)).save(originals / 'asset.a.png')
        (library / 'library.json').write_text(json.dumps({'sources': [{'id':'photos', 'kind':'photos',
                                      'path':str(library / 'generated originals')}]}))
        with sqlite3.connect(reference.source) as c:
            c.executemany('INSERT INTO assets VALUES (?, ?, ?, ?, 1, 0, NULL)',
                          [('asset', 'photos', str(library / 'derivative.png'), 'x')])
        expected = native_worker.execute(request | dict(cache=str(reference.path), mailbox=str(reference.path)))
        self.probe(request)
        self.assertEqual(json.loads((Path(request['mailbox']) / 'hashes.json').read_text()), expected)
        with sqlite3.connect(reference.source) as c:
            c.execute('INSERT INTO assets VALUES (?, ?, NULL, ?, 1, 0, NULL)', ('absent', 'photos', 'pixiv'))
        with self.assertRaises(AttributeError):
            native_worker.execute(request | dict(cache=str(reference.path), mailbox=str(reference.path)))
        (Path(request['mailbox']) / 'hashes.json').unlink()
        self.probe(request, success=False)
        self.assertFalse((Path(request['mailbox']) / 'hashes.json').exists())

    def assert_groups_parity(self, request, reference):
        core_before, source_before = reference.db.read_bytes(), reference.source.read_bytes()
        expected = groups.run(reference, log=lambda _: None)
        actual = self.probe(request)
        self.assertEqual(actual['result'], expected)
        sql = 'SELECT asset,artist_id,method,score,evidence,status FROM image_artists ORDER BY asset,artist_id'
        with sqlite3.connect(Path(request['cache']) / 'provenance.sqlite') as c, sqlite3.connect(reference.path / 'provenance.sqlite') as r:
            self.assertEqual(c.execute(sql).fetchall(), r.execute(sql).fetchall())
        self.assertEqual(reference.db.read_bytes(), core_before)
        self.assertEqual(reference.source.read_bytes(), source_before)

    def test_groups_exact_evidence_conflict_votes_and_repeated_run(self):
        request, reference = self.group_fixture()
        self.assert_groups_parity(request, reference)
        self.assert_groups_parity(request, reference)

    def test_groups_partial_old_core_stops_at_first_missing_table(self):
        request, reference = self.group_fixture(partial=True)
        self.assert_groups_parity(request, reference)

    def test_stale_owner_rejects_before_local_cache_changes(self):
        request, reference = self.group_fixture()
        request['owner_pid'] = 4294967295
        before = (Path(request['cache']) / 'provenance.sqlite').read_bytes()
        self.probe(request, success=False)
        self.assertEqual((Path(request['cache']) / 'provenance.sqlite').read_bytes(), before)
