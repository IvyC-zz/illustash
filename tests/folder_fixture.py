"""Synthetic folder library: a reference index plus a few generated images. Temporary directories only."""
import os
import sqlite3
import tempfile
import zlib
from pathlib import Path
from unittest import mock

from PIL import Image

from tests import library_fixture as library
from tests.synthetic_source import source_fixture as fixture


IMAGES = Path(__file__).resolve().parent / 'fixtures' / 'folder_images'


def image(path, colour, size=(40, 30)):
    """A solid-colour image. The committed copies keep the bytes independent of the local
    encoder, so scans match recorded expectations on every platform."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fixed = IMAGES / f'{colour}-{size[0]}x{size[1]}{path.suffix.lower()}'
    if fixed.is_file():
        path.write_bytes(fixed.read_bytes())
    else:
        Image.new('RGB', size, colour).save(path)


def settle(*folders):
    """Content-derived modification times for generated files, so scans that store (size, mtime)
    match recorded expectations (#9): a rename keeps it, new content changes it. Times a test set
    on purpose (before 2020) are kept. On macOS an earlier mtime also moves the birth time."""
    for folder in folders:
        for path in sorted(folder.rglob('*')) if folder.exists() else ():
            if path.is_file() and path.stat().st_mtime >= 1_600_000_000:
                when = (1_500_000_000 + zlib.crc32(path.read_bytes()) % 50_000_000) * 1_000_000_000
                os.utime(path, ns=(when, when))


class FolderSource:
    """setUp/tearDown/rows for tests that build a synthetic folder library (former tests/test_folder.py)."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        patcher = mock.patch.object(library, 'APP_SUPPORT', self.root / 'support')
        patcher.start()
        self.addCleanup(patcher.stop)
        self.pics = self.root / 'Pictures' / 'pixiv'
        image(self.pics / 'a.png', 'red')
        image(self.pics / 'sub' / 'b.jpg', 'green')
        image(self.pics / '.hidden.png', 'blue')
        (self.pics / 'notes.txt').write_text('not an image')
        settle(self.pics)
        (self.root / 'P.pilib').mkdir()
        self.lib = library.Library(self.root / 'P.pilib')
        self.source = library.folder_source(self.pics, 'pixiv')

    def tearDown(self):
        self.temp.cleanup()

    def rows(self, sql):
        with sqlite3.connect(self.lib.source) as c:
            return c.execute(sql).fetchall()
