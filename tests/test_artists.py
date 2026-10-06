"""核心读取画师：只读 provenance.sqlite，详情里的画师、主页和原帖，按画师筛选图库。只用临时库。"""
import tempfile
import unittest
from pathlib import Path

from web import artists, library
from web.store import Invalid, Store
from provenance import db
from tests.test_web import fixture


class ArtistsReadTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.lib = library.create(self.root / 'A.pilib')
        with db.connect(self.lib) as c:
            self.a = db.artist_for(c, [('danbooru', 'foo_(bar)', ''), ('x', 'foo_bar', ''), ('pixiv', '9', '')], 'test')
            self.b = db.artist_for(c, [('danbooru', 'plain_name', '')], 'test')
            c.executemany("INSERT INTO image_artists(asset,artist_id,method,status) VALUES (?,?,'test','auto')",
                          [('a', self.a), ('b', self.a), ('c', self.b)])
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,method) VALUES ('a','danbooru','1','https://danbooru.donmai.us/posts/1','test')")
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,method) VALUES ('a','pixiv','5','https://www.pixiv.net/artworks/5','test')")
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,method) VALUES ('a','danbooru','2','https://danbooru.donmai.us/posts/2','test')")

    def tearDown(self):
        self.temp.cleanup()

    def test_detail_names_homes_posts_and_counts(self):
        info = artists.for_asset(self.lib, 'a')
        [a] = info['artists']
        self.assertEqual((a['name'], a['count']), ('@foo_bar', 2))
        self.assertEqual([(h['site'], h['label']) for h in a['homes']], [('X', 'X'), ('Pixiv', 'Pixiv'), ('Danbooru', 'Danbooru')])
        self.assertEqual([p['site'] for p in info['posts']], ['Pixiv', 'Danbooru'])       # the original first, one per site
        self.assertEqual(artists.for_asset(self.lib, 'c')['artists'][0]['name'], 'plain name')
        self.assertEqual(artists.for_asset(self.lib, 'nobody'), {'artists': [], 'posts': [], 'maybe': []})
        self.assertEqual(artists.for_asset(library.create(self.root / 'Empty.pilib'), 'a'), {'artists': [], 'posts': []})

    def test_group_candidates_are_maybe_until_confirmed(self):
        """A group recommendation is 「也许是 TA」 with its reason; 是 TA makes it the picture's artist and counts it."""
        import json
        with db.connect(self.lib) as c:
            c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES ('d',?,'group_oc',0.9,?,'candidate')",
                      (self.a, json.dumps({'group': 'oc', 'count': 2})))
        info = artists.for_asset(self.lib, 'd')
        self.assertEqual(info['artists'], [])
        self.assertEqual([(m['id'], m['reason']) for m in info['maybe']], [(self.a, '同一原创角色里有 2 张是 TA 画的')])
        self.assertEqual(sorted(artists.assets_of(self.lib, self.a)), ['a', 'b'])            # not counted yet
        yes = {self.a: {'d': 'yes'}}
        info = artists.for_asset(self.lib, 'd', verdicts={self.a: 'yes'}, rejected=yes)
        self.assertEqual(([a['id'] for a in info['artists']], info['maybe']), ([self.a], []))
        self.assertTrue(info['artists'][0]['confirmed'])
        self.assertEqual(info['artists'][0]['count'], 3)
        self.assertEqual(sorted(artists.assets_of(self.lib, self.a, rejected=yes)), ['a', 'b', 'd'])
        self.assertEqual(artists.for_asset(self.lib, 'd', verdicts={self.a: 'no'})['maybe'], [])   # 不是: gone

    def test_merged_artist_shows_the_kept_one(self):
        with db.connect(self.lib) as c:
            c.execute('UPDATE artists SET merged_into=? WHERE id=?', (self.a, self.b))
        self.assertEqual(artists.for_asset(self.lib, 'c')['artists'][0]['id'], self.a)
        self.assertEqual(sorted(artists.assets_of(self.lib, self.a)), ['a', 'b', 'c'])

    def test_library_filtered_to_an_artist(self):
        store = Store(fixture(self.root), self.root / 'app.sqlite')
        store.refresh()
        with self.assertRaises(Invalid):
            store.ids({'artist': self.a})                          # no provenance wired in
        store.artist_assets = lambda artist: artists.assets_of(self.lib, artist)
        self.assertEqual(sorted(store.ids({'artist': self.a})), ['a', 'b'])
        self.assertEqual(store.assets(artist=self.b)['total'], 1)

    def test_verdicts_hide_or_confirm_an_artist_and_undo(self):
        store = Store(fixture(self.root), self.root / 'app.sqlite')
        store.refresh()
        # 保留独立只读 provenance 参考；核心判断用公开证据包注册同一组画师身份。
        import copy
        import sqlite3
        from provenance import export
        from web import terms_core
        incoming = copy.copy(self.lib)
        incoming.path = self.root / '画师交付输入'
        incoming.path.mkdir()
        incoming.config = self.lib.config | {'id': store.library_id()}
        for source, target in ((store.path, incoming.path / 'library.sqlite'),
                               (db.path_of(self.lib), db.path_of(incoming))):
            with sqlite3.connect(source) as src, sqlite3.connect(target) as dst:
                src.backup(dst)
        terms_core.import_package(store.path, store.library_id(), export.write(incoming, self.root / '画师证据'))
        read = lambda asset: artists.for_asset(self.lib, asset, verdicts=store.artist_verdicts(asset), rejected=store.artist_verdicts())
        undo = store.judge_artist(['a'], self.a, 'no', '@foo_bar')
        self.assertEqual(undo['label'], '已标记 1 张不是 @foo_bar 画的')
        self.assertEqual(read('a')['artists'], [])                             # hidden on that picture only
        self.assertEqual(len(read('a')['posts']), 2)                           # posts not tied to the artist stay
        self.assertEqual(read('b')['artists'][0]['count'], 1)                  # and no longer counted
        self.assertEqual(artists.assets_of(self.lib, self.a, rejected=store.artist_verdicts()), ['b'])
        store.undo(undo['id'])
        self.assertEqual(read('a')['artists'][0]['count'], 2)
        store.judge_artist(['a'], self.a, 'yes')
        self.assertTrue(read('a')['artists'][0]['confirmed'])
        store.judge_artist(['a'], self.a, None)
        self.assertEqual(store.artist_verdicts('a'), {})
        with db.connect(self.lib) as c:                                         # machine results never touched
            self.assertEqual(c.execute('SELECT count(*) FROM image_artists').fetchone()[0], 3)
        with self.assertRaises(Invalid):
            store.judge_artist(['a'], self.a, 'maybe')
        with self.assertRaises(Invalid):
            store.judge_artist(['nobody'], self.a, 'no')

    def test_a_rejected_artist_takes_its_matched_posts_along(self):
        with db.connect(self.lib) as c:
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES ('c','pixiv','7','https://www.pixiv.net/artworks/7',?,'gallery_match')", (self.b,))
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES ('c','danbooru','8','https://danbooru.donmai.us/posts/8',?,'pixel_hash')", (self.b,))
        self.assertEqual([p['site'] for p in artists.for_asset(self.lib, 'c', verdicts={self.b: 'no'})['posts']], ['Danbooru'])

    def test_acceptance_sample_is_fixed_and_keeps_answered_pictures(self):
        first = artists.audit(self.lib, {'a', 'b', 'c'}, {}, n=2)
        self.assertEqual((first['pool'], len(first['items'])), (3, 2))
        self.assertEqual(artists.audit(self.lib, {'a', 'b', 'c'}, {}, n=2), first)                     # same pictures every time
        self.assertEqual({i['uuid'] for i in artists.audit(self.lib, {'a', 'b'}, {}, n=5)['items']}, {'a', 'b'})   # only pictures in the library
        uuid = first['items'][0]['uuid']
        artist = first['items'][0]['artists'][0]['id']
        again = artists.audit(self.lib, {'a', 'b', 'c'}, {artist: {uuid: 'no'}}, n=2)
        self.assertEqual([i['uuid'] for i in again['items']], [i['uuid'] for i in first['items']])    # answering never redraws
        self.assertTrue(again['items'][0]['artists'][0]['rejected'])                                  # a 不对 stays visible here
        self.assertEqual(first['items'][0]['artists'][0]['methods'], ['test'])

    def test_runs_and_coverage(self):
        self.assertEqual(artists.runs(self.lib), {'runs': [], 'coverage': None})
        with db.connect(self.lib) as c:
            c.execute("INSERT INTO runs(kind,steps,status,found,traffic_mb,report) VALUES ('full','pixel_hash','done',3,1.5,?)",
                      ('{"numbers": {"pictures": {"x": 10}, "with_artist": {"x": 3}, "with_original_post": 2}}',))
            c.execute("INSERT INTO runs(kind,steps) VALUES ('new','pixel_hash,verify')")
        out = artists.runs(self.lib)
        self.assertEqual([r['status'] for r in out['runs']], ['running', 'done'])   # newest first
        self.assertEqual(out['coverage']['with_artist'], {'x': 3})
        self.assertIsNone(artists.runs(library.create(self.root / 'Empty.pilib')))


if __name__ == '__main__':
    unittest.main()
