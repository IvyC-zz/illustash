"""独立的旧版本地验证契约；所有图片和数据库都是合成的。"""
from contextlib import closing
import json
import copy
import shutil
import sqlite3
from pathlib import Path
from PIL import Image
from tests import provenance_fixture as db
from tests.provenance_expectations import verify
from tests import numeric_reference_mode
from tests import test_native_provenance_local as local_driver
import unittest


class NativeProvenanceMatchTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._missing_numeric_candidate = None
        try:
            local_driver.NativeLocalProvenanceTest.setUpClass.__func__(cls)
        except unittest.SkipTest as error:
            if not numeric_reference_mode.defer_missing(cls, error):
                raise

    def setUp(self):
        numeric_reference_mode.check_candidate(self)
        local_driver.NativeLocalProvenanceTest.setUp(self)

    probe = local_driver.NativeLocalProvenanceTest.probe
    fixture_local = local_driver.NativeLocalProvenanceTest.fixture_local

    def fixture_match(self, cached=True, many=False):
        request, reference = self.fixture_local('local_match')
        library = Path(request['library'])
        image = Image.new('RGB', (64, 48))
        image.putdata([(i*13 % 256, i*47 % 256, i*59 % 256) for i in range(64*48)])
        path = library / 'generated.png'
        image.save(path)
        flat = library / 'flat.png'
        Image.new('RGB', image.size, 'gray').save(flat)
        with sqlite3.connect(reference.db) as c:
            c.executescript('CREATE TABLE assets(uuid TEXT,src_path TEXT); CREATE TABLE image_sigs(uuid TEXT,ratio REAL,grid BLOB,ok INTEGER); CREATE TABLE assertions(uuid TEXT,entity TEXT,verdict TEXT,origin TEXT);')
            c.executemany('INSERT INTO assets VALUES (?,?)', [('drawing',str(path)),('flat',str(flat)),('missing',str(library/'missing.png'))])
            c.execute("INSERT INTO assertions VALUES ('drawing','A','rejected','manual')")
            if cached:
                c.executemany('INSERT INTO image_sigs VALUES (?,?,?,1)', [(u,*verify.signature(p)) for u,p in [('drawing',path),('flat',flat)]])
            c.execute('INSERT INTO image_sigs VALUES (?,?,?,1)',('stale',4/3,bytes(range(256))))
        import io
        stream = io.BytesIO();image.save(stream,format='PNG')
        sig = verify._sig(stream.getvalue())
        for cache in (reference.path,Path(request['cache'])):
            with db.connect(type('Lib',(),dict(path=cache))()) as c:
                c.execute("INSERT INTO artists(id) VALUES ('A'),('B')")
                # 匹配到的这张图的人工否定只存在 core 里；候选的机器数据不会改写它。
                # 正式发布时套用这个裁决。
                c.execute("INSERT INTO image_artists(asset,artist_id,method,score,status) VALUES ('drawing','A','ocr',0.7,'candidate')")
                for post in range(4097 if many else 1):
                    c.execute('INSERT INTO ref_sigs(site,post_id,artist_id,ratio,grid,colour,url) VALUES (?,?,?,?,?,?,?)',
                              ('danbooru',str(post),'A',*sig,''))
                for post,ratio,grid,colour,artist in [('flat',4/3,bytes([123])*256,sig[2],'A'),
                              ('ratio',2.0,sig[1],sig[2],'A'),('colour',4/3,sig[1],bytes([255])*1728,'A'),
                              ('unowned',4/3,sig[1],sig[2],None)]:
                    c.execute('INSERT INTO ref_sigs(site,post_id,artist_id,ratio,grid,colour,url) VALUES (?,?,?,?,?,?,?)',
                              ('pixiv',post,artist,ratio,grid,colour,''))
        return request,reference

    def rows(self, path):
        with sqlite3.connect(path/'provenance.sqlite') as c:
            return {table:c.execute(f'SELECT {columns} FROM {table} ORDER BY 1,2').fetchall() for table,columns in
                    dict(image_sources='asset,site,post_id,url,artist_id,method,similarity',
                         image_artists='asset,artist_id,method,score,evidence,status',
                         lookups='target,backend,attempts,last_result').items()}

    def parity(self, request, reference):
        core, source = reference.db.read_bytes(), reference.source.read_bytes()
        expected = verify.local_run(reference,log=lambda _:None)
        actual = self.probe(request)
        self.assertEqual(actual['identity'],request['identity'])
        self.assertEqual(actual['result'],expected)
        self.assertEqual(self.rows(Path(request['cache'])),self.rows(reference.path))
        self.assertEqual(reference.db.read_bytes(),core)
        self.assertEqual(reference.source.read_bytes(),source)
        return actual['result']

    def complete_parity(self, request, reference, references):
        from tests import numeric_reference_assertions as checks
        from tests.numpy_provenance_reference import local_run
        core, source = reference.db.read_bytes(), reference.source.read_bytes()
        # 正式缓存自带交付/元数据表（app_provenance_cache::connect）。将它们作为
        # 相同的输入，并保留哨兵行；本地匹配不能改写这些与本次任务无关的状态。
        for folder in (reference.path, Path(request['cache'])):
            with closing(sqlite3.connect(folder/'provenance.sqlite')) as cache, cache:
                cache.executescript("""
                    CREATE TABLE native_delivery(task_id TEXT PRIMARY KEY,package TEXT NOT NULL,
                        status TEXT NOT NULL DEFAULT 'pending',error TEXT NOT NULL DEFAULT '',
                        parts INTEGER NOT NULL DEFAULT 1 CHECK(parts>=1));
                    CREATE TABLE native_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
                    INSERT INTO native_delivery VALUES ('synthetic-existing','unchanged','done','',1);
                    INSERT INTO native_meta VALUES ('synthetic','unchanged');
                """)
        expected = []
        for kind in references:
            isolated = copy.copy(reference)
            isolated.path = reference.path.with_name(reference.path.name + '-' + kind)
            shutil.copytree(reference.path, isolated.path)
            result = local_run(isolated) if kind == 'numpy' else verify.local_run(isolated, log=lambda _: None)
            expected.append((kind, result, checks.comparable_cache(checks.cache_state(isolated.path))))
        candidate = Path(request['cache'])
        before = checks.cache_state(candidate)
        started = checks.utc_second()
        actual = self.probe(request)
        finished = checks.utc_second()
        after = checks.cache_state(candidate)
        checks.assert_cache_preserved(self, before, after, started, finished)
        for kind, result, state in expected:
            with numeric_reference_mode.comparing(self, kind):
                self.assertEqual(actual['identity'], request['identity'])
                self.assertEqual(actual['result'], result)
                self.assertEqual(checks.comparable_cache(after), state)
                self.assertEqual(reference.db.read_bytes(), core)
                self.assertEqual(reference.source.read_bytes(), source)
        return actual['result']

    def test_cached_signatures_thresholds_machine_only_and_repeat(self):
        request,reference=self.fixture_match()
        self.assertEqual(self.parity(request,reference),dict(pictures=2,matches=1,stopped=''))
        self.assertEqual(self.parity(request,reference),dict(pictures=0,matches=0,stopped=''))
        for cache in (reference.path,Path(request['cache'])):
            with sqlite3.connect(cache/'provenance.sqlite') as c:c.execute("DELETE FROM lookups WHERE backend='ref_compare'")
        self.assertEqual(self.parity(request,reference),dict(pictures=2,matches=0,stopped=''))

    def test_missing_core_signatures_are_computed_via_152_reader(self):
        request,reference=self.fixture_match(False)
        self.parity(request,reference)

    def test_reference_chunk_boundary_keeps_scores_and_counts(self):
        request,reference=self.fixture_match(many=True)
        self.assertEqual(self.parity(request,reference),dict(pictures=2,matches=4097,stopped=''))

    def test_first_source_write_failure_rolls_back_pair_without_seen_mark(self):
        request,reference=self.fixture_match()
        for cache in (reference.path,Path(request['cache'])):
            with sqlite3.connect(cache/'provenance.sqlite') as c:
                c.execute("CREATE TRIGGER fail_artist BEFORE INSERT ON image_artists BEGIN SELECT RAISE(ABORT,'synthetic transaction failure'); END")
        before=self.rows(Path(request['cache']))
        with self.assertRaises(sqlite3.IntegrityError):verify.local_run(reference,log=lambda _:None)
        self.probe(request,success=False)
        self.assertEqual(self.rows(Path(request['cache'])),before)
        self.assertEqual(self.rows(reference.path),before)

    def test_invalid_reference_grid_is_fatal_not_marked_seen(self):
        request,reference=self.fixture_match()
        for cache in (reference.path,Path(request['cache'])):
            with sqlite3.connect(cache/'provenance.sqlite') as c:c.execute("UPDATE ref_sigs SET grid=? WHERE post_id='0'",(b'short',))
        with self.assertRaises(ValueError):verify.local_run(reference,log=lambda _:None)
        self.probe(request,success=False)
        self.assertEqual(self.rows(Path(request['cache']))['lookups'],[])

    def test_malformed_reference_colour_is_skipped_and_picture_marked_seen(self):
        request,reference=self.fixture_match()
        for cache in (reference.path,Path(request['cache'])):
            with sqlite3.connect(cache/'provenance.sqlite') as c:c.execute("UPDATE ref_sigs SET colour=? WHERE post_id='0'",(b'short',))
        self.assertEqual(self.parity(request,reference),dict(pictures=2,matches=0,stopped=''))

    def test_absent_original_path_is_fatal_in_legacy_library(self):
        request,reference=self.fixture_match()
        with sqlite3.connect(reference.db) as c:c.execute("INSERT INTO assets VALUES ('null',NULL)")
        with self.assertRaises(AttributeError):verify.local_run(reference,log=lambda _:None)
        self.probe(request,success=False)
        self.assertEqual(self.rows(Path(request['cache']))['lookups'],[])

    def test_stored_ratio_float32_boundary_and_flat_rejection_are_exact(self):
        references = numeric_reference_mode.references(self)
        import numpy as np
        request,reference=self.fixture_match()
        library=Path(request['library']);path=library/'generated.png'
        ratio,grid=verify.signature(path)
        base=np.float32(ratio)
        edge=np.float32(base*np.exp(np.float32(.03)))
        values=[edge]
        lower=upper=edge
        for _ in range(12):
            lower=np.nextafter(lower,np.float32(0));upper=np.nextafter(upper,np.float32(np.inf))
            values.extend([lower,upper])
        with sqlite3.connect(reference.db) as c:
            for i,value in enumerate(values):
                asset=f'ratio-{i}'
                c.execute('INSERT INTO assets VALUES (?,?)',(asset,str(path)))
                c.execute('INSERT INTO image_sigs VALUES (?,?,?,1)',(asset,float(value),grid))
        self.complete_parity(request,reference,references)
