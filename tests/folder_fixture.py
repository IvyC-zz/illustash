"""合成的文件夹图库：一份参考索引加几张生成的图片。只用临时目录。"""
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
    """一张纯色图片。用已提交的副本，字节就不依赖本机编码器，扫描结果在每个平台上都与录制的期望一致。"""
    path.parent.mkdir(parents=True, exist_ok=True)
    fixed = IMAGES / f'{colour}-{size[0]}x{size[1]}{path.suffix.lower()}'
    if fixed.is_file():
        path.write_bytes(fixed.read_bytes())
    else:
        Image.new('RGB', size, colour).save(path)


def settle(*folders):
    """给生成的文件设置由内容推出的修改时间，让存储 (size, mtime) 的扫描与录制的期望一致（#9）：
    改名不变，内容变了才变。测试有意设置的时间（2020 年之前）保留。在 macOS 上，把 mtime 改早也会让创建时间跟着变。"""
    for folder in folders:
        for path in sorted(folder.rglob('*')) if folder.exists() else ():
            if path.is_file() and path.stat().st_mtime >= 1_600_000_000:
                when = (1_500_000_000 + zlib.crc32(path.read_bytes()) % 50_000_000) * 1_000_000_000
                os.utime(path, ns=(when, when))


class FolderSource:
    """构建合成文件夹图库的测试所用的 setUp/tearDown/rows（原 tests/test_folder.py）。"""
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
