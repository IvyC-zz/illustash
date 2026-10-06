"""Explicit terms-only startup upgrade on genuinely produced historical synthetic libraries.

Set ILLUSTASH_TERMS_BIN to the release under test. The v5/v6 libraries in
tests/fixtures/library_upgrade were produced once by the historical producers
5e7e7f7 / d9c49b4, whose commits predate the public repository; each test starts
from a byte-identical copy. To regenerate them, set ILLUSTASH_TERMS_V5_BIN /
ILLUSTASH_TERMS_V6_BIN to those producers and run
`python -m tests.test_native_library_upgrade --write-fixtures`. Tests never build,
find a default library, or change an existing golden; the native host has a
Python-sentinel-only PATH.
"""
from contextlib import closing
import gzip
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from unittest.mock import patch

from PIL import Image

from tests import test_native_lifecycle as lifecycle
from tests.synthetic_source import synthetic_reference
from tests.test_read_http import INTERRUPT, SPAWN, free_port, wait_until
from tools.build_db import SCHEMA as SOURCE_SCHEMA
from tools.verify_read_http import difference, fetch
from web import effects, library, terms_core, terms_route
from web.server import Server
from web.store import Store

# Actual unused hybrid tables from d9c49b4:web/identity.py:45-49. The current Python
# constructor no longer installs them; preserve their historical definitions as v7 input.
OLD_IDENTITY = '''
CREATE TABLE IF NOT EXISTS identity_claims(uuid TEXT PRIMARY KEY,character_id TEXT,variant_id TEXT,
 status TEXT NOT NULL CHECK(status IN ('assigned','undetermined')),options_json TEXT NOT NULL,
 d1 REAL,d2 REAL,reason TEXT NOT NULL,bank_version TEXT NOT NULL,scope_json TEXT NOT NULL DEFAULT '[]');
CREATE TABLE IF NOT EXISTS identity_calibration(id TEXT PRIMARY KEY,payload TEXT NOT NULL,
 created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
INSERT INTO meta VALUES ('identity_calibration_active','synthetic-old'),('identity_bank_error','synthetic-old');
'''

# tools/build_db.py:SCHEMA is identical at both historical producer commits. Pin
# it so a future source-schema change cannot silently rewrite the old input.
HISTORICAL_SOURCE_SHA256 = '8b2c4aae7adb8e413bbe4ec309cfd523e8ae3f9bcacc6c4c6e91269aaee36c05'

FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'library_upgrade'
PRODUCERS = {5: '5e7e7f7e585f1cf8a17a8703e900a0f8fe99edef', 6: 'd9c49b42eed12c5cc810215be4acf70688617679'}
FOLDER = '<test folder>'


def tree(root):
    state = {}
    for p in root.rglob('*'):
        name = p.relative_to(root).as_posix()
        if p.is_symlink():
            state['link:' + name] = str(p.readlink())
        elif p.is_file():
            state['file:' + name] = hashlib.sha256(p.read_bytes()).hexdigest()
        elif p.is_dir():
            state['dir:' + name] = None
    return state


def database(path):
    """Exact DDL and every typed cell/rowid; cloned inputs need no UUID or clock mapping."""
    with closing(sqlite3.connect(path)) as c:
        schema = c.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name').fetchall()
        rows = {}
        for kind, name, _, sql in schema:
            if kind != 'table':
                continue
            quoted = '"' + name.replace('"', '""') + '"'
            selection = '*' if 'WITHOUT ROWID' in (sql or '').upper() else 'rowid,*'
            rows[name] = sorted([tuple((type(v).__name__, v) for v in r)
                                 for r in c.execute('SELECT ' + selection + ' FROM ' + quoted)], key=repr)
        return schema, rows


class NativeLibraryUpgradeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls, producers=False):
        cls.binaries = {}
        for version, variable in ((7, 'ILLUSTASH_TERMS_BIN'), (5, 'ILLUSTASH_TERMS_V5_BIN'),
                                  (6, 'ILLUSTASH_TERMS_V6_BIN'))[:3 if producers else 1]:
            configured = os.environ.get(variable)
            if not configured or not Path(configured).is_file():
                raise unittest.SkipTest(f'set {variable} explicitly; absence is not upgrade acceptance')
            cls.binaries[version] = Path(configured).resolve()
        cls.resources = Path(os.environ.get('ILLUSTASH_NATIVE_ROOT') or cls.binaries[7].parents[4]).resolve()

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-native-upgrade-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.shared, self.support, self.home = [self.root / name for name in ('shared', 'support', 'home')]
        self.shared.mkdir()
        self.reference = synthetic_reference(self.shared)
        self.assertEqual(hashlib.sha256(SOURCE_SCHEMA.encode()).hexdigest(), HISTORICAL_SOURCE_SHA256)
        # The shared read fixture omits scan columns. Historical folder libraries
        # cloned build_db's full source schema, including nullable stable_hash.
        with closing(sqlite3.connect(self.reference)) as c:
            for table in ('assets', 'labels', 'decisions', 'runs'):
                c.execute('DROP TABLE ' + table)
            c.executescript(SOURCE_SCHEMA)
            c.commit()
        self.folder = self.root / 'folder'; self.folder.mkdir()
        for name, value in (('HOME', self.home / 'Pictures' / 'Illustash'), ('APP_SUPPORT', self.support),
                            ('SHARED', self.shared), ('REFERENCE', self.reference)):
            scoped = patch.object(library, name, value)
            scoped.start(); self.addCleanup(scoped.stop)
        self.sentinel = self.root / 'python-was-launched'
        bin_dir = self.root / 'bin'; bin_dir.mkdir()
        for name in ('python', 'python3', 'python3.12'):
            path = bin_dir / name
            path.write_text('#!/bin/sh\n/bin/echo forbidden > "$ILLUSTASH_PYTHON_SENTINEL"\nexit 97\n')
            path.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(bin_dir), 'HOME': str(self.home), 'USERPROFILE': str(self.home),
                    'ILLUSTASH_APP_SUPPORT': str(self.support), 'ILLUSTASH_SHARED': str(self.shared),
                    'ILLUSTASH_PYTHON_SENTINEL': str(self.sentinel)}

    def package(self, version, name):
        """A fresh copy of the historical v5/v6 library, its source pointed at this test's folder."""
        fixture = FIXTURES / f'v{version}'
        produced = json.loads((fixture / 'fixture.json').read_text(encoding='utf-8'))
        self.assertEqual(produced['producer'], PRODUCERS[version])
        path = self.root / (name + '.pilib')
        for sub in ('', 'features', 'thumbnails', 'backups'):
            (path / sub).mkdir()
        for database_name in ('library.sqlite', 'source.sqlite'):
            (path / database_name).write_bytes(gzip.decompress((fixture / (database_name + '.gz')).read_bytes()))
        config = json.loads((fixture / 'library.json').read_text(encoding='utf-8'))
        self.assertEqual(config['sources'][0]['path'], FOLDER)
        config['name'], config['sources'][0]['path'] = name, str(self.folder)
        library.write_config(path, config)
        return library.Library(path, config), produced['action']

    def produce(self, version, name):
        """Run the historical producer; only --write-fixtures calls this."""
        producer = self.binaries[version]
        # Old producers predate resident serve/wd-pending; the unchanged one-shot protocol
        # and asset-free reference perform a complete successful initial refresh.
        with patch.object(terms_core, 'binary', return_value=producer), \
                patch.dict(os.environ, {'ILLUSTASH_TERMS_DAEMON': '0'}):
            lib = library.new_folder_library(self.folder, self.root / (name + '.pilib'), name, 'other')
            store = Store(lib.source, lib.db)
            store.refresh()
            # v5 refresh predates merged finalize_labels; its historical Python startup
            # finishes with the independent rederive command, not manually patched meta.
            store.refresh_general_hits()
            self.assertTrue(store.ready() and store.general_ready() and store.ratings_ready())
            with closing(sqlite3.connect(lib.db)) as c:
                alice = c.execute("SELECT entity_id FROM links WHERE system='ccip' AND key='alice'").fetchone()[0]
            command = terms_route.set_profile(store, alice, name='升级前人工名称', aliases=['人工别名'])
            command.update(label='升级前人工资料', name_lang='zh')  # Required old v5 request fields.
            action = store._core(command)['action']
        with closing(sqlite3.connect(lib.db)) as c:
            self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0], str(version))
            for marker in ('terms_artists', 'terms_labels', 'terms_characters', 'terms_names'):
                self.assertEqual(c.execute('SELECT value FROM meta WHERE key=?', (marker,)).fetchone()[0], 'core')
            ddl = c.execute("SELECT sql FROM sqlite_schema WHERE name='candidates'").fetchone()[0]
            self.assertEqual('WITHOUT ROWID' in ddl.upper(), version == 6)
            c.executescript(OLD_IDENTITY)
            c.commit()
            c.execute('PRAGMA journal_mode=DELETE')
        with closing(sqlite3.connect(lib.source)) as c:
            self.assertIn('stable_hash', {r[1] for r in c.execute('PRAGMA table_info(assets)')})
            c.execute('PRAGMA journal_mode=DELETE')
        self.assertFalse(self.sentinel.exists())
        return lib, action

    def args(self, lib, *, upgrade=True, port=None, sync=False):
        # Migration/history/undo comparisons exclude independent startup tasks.
        # Default automatic sync is exercised separately with genuine source DDL.
        return [str(self.binaries[7]), 'http', '--no-names', *([] if sync else ['--no-sync']), '--library', str(lib.path),
                '--port', str(port or free_port()), '--root', str(self.resources), *(['--upgrade'] if upgrade else [])]

    def failed(self, lib, *, upgrade=True, port=None):
        result = subprocess.run(self.args(lib, upgrade=upgrade, port=port), env=self.env,
                                capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse(self.sentinel.exists())
        return result

    def stop(self, process):
        if process.poll() is None:
            process.send_signal(INTERRUPT)
            try:
                process.wait(5)
            except subprocess.TimeoutExpired:
                process.kill(); process.wait(3)

    def start(self, lib, *, upgrade=True, sync=False):
        port = free_port(); origin = f'http://127.0.0.1:{port}'
        log = tempfile.TemporaryFile(); self.addCleanup(log.close)
        process = subprocess.Popen(self.args(lib, upgrade=upgrade, port=port, sync=sync), env=self.env, stdout=log, stderr=log, **SPAWN)
        self.addCleanup(self.stop, process)
        try:
            wait_until(lambda: fetch(origin, {'url': '/api/bootstrap'}, origin)['status'] == 200, timeout=10)
        except AssertionError:
            log.seek(0)
            self.fail(f'upgrade startup failed ({process.poll()}): {log.read().decode(errors="replace")}')
        self.assertFalse(self.sentinel.exists())
        return process, origin

    def sync_finished(self, origin):
        result = None

        def terminal():
            nonlocal result
            response = fetch(origin, {'url': '/api/sync'}, origin)
            self.assertEqual(response['status'], 200, response['body'])
            result = json.loads(response['body'])
            return bool(result['history']) and result['history'][0]['status'] != 'running' \
                and result['state']['status'] not in ('idle', 'scanning', 'recognising', 'importing')

        wait_until(terminal, timeout=10)
        return result

    def backups(self, lib, before):
        return [p for p in lib.path.rglob('*.sqlite') if p.relative_to(lib.path).as_posix() not in before]

    def reads(self, lib, origin):
        store = Store.open(lib.source, lib.db)
        server = Server(('127.0.0.1', 0), store, lib.thumbnails, public_origin=origin)
        server.library = lib
        server.forced_off = {'index_check', 'names', 'provenance', 'similar', 'sync'}
        server.token = json.loads(fetch(origin, {'url': '/api/bootstrap'}, origin)['body'])['token']
        thread = threading.Thread(target=server.serve_forever, daemon=True); thread.start()
        try:
            reference = f'http://127.0.0.1:{server.server_port}'
            for url in lifecycle.CORE_READS:
                with self.subTest(url=url):
                    left, right = fetch(reference, {'url': url}, origin), fetch(origin, {'url': url}, origin)
                    self.assertIsNone(difference(left, right), (url, difference(left, right)))
                    self.assertEqual(right['route'], 'core')
                    for header in lifecycle.HEADERS:
                        self.assertEqual(left['headers'].get(header), right['headers'].get(header), (url, header))
            before = database(lib.db)
            for token, status in ((server.token, 400), ('not-current', 403)):
                out = fetch(origin, {'url': '/api/startup', 'method': 'POST', 'body': b'{}',
                                     'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': token}}, origin)
                self.assertEqual(out['status'], status)
            self.assertEqual(database(lib.db), before)
        finally:
            server.shutdown(); thread.join(); server.server_close()

    def test_true_v5_v6_upgrade_matches_existing_migrate_history_undo_and_27_reads(self):
        for version in (5, 6):
            with self.subTest(version=version):
                lib, action = self.package(version, f'v{version}')
                before, source = database(lib.db), lib.source.read_bytes()
                existing = {p.relative_to(lib.path).as_posix() for p in lib.path.rglob('*.sqlite')}
                baseline = self.root / f'baseline-v{version}.pilib'; shutil.copytree(lib.path, baseline)
                expected = subprocess.run([str(self.binaries[7]), 'migrate', '--db', str(baseline / 'library.sqlite')],
                                          env=self.env, capture_output=True, timeout=20)
                self.assertEqual(expected.returncode, 0, expected.stderr)
                # Compare equivalent startup consumers: the bare migrate CLI leaves verdict
                # events pending, while the native HTTP host performs no-learner dispatch.
                baseline_store = Store.open(baseline / 'source.sqlite', baseline / 'library.sqlite')
                server = Server(('127.0.0.1', 0), baseline_store, baseline / 'thumbnails')
                try:
                    effects.drain(baseline_store, server._after_operation)
                finally:
                    server.server_close()
                process, origin = self.start(lib)
                self.assertEqual(database(lib.db), database(baseline / 'library.sqlite'))
                upgraded = database(lib.db)
                self.assertEqual(lib.source.read_bytes(), source)
                backups = self.backups(lib, existing)
                self.assertEqual(len(backups), 1)
                self.assertEqual(database(backups[0]), before)
                self.reads(lib, origin)
                with closing(sqlite3.connect(lib.db)) as c:
                    self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0], '7')
                    self.assertEqual(c.execute("SELECT count(*) FROM sqlite_schema WHERE name IN ('identity_claims','identity_calibration')").fetchone()[0], 0)
                self.stop(process)
                results = []
                for db in (lib.db, baseline / 'library.sqlite'):
                    out = subprocess.run([str(self.binaries[7]), 'undo', '--db', str(db), '--action', str(action)],
                                         env=self.env, capture_output=True, timeout=20)
                    self.assertEqual(out.returncode, 0, out.stderr)
                    results.append(out.stdout)
                self.assertEqual(*results)
                self.assertEqual(database(lib.db), database(baseline / 'library.sqlite'))
                process, origin = self.start(lib, upgrade=False)
                self.reads(lib, origin)
                self.stop(process)
                # Restore only a fresh, quiescent test-owned clone after all HTTP/SQLite
                # connections above have closed. Never run an old producer against current7.
                restored = self.root / f'restored-v{version}.pilib'
                self.assertTrue(restored.is_relative_to(self.root))
                shutil.copytree(lib.path, restored)
                restored_db = restored / 'library.sqlite'
                for suffix in ('-wal', '-shm'):
                    Path(str(restored_db) + suffix).unlink(missing_ok=True)
                shutil.copyfile(backups[0], restored_db)
                self.assertEqual(database(restored_db), before)
                with closing(sqlite3.connect(restored_db)) as c:
                    self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0], str(version))
                    self.assertEqual(c.execute('SELECT undone FROM actions WHERE id=?', (action,)).fetchone()[0], 0)
                recovered = library.Library(restored, json.loads((restored / 'library.json').read_text()))
                process, origin = self.start(recovered)
                self.assertEqual(database(recovered.db), upgraded)
                self.assertEqual(recovered.source.read_bytes(), source)
                self.reads(recovered, origin)
                self.stop(process)

    def test_plain_startup_rejects_true_old_schemas_without_changes(self):
        for version in (5, 6):
            with self.subTest(version=version):
                lib, _ = self.package(version, f'plain-v{version}')
                before = tree(self.root)
                self.failed(lib, upgrade=False)
                self.assertEqual(tree(self.root), before)

    def test_historical_source_default_auto_sync_records_each_start_and_imports_new_file(self):
        for version in (5, 6):
            with self.subTest(version=version):
                lib, action = self.package(version, f'auto-v{version}')
                before, source = database(lib.db), database(lib.source)
                existing = {p.relative_to(lib.path).as_posix() for p in lib.path.rglob('*.sqlite')}
                process, origin = self.start(lib, sync=True)
                state = self.sync_finished(origin)
                self.assertEqual(state['state']['status'], 'done', state)
                self.assertEqual(len(state['history']), 1)
                run = state['history'][0]
                self.assertEqual(run['status'], 'done')
                self.assertEqual(run['message'], '')
                self.assertEqual(tuple(run[k] for k in ('found', 'removed', 'imported', 'failed')), (0, 0, 0, 0))
                self.assertEqual(database(lib.source), source)  # No implicit source-schema repair.
                backups = self.backups(lib, existing)
                self.assertEqual(len(backups), 1)
                self.assertEqual(database(backups[0]), before)
                self.stop(process)

                # An ordinary restart creates another sync run even with --upgrade
                # on current7; migrating is a no-op, starting sync is not.
                process, origin = self.start(lib, sync=True)
                state = self.sync_finished(origin)
                self.assertEqual(state['state']['status'], 'done', state)
                self.assertEqual([r['status'] for r in state['history']], ['done', 'done'])
                self.assertEqual(database(lib.source), source)
                self.assertEqual(self.backups(lib, existing), backups)
                self.stop(process)

                # Exercise real folder writes, not just the asset-free SELECT path.
                image = self.folder / f'synthetic-v{version}.png'
                Image.new('RGB', (4, 3), 'red').save(image)
                process, origin = self.start(lib, upgrade=False, sync=True)
                state = self.sync_finished(origin)
                self.assertEqual(state['state']['status'], 'needs_recognition', state)
                self.assertEqual([r['status'] for r in state['history']], ['needs_recognition', 'done', 'done'])
                run = state['history'][0]
                self.assertEqual(tuple(run[k] for k in ('found', 'removed', 'imported', 'failed')), (1, 0, 1, 0))
                self.assertIn('本次启动没有识别 worker', run['message'])
                with closing(sqlite3.connect(lib.source)) as c:
                    self.assertEqual(c.execute('SELECT stable_hash,width,height,src_path FROM assets').fetchall(),
                                     [(hashlib.md5(image.read_bytes()).hexdigest(), 4, 3, str(image))])
                with closing(sqlite3.connect(lib.db)) as c:
                    self.assertEqual(c.execute('SELECT count(*) FROM assets').fetchone()[0], 1)
                    self.assertEqual(c.execute("SELECT seq FROM sqlite_sequence WHERE name='sync_runs'").fetchone()[0], 3)
                    self.assertEqual(c.execute('SELECT undone FROM actions WHERE id=?', (action,)).fetchone()[0], 0)
                self.assertFalse(self.sentinel.exists())
                self.stop(process)
                image.unlink()

    def test_reduced_source_missing_hash_is_sync_error_after_successful_upgrade(self):
        # This deliberately incomplete read-only fixture is not a historical
        # producer schema. Reproduce the original root cause independently of
        # the migration equivalence check instead of hiding it with --no-sync.
        for version in (5, 6):
            with self.subTest(version=version):
                lib, _ = self.package(version, f'missing-hash-v{version}')
                with closing(sqlite3.connect(lib.source)) as c:
                    c.execute('ALTER TABLE assets DROP COLUMN stable_hash')
                    c.commit()
                before, source = database(lib.db), database(lib.source)
                existing = {p.relative_to(lib.path).as_posix() for p in lib.path.rglob('*.sqlite')}
                process, origin = self.start(lib, sync=True)
                state = self.sync_finished(origin)
                self.assertEqual(state['state']['status'], 'error', state)
                self.assertEqual(len(state['history']), 1)
                self.assertEqual(state['history'][0]['status'], 'error')
                for message in (state['state']['message'], state['history'][0]['message']):
                    self.assertIn('数据库错误：no such column: stable_hash', message)
                    self.assertIn('SELECT uuid,stable_hash FROM assets', message)
                with closing(sqlite3.connect(lib.db)) as c:
                    self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0], '7')
                self.assertEqual(database(lib.source), source)
                backups = self.backups(lib, existing)
                self.assertEqual(len(backups), 1)
                self.assertEqual(database(backups[0]), before)
                self.stop(process)

    def test_occupied_port_and_invalid_preflight_do_not_write_or_backup(self):
        lib, _ = self.package(5, 'preflight')
        before = tree(self.root)
        result = subprocess.run(self.args(lib) + ['--backup'], env=self.env, capture_output=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stderr.decode().strip(), '--upgrade 不可与 --backup 同用')
        self.assertEqual(tree(self.root), before)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0)); listener.listen()
            before = tree(self.root)
            self.failed(lib, port=listener.getsockname()[1])
            self.assertEqual(tree(self.root), before)
        for key, value in (('schema_version', '35'), ('terms_names', 'pending')):
            with self.subTest(key=key):
                with closing(sqlite3.connect(lib.db)) as c:
                    old = c.execute('SELECT value FROM meta WHERE key=?', (key,)).fetchone()[0]
                    c.execute('UPDATE meta SET value=? WHERE key=?', (value, key)); c.commit()
                before = tree(self.root)
                self.failed(lib)
                self.assertEqual(tree(self.root), before)
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute('UPDATE meta SET value=? WHERE key=?', (old, key)); c.commit()
        lib.source.rename(lib.path / 'saved-source.sqlite')
        before = tree(self.root)
        self.failed(lib)
        self.assertEqual(tree(self.root), before)

    def test_injected_migration_failure_rolls_back_and_retains_preupgrade_backup(self):
        lib, _ = self.package(5, 'rollback')
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute("CREATE TRIGGER fail_upgrade BEFORE INSERT ON meta WHEN NEW.key='terms_schema' AND NEW.value='7' "
                      "BEGIN SELECT RAISE(ABORT,'synthetic-upgrade-failure'); END")
            c.commit()
        before, source = database(lib.db), lib.source.read_bytes()
        existing = {p.relative_to(lib.path).as_posix() for p in lib.path.rglob('*.sqlite')}
        result = self.failed(lib)
        self.assertIn(b'synthetic-upgrade-failure', result.stderr)
        self.assertEqual(database(lib.db), before)
        self.assertEqual(lib.source.read_bytes(), source)
        backups = self.backups(lib, existing)
        self.assertEqual(len(backups), 1)
        self.assertEqual(database(backups[0]), before)
        saved = backups[0].read_bytes()
        with closing(sqlite3.connect(lib.db)) as c:
            c.execute('DROP TRIGGER fail_upgrade'); c.commit()
        process, origin = self.start(lib)
        self.assertEqual(backups[0].read_bytes(), saved)
        self.reads(lib, origin)
        self.stop(process)

    def test_committed_upgrade_registry_failure_recovers_with_plain_start_and_repeated_flag_is_noop(self):
        lib, _ = self.package(6, 'registration-recovery')
        before, source, config = database(lib.db), lib.source.read_bytes(), (lib.path / 'library.json').read_bytes()
        existing = {p.relative_to(lib.path).as_posix() for p in lib.path.rglob('*.sqlite')}
        self.support.mkdir(parents=True, exist_ok=True)
        blocked = self.support / 'libraries.tmp'; blocked.mkdir()
        port = free_port()
        result = self.failed(lib, port=port)
        self.assertIn('升级已提交，服务注册失败'.encode(), result.stderr)
        self.assertIn('升级前备份'.encode(), result.stderr)
        backups = self.backups(lib, existing)
        self.assertEqual(len(backups), 1)
        self.assertIn(str(backups[0]).encode(), result.stderr)
        self.assertEqual(database(backups[0]), before)
        self.assertEqual(lib.source.read_bytes(), source)
        self.assertEqual((lib.path / 'library.json').read_bytes(), config)
        with closing(sqlite3.connect(lib.db)) as c:
            self.assertEqual(c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone()[0], '7')
        # Registration follows the committed migration; this failure is never reported as rollback.
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', port))
        blocked.rmdir()
        process, origin = self.start(lib, upgrade=False)
        self.reads(lib, origin)
        self.stop(process)
        current = database(lib.db)
        backup_state = {p.relative_to(lib.path).as_posix(): p.read_bytes() for p in lib.path.rglob('*.sqlite')
                        if p not in (lib.source, lib.db)}
        process, origin = self.start(lib)  # Explicit --upgrade on current7 is an ordinary no-op.
        self.reads(lib, origin)
        self.stop(process)
        self.assertEqual(database(lib.db), current)
        self.assertEqual(lib.source.read_bytes(), source)
        self.assertEqual((lib.path / 'library.json').read_bytes(), config)
        self.assertEqual({p.relative_to(lib.path).as_posix(): p.read_bytes() for p in lib.path.rglob('*.sqlite')
                          if p not in (lib.source, lib.db)}, backup_state)
        self.assertFalse(self.sentinel.exists())


def write_fixtures():
    """Produce tests/fixtures/library_upgrade with the historical producers (see module docstring)."""
    NativeLibraryUpgradeTest.setUpClass(producers=True)
    case = NativeLibraryUpgradeTest('test_plain_startup_rejects_true_old_schemas_without_changes')
    case.setUp()
    try:
        for version in (5, 6):
            lib, action = case.produce(version, f'v{version}')
            leftovers = [p.name for p in lib.path.iterdir() if p.name.endswith(('-wal', '-shm', '-journal'))]
            assert not leftovers, leftovers
            fixture = FIXTURES / f'v{version}'
            fixture.mkdir(parents=True, exist_ok=True)
            for name in ('library.sqlite', 'source.sqlite'):
                (fixture / (name + '.gz')).write_bytes(gzip.compress((lib.path / name).read_bytes(), mtime=0))
            config = json.loads((lib.path / 'library.json').read_text(encoding='utf-8'))
            assert config['sources'][0]['path'] == str(case.folder)
            config['sources'][0]['path'] = FOLDER
            (fixture / 'library.json').write_text(json.dumps(config, ensure_ascii=False, indent=2) + '\n',
                                                  encoding='utf-8', newline='\n')
            (fixture / 'fixture.json').write_text(json.dumps(
                {'producer': PRODUCERS[version], 'action': action,
                 'generator': 'python -m tests.test_native_library_upgrade --write-fixtures'}, indent=2) + '\n',
                encoding='utf-8', newline='\n')
            print(f'v{version}: action {action} -> {fixture}')
    finally:
        case.doCleanups()


if __name__ == '__main__':
    import sys
    if sys.argv[1:] == ['--write-fixtures']:
        write_fixtures()
    else:
        unittest.main()
