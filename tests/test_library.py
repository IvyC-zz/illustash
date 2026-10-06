"""图库包：建立、打开、格式检查、最近打开，以及把旧的 review/ 布局迁成图库包。只用临时目录。"""
import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import numpy as np

from web import library
from web.features import Features
from web.store import Store
from tests.test_web import fixture


class LibraryPackageTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        patcher = mock.patch.object(library, 'APP_SUPPORT', self.root / 'support')
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        self.temp.cleanup()

    def test_create_open_and_refuse_newer_format(self):
        lib = library.create(self.root / 'A.pilib', 'A', [library.photos_source('/tmp/Photos.photoslibrary')])
        self.assertTrue(library.is_library(lib.path))
        for sub in ('features', 'thumbnails', 'backups'):
            self.assertTrue((lib.path / sub).is_dir())
        again = library.open_library(lib.path)
        self.assertEqual(again.config['id'], lib.config['id'])
        self.assertFalse(again.frozen)
        self.assertTrue(library.create(self.root / 'T.pilib', test={'persona': '连续型'}).frozen)
        self.assertEqual(again.photos(), Path('/tmp/Photos.photoslibrary'))
        with self.assertRaises(library.LibraryError):
            library.create(lib.path)                      # never overwrite
        with self.assertRaises(library.LibraryError):
            library.create(self.root / 'B.folder')        # wrong extension
        lib.config['format'] = library.FORMAT + 1
        lib.save()
        with self.assertRaises(library.LibraryError):
            library.open_library(lib.path)
        with self.assertRaises(library.LibraryError):
            library.open_library(self.root)               # not a package

    def test_recent_keeps_order_and_drops_missing(self):
        a = library.create(self.root / 'A.pilib')
        b = library.create(self.root / 'B.pilib')
        library.remember(a)
        library.remember(b)
        library.remember(a)
        self.assertEqual([Path(i['path']) for i in library.recent()], [a.path, b.path])
        self.assertEqual(library.last(), a.path)
        (a.path / library.CONFIG).unlink()
        self.assertEqual(library.last(), b.path)

    def test_paths_inside_and_outside_a_package(self):
        lib = library.create(self.root / 'A.pilib')
        self.assertEqual(library.backups_dir(lib.db), lib.backups)
        self.assertEqual(library.features_dir(lib.source), lib.features)
        self.assertEqual(library.shared_dir(lib.source), library.SHARED)
        bare = self.root / 'bare'
        bare.mkdir()
        self.assertEqual(library.backups_dir(bare / 'app.sqlite'), bare.resolve() / 'library-backups')
        self.assertEqual(library.features_dir(bare / 'source.db'), bare.resolve())
        self.assertEqual(library.shared_dir(bare / 'source.db'), bare.resolve())

    def _legacy_review(self):
        review = self.root / 'review'
        review.mkdir()
        source = fixture(review)
        os.replace(source, review / 'pi.db')
        store = Store(review / 'pi.db', review / 'library.sqlite')
        store.refresh()
        alice = store.entities('Alice')[0]['id']
        store.change(['a'], 'rejected', alice)
        (review / 'full-manifest.csv').write_text('uuid\nA\n')
        np.save(review / 'full-features.npy', np.array([[1., 0.]], np.float32))
        (review / 'web-thumbnails').mkdir()
        (review / 'web-thumbnails' / 'x.jpg').write_bytes(b'jpg')
        (review / 'char-index').mkdir()                  # shared, must stay behind
        return review, store

    def test_adopt_moves_everything_and_links_back(self):
        review, before = self._legacy_review()
        with before.connect() as c:
            assertions = c.execute('SELECT * FROM assertions ORDER BY uuid,entity').fetchall()
        lib = library.adopt(review, self.root / 'pics' / '我的图库.pilib', photos='/tmp/P.photoslibrary')
        self.assertTrue(lib.source.is_file() and not lib.source.is_symlink())
        self.assertTrue(lib.db.is_file())
        self.assertTrue((lib.features / 'full-features.npy').is_file())
        self.assertTrue((lib.thumbnails / 'x.jpg').is_file())
        self.assertTrue((lib.path / 'album-char-map.json').is_file())
        self.assertTrue((review / 'char-index').is_dir())
        for old in library.LINK_BACK:
            self.assertTrue((review / old).is_symlink())
        self.assertEqual(json.loads((lib.path / 'library.json').read_text())['sources'][0]['kind'], 'photos')
        after = Store(lib.source, lib.db)
        with after.connect() as c:
            self.assertEqual(c.execute('SELECT * FROM assertions ORDER BY uuid,entity').fetchall(), assertions)
        self.assertEqual(after.backup().parent, lib.backups)
        self.assertEqual(Features(lib.source).load().uuids, ['A'])
        with self.assertRaises(library.LibraryError):
            library.adopt(review, self.root / 'again.pilib')   # already adopted: pi.db is a symlink now

    def test_adopt_refuses_an_open_database_and_leaves_review_alone(self):
        review, _ = self._legacy_review()
        holder = sqlite3.connect(review / 'library.sqlite')
        holder.execute('BEGIN IMMEDIATE')
        holder.execute("INSERT INTO meta VALUES ('held','1')")
        try:
            with self.assertRaises(library.LibraryError):
                library.adopt(review, self.root / 'x.pilib')
        finally:
            holder.rollback()
            holder.close()
        self.assertTrue((review / 'pi.db').is_file() and not (review / 'pi.db').is_symlink())
        self.assertFalse((self.root / 'x.pilib').exists())


    def test_source_links_and_presets(self):
        lib = library.create(self.root / 'A.pilib', 'A', [library.photos_source('/nonexistent/P.photoslibrary')])
        with sqlite3.connect(lib.source) as c:
            c.execute('CREATE TABLE assets(uuid TEXT PRIMARY KEY, src_path TEXT)')
            c.executemany('INSERT INTO assets VALUES (?,?)', [('U1', '/a'), ('U2', '/b')])
        library.upgrade_source(lib)            # Photos unreadable: linked, preset left for the next sync
        library.upgrade_source(lib)            # idempotent
        with sqlite3.connect(lib.source) as c:
            self.assertEqual(c.execute('SELECT asset,system,key,source_id FROM asset_links ORDER BY asset').fetchall(),
                             [('U1', 'photos', 'U1', 's1'), ('U2', 'photos', 'U2', 's1')])
            self.assertEqual(c.execute('SELECT uuid,source_id,preset FROM assets ORDER BY uuid').fetchall(),
                             [('U1', 's1', None), ('U2', 's1', None)])
            library.link_photos(c, lib.config['sources'][0], [('U1', 'com.atebits.Tweetie2'), ('U2', 'com.tencent.xin')])
            self.assertEqual(c.execute('SELECT preset FROM assets ORDER BY uuid').fetchall(), [('x',), ('other',)])

    def test_restart_arguments_point_at_the_new_library(self):
        from web.server import restart_argv
        self.assertEqual(restart_argv(['--port', '8777', '--library', 'old.pilib', '--refresh', '--no-sync'], 'new.pilib'),
                         ['--port', '8777', '--no-sync', '--no-open', '--library', 'new.pilib'])
        self.assertEqual(restart_argv(['--source=x.db', '--db', 'y.sqlite', '--no-open'], 'n.pilib'),
                         ['--no-open', '--library', 'n.pilib'])


    def test_export_import_forget_and_presets(self):
        import zipfile
        review, _ = self._legacy_review()
        lib = library.adopt(review, self.root / 'mine.pilib')
        (lib.thumbnails / 'big.jpg').write_bytes(b'x' * 100)            # never exported
        archive = library.export_library(lib, self.root / 'out' / 'mine.pilib.zip')
        with zipfile.ZipFile(archive) as z:
            self.assertEqual(set(z.namelist()), {'library.json', 'source.sqlite', 'library.sqlite', 'features/full-features.npy',
                                                 'features/full-manifest.csv', 'album-char-map.json'})
        home = self.root / 'home'
        first = library.import_library(archive, home)
        second = library.import_library(archive, home)                  # never overwrites: a second copy
        self.assertEqual((first.path.name, second.path.name), ('我的图库.pilib', '我的图库 2.pilib'))
        with Store(first.source, first.db).connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM assertions").fetchone()[0], 1)
        self.assertEqual(first.config['imported_from'], 'mine.pilib.zip')
        bad = self.root / 'bad.zip'
        with zipfile.ZipFile(bad, 'w') as z:
            z.writestr('library.json', '{"format": 1}')
            z.writestr('source.sqlite', '')
            z.writestr('../evil', 'x')
        with self.assertRaises(library.LibraryError):
            library.import_library(bad, home)
        library.remember(first)
        library.remember(second)
        library.forget(first.path)
        self.assertEqual([Path(i['path']) for i in library.recent()], [second.path])
        self.assertTrue(first.path.exists())                             # forgetting never deletes
        with self.assertRaises(library.LibraryError):
            library.set_preset(lib, 's1', 'x')                          # a Photos source has no preset to set

    def test_unique_path_strips_separators(self):
        self.assertEqual(library.unique_path(self.root, 'a/b:c').name, 'abc.pilib')
        self.assertEqual(library.unique_path(self.root, '  ').name, '图库.pilib')


if __name__ == '__main__':
    unittest.main()
