"""画师切到词条核心（docs/TERMS_ARTISTS.md 第 3 段）：切换、核心读写、撤销新旧步骤、worker 交证据包。只用临时库。"""
import contextlib
import io
import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from web import artists, library, terms_core
from web.provenance_job import Runner
from web.server import adopt_artists
from web.store import Invalid, Store
from provenance import db, export
from tests.test_web import fixture
from tests import terms_seed
from tests.test_terms_core import core


class CanonicalTest(unittest.TestCase):
    def test_floats_match_serde_json(self):
        # The same literals native/terms/src/strict_json.rs pins serde_json to (float_layout).
        values = [1e-7, 5.72e-5, 1e-5, 0.1, 123.0, 1e15, 1e16, 1e21, 1.5e22, -0.0, 2.099447491238697e-9, 0.97, -3.25]
        self.assertEqual(export.canonical(values),
                         '[1e-7,0.0000572,0.00001,0.1,123.0,1000000000000000.0,1e+16,1e+21,1.5e+22,-0.0,2.099447491238697e-9,0.97,-3.25]')
        self.assertEqual(export.canonical({'b': [1, True, None, 'é\n'], 'a': {'y': 2.5e-05}}), '{"a":{"y":0.000025},"b":[1,true,null,"é\\n"]}')


@unittest.skipIf(os.name == 'nt', 'macOS/Linux only for now')
class ArtistsInCoreTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.lib = library.create(self.root / 'A.pilib')
        self.store = Store(fixture(self.root), self.root / '核心准备.sqlite')
        self.store.refresh()
        with db.connect(self.lib) as c:
            self.a = db.artist_for(c, [('danbooru', 'foo_(bar)', ''), ('x', 'foo_bar', 'https://x.com/foo_bar')], 'test')
            self.b = db.artist_for(c, [('danbooru', 'plain_name', '')], 'test')
            c.executemany("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?)",
                          [('a', self.a, 'pixel_hash', 1, '{"danbooru_post": 1}', 'auto'),
                           ('b', self.a, 'pixel_hash', 1, '{"danbooru_post": 2}', 'auto'),
                           ('c', self.b, 'ocr', 0.8, '{"text": "@plain"}', 'candidate')])
            c.execute("INSERT INTO artist_names(artist_id,source,name) VALUES (?,'pixiv','フー')", (self.a,))
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES ('a','pixiv','5','https://www.pixiv.net/artworks/5',?,'gallery_match')", (self.a,))
        self.store = terms_seed.startup_input(self.store, db=self.lib.db)
        # 旧判断及其原历史作为启动输入；被测判断和撤销均在完整核心里执行。
        with self.store.connect() as c:
            c.execute("INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('a',?,'no')", (self.a,))
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)',
                      ('已标记 1 张不是 フー 画的', json.dumps([{'artist_verdict':{'uuid':'a','artist':self.a,'had':None}}])))
            self.legacy = Store.last_action(c)

    def tearDown(self):
        self.temp.cleanup()

    def adopt(self):
        adopted = adopt_artists(self.store, self.lib)
        if adopted:
            terms_seed.switch(self.store, artists=False)
        return adopted

    def shown(self, asset):
        return [(a['id'], a['count'], a['confirmed']) for a in
                artists.for_asset(self.lib, asset, self.store.uuids(), self.store.artist_verdicts(asset), self.store.artist_verdicts())['artists']]

    def frozen(self):
        """The old table's rows; None once the switch has dropped it (terms schema 4 keeps no frozen copy)."""
        with sqlite3.connect(Path(self.lib.path) / 'library.sqlite') as c:
            if not c.execute("SELECT 1 FROM sqlite_master WHERE name='artist_verdicts'").fetchone():
                return None
            return c.execute('SELECT uuid,verdict FROM artist_verdicts ORDER BY uuid').fetchall()

    def test_the_whole_hand_over(self):
        before = (self.shown('a'), self.shown('b'), artists.name_of(self.lib, self.a))
        self.assertTrue(self.adopt())
        self.assertTrue(self.adopt())                      # once only
        self.assertEqual((self.shown('a'), self.shown('b'), artists.name_of(self.lib, self.a)), before)
        self.assertEqual(self.shown('b'), [(self.a, 1, False)])
        self.assertEqual(self.shown('c'), [])                                     # OCR alone is only 也许是
        # The first package after cutover recognises every attribution the cutover already took.
        runner = Runner(self.lib)
        first = runner.deliver()
        self.assertEqual((first['status'], first['added'], first['duplicate']), ('done', 0, 3), first)
        # The core writes 是 / 不是 now; the old table stays as it was.
        step = self.store.judge_artist(['b'], self.a, 'no', 'フー')
        self.assertEqual(step['label'], '已标记 1 张不是 フー 画的')
        self.assertEqual(self.shown('b'), [])
        self.assertEqual(self.store.artist_verdicts(), {self.a: {'a': 'no', 'b': 'no'}})
        self.assertIsNone(self.frozen())                                          # the switch dropped the old table
        # Undo the core's step, then the old one (the core took it over at cutover).
        after = self.store.undo(step['id'])
        self.assertEqual(after['id'], self.legacy['id'])
        self.assertEqual(self.shown('b'), [(self.a, 1, False)])
        self.assertIsNone(self.store.undo(self.legacy['id']))
        self.assertEqual(self.shown('a'), [(self.a, 2, False)])
        self.assertIsNone(self.frozen())
        # 是 on an OCR-only picture shows it, confirmed.
        self.store.judge_artist(['c'], self.b, 'yes', 'plain name')
        self.assertEqual(self.shown('c'), [(self.b, 1, True)])
        self.assertEqual(sorted(artists.assets_of(self.lib, self.a)), ['a', 'b'])
        audit = artists.audit(self.lib, self.store.uuids(), self.store.artist_verdicts(), 50)
        self.assertEqual((audit['pool'], [i['uuid'] for i in audit['items']] and sorted(i['uuid'] for i in audit['items'])), (2, ['a', 'b']))
        # Posts are still provenance's.
        self.assertEqual(artists.for_asset(self.lib, 'a', self.store.uuids())['posts'], [{'site': 'Pixiv', 'url': 'https://www.pixiv.net/artworks/5'}])

    def test_new_worker_results_arrive_as_packages(self):
        self.adopt()
        runner = Runner(self.lib)
        with db.connect(self.lib) as c:
            new = db.artist_for(c, [('x', 'newcomer', '')], 'test')
            c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES ('c',?,'gallery_match',0.97,'{}','auto')", (new,))
            c.execute("UPDATE image_artists SET score=0.9,updated_at='2026-09-29 00:00:00' WHERE asset='c' AND artist_id=?", (self.b,))
        done = runner.deliver()
        self.assertEqual((done['added'], done['created_terms']), (2, 1), done)
        self.assertEqual(self.shown('c'), [(new, 1, False)])                     # the key is provenance's artist id
        self.assertEqual(artists.name_of(self.lib, new), '@newcomer')
        self.assertTrue(runner.deliver()['already'])                              # same snapshot, nothing written
        self.assertFalse((Path(self.lib.path) / '.evidence').exists() and any((Path(self.lib.path) / '.evidence').iterdir()))

    def test_without_the_core_the_library_opens_read_only(self):
        with mock.patch.dict(os.environ, {'ILLUSTASH_TERMS_BIN': str(self.root / 'missing')}), contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(adopt_artists(self.store, self.lib))
            self.assertIsNone(Runner(self.lib).deliver())
        self.assertEqual(self.shown('a'), [])
        self.assertIn('找不到词条核心程序', self.store.read_only)
        with self.assertRaises(Invalid) as refused:
            self.store.judge_artist(['b'], self.a, 'no')
        self.assertEqual(str(refused.exception), self.store.read_only)
        with self.assertRaises(sqlite3.DatabaseError), self.store.connect() as c:
            c.execute("INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('b',?,'no')", (self.a,))                          # the old tables are not written any more
        self.assertEqual(self.frozen(), [('a', 'no')])

    def test_a_failed_hand_over_is_recorded_not_raised(self):
        self.adopt()
        runner = Runner(self.lib)
        with mock.patch.object(terms_core, 'import_package', side_effect=terms_core.Failed('boom')):
            self.assertEqual(runner.deliver(), {'status': 'failed', 'error': 'boom'})
        self.assertEqual(runner.deliver()['status'], 'done')

    def test_core_refusals_reach_people(self):
        self.adopt()
        with self.assertRaisesRegex(Invalid, '词条不存在'):
            self.store.judge_artist(['a'], 'NO-SUCH-ARTIST', 'yes')
        with self.assertRaisesRegex(Invalid, '图片不存在'):
            self.store.judge_artist(['nope'], self.a, 'yes')


if __name__ == '__main__':
    unittest.main()
