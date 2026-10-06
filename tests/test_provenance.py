"""溯源模块：像素哈希算法、原帖解析、查询流程（假的网络）、画师链接。只用临时目录，不联网。"""
import hashlib
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np
from PIL import Image

from web import folder, library
from provenance import db
from provenance.net import Stopped
from provenance.sources import pixel_hash
from tests.test_folder import FolderSourceTest, image


class PixelHashTest(unittest.TestCase):
    def test_matches_danbooru_layout(self):
        pixels = np.arange(4 * 3 * 3, dtype=np.uint8).reshape(3, 4, 3)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'a.png'
            Image.fromarray(pixels, 'RGB').save(path)
            rgba = np.concatenate([pixels, np.full((3, 4, 1), 255, np.uint8)], axis=2).tobytes()
            expected = hashlib.md5(b'P7\nWIDTH 4\nHEIGHT 3\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n' + rgba).hexdigest()
            self.assertEqual(pixel_hash.pixel_hash(path), expected)
            Image.fromarray(pixels, 'RGB').save(Path(tmp) / 'b.png', pnginfo=None, optimize=True)
            self.assertEqual(pixel_hash.pixel_hash(Path(tmp) / 'b.png'), expected)   # different bytes, same pixels

    def test_links_and_upstream_post(self):
        post = {'id': 7, 'tag_string_artist': 'foo_(bar) banned_artist', 'source': 'https://x.com/Foo_Bar/status/123'}
        self.assertEqual([l[:2] for l in pixel_hash.links_of(post)], [('danbooru', 'foo_(bar)'), ('x', 'foo_bar')])
        self.assertEqual(pixel_hash.source_post(post), ('x', '123', 'https://x.com/Foo_Bar/status/123'))
        self.assertEqual(pixel_hash.source_post({'source': 'https://i.pximg.net/img-original/img/2024/01/01/00/00/00/111_p0.png'})[:2], ('pixiv', '111'))
        self.assertIsNone(pixel_hash.source_post({'source': 'https://example.com/a.png'}))


class FakeNet:
    def __init__(self, answers):
        self.answers, self.used, self.calls = answers, 0, []

    def get(self, url, params=None, **_):
        hashes = [v for k, v in params if k == 'search[pixel_hash][]']
        self.calls.append(hashes)
        out = []
        for h in hashes:
            answer = self.answers(h)
            if isinstance(answer, Exception):
                raise answer
            out += [dict(a, pixel_hash=h) for a in answer]
        return out


class PixelHashRunTest(FolderSourceTest):
    # Reuses the folder fixture; the folder tests themselves run in test_folder.
    test_new_library_from_a_folder = test_scan_moves_deletes_and_changes = test_guess_preset = None
    test_library_follows_a_moved_file = test_user_preset_rewrites_the_images = None

    def test_run_records_hits_and_skips_what_was_looked_up(self):
        library.set_preset(self.lib, 's1', 'x')
        folder.scan(self.source, self.lib.source)
        target = pixel_hash.pixel_hash(self.pics / 'a.png')
        post = {'id': 42, 'tag_string_artist': 'artist_a', 'source': 'https://www.pixiv.net/artworks/555'}
        net = FakeNet(lambda h: [{'id': 1, 'post': post}] if h == target else [])
        self.assertEqual({k: v for k, v in pixel_hash.run(self.lib, net, log=lambda m: None, workers=1).items() if k in ('looked', 'hits', 'misses')},
                         {'looked': 2, 'hits': 1, 'misses': 1})
        with db.connect(self.lib) as c:
            self.assertEqual({(r['site'], r['post_id']) for r in c.execute('SELECT site,post_id FROM image_sources')},
                             {('danbooru', '42'), ('pixiv', '555')})
            artist = c.execute("SELECT artist_id FROM artist_links WHERE system='danbooru' AND key='artist_a'").fetchone()[0]
            self.assertEqual([tuple(r) for r in c.execute('SELECT artist_id,status FROM image_artists')], [(artist, 'auto')])
            self.assertEqual([tuple(r) for r in c.execute('SELECT source FROM seeds')], [('pixel_hash',)])
        again = FakeNet(lambda h: [])
        pixel_hash.run(self.lib, again, log=lambda m: None, workers=1)
        self.assertEqual(again.calls, [])                                   # both already looked up
        self.assertEqual(len(net.calls), 1)                                 # one request carried both hashes

    def test_refusal_stops_without_recording_misses(self):
        library.set_preset(self.lib, 's1', 'x')
        folder.scan(self.source, self.lib.source)
        result = pixel_hash.run(self.lib, FakeNet(lambda h: Stopped('danbooru.donmai.us: HTTP 429')), log=lambda m: None, workers=1)
        self.assertIn('429', result['stopped'])
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM lookups').fetchone()[0], 0)

    def test_other_presets_are_not_looked_up(self):
        folder.scan(self.source, self.lib.source)                          # the fixture folder is 'pixiv'
        library.set_preset(self.lib, 's1', 'other')
        net = FakeNet(lambda h: [])
        pixel_hash.run(self.lib, net, log=lambda m: None, workers=1)
        self.assertEqual(net.calls, [])


class VerifyTest(FolderSourceTest):
    test_new_library_from_a_folder = test_scan_moves_deletes_and_changes = test_guess_preset = None
    test_library_follows_a_moved_file = test_user_preset_rewrites_the_images = None

    def setUp(self):
        super().setUp()
        from provenance import verify   # these tests are about matching, not which artists qualify (see test_only_artists_worth_it)
        patch = mock.patch.object(verify, 'MIN_PICTURES', 0)
        patch.start()
        self.addCleanup(patch.stop)

    def test_only_artists_worth_it(self):
        """Automatically only artists with pictures here, open save runs, or many Danbooru posts (2026-09-29)."""
        from provenance import verify
        with db.connect(self.lib) as c:
            ids = {n: db.artist_for(c, [('danbooru', n, '')], 'test') for n in ('many', 'popular', 'cold')}
            for n in ids:
                c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'pixel_hash')", (ids[n],))
            for i in range(3):
                c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,'pixel_hash',1,'{}','auto')", (f'a{i}', ids['many']))
            asked = []
            class Net:
                def get(self, url, params=None, **_):
                    asked.append(params['search[name_comma]'])
                    return [{'name': 'popular', 'post_count': 2500}, {'name': 'cold', 'post_count': 12}]
            with mock.patch.object(verify, 'MIN_PICTURES', 3):
                keep, cold = verify.chosen(c, self.lib, Net(), list(ids.values()), log=lambda m: None)
                self.assertEqual((keep, cold), ([ids['many'], ids['popular']], 1))
                verify.chosen(c, self.lib, Net(), list(ids.values()), log=lambda m: None)
            self.assertEqual(len(asked), 1)                     # post counts kept for 30 days
            self.assertNotIn('many', asked[0])                  # no need to ask about an artist already worth it

    def test_public_works_matched_picture_by_picture(self):
        import io
        from web.store import Store
        from provenance import verify
        folder.scan(self.source, self.lib.source)
        Store(self.lib.source, self.lib.db).refresh()          # no image_sigs yet: verification computes them
        image(self.pics / 'a.png', 'red')
        pixels = np.zeros((60, 80, 3), np.uint8); pixels[:, :40] = (200, 30, 30); pixels[:30, 40:] = (20, 200, 90)
        Image.fromarray(pixels).save(self.pics / 'a.png')      # a picture with some structure
        folder.scan(self.source, self.lib.source)
        def thumb(img, size):
            b = io.BytesIO(); img.resize(size).save(b, 'JPEG'); return b.getvalue()
        same = thumb(Image.fromarray(pixels), (180, 135))
        other = thumb(Image.fromarray(pixels[:, ::-1].copy()), (180, 135))
        class Net:
            used, headers = 0, {}
            def get(self, url, params=None, raw=False, extra=None):
                if 'posts.json' in url:
                    return [{'id': 1, 'image_width': 800, 'image_height': 600, 'preview_file_url': 'https://cdn/1.jpg'},
                            {'id': 2, 'image_width': 800, 'image_height': 600, 'preview_file_url': 'https://cdn/2.jpg'}]
                if 'artists.json' in url:
                    return [{'name': 'artist_a', 'urls': [{'url': 'https://www.pixiv.net/users/77'}]}]
                if url == 'https://cdn/1.jpg':
                    return same
                if url == 'https://cdn/2.jpg':
                    return other
                if 'profile/all' in url:
                    return {'body': {'illusts': {}}}
                raise AssertionError(url)
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
        result = verify.run(self.lib, Net(), log=lambda m: None)
        self.assertEqual((result['artists'], result['matched_artists']), (1, 1))
        a = dict(self.rows("SELECT key,asset FROM asset_links WHERE system='file'"))['a.png']
        with db.connect(self.lib) as c:
            self.assertEqual([tuple(r) for r in c.execute('SELECT asset,site,post_id FROM image_sources')], [(a, 'danbooru', '1')])
            self.assertEqual(c.execute("SELECT key FROM artist_links WHERE system='pixiv'").fetchone()[0], '77')
        self.assertEqual(verify.run(self.lib, Net(), log=lambda m: None)['artists'], 0)   # verified once

    def _drawn_library(self):
        """A library with one structured picture; returns (asset, a thumbnail of it, a thumbnail of something else)."""
        import io
        from web.store import Store
        folder.scan(self.source, self.lib.source)
        Store(self.lib.source, self.lib.db).refresh()
        pixels = np.zeros((60, 80, 3), np.uint8); pixels[:, :40] = (200, 30, 30); pixels[:30, 40:] = (20, 200, 90)
        Image.fromarray(pixels).save(self.pics / 'a.png')
        folder.scan(self.source, self.lib.source)
        def thumb(img):
            b = io.BytesIO(); img.resize((180, 135)).save(b, 'JPEG'); return b.getvalue()
        asset = dict(self.rows("SELECT key,asset FROM asset_links WHERE system='file'"))['a.png']
        return asset, thumb(Image.fromarray(pixels)), thumb(Image.fromarray(pixels[:, ::-1].copy()))

    def _site(self, same, other, posts, works=None):
        """Danbooru `posts` (id, source) with previews at https://cdn/<id>.jpg (post 1 is the drawing), and a Pixiv
        account whose `works` have 150 px thumbnails (work 7 is the drawing). Records every thumbnail asked for."""
        asked = []
        class Net:
            used, headers = 0, {}
            def get(self, url, params=None, raw=False, extra=None):
                if 'posts.json' in url:
                    return [{'id': i, 'image_width': 800, 'image_height': 600, 'preview_file_url': f'https://cdn/{i}.jpg', 'source': src}
                            for i, src in posts]
                if 'artists.json' in url:
                    return [{'name': 'artist_a', 'urls': [{'url': 'https://www.pixiv.net/users/77'}]}] if works is not None else []
                if 'profile/all' in url:
                    return {'body': {'illusts': {w: None for w in works or []}}}
                if 'profile/illusts' in url:
                    ids = [v for k, v in params if k == 'ids[]']
                    return {'body': {'works': {w: {'id': w, 'pageCount': 1, 'url': 'https://i.pximg.net/c/250x250/img-master/img/2026/01/02/03/04/05/x.jpg'} for w in ids}}}
                asked.append(url)
                if url in ('https://cdn/1.jpg',) or '/7_p0_' in url:
                    return same
                return other
        return Net(), asked

    def test_kept_signatures_mean_no_second_download(self):
        from provenance import verify
        asset, same, other = self._drawn_library()
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
        net, asked = self._site(same, other, [(1, ''), (2, '')], works=['7', '8'])
        self.assertEqual(verify.run(self.lib, net, log=lambda m: None)['matches'], 2)   # Danbooru post 1 and Pixiv work 7
        self.assertEqual(len(asked), 4)
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM ref_sigs WHERE colour IS NOT NULL').fetchone()[0], 4)
            c.execute("DELETE FROM image_artists"); c.execute("DELETE FROM image_sources"); c.execute("DELETE FROM lookups")
        asked.clear()                                        # e.g. new pictures synced: every artist is compared again
        self.assertEqual(verify.run(self.lib, net, log=lambda m: None)['matches'], 2)
        self.assertEqual(asked, [])                          # from the kept signatures alone
        with db.connect(self.lib) as c:
            self.assertEqual({tuple(r) for r in c.execute('SELECT site,post_id FROM image_sources')}, {('danbooru', '1'), ('pixiv', '7')})

    def test_known_and_duplicate_posts_are_not_downloaded(self):
        from provenance import verify
        asset, same, other = self._drawn_library()
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
            c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?, 'danbooru', '3', '', ?, 'pixel_hash')", (asset, artist))
        posts = [(1, ''), (3, ''), (4, 'https://i.pximg.net/img-original/img/2026/01/02/03/04/05/8_p0.png'),
                 (5, 'https://www.pixiv.net/artworks/99'), (6, 'https://i.pximg.net/img-original/img/x/8_p4.png')]
        net, asked = self._site(same, other, posts, works=['7', '8'])
        verify.run(self.lib, net, log=lambda m: None)
        cdn = sorted(u for u in asked if u.startswith('https://cdn/'))
        # 3: already tied by the pixel hash; 4: Pixiv work 8, looked at on Pixiv; 5: a work outside the account's list;
        # 6: page 5 of work 8, beyond the pages looked at on Pixiv
        self.assertEqual(cdn, ['https://cdn/1.jpg', 'https://cdn/5.jpg', 'https://cdn/6.jpg'])

    def test_a_stop_mid_artist_keeps_what_was_paid_for(self):
        from provenance import verify
        from provenance.net import BudgetExceeded
        asset, same, other = self._drawn_library()
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
        net, asked = self._site(same, other, [(1, ''), (2, '')])
        real = net.get
        def budget(url, *a, **k):
            if url == 'https://cdn/2.jpg':
                raise BudgetExceeded('已用 300 MB')
            return real(url, *a, **k)
        net.get = budget
        self.assertIn('300 MB', verify.run(self.lib, net, log=lambda m: None)['stopped'])
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM ref_sigs").fetchone()[0], 1)            # post 1 kept
            self.assertEqual(c.execute("SELECT asset FROM image_artists").fetchone()[0], asset)       # and its match
            self.assertIsNone(c.execute("SELECT 1 FROM lookups WHERE backend='verify'").fetchone())   # the artist is not done

    def test_v4_store_gains_the_colour_signature(self):
        path = db.path_of(self.lib)
        path.unlink(missing_ok=True)
        old = sqlite3.connect(path)
        old.executescript("""CREATE TABLE ref_sigs(site TEXT NOT NULL, post_id TEXT NOT NULL, page INTEGER NOT NULL DEFAULT 0,
            artist_id TEXT, ratio REAL NOT NULL, grid BLOB NOT NULL, url TEXT, PRIMARY KEY(site, post_id, page));
            INSERT INTO ref_sigs(site,post_id,ratio,grid) VALUES ('danbooru','1',1.0,x'00'); PRAGMA user_version=4;""")
        old.close()
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute('PRAGMA user_version').fetchone()[0], db.VERSION)
            self.assertEqual(tuple(c.execute('SELECT post_id, colour FROM ref_sigs').fetchone()), ('1', None))   # kept; re-signed on need

    def test_strongest_leads_are_verified_first(self):
        from provenance import verify
        with db.connect(self.lib) as c:
            ids = {name: db.artist_for(c, [('danbooru', name, '')], 'test') for name in ('related', 'ocr1', 'ocr2', 'hash')}
            for name, source in (('related', 'pixiv_related'), ('ocr1', 'ocr'), ('ocr2', 'ocr'), ('hash', 'pixel_hash'), ('ocr1', 'pixiv_related')):
                c.execute('INSERT INTO seeds(artist_id,source) VALUES (?,?)', (ids[name], source))
            for asset, name in (('a', 'ocr2'), ('b', 'ocr2'), ('c', 'ocr1')):
                c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,'ocr',1,'{}','candidate')", (asset, ids[name]))
            order = [r[0] for r in c.execute(verify.PRIORITY)]
        self.assertEqual(order, [ids['hash'], ids['ocr2'], ids['ocr1'], ids['related']])

    def test_progress_inside_one_artist(self):
        """One artist can mean 1,000+ thumbnails (15+ minutes): progress must come from inside it (the runner's idle timeout)."""
        from provenance import verify
        from web.store import Store
        folder.scan(self.source, self.lib.source)
        Store(self.lib.source, self.lib.db).refresh()
        class Net:
            used, headers = 0, {}
            def get(self, url, params=None, raw=False, extra=None):
                if 'posts.json' in url:
                    return [{'id': i, 'image_width': 8, 'image_height': 6, 'preview_file_url': f'https://cdn/{i}.jpg'} for i in range(250)]
                return None if raw else []
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'busy', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
        logs, numbers = [], []
        def log(text):
            logs.append(text)
        log.progress = lambda **n: numbers.append(n)
        with mock.patch.object(verify.Progress, 'EVERY', 0):   # every work, not every 5 s
            verify.run(self.lib, Net(), log=log)
        works = [n for n in numbers if n['sub_total']]
        self.assertEqual([n['sub_done'] for n in works][-1], 250)            # the second bar moves work by work
        self.assertEqual({(n['label'], n['total'], n['sub_total']) for n in works}, {('画师', 1, 250)})
        self.assertEqual(numbers[-1]['done'], 1)                            # the first bar: the artist finished
        self.assertTrue(any('作品 250/250' in l and '已下载小图 250 张' in l for l in logs))

    def test_new_pictures_meet_kept_signatures_offline(self):
        """After a sync: new pictures against the signatures already kept, no request; each picture only once."""
        from provenance import verify
        asset, same, other = self._drawn_library()
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            for post, thumb in (('1', same), ('2', other)):
                c.execute('INSERT INTO ref_sigs(site,post_id,page,artist_id,ratio,grid,colour,url) VALUES (?,?,0,?,?,?,?,?)',
                          ('danbooru', post, artist, *verify._sig(thumb, 800 / 600), ''))
        result = verify.local_run(self.lib, log=lambda m: None)
        self.assertEqual((result['pictures'] > 0, result['matches']), (True, 1))
        with db.connect(self.lib) as c:
            self.assertEqual(tuple(c.execute('SELECT asset,site,post_id,url FROM image_sources').fetchone()),
                             (asset, 'danbooru', '1', 'https://danbooru.donmai.us/posts/1'))
            self.assertEqual(c.execute('SELECT artist_id FROM image_artists').fetchone()[0], artist)
        self.assertEqual(verify.local_run(self.lib, log=lambda m: None)['pictures'], 0)   # compared once
        with db.connect(self.lib) as c:
            c.execute("DELETE FROM lookups WHERE backend='ref_compare'")
        self.assertEqual(verify.local_run(self.lib, log=lambda m: None)['matches'], 0)    # already recorded: not counted again

    def test_local_match_needs_no_authorisation(self):
        from provenance import run
        self.assertIn('local_match', run.LOCAL_STEPS)
        from web.provenance_job import KINDS
        self.assertTrue(all(KINDS[k][0].split(',').index('local_match') < KINDS[k][0].split(',').index('pixel_hash') for k in ('full', 'new')))

    def test_old_works_are_found_around_a_known_one(self):
        """Saves come in runs down an artist's page: a work known to be in the library brings its neighbours in, even
        far below the newest PIXIV_WORKS (2026-09-29)."""
        from provenance import verify
        asset, same, other = self._drawn_library()
        works = [str(i) for i in range(1, 501)]                # work 7 (the drawing) is the 494th newest
        for anchored in (False, True):
            with db.connect(self.lib) as c:
                for table in ('image_sources', 'image_artists', 'lookups', 'ref_sigs', 'seeds', 'artist_links', 'artists'):
                    c.execute(f'DELETE FROM {table}')
                artist = db.artist_for(c, [('pixiv', '77', '')], 'test')
                c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
                if anchored:                                   # e.g. found by IQDB: work 10, three works newer
                    c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES ('other', 'pixiv', '10', '', ?, 'iqdb')", (artist,))
            net, asked = self._site(same, other, [], works=works)
            result = verify.run(self.lib, net, log=lambda m: None)
            self.assertEqual(result['matches'], 1 if anchored else 0)
            self.assertLessEqual(len(asked), verify.PIXIV_WORKS + verify.NEIGHBOUR_EXTRA)

    def test_weak_leads_are_tried_on_their_newest_works_first(self):
        """An OCR-only artist with nothing known in the library: 30 posts first, the rest only after a match (2026-09-29)."""
        from provenance import verify
        asset, same, other = self._drawn_library()
        for drawing_at, expect_asked, expect_cursor in ((70, verify.PROBE, 'probe'), (5, 100, None)):
            with db.connect(self.lib) as c:
                for table in ('image_sources', 'image_artists', 'lookups', 'ref_sigs', 'seeds', 'artist_links', 'artists'):
                    c.execute(f'DELETE FROM {table}')
                artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
                c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'ocr')", (artist,))
            posts = [(1 if n == drawing_at else 1000 + n, '') for n in range(100)]
            net, asked = self._site(same, other, posts)
            verify.run(self.lib, net, log=lambda m: None)
            self.assertEqual(len(asked), expect_asked)
            with db.connect(self.lib) as c:
                self.assertEqual(c.execute("SELECT cursor FROM lookups WHERE backend='verify'").fetchone()[0], expect_cursor)

    def test_known_works_still_move_the_bar(self):
        """An artist whose works are all known downloads nothing; the works bar must still move (2026-09-28 run 14)."""
        from provenance import verify
        asset, same, other = self._drawn_library()
        with db.connect(self.lib) as c:
            artist = db.artist_for(c, [('danbooru', 'artist_a', '')], 'test')
            c.execute("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", (artist,))
            for i in (1, 2):
                c.execute("INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?, 'danbooru', ?, '', ?, 'pixel_hash')", (asset, str(i), artist))
        net, asked = self._site(same, other, [(1, ''), (2, '')])
        numbers = []
        log = lambda text: None
        log.progress = lambda **n: numbers.append(n)
        with mock.patch.object(verify.Progress, 'EVERY', 0):
            verify.run(self.lib, net, log=log)
        self.assertEqual(asked, [])
        self.assertIn((2, 2), {(n['sub_done'], n['sub_total']) for n in numbers})

    def test_a_pixiv_refusal_keeps_danbooru_going_and_retries_later(self):
        from provenance import verify
        from web.store import Store
        folder.scan(self.source, self.lib.source)
        Store(self.lib.source, self.lib.db).refresh()
        class Net:
            used, headers = 0, {}
            def get(self, url, params=None, raw=False, extra=None):
                if 'pixiv' in url:
                    raise Stopped('www.pixiv.net: HTTP 403')
                if 'posts.json' in url and 'tags=a' in url + str(params):   # Danbooru finds something for `both`
                    return [{'id': 9, 'image_width': 8, 'image_height': 6, 'preview_file_url': 'https://cdn/9.jpg'}]
                return None if raw else []
        with db.connect(self.lib) as c:
            both = db.artist_for(c, [('danbooru', 'a', ''), ('pixiv', '1', '')], 'test')
            only = db.artist_for(c, [('danbooru', 'b', '')], 'test')
            c.executemany("INSERT INTO seeds(artist_id,source) VALUES (?, 'test')", [(both,), (only,)])
        result = verify.run(self.lib, Net(), log=lambda m: None)
        self.assertEqual(result['artists'], 2)                               # did not stop at the first refusal
        self.assertIn('403', result['stopped'])
        with db.connect(self.lib) as c:
            self.assertEqual({r[0] for r in c.execute("SELECT target FROM lookups WHERE backend='verify'")}, {'artist:' + only})
        # even when Danbooru matched pictures, an artist whose Pixiv was refused is not done (2026-09-28 run 11)
        with mock.patch.object(verify, 'danbooru', return_value=3), \
             mock.patch.object(verify, 'pixiv', side_effect=Stopped('www.pixiv.net: 登录已过期')):
            result = verify.run(self.lib, Net(), log=lambda m: None, artists=[both])
        self.assertEqual(result['matches'], 3)
        with db.connect(self.lib) as c:
            self.assertIsNone(c.execute("SELECT 1 FROM lookups WHERE backend='verify' AND target=?", ('artist:' + both,)).fetchone())


class AccountSourceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.lib = library.create(self.root / 'A.pilib')

    def tearDown(self):
        self.temp.cleanup()

    def archive(self):
        data = self.root / 'twitter-2026' / 'data'
        data.mkdir(parents=True)
        (data / 'following.js').write_text('window.YTD.following.part0 = ' + json.dumps(
            [{'following': {'accountId': '111', 'userLink': 'https://twitter.com/intent/user?user_id=111'}},
             {'following': {'accountId': '222', 'userLink': 'https://twitter.com/intent/user?user_id=222'}}]))
        (data / 'direct-messages.js').write_text('never read')
        return data.parent

    def test_x_archive_import_and_resolve(self):
        from provenance.sources import x_archive
        folder_ = self.archive()
        import zipfile
        zipped = self.root / 'archive.zip'
        with zipfile.ZipFile(zipped, 'w') as z:
            z.write(folder_ / 'data' / 'following.js', 'twitter-2026/data/following.js')
        self.assertEqual(x_archive.following_ids(zipped), ['111', '222'])
        self.assertEqual(x_archive.import_archive(self.lib, folder_), 2)
        self.assertEqual(x_archive.import_archive(self.lib, zipped), 0)        # already known
        asked = []
        class Net:
            used, headers = 0, {}
            def get(self, url, params=None, **_):
                asked.append(params['search[url_matches]'])
                return [{'id': 5, 'name': 'artist_one', 'is_banned': False}] if params['search[url_matches]'].endswith('=111') else []
        result = x_archive.run(self.lib, Net(), log=lambda m: None)
        self.assertEqual((result['resolved'], result['unresolved']), (1, 1))
        with db.connect(self.lib) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM seeds WHERE source='x_following'").fetchone()[0], 1)
            artist = c.execute("SELECT artist_id FROM artist_links WHERE system='x_id' AND key='111'").fetchone()[0]
            self.assertEqual(c.execute("SELECT artist_id FROM artist_links WHERE system='danbooru' AND key='artist_one'").fetchone()[0], artist)
            self.assertIsNotNone(c.execute("SELECT next_due_at FROM leads WHERE key='222'").fetchone()[0])
        asked.clear()
        x_archive.run(self.lib, Net(), log=lambda m: None)
        self.assertEqual(asked, [])                                            # resolved done; unresolved waits 30 days

    def test_pixiv_related_expands_verified_artists_only(self):
        from provenance.sources import pixiv_related
        with db.connect(self.lib) as c:
            verified = db.artist_for(c, [('pixiv', '10', '')], 'test')
            unverified = db.artist_for(c, [('pixiv', '20', '')], 'test')
            c.execute("INSERT INTO lookups(target,backend,last_result) VALUES (?, 'verify', 'hit')", ('artist:' + verified,))
        class Net:
            used = 0
            headers = {'www.pixiv.net': {'Cookie': 'PHPSESSID=1_x'}}
            def get(self, url, params=None, **_):
                assert '/user/10/' in url
                return {'body': {'recommendUsers': [{'userId': '10'}, {'userId': '30'}, {'userId': '20'}]}}
        self.assertEqual(pixiv_related.run(self.lib, Net(), log=lambda m: None)['candidates'], 2)
        with db.connect(self.lib) as c:
            self.assertEqual(sorted(r[0] for r in c.execute("SELECT l.key FROM seeds s JOIN artist_links l ON l.artist_id=s.artist_id WHERE s.source='pixiv_related'")),
                             ['20', '30'])
        self.assertEqual(pixiv_related.run(self.lib, Net(), log=lambda m: None)['expanded'], 0)   # one hop per artist
        class NoSession(Net):
            headers = {}
        self.assertIn('Pixiv', pixiv_related.run(self.lib, NoSession(), log=lambda m: None)['stopped'])
        class AppWindow(NoSession):      # login in the desktop app's Pixiv window: no Cookie header, routed via the window
            via = {'www.pixiv.net': object()}
        from provenance import accounts
        with mock.patch.object(accounts, '_meta', return_value={'pixiv': {'user_id': '1', 'via': 'app'}}):
            self.assertEqual(pixiv_related.run(self.lib, AppWindow(), log=lambda m: None)['stopped'], '')


class ArtistLinkTest(unittest.TestCase):
    def test_links_join_one_artist(self):
        with tempfile.TemporaryDirectory() as tmp:
            lib = library.create(Path(tmp).resolve() / 'A.pilib')
            with db.connect(lib) as c:
                a = db.artist_for(c, [('danbooru', 'foo', ''), ('x', 'foo', '')], 'test')
                b = db.artist_for(c, [('x', 'foo', ''), ('pixiv', '9', '')], 'test')
                self.assertEqual(a, b)
                self.assertEqual(c.execute('SELECT count(*) FROM artist_links WHERE artist_id=?', (a,)).fetchone()[0], 3)


del FolderSourceTest   # imported only as a base class; its own tests run in test_folder

if __name__ == '__main__':
    unittest.main()
