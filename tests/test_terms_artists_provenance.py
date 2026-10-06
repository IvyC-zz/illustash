"""main 合进来的找回画师行为与词条核心（docs/TERMS_CHARACTERS_PLAN.md「当前恢复入口」）：IQDB、同一张图的自动认定，
分组推荐 / OCR 的「也许是 TA」及原因，候选重算后失效，查无结果，重复交付，切换后对候选的是 / 不是与撤销。

对照：main 的旧读法（artists._for_asset、_assets）直接作用在当前的 provenance.sqlite 与当前人工判断上，就是
main 应该显示的；切换后核心的读路径必须与它相同。唯一的已知差别单独测：用户确认过的候选在重算后不再被提出时，
核心里「是」继续有效（机器证据更新不抹掉人工裁决），旧读法里它会消失。只用临时库，不联网。"""
import json
import os
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from web import artists, library
from web.provenance_job import Runner
from web.server import adopt_artists, adopt_terms
from web.store import Store
from provenance import db
from provenance.net import Net
from provenance.sources import iqdb
from tests.test_web import fixture
from tests import terms_seed
from tests.test_terms_core import core


def norm(view):
    """顺序不计：同一张图上的画师、候选各按 id 排。"""
    return {'artists': sorted(view['artists'], key=lambda a: a['id']), 'posts': view['posts'],
            'maybe': sorted(view.get('maybe', []), key=lambda m: m['id'])}


@unittest.skipIf(os.name == 'nt', 'macOS/Linux only for now')
class ProvenanceWithCore(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name).resolve()
        self.lib = library.create(root / 'A.pilib')
        self.store = Store(fixture(root), root / '核心准备.sqlite')
        self.store.refresh()
        with db.connect(self.lib) as c:
            self.x = db.artist_for(c, [('danbooru', 'artist_x', '')], 'test')
            self.y = db.artist_for(c, [('danbooru', 'artist_y', '')], 'test')
            self.z = db.artist_for(c, [('danbooru', 'artist_z', '')], 'test')
            c.executemany("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?)", [
                ('a', self.x, 'iqdb', 0.97, '{"post": 1}', 'auto'),
                ('a', self.z, 'group_run', 0.3, '{"group": "run", "count": 2, "from": ["b"]}', 'candidate'),
                ('b', self.x, 'same_picture', 1.0, '{"group": "duplicate", "count": 1}', 'auto'),
                ('b', self.y, 'ocr', 0.8, '{"text": "@y"}', 'candidate'),
                ('c', self.y, 'group_oc', 0.9, '{"group": "oc", "count": 3, "from": ["a"]}', 'candidate')])
        self.store = terms_seed.startup_input(self.store, db=self.lib.db)
        with self.store.connect() as c:
            c.execute("INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('c',?,'yes')", (self.y,))
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)',
                      ('已确认 1 张是 Y 画的', json.dumps([{'artist_verdict':{'uuid':'c','artist':self.y,'had':None}}])))

    def adopt(self):
        adopted = adopt_artists(self.store, self.lib)
        if adopted:
            terms_seed.switch(self.store, artists=False)
        return adopted

    def legacy(self, asset):
        """main 的旧读法，作用在当前的溯源库与当前的人工判断上。"""
        c = artists._open(self.lib)
        try:
            return artists._for_asset(c, asset, self.store.uuids(), self.store.artist_verdicts(asset),
                                      self.store.artist_verdicts(), False)
        finally:
            c.close()

    def legacy_assets(self, artist):
        c = artists._open(self.lib)
        try:
            verdicts = self.store.artist_verdicts()
            return sorted(a for a in artists._assets(c, artist, artists._yes(verdicts, artist)) if a not in artists._no(verdicts, artist))
        finally:
            c.close()

    def shown(self, asset):
        return artists.for_asset(self.lib, asset, self.store.uuids(), self.store.artist_verdicts(asset), self.store.artist_verdicts())

    def assert_same(self, *, skip=(), skip_artists=()):
        for asset in ('a', 'b', 'c'):
            if asset not in skip:
                self.assertEqual(norm(self.shown(asset)), norm(self.legacy(asset)), asset)
        for artist in (self.x, self.y, self.z):
            if artist in skip_artists:
                continue
            self.assertEqual(sorted(artists.assets_of(self.lib, artist, self.store.artist_verdicts())), self.legacy_assets(artist))

    def provenance(self, *statements):
        with db.connect(self.lib) as c:
            for sql, args in statements:
                c.execute(sql, args)

    def test_new_methods_candidates_and_their_lifecycle(self):
        self.assert_same()                                             # 切换之前：旧读法本身
        self.assertTrue(self.adopt())
        self.assert_same()
        view = self.shown('a')
        self.assertEqual([a['methods'] for a in view['artists']], [['iqdb']])
        self.assertEqual([(m['id'], m['reason']) for m in view['maybe']], [(self.z, '同一段保存的图里有 2 张是 TA 画的')])
        self.assertEqual(self.shown('c')['artists'][0]['confirmed'], True)
        runner = Runner(self.lib)
        first = runner.deliver()
        self.assertEqual((first['status'], first['added']), ('done', 0), first)
        self.assert_same()
        again = runner.deliver()                                       # 同样的快照：不重复入库、结果不变
        self.assertTrue(again['already'] or again['added'] == 0, again)
        self.assert_same()
        # 分组推荐重算：a 上的候选没再提出；OCR 候选改了分数。
        self.provenance(("DELETE FROM image_artists WHERE asset='a' AND method='group_run'", ()),
                        ("UPDATE image_artists SET score=0.9,evidence='{\"text\": \"@yy\"}' WHERE asset='b' AND method='ocr'", ()))
        runner.deliver()
        self.assert_same()
        self.assertEqual(self.shown('a')['maybe'], [])
        # 切换后对候选说「是」、对 IQDB 认定说「不是」，再撤销。
        yes = self.store.judge_artist(['b'], self.y, 'yes', 'Y')
        self.assert_same()
        no = self.store.judge_artist(['a'], self.x, 'no', 'X')
        self.assert_same()
        self.store.undo(no['id'])
        self.store.undo(yes['id'])
        self.assert_same()
        # 查无结果：b 这次什么都没有。
        self.provenance(("DELETE FROM image_artists WHERE asset='b'", ()))
        runner.deliver()
        self.assert_same()
        self.assertEqual(self.shown('b')['artists'], [])

    def test_a_confirmed_candidate_stays_when_the_machine_stops_offering_it(self):
        """唯一的已知差别：机器证据更新不抹掉人工裁决。"""
        self.assertTrue(self.adopt())
        runner = Runner(self.lib)
        runner.deliver()
        self.provenance(("DELETE FROM image_artists WHERE asset='c'", ()))
        runner.deliver()
        self.assert_same(skip=('c',), skip_artists=(self.y,))
        self.assertEqual([(a['id'], a['confirmed']) for a in self.shown('c')['artists']], [(self.y, True)])
        self.assertEqual(self.legacy('c')['artists'], [])              # 旧读法：行没了，确认也跟着不见了
        self.assertIn('c', artists.assets_of(self.lib, self.y, self.store.artist_verdicts()))


FAKE_IQDB = r'''
import json, sys
for line in sys.stdin:
    a = json.loads(line)['asset']
    if a == 'c':
        post = {'id': 42, 'tag_string_artist': 'iqdb_artist', 'source': ''}
        print(json.dumps({'asset': a, 'status': 'hit', 'score': 91.5, 'post': post, 'bytes': 100}), flush=True)
    else:
        print(json.dumps({'asset': a, 'status': 'miss', 'score': 27.0, 'post': None, 'bytes': 100}), flush=True)
'''


@unittest.skipIf(os.name == 'nt', 'macOS/Linux only for now')
class IqdbOrderWithCore(unittest.TestCase):
    """IQDB 的收益排序读 asset_rating：切换前是表，标签切到核心后是核心投影的视图，两种状态下排序相同；排序只决定
    先问哪张图，不影响人工裁决、撤销、分组候选和交包。"""
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name).resolve()
        self.lib = library.create(root / 'A.pilib')
        source = fixture(root)
        with sqlite3.connect(source) as c:
            c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [
                ('a', 'rating', 'general', 0.8), ('b', 'rating', 'explicit', 0.7), ('c', 'rating', 'sensitive', 0.9)])
        self.store = Store(source, root / '核心准备.sqlite')
        self.store.refresh()
        self.store = terms_seed.startup_input(self.store, db=self.lib.db)
        with sqlite3.connect(self.lib.source) as s:
            s.execute('CREATE TABLE IF NOT EXISTS assets(uuid TEXT,added_at TEXT)')
            s.executemany('INSERT INTO assets(uuid,added_at) VALUES (?,?)', [(a, '2025-01-01T10:00:00') for a in 'abc'])
        # 原队列：Pixiv 保存段代表图 a 与 X 图交替 → a b c；收益排序：(x,sensitive) c → (pixiv,general) a → (x,explicit) b。
        self.pictures = [{'asset': 'a', 'preset': 'pixiv', 'source_id': 's1', 'src_path': str(root / 'a.png')},
                         {'asset': 'b', 'preset': 'x', 'source_id': 's1', 'src_path': str(root / 'b.png')},
                         {'asset': 'c', 'preset': 'x', 'source_id': 's1', 'src_path': str(root / 'c.png')}]
        for patch in (mock.patch.object(iqdb.assets, 'pictures', side_effect=lambda *_: [dict(p) for p in self.pictures]),
                      mock.patch('provenance.sources.groups.unsolved_originals', return_value=[])):
            patch.start()
            self.addCleanup(patch.stop)
        with db.connect(self.lib) as c:
            self.x = db.artist_for(c, [('danbooru', 'artist_x', '')], 'test')
            c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES "
                      "('b',?,'group_run',0.3,'{\"group\": \"run\", \"count\": 2, \"from\": [\"a\"]}','candidate')", (self.x,))
        self.fake = root / 'fake_iqdb'
        self.fake.write_text(f'#!{sys.executable}\n' + FAKE_IQDB)
        self.fake.chmod(0o755)

    def kind(self, name):
        with self.store.connect() as c:
            return c.execute('SELECT type FROM sqlite_master WHERE name=?', (name,)).fetchone()[0]

    def order(self, only=None):
        with db.connect(self.lib) as c:
            return [p['asset'] for p in iqdb.order(self.lib, c, only)]

    def shown(self, asset):
        return norm(artists.for_asset(self.lib, asset, self.store.uuids(), self.store.artist_verdicts(asset),
                                      self.store.artist_verdicts()))

    def legacy(self, asset):
        c = artists._open(self.lib)
        try:
            return norm(artists._for_asset(c, asset, self.store.uuids(), self.store.artist_verdicts(asset),
                                           self.store.artist_verdicts(), False))
        finally:
            c.close()

    def test_order_is_the_same_on_the_table_and_on_the_core_view(self):
        ratings = {'a': 'general', 'b': 'explicit', 'c': 'sensitive'}
        self.assertEqual(self.kind('asset_rating'), 'table')
        self.assertEqual(iqdb._ratings(self.lib), ratings)
        self.assertEqual(self.order(), ['c', 'a', 'b'])
        self.assertEqual(self.order(only=['c', 'b']), ['b', 'c'])            # 手动选图：原顺序，不排序
        adopt_terms(self.store)
        self.assertTrue(adopt_artists(self.store, self.lib))
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_characters'").fetchone()[0], 'core')
        self.assertEqual(self.kind('asset_rating'), 'view')
        self.assertEqual(iqdb._ratings(self.lib), ratings)
        self.assertEqual(self.order(), ['c', 'a', 'b'])
        self.assertEqual(self.order(only=['c', 'b']), ['b', 'c'])

    def test_prioritised_run_hands_over_like_before(self):
        adopt_terms(self.store)
        self.assertTrue(adopt_artists(self.store, self.lib))
        yes = self.store.judge_artist(['a'], self.x, 'yes', 'X')
        self.assertEqual(self.order(), ['c', 'a', 'b'])   # 与 main 相同：「已解决」只看溯源库的 image_artists，人的「是」不在那里
        runner = Runner(self.lib)
        runner.deliver()
        before_b = self.shown('b')
        self.assertEqual([m['id'] for m in before_b['maybe']], [self.x])  # 分组候选
        net = Net(1)
        net.authorized = True
        result = iqdb.run(self.lib, net, log=lambda m: None, run_binary=self.fake)
        self.assertEqual((result['asked'], result['hits'], result['artists']), (3, 1, 1))
        self.assertEqual(self.order(), [])                                   # 命中已解决，未命中在退避期：无遗漏、无重复
        self.assertEqual(runner.deliver()['status'], 'done')
        for asset in 'bc':   # a：溯源库里没有行的图上的「是」，旧读法看不到（TERMS_ARTISTS 第 3 节的已知差别），单独断言
            self.assertEqual(self.shown(asset), self.legacy(asset), asset)
        found = self.shown('c')['artists']
        self.assertEqual([a['methods'] for a in found], [['iqdb']])
        self.assertEqual(self.shown('b'), before_b)                           # 分组候选不受影响
        self.assertTrue(self.shown('a')['artists'][0]['confirmed'])
        no = self.store.judge_artist(['c'], found[0]['id'], 'no', 'IQDB')
        self.assertEqual(self.shown('c'), self.legacy('c'))
        self.store.undo(no['id'])
        self.store.undo(yes['id'])
        self.assertEqual(self.shown('a')['artists'], [])
        for asset in 'abc':
            self.assertEqual(self.shown(asset), self.legacy(asset), asset)
        self.assertEqual(self.order(), [])                                   # 撤销人工裁决不改 IQDB 的退避


if __name__ == '__main__':
    unittest.main()
