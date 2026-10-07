"""Synthetic folder library: a reference index plus a few generated images. Temporary directories only."""
import sqlite3
import tempfile
from pathlib import Path
from unittest import mock

from PIL import Image

from web import library
from tests.synthetic_source import source_fixture as fixture


def image(path, colour, size=(40, 30)):
    path.parent.mkdir(parents=True, exist_ok=True)
    Image.new('RGB', size, colour).save(path)


class FolderSource:
    """setUp/tearDown/rows for tests that build a synthetic folder library (former tests/test_folder.py)."""
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
