"""Native library-create acceptance using temporary packages and the old fresh-store baseline.

Python constructs reference inputs and expected data only. The explicit native process has no
Python PATH, never scans the folder, and must not update the recent-library registry. Schema and
all typed table rows including rowids are compared. The old baseline receives the same two
CCIP birth UUIDs through the existing new_ids protocol; only the named runtime timestamp fields
below are mapped. Requires an explicitly configured factory release.
"""
from contextlib import closing
from pathlib import Path
import sqlite3

from tests.synthetic_source import synthetic_reference
from web import library, terms_characters



import datetime
import hashlib
import json
import os
import subprocess
import tempfile
import time
import unittest
import uuid
from unittest.mock import patch

from web.server import open_store
from web.store import Store


def tree(root):
    return {p.relative_to(root).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in root.rglob('*') if p.is_file()}


class NativeLibraryCreateTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        configured = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not configured:
            raise unittest.SkipTest('set explicit factory release; absence is not migration acceptance')
        cls.binary = Path(configured).resolve()
        if not cls.binary.is_file():
            raise unittest.SkipTest('configured factory release does not exist')

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-native-create-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.started = time.time() - 2
        self.home, self.shared, self.support = [self.root / p for p in ('home', 'shared', 'support')]
        self.shared.mkdir()
        self.reference = synthetic_reference(self.shared)
        self.folder = self.root / 'downloads-Pixiv'; self.folder.mkdir()
        # A source file must not be scanned, decoded, indexed or uploaded by the factory.
        (self.folder / 'not-scanned.png').write_bytes(b'not-a-real-image')
        for name, value in (('APP_SUPPORT', self.support), ('SHARED', self.shared),
                            ('HOME', self.home / 'Pictures' / 'Illustash'), ('REFERENCE', self.reference)):
            scoped = patch.object(library, name, value)
            scoped.start(); self.addCleanup(scoped.stop)
        bin_dir = self.root / 'bin'; bin_dir.mkdir()
        self.sentinel = self.root / 'python-was-launched'
        for name in ('python', 'python3', 'python3.12'):
            executable = bin_dir / name
            executable.write_text('#!/bin/sh\n/bin/echo forbidden > "$ILLUSTASH_PYTHON_SENTINEL"\nexit 97\n')
            executable.chmod(0o755)
        self.env = {**os.environ, 'PATH': str(bin_dir), 'HOME': str(self.home), 'USERPROFILE': str(self.home),
                    'USERPROFILE': str(self.home), 'ILLUSTASH_APP_SUPPORT': str(self.support),
                    'ILLUSTASH_SHARED': str(self.shared), 'ILLUSTASH_PYTHON_SENTINEL': str(self.sentinel),
                    'ILLUSTASH_TERMS_BIN': str(self.binary)}

    def command(self, dest, *, reference=None, folder=None, name=None, preset=None):
        args = [str(self.binary), 'library-create', '--dest', str(dest), '--folder', str(folder or self.folder),
                '--reference', str(reference or self.reference)]
        if name is not None:
            args += ['--name', name]
        if preset is not None:
            args += ['--preset', preset]
        return subprocess.run(args, env=self.env, capture_output=True, timeout=20)

    def timestamp(self, value, *, utc=False, iso=False):
        self.assertIsInstance(value, str)
        if iso:
            parsed = datetime.datetime.fromisoformat(value)
            self.assertIsNotNone(parsed.tzinfo)
            self.assertEqual(value, parsed.isoformat(timespec='seconds'))
            self.assertEqual(parsed.utcoffset(), datetime.datetime.now().astimezone().utcoffset())
        else:
            parsed = datetime.datetime.strptime(value, '%Y-%m-%d %H:%M:%S')
            self.assertEqual(parsed.strftime('%Y-%m-%d %H:%M:%S'), value)
            if utc:
                parsed = parsed.replace(tzinfo=datetime.timezone.utc)
        self.assertGreaterEqual(parsed.timestamp(), self.started)
        self.assertLessEqual(parsed.timestamp(), time.time() + 2)
        return '<generated-time>'

    def state(self, path, *, application=False):
        """Full schema and rowids/typed cells; only explicitly listed runtime clocks are mapped."""
        with closing(sqlite3.connect(path)) as c:
            self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0], 'ok')
            schema = c.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name').fetchall()
            ids = {}
            if application:
                linked = c.execute("SELECT l.key,t.key FROM term_links l JOIN terms t ON t.id=l.term_id "
                                   "WHERE l.system='ccip' AND l.current=1 AND t.kind='character' ORDER BY l.key").fetchall()
                self.assertEqual([r[0] for r in linked], ['alice', 'bob'])
                for key, ident in linked:
                    self.assertEqual(str(uuid.UUID(ident)), ident.lower())
                    ids[ident] = '<generated-character:' + key + '>'
                self.assertEqual(len(ids), 2)
            data = {}
            for kind, table, _, ddl in schema:
                if kind != 'table':
                    continue
                quoted = '"' + table.replace('"', '""') + '"'
                columns = [r[1] for r in c.execute('PRAGMA table_info(' + quoted + ')')]
                selection = '*'
                if 'WITHOUT ROWID' not in (ddl or '').upper():
                    columns.insert(0, '<rowid>')
                    selection = 'rowid,*'
                rows = []
                for raw in c.execute('SELECT ' + selection + ' FROM ' + quoted):
                    row = list(raw)
                    for i, column in enumerate(columns):
                        value = row[i]
                        if application and (table, column) in {
                                ('terms', 'created_at'), ('term_links', 'added_at'), ('links', 'added_at'),
                                ('term_evidence_sets', 'created_at')}:
                            row[i] = self.timestamp(value, utc=True)
                        if application and table == 'term_evidence_sets' and column == 'producer' and value.startswith('refresh:'):
                            self.timestamp(value[len('refresh:'):])
                            row[i] = 'refresh:<generated-time>'
                        if application and table == 'meta' and column == 'value' and raw[columns.index('key')] in {
                                'indexed', 'general_indexed', 'rating_indexed'}:
                            row[i] = self.timestamp(value)
                    rows.append(tuple((type(value).__name__, value) for value in row))
                data[table] = sorted(rows, key=repr)
            return schema, data

    def test_fresh_full_schema_data_names_defaults_without_python_or_scan(self):
        name = '合成 ✨'
        dest = self.root / 'native.pilib'
        reference_before, folder_before = tree(self.shared), tree(self.folder)
        result = self.command(dest, name=name, preset='other')
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        self.assertEqual(len(result.stdout.splitlines()), 1)
        self.assertEqual(json.loads(result.stdout), {'path': str(dest), 'name': name})
        with closing(sqlite3.connect(dest / 'library.sqlite')) as c:
            ids = dict(c.execute("SELECT l.key,t.key FROM term_links l JOIN terms t ON t.id=l.term_id "
                                 "WHERE l.system='ccip' AND l.current=1 AND t.kind='character'"))
        self.assertEqual(set(ids), {'alice', 'bob'})
        self.assertEqual(len(set(ids.values())), 2)
        expected = library.new_folder_library(self.folder, self.root / 'old.pilib', name, 'other')
        old_store = open_store(expected.source, expected.db)
        refresh = terms_characters.refresh
        # Align only the pre-existing new_ids input. Random UUID ordering otherwise changes even
        # old/old terms.id and its references; no output rows, rowids or other payload are normalized.
        def same_ids(store, src, c):
            return refresh(store, src, c, new_ids={'ccip': ids})
        with patch.object(terms_characters, 'refresh', side_effect=same_ids) as aligned:
            old_store.refresh()
        self.assertEqual(aligned.call_count, 1)
        config = json.loads((dest / 'library.json').read_text())
        self.assertEqual(str(uuid.UUID(config['id'])).upper(), config['id'])
        self.timestamp(config['created_at'], iso=True)
        self.assertEqual({k: v for k, v in config.items() if k not in {'id', 'created_at'}},
                         {k: v for k, v in expected.config.items() if k not in {'id', 'created_at'}})
        self.assertEqual(self.state(expected.source), self.state(dest / 'source.sqlite'))
        self.assertEqual(self.state(expected.db, application=True), self.state(dest / 'library.sqlite', application=True))
        native_store = Store.open(dest / 'source.sqlite', dest / 'library.sqlite')
        self.assertEqual(native_store.startup(), old_store.startup())
        self.assertEqual(native_store.prefs(), old_store.prefs())
        self.assertEqual(native_store.fold_kinds(), old_store.fold_kinds())
        for store in (old_store, native_store):
            self.assertTrue(store.ready())
            self.assertEqual(store.assets()['total'], 0)
            self.assertEqual([(v['name'], v['count'], v['unchecked']) for v in store.characters()],
                             [('Alice', 0, 0), ('Bob', 0, 0)])
        self.assertEqual(reference_before, tree(self.shared))
        self.assertEqual(folder_before, tree(self.folder))
        self.assertFalse((self.support / 'libraries.json').exists())
        self.assertFalse(self.sentinel.exists())
        for sub in ('features', 'thumbnails', 'backups'):
            self.assertTrue((dest / sub).is_dir())
            self.assertEqual(list((dest / sub).iterdir()), [])

    def test_default_name_auto_preset_and_three_explicit_presets(self):
        self.assertEqual(library.PRESETS, ('x', 'pixiv', 'other'))
        for index, preset in enumerate((None, 'x', 'pixiv', 'other')):
            with self.subTest(preset=preset):
                dest = self.root / f'different-dest-{index}.pilib'
                result = self.command(dest, preset=preset)
                self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
                config = json.loads((dest / 'library.json').read_text())
                self.assertEqual(config['name'], self.folder.name)
                # The native factory never guesses the origin from the folder name (#107): no preset is 「其他」.
                expected = library.folder_source(self.folder, preset or 'other')
                expected['preset_origin'] = 'user' if preset else 'auto'
                self.assertEqual(config['sources'], [expected])
        self.assertFalse(self.sentinel.exists())

    def test_invalid_reference_cleanup_and_inputs_never_overwrite(self):
        missing = self.root / 'missing.sqlite'
        corrupt = self.root / 'corrupt.sqlite'; corrupt.write_bytes(b'not sqlite')
        bad_schema = self.root / 'bad-schema.sqlite'
        with closing(sqlite3.connect(self.reference)) as source, closing(sqlite3.connect(bad_schema)) as target:
            source.backup(target)
            target.execute('DROP TABLE characters'); target.commit()
        cases = [({'reference': missing}, 'missing.pilib'), ({'reference': corrupt}, 'corrupt.pilib'),
                 ({'reference': bad_schema}, 'bad-schema.pilib'), ({'preset': 'invalid'}, 'bad-preset.pilib'),
                 ({'folder': self.root / 'no-folder'}, 'bad-folder.pilib'), ({}, 'wrong-extension.folder')]
        for kwargs, filename in cases:
            with self.subTest(case=filename):
                dest = self.root / filename
                before = tree(self.root)
                result = self.command(dest, **kwargs)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(dest.exists())
                self.assertEqual(before, tree(self.root))
        existing = self.root / 'existing.pilib'; existing.mkdir()
        (existing / 'keep.txt').write_text('existing package must survive')
        before = tree(self.root)
        self.assertNotEqual(self.command(existing).returncode, 0)
        self.assertEqual(before, tree(self.root))
        self.assertFalse((self.support / 'libraries.json').exists())
        self.assertFalse(self.sentinel.exists())

    def test_incomplete_explicit_reference_rejected_before_publication(self):
        for table, column in [('assets', 'hidden'), ('assets', 'stable_hash'), ('assets', 'orig_size'),
                              ('embeddings', None), ('embeddings', 'model'), ('labels', 'conf'),
                              ('labels', 'source'), ('labels', 'model_ver'), ('labels', 'run_id'),
                              ('decisions', 'note'), ('characters', 'wd_tag'), ('character_aliases', 'alias')]:
            with self.subTest(table=table, column=column):
                reference = self.root / ('bad-' + table + '-' + str(column) + '.sqlite')
                with closing(sqlite3.connect(self.reference)) as src, closing(sqlite3.connect(reference)) as dst:
                    src.backup(dst)
                    if column:
                        columns = [row[1] for row in dst.execute(f'PRAGMA table_info({table})') if row[1] != column]
                        dst.execute(f'CREATE TABLE incomplete AS SELECT {",".join(columns)} FROM {table}')
                        dst.execute(f'DROP TABLE {table}')
                        dst.execute(f'ALTER TABLE incomplete RENAME TO {table}')
                    else:
                        dst.execute(f'DROP TABLE {table}')
                    dst.commit()
                before = reference.read_bytes()
                dest = self.root / ('rejected-' + table + '-' + str(column) + '.pilib')
                self.assertNotEqual(self.command(dest, reference=reference).returncode, 0)
                self.assertFalse(dest.exists())
                self.assertEqual(reference.read_bytes(), before)
