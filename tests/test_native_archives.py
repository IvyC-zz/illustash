"""#42 acceptance. All paths and packages are temporary synthetic fixtures; no real library input."""
from contextlib import closing
import json
import os
from pathlib import Path
import shutil
import sqlite3
import threading
import time
import unittest
from unittest.mock import patch
import zipfile

from tests import test_native_lifecycle as lifecycle
from tests.test_native_human_labels import snapshot
from tests.test_read_http import wait_until
from web import library


class NativeArchivesTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    setUp = lifecycle.NativeLifecycleTest.setUp
    package = lifecycle.NativeLifecycleTest.package
    stop = lifecycle.NativeLifecycleTest.stop
    start = lifecycle.NativeLifecycleTest.start
    request = lifecycle.NativeLifecycleTest.request
    pair = lifecycle.NativeLifecycleTest.pair

    def state(self):
        return json.loads(self.request('/api/libraries')['body'])['export']

    def begin(self):
        response = self.request('/api/libraries/export', {})
        self.assertEqual(response['status'], 200, response)
        self.assertEqual(response['route'], 'core')
        return json.loads(response['body'])

    def done(self):
        wait_until(lambda: self.state()['status'] in ('done', 'error'), timeout=15)
        result = self.state()
        self.assertEqual(result['status'], 'done', result)
        return Path(result['path'])

    def test_roundtrip_wal_archive_contents_database_projection_and_same_id(self):
        self.lib.config['sources'] = [library.folder_source(self.root / 'Pixiv')]
        self.lib.save()
        for rel, data in (('features/full-features.npy', b'synthetic-features\0\xff'),
                          ('features/full-manifest.csv', b'id,synthetic\n'), ('album-char-map.json', b'{"synthetic":1}')):
            (self.lib.path / rel).write_bytes(data)
        (self.lib.thumbnails / 'excluded').write_bytes(b'excluded')
        (self.lib.backups / 'excluded').write_bytes(b'excluded')
        connections = []
        for db in (self.lib.source, self.lib.db):
            c = sqlite3.connect(db); connections.append(c); self.addCleanup(c.close)
            c.execute('PRAGMA journal_mode=WAL'); c.execute('PRAGMA wal_autocheckpoint=0')
            c.execute('CREATE TABLE archive_probe(v BLOB, n INTEGER, r REAL, t TEXT)')
            c.execute("INSERT INTO archive_probe(rowid,v,n,r,t) VALUES (37,?,7,1.5,'合成 WAL')", (b'\0\xff',)); c.commit()
            self.assertGreater(Path(str(db) + '-wal').stat().st_size, 0)
        legacy = library.export_library(self.lib, self.root / 'legacy.zip')
        initial = self.begin()
        expected = {'status':'running', 'path':str(Path(self.env['HOME']) / 'Downloads' / f'{self.lib.name}-{time.strftime("%Y%m%d")}.pilib.zip')}
        self.assertEqual(initial, expected)
        native = self.done()
        self.assertEqual(self.state()['bytes'], native.stat().st_size)
        left, right = self.root / 'legacy-unpacked', self.root / 'native-unpacked'
        with zipfile.ZipFile(legacy) as a, zipfile.ZipFile(native) as b:
            self.assertEqual(set(a.namelist()), set(b.namelist()))
            self.assertEqual(set(a.namelist()), set(library.EXPORTED))
            a.extractall(left); b.extractall(right)
        for rel in library.EXPORTED:
            if rel.endswith('.sqlite'):
                self.assertEqual(snapshot(left / rel), snapshot(right / rel), rel)
                self.assertEqual(snapshot(self.lib.path / rel), snapshot(right / rel), rel)
            else:
                self.assertEqual((left / rel).read_bytes(), (right / rel).read_bytes(), rel)
        expected_path = library.unique_path(library.HOME, self.lib.name)
        expected_bytes = json.dumps({'path':str(expected_path), 'name':self.lib.name}, ensure_ascii=False).encode()
        response = self.request('/api/libraries/import', {'path':str(native)})
        self.assertEqual(response['status'], 200, response)
        self.assertEqual(response['body'], expected_bytes)
        imported = library.open_library(expected_path)
        self.assertEqual(imported.config['id'], self.lib.config['id'])
        self.assertEqual(imported.config['imported_from'], native.name)
        for rel in ('source.sqlite', 'library.sqlite'):
            self.assertEqual(snapshot(self.lib.path / rel), snapshot(imported.path / rel))
        old_token = self.token
        response = self.request('/api/libraries/open', {'path':str(imported.path)})
        self.assertEqual(response['body'], expected_bytes)
        self.assertEqual(self.state()['status'], 'idle')
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertNotEqual(self.token, old_token)
        self.assertEqual(self.request('/api/libraries/export', {}, token=old_token)['status'], 403)
        self.begin(); second = self.done()
        self.assertEqual(json.loads(self.request('/api/libraries')['body'])['current'], str(imported.path))
        self.assertTrue(second.exists())
        self.assertFalse(self.sentinel.exists())

    def test_preset_success_bytes_database_and_forget_preserves_package(self):
        source = library.folder_source(self.root / 'folder')
        self.lib.config['sources'] = [source]; self.lib.save()
        with closing(sqlite3.connect(self.lib.source)) as c:
            library.ensure_source_schema(c)
            c.execute("UPDATE assets SET source_id='s1',preset='other'"); c.commit()
        expected_config = self.root / 'expected.pilib'; shutil.copytree(self.lib.path, expected_config)
        expected = library.open_library(expected_config)
        result = library.set_preset(expected, 's1', 'pixiv')
        response = self.request('/api/libraries/preset', {'source':'s1', 'preset':'pixiv'})
        self.assertEqual(response['status'], 200, response)
        self.assertEqual(response['body'], json.dumps(result, ensure_ascii=False).encode())
        self.assertEqual(self.lib.path.joinpath('library.json').read_bytes(), expected.path.joinpath('library.json').read_bytes())
        self.assertEqual(snapshot(self.lib.source), snapshot(expected.source))
        self.assertEqual(json.loads(self.request('/api/libraries')['body'])['info']['sources'][0], result)
        for body in ({'source':'absent','preset':'x'}, {'source':'s1','preset':'bad'}, {'source':None, 'preset':[]}):
            self.assertEqual(self.request('/api/libraries/preset', body)['status'], self.request('/api/libraries/preset', body, base=self.direct)['status'])
        other, _ = self.package('other'); library.remember(other, front=False)
        before = snapshot(other.db)
        response = self.pair('/api/libraries/forget', {'path':str(other.path)})
        self.assertEqual(response['body'], b'{"ok": true}')
        self.assertEqual(snapshot(other.db), before)
        self.assertEqual(self.request('/api/libraries/forget', {'path':str(self.lib.path)})['status'], 400)
        self.assertTrue(other.path.is_dir())

    def test_invalid_archives_status_paths_crc_and_cleanup(self):
        good = library.export_library(self.lib, self.root / 'good.zip')
        with zipfile.ZipFile(good) as z:
            files = {name:z.read(name) for name in z.namelist()}
        variants = [b'not zip', good.read_bytes()[:100]]
        # Windows 上 zipfile 写入时会把反斜杠换成 `/`，构造不出反斜杠条目；该情形由 app_archives 的单元测试覆盖。
        names = ('../escape', '/absolute', 'features/../library.json', 'features\\full-features.npy', 'unexpected', 'features/')
        for name in (n for n in names if os.name != 'nt' or '\\' not in n):
            out = self.root / f'bad-{len(variants)}.zip'
            with zipfile.ZipFile(out, 'w') as z:
                for n,b in files.items(): z.writestr(n,b)
                z.writestr(name,b'bad')
            variants.append(out.read_bytes())
        for config in ('{', '[]', '{"format":2,"name":"future"}', '{"format":"1","name":"invalid"}', '{"format":1.0,"name":"float"}'):
            out = self.root / f'bad-{len(variants)}.zip'
            with zipfile.ZipFile(out, 'w') as z:
                for n,b in files.items(): z.writestr(n,config if n == 'library.json' else b)
            variants.append(out.read_bytes())
        out = self.root / 'bad-crc.zip'
        with zipfile.ZipFile(out,'w',compression=zipfile.ZIP_STORED) as z:
            for n,b in files.items(): z.writestr(n,b)
            z.writestr('features/full-features.npy', b'synthetic-crc-needle')
        data = bytearray(out.read_bytes()); data[data.index(b'synthetic-crc-needle')] ^= 1; variants.append(data)
        for i,data in enumerate(variants):
            archive = self.root / f'case-{i}.zip'; archive.write_bytes(data)
            before = set(library.HOME.glob('*')) if library.HOME.exists() else set()
            with self.subTest(i=i):
                self.assertEqual(self.request('/api/libraries/import', {'path':str(archive)})['status'], self.request('/api/libraries/import', {'path':str(archive)}, base=self.direct)['status'])
                self.assertEqual(set(library.HOME.glob('*')) if library.HOME.exists() else set(), before)
        self.assertFalse((self.root / 'escape').exists())
        self.assertFalse(self.sentinel.exists())

    def test_repeated_export_busy_switch_cancel_same_id_and_exit_cleanup(self):
        # A held rollback-journal write lock deterministically keeps backup active without huge data.
        other, _ = self.package('other')
        other.config['id'] = self.lib.config['id']; other.save()
        lock = sqlite3.connect(self.lib.source); self.addCleanup(lock.close)
        lock.execute('BEGIN EXCLUSIVE')
        initial = self.begin()
        self.assertEqual(self.request('/api/libraries/export', {})['status'], 400)
        self.assertEqual(self.state(), initial)
        before = time.monotonic()
        opened = self.request('/api/libraries/open', {'path':str(other.path)})
        self.assertEqual(opened['status'], 200, opened)
        self.assertLess(time.monotonic()-before, 2)
        self.assertEqual(self.state()['status'], 'idle')
        self.assertFalse(Path(initial['path']).exists())
        self.assertEqual(list(Path(initial['path']).parent.glob('.illustash-export-*')), [])
        lock.rollback()
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.begin(); self.done()
        lock2 = sqlite3.connect(other.source); self.addCleanup(lock2.close); lock2.execute('BEGIN EXCLUSIVE')
        self.begin(); self.stop(self.process)
        self.assertEqual(list(Path(self.env['HOME']).joinpath('Downloads').glob('.illustash-export-*')), [])
        self.assertFalse(self.sentinel.exists())

    def test_export_error_atomic_existing_destination_and_retry(self):
        initial = self.begin(); dest = self.done(); before = dest.read_bytes()
        feature = self.lib.features / 'full-features.npy'; feature.mkdir()
        self.begin(); wait_until(lambda:self.state()['status']=='error', timeout=10)
        self.assertEqual(dest.read_bytes(), before)
        self.assertEqual(list(dest.parent.glob('.illustash-export-*')), [])
        feature.rmdir(); feature.write_bytes(b'retry')
        self.begin(); self.done()
        with zipfile.ZipFile(dest) as z: self.assertEqual(z.read('features/full-features.npy'), b'retry')

    def test_choose_invalid_kind_status_and_security(self):
        for body in ({}, {'kind':'bad'}, {'kind':[]}, {'kind':None}):
            self.assertEqual(self.request('/api/libraries/choose', body)['status'], self.request('/api/libraries/choose', body, base=self.direct)['status'])
        self.assertEqual(self.request('/api/libraries/choose', {'kind':'bad'}, token='bad')['status'],403)

    def test_parallel_export_one_start_and_failed_switch_keeps_job(self):
        lock = sqlite3.connect(self.lib.source); self.addCleanup(lock.close); lock.execute('BEGIN EXCLUSIVE')
        responses = []
        barrier = threading.Barrier(8)
        def start():
            barrier.wait(); responses.append(self.request('/api/libraries/export', {})['status'])
        threads = [threading.Thread(target=start) for _ in range(8)]
        for t in threads: t.start()
        for t in threads: t.join(5)
        self.assertEqual(sorted(responses), [200] + [400]*7)
        initial = self.state()
        self.assertEqual(self.request('/api/libraries/open', {'path':str(self.root / 'missing.pilib')})['status'], 400)
        self.assertEqual(self.state(), initial)
        lock.rollback(); self.done()

    def test_import_unique_names_config_bytes_registry_failure_and_existing_paths(self):
        archive = library.export_library(self.lib, self.root / 'synthetic.zip')
        legacy = library.import_library(archive, self.root / 'legacy-imports')
        library.HOME.mkdir(parents=True, exist_ok=True)
        taken = library.HOME / (self.lib.name + '.pilib'); taken.write_bytes(b'keep-existing')
        response = self.request('/api/libraries/import', {'path':str(archive)})
        self.assertEqual(response['status'], 200, response)
        path = Path(json.loads(response['body'])['path'])
        self.assertEqual(path.name, self.lib.name + ' 2.pilib')
        self.assertEqual((path / 'library.json').read_bytes(), (legacy.path / 'library.json').read_bytes())
        self.assertEqual(taken.read_bytes(), b'keep-existing')
        tmp = self.support / 'libraries.tmp'; tmp.mkdir()
        response = self.request('/api/libraries/import', {'path':str(archive)})
        self.assertEqual(response['status'], 400)
        published = library.HOME / (self.lib.name + ' 3.pilib')
        self.assertEqual((published / 'library.json').read_bytes(), (legacy.path / 'library.json').read_bytes())
        self.assertEqual(snapshot(published / 'library.sqlite'), snapshot(self.lib.db))
        self.assertEqual(json.loads(self.request('/api/libraries')['body'])['current'], str(self.lib.path))

    def test_management_filesystem_failures_status_and_source_database_boundary(self):
        other, _ = self.package('other'); library.remember(other, front=False)
        tmp = self.support / 'libraries.tmp'; tmp.mkdir()
        body = {'path':str(other.path)}
        self.assertEqual(self.request('/api/libraries/forget',body)['status'], self.request('/api/libraries/forget',body,base=self.direct)['status'])
        source = library.folder_source(self.root / 'folder')
        self.lib.config['sources']=[source];self.lib.save()
        config_tmp = self.lib.path / 'library.json.tmp';config_tmp.mkdir()
        before = snapshot(self.lib.source)
        body={'source':'s1','preset':'x'}
        self.assertEqual(self.request('/api/libraries/preset',body)['status'], self.request('/api/libraries/preset',body,base=self.direct)['status'])
        self.assertEqual(snapshot(self.lib.source),before)

    def test_import_legacy_zip_compressions(self):
        exported = library.export_library(self.lib, self.root / 'exported.zip')
        with zipfile.ZipFile(exported) as z: files = {name:z.read(name) for name in z.namelist()}
        for compression in (zipfile.ZIP_STORED,zipfile.ZIP_DEFLATED,zipfile.ZIP_BZIP2,zipfile.ZIP_LZMA):
            with self.subTest(compression=compression):
                archive=self.root / f'compression-{compression}.zip'
                with zipfile.ZipFile(archive,'w',compression=compression) as z:
                    for name,data in files.items(): z.writestr(name,data)
                expected=library.import_library(archive,self.root / 'legacy-compression')
                path=library.unique_path(library.HOME,self.lib.name)
                result=self.request('/api/libraries/import',{'path':str(archive)})
                self.assertEqual(result['status'],200,result)
                self.assertEqual(result['body'],json.dumps({'path':str(path),'name':self.lib.name},ensure_ascii=False).encode())
                self.assertEqual((path / 'library.json').read_bytes(),(expected.path / 'library.json').read_bytes())
                for name in ('source.sqlite','library.sqlite'): self.assertEqual(snapshot(path / name),snapshot(expected.path / name))

                if compression == zipfile.ZIP_LZMA:
                    corrupted = bytearray(archive.read_bytes())
                    central = corrupted.rfind(b'PK\x01\x02')
                    corrupted[central+16] ^= 1
                    bad = self.root / 'lzma-crc.zip'; bad.write_bytes(corrupted)
                    before = set(library.HOME.iterdir())
                    body = {'path':str(bad)}
                    self.assertEqual(self.request('/api/libraries/import',body)['status'],self.request('/api/libraries/import',body,base=self.direct)['status'])
                    self.assertEqual(set(library.HOME.iterdir()),before)
