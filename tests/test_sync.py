"""Photos 增量同步：扫描差异、增量导入与全量刷新等价；只使用临时库与假数据。"""
import hashlib
import sqlite3
import tempfile
import unittest
from pathlib import Path
from web.store import Store
from tests.test_web import fixture
from tests.test_web_v2 import assert_memberships_current


def snapshot(store):
    """All library tables with entity ids replaced by stable keys, so two stores can be compared."""
    with store.connect() as c:
        # An identity's stable key across two stores: its links, else its name.
        links = {}
        for i, system, k in c.execute('SELECT entity_id,system,key FROM links ORDER BY system,key'):
            links.setdefault(i, []).append(system + ':' + k)
        key = {i: ' '.join(links.get(i, [])) or n for i, n in c.execute('SELECT id,name FROM entities')}
        rows = lambda sql: sorted(tuple(key.get(v, v) for v in r) for r in c.execute(sql))
        return {
            'assets': rows('SELECT * FROM assets'),
            'candidates': rows('SELECT * FROM candidates'),
            'general_hits': rows('SELECT uuid,value FROM general_hits'),
            'asset_rating': rows('SELECT * FROM asset_rating'),
            'family_evidence': rows('SELECT * FROM family_evidence'),
            'single_assets': rows('SELECT * FROM single_assets'),
            'memberships': rows('SELECT * FROM memberships'),
            'duplicates': rows('SELECT * FROM duplicates'),
            'auto_rules': rows('SELECT * FROM auto_rules'),
            'excluded': rows('SELECT * FROM excluded'),
            'assertions': rows('SELECT * FROM assertions'),
            'imported': rows('SELECT * FROM imported'),
        }


class ImportTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        with sqlite3.connect(self.source) as c:
            c.execute('ALTER TABLE assets ADD COLUMN stable_hash TEXT')
            c.execute("UPDATE assets SET stable_hash='h-' || uuid")

    def tearDown(self):
        self.temp.cleanup()

    def add_images(self):
        """New images as a sync would write them: a copy of a, a solo bob image, a screenshot, an album image."""
        with sqlite3.connect(self.source) as c:
            c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?,?)', [
                ('d', 'd.png', '', 240, 300, '2026-09-23', 'orig', 1, 0, 'h-a'),
                ('e', 'e.png', '', 120, 150, '2026-09-23', 'orig', 1, 0, 'h-e'),
                ('f', 'f.png', '', 120, 150, '2026-09-23', 'orig', 1, 0, 'h-f'),
                ('g', 'g.png', '', 120, 150, '2026-09-23', 'orig', 1, 0, 'h-g')])
            c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [
                ('d', 'character', 'alice_tag', 0.9), ('d', 'general', 'blue_hair', 0.98), ('d', 'rating', 'general', 0.9),
                ('e', 'character', 'bob_tag', 0.8), ('e', 'character', 'alice_tag', 0.5), ('e', 'general', 'solo', 0.9),
                ('e', 'rating', 'explicit', 0.7), ('e', 'rating', 'general', 0.2), ('e', 'char_ext', 'bob', 0.97),
                ('f', 'general', 'chat_log', 0.8), ('f', 'rating', 'general', 0.9),
                ('g', 'character', 'unknown_tag', 0.6), ('g', 'rating', 'sensitive', 0.6)])
            c.execute("INSERT INTO decisions VALUES ('g','character','alice_album','album:Alice album pk=1')")

    def full(self, name):
        store = Store(self.source, self.root / name)
        store.refresh()
        return store

    def test_incremental_import_matches_full_refresh(self):
        store = self.full('inc.sqlite')
        store.change(['b'], 'rejected', store.entities('Bob')[0]['id'])
        self.add_images()
        result = store.import_changes(['d', 'e', 'f', 'g', 'nope'], run_id=1)
        self.assertEqual(result, {'added': 4, 'removed': 0})
        full = self.full('full.sqlite')
        full.change(['b'], 'rejected', full.entities('Bob')[0]['id'])
        self.assertEqual(snapshot(store), snapshot(full))
        assert_memberships_current(self, store)
        # The copy of a folds into its higher-resolution twin d; the chat screenshot is excluded.
        with store.connect() as c:
            self.assertEqual(c.execute('SELECT uuid FROM duplicates WHERE hidden=1').fetchall()[0][0], 'a')
            self.assertEqual(c.execute("SELECT reason FROM excluded WHERE uuid='f'").fetchone()[0], 'screenshot')
        self.assertEqual(sorted(r['uuid'] for r in store.assets(pool='recent')['items']), ['d', 'e', 'g'])

    def test_removed_images_leave_but_human_knowledge_stays(self):
        store = self.full('inc.sqlite')
        alice = store.entities('Alice')[0]['id']
        store.change(['c'], 'confirmed', alice)
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE assets SET trashed=1 WHERE uuid='c'")
        store.import_changes(removed=['c'])
        self.assertEqual(snapshot(store), snapshot(self.full('full.sqlite')) | {'assertions': snapshot(store)['assertions']})
        with store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM assertions WHERE uuid='c'").fetchone()[0], 1)
        self.assertNotIn('c', [r['uuid'] for r in store.assets()['items']])
        assert_memberships_current(self, store)
        # Back in Photos: the earlier confirmation applies again.
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE assets SET trashed=0 WHERE uuid='c'")
        store.import_changes(['c'])
        self.assertIn('c', [r['uuid'] for r in store.assets(entity=alice, status='confirmed')['items']])
        assert_memberships_current(self, store)

    def test_reindex_after_index_update_matches_full_refresh(self):
        store = self.full('inc.sqlite')
        store.change(['c'], 'confirmed', store.entities('Bob')[0]['id'])
        claims = lambda: {r for r in sqlite3.connect(self.source).execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")}
        before = claims()
        # A new character is patched in and the rescan moves claims: b now matches carol, c gains one, outside is gone.
        with sqlite3.connect(self.source) as c:
            c.execute("INSERT INTO characters VALUES ('carol','Carol',NULL,NULL,'Example',NULL)")
            c.execute("INSERT INTO character_aliases VALUES ('carol','卡萝')")
            c.execute("DELETE FROM labels WHERE dim='char_ext'")
            c.executemany("INSERT INTO labels VALUES (?,'char_ext',?,?)", [('b', 'carol', 0.97), ('c', 'carol', 0.96)])
        changed = {u for u, *_ in before ^ claims()}
        self.assertEqual(store.reindex_characters(changed), 2)       # `outside` is not in the library
        full = self.full('full.sqlite')
        full.change(['c'], 'confirmed', full.entities('Bob')[0]['id'])
        self.assertEqual(snapshot(store), snapshot(full))
        def entities(s):
            with s.connect() as c:
                return sorted(tuple(r) for r in c.execute('SELECT l.system,l.key,e.name,e.work,e.variant FROM links l JOIN entities e ON e.id=l.entity_id'))
        self.assertEqual(entities(store), entities(full))
        self.assertIn('卡萝', [m for m in store.entities('卡萝')[0]['matched'].split(' · ')])
        assert_memberships_current(self, store)


if __name__ == '__main__':
    unittest.main()


class SyncTest(unittest.TestCase):
    """Scan a fake Photos library, recognise with a stand-in, import incrementally."""
    X, PIXIV, SCREEN = 'com.atebits.Tweetie2', 'net.pxv.iphone', 'com.apple.springboard'

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        with sqlite3.connect(self.source) as c:
            for column in ('z_pk INTEGER', 'hidden INTEGER', 'stable_hash TEXT', 'orig_size INTEGER'):
                c.execute(f'ALTER TABLE assets ADD COLUMN {column}')
            for table in ('assets', 'labels', 'decisions'):
                c.execute(f'UPDATE {table} SET uuid=upper(uuid)')
            # Already recognised images carry a rating, the marker the sync uses.
            c.execute("INSERT INTO labels SELECT uuid,'rating','general',0.9 FROM assets")
        self.library = self.root / 'Photos Library.photoslibrary'
        (self.library / 'database').mkdir(parents=True)
        with sqlite3.connect(self.library / 'database' / 'Photos.sqlite') as p:
            p.execute('CREATE TABLE ZASSET(Z_PK INTEGER PRIMARY KEY,ZUUID TEXT,ZKIND INTEGER,ZTRASHEDSTATE INTEGER,ZWIDTH INTEGER,'
                      'ZHEIGHT INTEGER,ZADDEDDATE REAL,ZHIDDEN INTEGER,ZDIRECTORY TEXT,ZFILENAME TEXT,ZADDITIONALATTRIBUTES INTEGER)')
            p.execute('CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK INTEGER PRIMARY KEY,ZIMPORTEDBYBUNDLEIDENTIFIER TEXT,'
                      'ZORIGINALSTABLEHASH TEXT,ZORIGINALFILESIZE INTEGER)')
        for uuid, app, trashed in (('A', self.X, 0), ('B', self.PIXIV, 0), ('C', self.X, 1), ('D', self.X, 0),
                                   ('E', self.PIXIV, 0), ('F', self.SCREEN, 0)):
            self.photo(uuid, app, trashed)
        self.store = Store(self.source, self.root / 'app.sqlite')
        self.store.refresh()
        self.unreadable = {'E'}

    def tearDown(self):
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def photo(self, uuid, app, trashed=0):
        with sqlite3.connect(self.library / 'database' / 'Photos.sqlite') as p:
            pk = p.execute('INSERT INTO ZADDITIONALASSETATTRIBUTES(ZIMPORTEDBYBUNDLEIDENTIFIER,ZORIGINALSTABLEHASH,ZORIGINALFILESIZE) '
                           'VALUES (?,?,?)', (app, 'h-' + uuid, 100)).lastrowid
            p.execute('INSERT INTO ZASSET(ZUUID,ZKIND,ZTRASHEDSTATE,ZWIDTH,ZHEIGHT,ZADDEDDATE,ZHIDDEN,ZDIRECTORY,ZFILENAME,'
                      'ZADDITIONALATTRIBUTES) VALUES (?,0,?,120,150,780000000,0,?,?,?)', (uuid, trashed, uuid[0], uuid + '.png', pk))

    def recognise(self, pi_db, uuids):
        """Stand-in for web.infer: labels the readable images, reports the others as failed."""
        ok = [u for u in uuids if u not in self.unreadable]
        with sqlite3.connect(pi_db) as db:
            db.executemany("INSERT INTO labels VALUES (?,'rating','general',0.9)", [(u,) for u in ok])
            db.executemany("INSERT INTO labels VALUES (?,'character','bob_tag',0.8)", [(u,) for u in ok])
        yield {'ready': True, 'total': len(uuids)}
        yield {'batch': ok, 'failed': [{'uuid': u, 'error': 'OSError: not downloaded'} for u in uuids if u in self.unreadable],
               'done': len(uuids), 'total': len(uuids)}

    def sync(self, **kw):
        from web.sync import Syncer
        syncer = Syncer(self.store, self.library, recogniser=self.recognise, **kw)
        syncer.run()
        return syncer.status()

    def test_sync_collects_recognises_and_imports(self):
        state = self.sync()
        self.assertEqual((state['status'], state['found'], state['removed'], state['imported'], state['failed']), ('done', 2, 1, 1, 1))
        with sqlite3.connect(self.source) as c:
            rows = dict(c.execute('SELECT uuid,trashed FROM assets'))
            kind, path = c.execute("SELECT src_kind,src_path FROM assets WHERE uuid='D'").fetchone()
        self.assertEqual(rows['C'], 1)                    # trashed in Photos: kept, marked
        self.assertNotIn('F', rows)                       # screenshots are not collected yet
        self.assertEqual((kind, Path(path).name), ('deriv', 'D_1_105_c.jpeg'))
        self.assertEqual(sorted(self.store.library_uuids()), ['A', 'B', 'D'])
        self.assertEqual([r['uuid'] for r in self.store.assets(pool='recent')['items']], ['D'])
        bob = self.store.entities('Bob')[0]['id']
        self.assertIn('D', [r['uuid'] for r in self.store.assets(entity=bob)['items']])
        run = self.store.sync_history()[0]
        self.assertEqual((run['status'], run['found'], run['imported'], run['failed']), ('done', 2, 1, 1))
        self.assertEqual(run['sources'][self.SCREEN], {'name': '截屏', 'photos': 1, 'collected': 0})
        self.assertIn('iCloud', run['message'])
        # E becomes readable: the next sync picks it up; nothing else changes.
        self.unreadable = set()
        state = self.sync()
        self.assertEqual((state['found'], state['imported'], state['failed']), (0, 1, 0))
        self.assertEqual(sorted(self.store.library_uuids()), ['A', 'B', 'D', 'E'])
        self.assertEqual([r['uuid'] for r in self.store.assets(pool='recent')['items']], ['E'])
        # A trashed image that comes back returns with its earlier knowledge.
        with sqlite3.connect(self.library / 'database' / 'Photos.sqlite') as p:
            p.execute("UPDATE ZASSET SET ZTRASHEDSTATE=0 WHERE ZUUID='C'")
        self.assertEqual(self.sync()['found'], 1)
        self.assertIn('C', self.store.library_uuids())

    def test_large_backlog_is_left_for_windows(self):
        state = self.sync(local_limit=1)
        self.assertEqual(state['status'], 'needs_windows')
        self.assertEqual(self.store.sync_history()[0]['status'], 'needs_windows')
        self.assertNotIn('D', self.store.library_uuids())

    def test_retry_resolves_original_downloaded_after_first_scan(self):
        from web.sync import scan, backlog
        scan(self.library, self.source)
        original = self.library / 'originals' / 'E' / 'E.png'
        original.parent.mkdir(parents=True)
        from PIL import Image
        Image.new('RGB', (20, 20)).save(original)
        scan(self.library, self.source)
        with sqlite3.connect(self.source) as c:
            kind, path = c.execute("SELECT src_kind,src_path FROM assets WHERE uuid='E'").fetchone()
        self.assertEqual((kind, path), ('orig', str(original)))
        self.assertIn('E', backlog(self.source, self.store.library_uuids())[0])
        self.unreadable.clear()
        self.sync()
        self.assertEqual(self.store.image_path('E'), original)

    def test_interrupted_run_is_marked(self):
        first = self.store.sync_begin()
        self.store.sync_begin()
        self.assertEqual(next(r for r in self.store.sync_history() if r['id'] == first)['status'], 'interrupted')
