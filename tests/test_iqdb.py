"""IQDB 种子来源：收益排序与原队列保底、结果入库、未命中退避、拒绝即停。
用假的 illustash-iqdb 程序代替真实查询，不联网。签名算法本身由 crates/iqdb 的 cargo test 覆盖。"""
import json
import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from provenance import db, evidence
from provenance.net import Net
from provenance.sources import iqdb
from tests.native_worker_fixture import command as worker_command
from web import library


FAKE = r'''
import json, sys
for line in sys.stdin:
    item = json.loads(line)
    a = item['asset']
    if a == 'refused':
        print(json.dumps({'stopped': 'danbooru.donmai.us: HTTP 429'}), flush=True); break
    if a.startswith('hit'):
        post = {'id': 42, 'tag_string_artist': 'some_artist', 'source': 'https://i.pximg.net/img-original/img/2024/01/02/03/04/05/777_p0.png'}
        print(json.dumps({'asset': a, 'status': 'hit', 'score': 91.5, 'post': post, 'bytes': 13000}), flush=True)
    elif a.startswith('net'):
        print(json.dumps({'asset': a, 'status': 'error', 'error': 'timeout'}), flush=True)
    else:
        print(json.dumps({'asset': a, 'status': 'miss', 'score': 27.0, 'post': None, 'bytes': 13000}), flush=True)
'''


class IqdbTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.lib = library.create(Path(self.temp.name).resolve() / 'A.pilib')
        fake = Path(self.temp.name) / 'fake_iqdb'
        fake.write_text(f'#!{sys.executable}\n' + FAKE)
        # Windows cannot execute the script itself; the shared fixture launcher wraps it in a .cmd.
        self.binary = Path(worker_command(fake))
        self.net = Net(1)
        self.net.authorized = True

    def run_with(self, assets_):
        todo = [{'asset': a, 'preset': 'x', 'src_path': f'/pics/{a}.jpg', 'source_id': 's1'} for a in assets_]
        with mock.patch.object(iqdb, 'order', return_value=todo):
            return iqdb.run(self.lib, self.net, log=lambda m: None, run_binary=self.binary)

    def test_hits_are_recorded_with_their_upstream_work(self):
        result = self.run_with(['hit-a', 'miss-b', 'net-c'])
        self.assertEqual((result['asked'], result['hits'], result['artists'], result['failed']), (3, 1, 1, 1))
        self.assertEqual(self.net.used, 2 * 13000)
        with db.connect(self.lib) as c:
            self.assertEqual({tuple(r) for r in c.execute("SELECT asset, site, post_id, method FROM image_sources")},
                             {('hit-a', 'danbooru', '42', 'iqdb'), ('hit-a', 'pixiv', '777', 'iqdb')})   # 777 anchors verification
            artist = c.execute("SELECT artist_id FROM image_artists WHERE asset='hit-a' AND method='iqdb' AND status='auto'").fetchone()[0]
            self.assertEqual(c.execute("SELECT key FROM artist_links WHERE artist_id=? AND system='danbooru'", (artist,)).fetchone()[0], 'some_artist')
            self.assertEqual(c.execute("SELECT source FROM seeds WHERE artist_id=?", (artist,)).fetchone()[0], 'iqdb')
            self.assertFalse(evidence.due(c, 'miss-b', 'iqdb'))                      # asked again in 90 days
            self.assertTrue(evidence.due(c, 'net-c', 'iqdb'))                        # network trouble: next run

    def test_a_refusal_stops_danbooru_for_the_run(self):
        result = self.run_with(['miss-a', 'refused', 'hit-b'])
        self.assertIn('429', result['stopped'])
        self.assertEqual(result['asked'], 1)
        self.assertIn('danbooru.donmai.us', self.net.stopped)
        self.assertIn('429', iqdb.run(self.lib, self.net, log=lambda m: None)['stopped'])   # no second try this run

    def test_a_lookup_asks_again_about_the_picked_pictures(self):
        """按需查找: the picked pictures still without an artist, even when their last miss is not due yet."""
        pics = [{'asset': a, 'preset': 'x', 'src_path': '', 'source_id': 's1'} for a in ('picked', 'other', 'solved')]
        with db.connect(self.lib) as c:
            iqdb._looked(c, 'picked', 'miss')
            artist = db.artist_for(c, [('danbooru', 'someone', '')], 'test')
            c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES ('solved',?,'iqdb',1,'{}','auto')", (artist,))
            with mock.patch.object(iqdb.assets, 'pictures', return_value=pics):
                self.assertEqual([p['asset'] for p in iqdb.order(self.lib, c, only=['picked', 'solved'])], ['picked'])

    def test_not_without_authorisation(self):
        self.net.authorized = False
        self.assertEqual(iqdb.run(self.lib, self.net, log=lambda m: None)['stopped'], '外部请求未授权')

    def test_order_save_run_representatives_and_x_first(self):
        def pic(a, preset, t):
            return {'asset': a, 'preset': preset, 'src_path': '', 'source_id': 's1', 'added': t}
        pics = [pic(f'p{i}', 'pixiv', f'2025-01-01T10:{i:02d}:00') for i in range(8)]      # one run of 8 saves
        pics += [pic('p-alone', 'pixiv', '2025-02-01T10:00:00'), pic('x1', 'x', '2025-01-01T09:00:00')]
        added = {p['asset']: p.pop('added') for p in pics}

        class Source:
            def __enter__(self): return self
            def __exit__(self, *a): return False
            def execute(self, q): return list(added.items())
        with mock.patch.object(iqdb.assets, 'pictures', return_value=pics), mock.patch.object(iqdb.assets, 'open_source', return_value=Source()):
            with db.connect(self.lib) as c:
                got = [p['asset'] for p in iqdb.order(self.lib, c)]
        self.assertEqual(got[:5], ['p0', 'x1', 'p2', 'p4', 'p-alone'])              # at most 3 per run of saves, X alternating
        self.assertEqual(sorted(got[5:]), ['p1', 'p3', 'p5', 'p6', 'p7'])

    def test_priority_reserves_every_fifth_slot_for_the_original_queue(self):
        low = [{'asset': f'low{i}', 'preset': 'pixiv'} for i in range(5)]
        high = [{'asset': f'high{i}', 'preset': 'x'} for i in range(10)]
        pictures = low + high
        ratings = {p['asset']: 'explicit' if p['preset'] == 'pixiv' else 'sensitive' for p in pictures}
        result = iqdb._prioritize(pictures, ratings)
        ids = [p['asset'] for p in result]
        self.assertEqual(ids[:5], ['high0', 'high1', 'high2', 'high3', 'low0'])
        self.assertEqual(ids[5:10], ['high4', 'high5', 'high6', 'high7', 'low1'])
        self.assertEqual(ids[10:], ['high8', 'high9', 'low2', 'low3', 'low4'])
        self.assertEqual(len(set(ids)), len(pictures))
        self.assertEqual({p['asset'] for p in pictures}, set(ids))
        self.assertEqual(pictures, low + high)   # do not mutate the caller's queue

    def test_priority_is_stable_and_never_drops_an_unknown_rating(self):
        for count in range(21):
            pictures = [{'asset': str(i), 'preset': 'x'} for i in range(count)]
            self.assertEqual(iqdb._prioritize(pictures, {}), pictures)
            self.assertEqual(iqdb._prioritize(pictures, {p['asset']: 'general' for p in pictures}), pictures)
        pictures = [{'asset': 'low', 'preset': 'pixiv'}, {'asset': 'unknown', 'preset': 'other'},
                    {'asset': 'high', 'preset': 'x'}]
        ratings = {'low': 'explicit', 'high': 'sensitive'}
        self.assertEqual([p['asset'] for p in iqdb._prioritize(pictures, ratings)], ['high', 'unknown', 'low'])

    def test_ratings_are_read_only_and_optional(self):
        self.assertEqual(iqdb._ratings(self.lib), {})
        with sqlite3.connect(self.lib.db) as core:
            core.execute('CREATE TABLE IF NOT EXISTS asset_rating(uuid TEXT PRIMARY KEY,rating TEXT)')
            core.execute("INSERT INTO asset_rating VALUES ('a','general')")
        self.assertEqual(iqdb._ratings(self.lib), {'a': 'general'})

    def test_automatic_order_prioritizes_only_eligible_pictures(self):
        pics = [{'asset': a, 'preset': 'pixiv' if a.startswith('p') else 'x',
                 'src_path': '', 'source_id': 's1'} for a in ('p-low', 'x-high', 'x-solved', 'x-cached')]
        with sqlite3.connect(self.lib.source) as source:
            source.execute('CREATE TABLE assets(uuid TEXT,added_at TEXT)')
            source.executemany('INSERT INTO assets VALUES (?,?)', [(p['asset'], '2025-01-01T10:00:00') for p in pics])
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'someone', '')], 'test')
            c.execute("INSERT INTO image_artists(asset,artist_id,method,status) VALUES ('x-solved',?,'test','auto')", (artist,))
            iqdb._looked(c, 'x-cached', 'miss')
            with mock.patch.object(iqdb.assets, 'pictures', return_value=pics), \
                 mock.patch.object(iqdb, '_ratings', return_value={'p-low': 'explicit', 'x-high': 'sensitive'}), \
                 mock.patch('provenance.sources.groups.unsolved_originals', return_value=[]):
                got = iqdb.order(self.lib, c)
        self.assertEqual([p['asset'] for p in got], ['x-high', 'p-low'])

    def test_manual_selection_does_not_use_priority(self):
        pics = [{'asset': 'low', 'preset': 'pixiv'}, {'asset': 'high', 'preset': 'x'}]
        with db.connect(self.lib) as c, mock.patch.object(iqdb.assets, 'pictures', return_value=pics), \
             mock.patch.object(iqdb, '_ratings', side_effect=AssertionError('manual lookup must not rank')):
            self.assertEqual(iqdb.order(self.lib, c, only=['low', 'high']), pics)


if __name__ == '__main__':
    unittest.main()
