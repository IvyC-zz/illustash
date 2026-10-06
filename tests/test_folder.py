"""文件夹来源：扫描、只对变化的文件算哈希、移动和改名保留资产、删除标记、内容变化重新识别。只用临时目录。"""
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from PIL import Image

from web import folder, library
from tests.test_web import fixture


def image(path, colour, size=(40, 30)):
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.new('RGB', size, colour).save(path)


class FolderSourceTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        patcher = mock.patch.object(library, 'APP_SUPPORT', self.root / 'support')
        patcher.start()
        self.addCleanup(patcher.stop)
        ref_dir = self.root / 'ref'
        ref_dir.mkdir()
        source = fixture(ref_dir)
        with sqlite3.connect(source) as c:                              # the real source schema's asset columns
            for col in ('z_pk INTEGER', 'hidden INTEGER', 'stable_hash TEXT', 'orig_size INTEGER'):
                c.execute(f'ALTER TABLE assets ADD COLUMN {col}')
            c.execute('CREATE TABLE embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model))')
        self.reference = library.export_reference(source, self.root / 'reference.sqlite')
        self.pics = self.root / 'Pictures' / 'pixiv'
        image(self.pics / 'a.png', 'red')
        image(self.pics / 'sub' / 'b.jpg', 'green')
        image(self.pics / '.hidden.png', 'blue')
        (self.pics / 'notes.txt').write_text('not an image')
        with mock.patch.object(library, 'REFERENCE', self.reference):
            self.lib = library.new_folder_library(self.pics, self.root / 'P.pilib')
        self.source = self.lib.config['sources'][0]

    def tearDown(self):
        self.temp.cleanup()

    def rows(self, sql):
        with sqlite3.connect(self.lib.source) as c:
            return c.execute(sql).fetchall()

    def scan(self):
        return folder.scan(self.source, self.lib.source)

    def test_new_library_from_a_folder(self):
        self.assertEqual(self.source['preset'], 'pixiv')
        self.assertEqual(self.source['preset_origin'], 'auto')
        self.assertEqual(self.rows('SELECT count(*) FROM characters'), [(2,)])   # the reference character index
        self.assertEqual(self.rows('SELECT count(*) FROM assets'), [(0,)])
        with self.assertRaises(library.LibraryError):
            library.folder_source(self.pics, 'weibo')

    def test_scan_moves_deletes_and_changes(self):
        r = self.scan()
        self.assertEqual((r['new'], r['gone']), (2, 0))
        assets = dict(self.rows("SELECT key,asset FROM asset_links WHERE system='file'"))
        self.assertEqual(set(assets), {'a.png', 'sub/b.jpg'})
        self.assertEqual(self.rows('SELECT DISTINCT preset,source_id,in_scope FROM assets'), [('pixiv', 's1', 1)])
        with mock.patch.object(folder, 'md5_of', wraps=folder.md5_of) as hashed:
            self.assertEqual(self.scan()['new'], 0)
            hashed.assert_not_called()                                   # nothing changed: nothing re-hashed
        os.rename(self.pics / 'a.png', self.pics / 'sub' / 'renamed.png')
        r = self.scan()
        self.assertEqual((r['new'], r['moved'], r['gone']), (0, 1, 0))
        self.assertEqual(dict(self.rows("SELECT key,asset FROM asset_links WHERE system='file'"))['sub/renamed.png'], assets['a.png'])
        self.assertTrue(self.rows(f"SELECT src_path FROM assets WHERE uuid='{assets['a.png']}'")[0][0].endswith('sub/renamed.png'))
        (self.pics / 'sub' / 'b.jpg').rename(self.root / 'b.jpg')
        self.assertEqual(self.scan()['gone'], 1)
        self.assertEqual(self.rows(f"SELECT trashed FROM assets WHERE uuid='{assets['sub/b.jpg']}'"), [(1,)])
        (self.root / 'b.jpg').rename(self.pics / 'back.jpg')             # comes back under another name
        r = self.scan()
        self.assertEqual((r['moved'], r['returned']), (1, 1))
        self.assertEqual(self.rows(f"SELECT trashed FROM assets WHERE uuid='{assets['sub/b.jpg']}'"), [(0,)])
        with sqlite3.connect(self.lib.source) as c:
            c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES (?, 'rating', 'general', 1)", (assets['sub/b.jpg'],))
        image(self.pics / 'back.jpg', 'yellow', (50, 50))
        os.utime(self.pics / 'back.jpg', ns=(1, 1))
        r = self.scan()
        self.assertEqual((r['changed'], r['new']), (1, 0))
        self.assertEqual(self.rows(f"SELECT count(*) FROM labels WHERE uuid='{assets['sub/b.jpg']}'"), [(0,)])   # recognised again
        self.assertEqual(self.rows(f"SELECT width FROM assets WHERE uuid='{assets['sub/b.jpg']}'"), [(50,)])

    def test_library_follows_a_moved_file(self):
        from web.store import Store
        self.scan()
        store = Store(self.lib.source, self.lib.db)
        store.refresh()
        asset = dict(self.rows("SELECT key,asset FROM asset_links WHERE system='file'"))['a.png']
        os.rename(self.pics / 'a.png', self.pics / 'moved.png')
        self.scan()
        self.assertEqual(store.refresh_paths(), 1)
        self.assertEqual(store.image_path(asset), self.pics / 'moved.png')
        self.assertEqual(store.refresh_paths(), 0)

    def test_user_preset_rewrites_the_images(self):
        self.scan()
        library.set_preset(self.lib, 's1', 'x')
        again = library.open_library(self.lib.path).config['sources'][0]
        self.assertEqual((again['preset'], again['preset_origin']), ('x', 'user'))
        self.assertEqual(self.rows('SELECT DISTINCT preset FROM assets'), [('x',)])
        with self.assertRaises(library.LibraryError):
            library.set_preset(self.lib, 's1', 'weibo')

    def test_guess_preset(self):
        self.assertEqual(library.guess_preset('/sdcard/Pictures/Twitter'), 'x')
        self.assertEqual(library.guess_preset('/Users/me/Downloads/pixiv-saves'), 'pixiv')
        self.assertEqual(library.guess_preset('/sdcard/Pictures/X'), 'x')
        self.assertEqual(library.guess_preset('/Users/me/Pictures/Xmas'), 'other')
        self.assertEqual(library.guess_preset('/sdcard/DCIM/Screenshots'), 'other')


if __name__ == '__main__':
    unittest.main()
