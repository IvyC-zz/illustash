"""M3 files: old handler (recorded, tests/fixed_expectations.py) vs native host, generated temporary assets only.

Cache hits and static bodies/headers are exact; errors compare status only. New JPEGs
use explicit pixel bounds, independent caches, no snapshot re-recording. No workers
or Python are permitted in the native process (the lifecycle harness installs sentinels).
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import hashlib
import io
import http.client
import socket
import os
from pathlib import Path
import sqlite3
import unittest

from PIL import Image, ImageChops, ImageStat, ImageDraw
from tests import test_native_lifecycle as lifecycle
from tests.test_native_lifecycle import NativeLifecycleTest, HEADERS


def key(path, ident, size):
    stat = path.stat()
    return hashlib.sha256(f'{ident}:{stat.st_mtime_ns}:{stat.st_size}:{size}'.encode()).hexdigest() + '.jpg'


def asset(self, ident, im=None, *, format='PNG', exif=None):
    path = self.root / (ident + '.data')  # Decode by signature, not extension.
    if im is not None:
        im.save(path, format=format, **({'exif': exif} if exif is not None else {}))
    with closing(sqlite3.connect(self.lib.db)) as c:
        c.execute("INSERT OR REPLACE INTO assets(uuid,src_path,width,height) VALUES (?,?,?,?)",
                  (ident, str(path), im.width if im else 1, im.height if im else 1))
        c.commit()
    return path


def compare_generated(self, ident, path, size='thumb'):
    url = f'/image?id={ident}&size={size}'
    target = self.lib.thumbnails / key(path, ident, size)
    target.unlink(missing_ok=True)
    old = lifecycle.NativeLifecycleTest.reference_response(self, url)
    self.assertEqual(old['status'], 200)
    target.unlink(missing_ok=True)   # written by the reference when recording
    new = self.request(url)
    self.assertEqual(new['status'], 200)
    self.assertEqual(new['route'], 'core')
    for header in HEADERS:
        if header != 'Content-Length':
            self.assertEqual(old['headers'].get(header), new['headers'].get(header), header)
    self.assertEqual(new['body'], target.read_bytes())
    with Image.open(io.BytesIO(old['body'])) as a, Image.open(io.BytesIO(new['body'])) as b:
        self.assertEqual(a.size, b.size, (ident, size))
        self.assertEqual(b.format, 'JPEG')
        # JPEG implementations/filter rounding differ. Bounds are fixed before results:
        # channel-mean absolute error <= 4/255 and >=99% pixels <=24/255 per channel.
        delta = ImageChops.difference(a.convert('RGB'), b.convert('RGB'))
        self.assertLessEqual(max(ImageStat.Stat(delta).mean), 4.0, (ident, size, ImageStat.Stat(delta).mean))
        for channel in delta.split():
            hist = channel.histogram()
            self.assertLessEqual(sum(hist[25:]) / (a.width * a.height), .01, (ident, size))
    return new


class NativeFilesTest(unittest.TestCase):
    # Reuse only the isolated package/server fixture, without unrelated test cases.
    setUpClass = classmethod(NativeLifecycleTest.setUpClass.__func__)
    setUp = NativeLifecycleTest.setUp
    package = NativeLifecycleTest.package
    stop = NativeLifecycleTest.stop
    start = NativeLifecycleTest.start
    request = NativeLifecycleTest.request
    pair = NativeLifecycleTest.pair


    def test_static_bytes_headers_and_security(self):
        for url in ('/', '/app.js', '/app.css', '/theme.js', '/favicon.svg', '/splash.js'):
            response = self.pair(url + '?cache=1')
            self.assertEqual(response['status'], 200)
            self.assertEqual(response['route'], 'core')
            for headers in ({'Host': 'evil.example'}, {'Origin': 'https://evil.example'}):
                self.assertEqual(self.request(url, headers=headers)['status'],
                                 lifecycle.NativeLifecycleTest.reference_response(self, url, headers=headers)['status'])
        self.assertFalse(self.sentinel.exists())


    def test_old_cache_hits_original_bytes_and_signed_key(self):
        path = asset(self, 'cache-雪', Image.new('RGB', (10, 12), 'red'))
        for mtime in (1_700_000_000_123_456_789, -1_234_567_890):
            os.utime(path, ns=(mtime, mtime))
            for size in ('thumb', 'medium', 'preview', 'full'):
                target = self.lib.thumbnails / key(path, 'cache-雪', size)
                payload = b'cached original bytes\x00\xff' + size.encode()
                target.write_bytes(payload)
                # Non-decodable original still serves existing cached bytes.
                response = self.pair('/image?id=cache-%E9%9B%AA&size=' + size)
                self.assertEqual(response['body'], payload)
                self.assertEqual(response['route'], 'core')
        self.assertFalse(self.sentinel.exists())


    def test_sizes_orientation_and_no_upscale(self):
        im = Image.new('RGB', (3601, 1801), '#408060')
        path = asset(self, 'large', im, format='JPEG')
        for size in ('thumb', 'medium', 'preview', 'full'):
            compare_generated(self, 'large', path, size)
        im = Image.new('RGB', (97, 43), '#408060')
        for x in range(97):
            for y in range(43):
                im.putpixel((x,y), (x * 2, y * 5, (x + y) % 256))
        for orientation in (*range(1, 9), 0, 263):
            exif = Image.Exif(); exif[274] = orientation
            for fmt in ('JPEG', 'PNG', 'WEBP', 'TIFF'):
                ident = f'exif-{fmt}-{orientation}'
                path = asset(self, ident, im, format=fmt, exif=exif)
                compare_generated(self, ident, path)
        self.assertFalse(self.sentinel.exists())


    def test_transparency_palette_and_formats(self):
        im = Image.new('RGBA', (1001, 503), (40, 120, 200, 0))
        for x in range(200, 800):
            for y in range(100, 400):
                im.putpixel((x,y), (80, 140, 180, 128 if x < 500 else 255))
        for fmt in ('PNG', 'WEBP'):
            ident = 'alpha-' + fmt
            path = asset(self, ident, im, format=fmt)
            result = compare_generated(self, ident, path)
            with Image.open(io.BytesIO(result['body'])) as out:
                for got, bg in zip(out.getpixel((2,2)), (244, 244, 242)):
                    self.assertLessEqual(abs(got - bg), 2)
        palette = Image.new('P', (601, 303), 0)
        palette.putpalette([40, 120, 200, 180, 40, 20] + [0] * 762)
        palette.paste(1, (150, 60, 450, 240))
        palette.info['transparency'] = 0
        for fmt in ('PNG', 'GIF'):
            ident = 'palette-' + fmt
            path = asset(self, ident, palette, format=fmt)
            compare_generated(self, ident, path)
        path = asset(self, 'CMYK-JPEG', Image.new('CMYK', (1001,503), (40,100,20,80)), format='JPEG')
        compare_generated(self, 'CMYK-JPEG', path)
        for mode in ('L', '1', 'RGB'):
            for fmt in ('PNG', 'BMP'):
                ident = f'{mode}-{fmt}'
                im = Image.new(mode, (1001,503) if mode == '1' else (37,19), 1)
                if mode == '1':
                    draw = ImageDraw.Draw(im)
                    for x in range(0,1001,7):
                        draw.rectangle((x,0,x+3,502), fill=0)
                path = asset(self, ident, im, format=fmt)
                compare_generated(self, ident, path)


    def test_error_status_query_semantics_and_failed_write_cleanup(self):
        missing = asset(self, 'missing')
        broken = asset(self, 'broken'); broken.write_bytes(b'not an image')
        truncated = asset(self, 'truncated', Image.new('RGB', (100,80), 'red'), format='JPEG')
        truncated.write_bytes(truncated.read_bytes()[:80])
        folder = asset(self, 'folder'); folder.mkdir()
        good = asset(self, 'good', Image.new('RGB', (12,8), 'red'))
        with closing(sqlite3.connect(self.lib.db)) as c:
            c.execute("INSERT INTO assets(uuid,src_path) VALUES ('null',NULL),('empty','')")
            c.commit()
        for url in ('/image', '/image?id=', '/image?id=not-present', '/image?id=missing',
                    '/image?id=broken', '/image?id=truncated', '/image?id=folder', '/image?id=null', '/image?id=empty',
                    '/image?id=good&size=raw', '/image?id=good&size=FULL'):
            self.assertEqual(self.request(url)['status'], lifecycle.NativeLifecycleTest.reference_response(self, url)['status'], url)
        for url in ('/image?id=bad&id=good', '/image?id=good&id=', '/image?id=good&size=',
                    '/image?id=good&size=bad&size=thumb', '/image?id=good&size=medium&size='):
            self.assertEqual(self.pair(url)['status'], 200)
        target = self.lib.thumbnails / key(good, 'good', 'full')
        target.mkdir()
        self.assertEqual(self.request('/image?id=good&size=full')['status'], 404)
        self.assertFalse(list(self.lib.thumbnails.glob('*.tmp')))
        self.assertFalse(self.sentinel.exists())


    def test_concurrent_same_key_atomic_cache_invalidation(self):
        path = asset(self, 'concurrent', Image.new('RGB', (1201,803), '#408060'))
        target = self.lib.thumbnails / key(path, 'concurrent', 'thumb')
        url = '/image?id=concurrent'
        with ThreadPoolExecutor(max_workers=12) as pool:
            results = list(pool.map(lambda _: self.request(url), range(24)))
        self.assertTrue(all(r['status'] == 200 and r['route'] == 'core' for r in results))
        self.assertTrue(all(r['body'] == target.read_bytes() for r in results))
        first = target.stat().st_mtime_ns
        self.request(url)
        self.assertEqual(target.stat().st_mtime_ns, first)
        os.utime(path, ns=(path.stat().st_atime_ns, path.stat().st_mtime_ns + 1))
        second = self.lib.thumbnails / key(path, 'concurrent', 'thumb')
        self.assertEqual(self.request(url)['status'], 200)
        self.assertTrue(second.is_file()); self.assertTrue(target.is_file())
        self.assertFalse(list(self.lib.thumbnails.glob('*.tmp')))
        self.assertFalse(self.sentinel.exists())

    def test_image_backpressure_does_not_block_switch_and_new_session_uses_new_path(self):
        second = self.package('second')
        source = asset(self, 'switch', Image.new('RGB', (12,8), 'red'))
        target = self.lib.thumbnails / key(source, 'switch', 'thumb')
        payload = b'old-session-cache' * 300_000
        target.write_bytes(payload)
        new_source = self.root / 'new-switch.png'
        Image.new('RGB', (12,8), '#40a060').save(new_source)
        with closing(sqlite3.connect(second.db)) as c:
            c.execute("INSERT INTO assets(uuid,src_path,width,height) VALUES ('switch',?,12,8)", (str(new_source),))
            c.commit()
        sock = socket.socket(); self.addCleanup(sock.close)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096); sock.settimeout(10)
        sock.connect(('127.0.0.1', self.port))
        sock.sendall((f'GET /image?id=switch HTTP/1.1\r\nHost: 127.0.0.1:{self.port}\r\n'
                      'Connection: close\r\n\r\n').encode())
        response = http.client.HTTPResponse(sock); self.addCleanup(response.close); response.begin()
        self.assertEqual(response.status, 200)
        self.assertEqual(int(response.getheader('Content-Length')), len(payload))
        self.assertEqual(self.request('/api/libraries/open', {'path': str(second.path)})['status'], 200)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024 * 1024)
        self.assertEqual(response.read(), payload)
        new = self.request('/image?id=switch')
        self.assertEqual(new['status'], 200); self.assertEqual(new['route'], 'core')
        self.assertEqual(new['body'], (second.thumbnails / key(new_source, 'switch', 'thumb')).read_bytes())
        with Image.open(io.BytesIO(new['body'])) as im:
            self.assertGreater(im.getpixel((2,2))[1], 140)
        self.assertEqual(target.read_bytes(), payload)
        self.assertFalse(self.sentinel.exists())

    def test_retired_file_route_and_missing_static_resource(self):
        self.stop(self.process)
        for route in ('GET:/image=forward', 'GET:/app.js=forward'):
            result = __import__('subprocess').run([str(self.binary), 'http', '--route', route],
                                                 env=self.env, capture_output=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('已退役', result.stderr.decode())
        self.resources = self.root / 'empty-root'
        (self.resources / 'tags').mkdir(parents=True)
        (self.resources / 'tags/tags_v3.csv').write_bytes((type(self).resources / 'tags/tags_v3.csv').read_bytes())
        self.process, _ = self.start(self.lib.path)
        self.assertEqual(self.request('/')['status'], 500)
        self.assertFalse(self.sentinel.exists())


del NativeLifecycleTest
