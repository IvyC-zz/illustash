"""Fixed expectations recorded from the former Python reference (#9 stage 3).

The native black-box tests used to run the historical Python application next to the native host
and compare both byte for byte. They now compare the native host with what that reference
produced, recorded once while the reference still existed:

- ``ILLUSTASH_REFERENCE=record`` runs the reference as before and saves, per test, every value it
  contributed (responses, library states, seed packages, values read from its library);
- the default replays those values. No Python reference code is imported in that mode.

Values are recorded in call order and must be requested in the same order with the same label.
Run-specific strings (session tokens, loopback origins, temporary roots) are registered with
``alias`` and stored as placeholders, so a replay substitutes its own. Library states are kept
as per-table digests of schema, storage types, values and physical rowids (views by their rows). Recordings live in ``tests/fixtures/reference/<module>.tar.xz`` and
cannot be re-recorded once the reference is gone. Clock-like text (the core stamps some rows and
responses with the current time) is compared as ``<clock>``.
"""
import atexit
import base64
import hashlib
import io
import json
import os
import re
import sqlite3
import tarfile
from contextlib import closing
from functools import lru_cache
from pathlib import Path

RECORD = os.environ.get('ILLUSTASH_REFERENCE') == 'record'
FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'reference'
_recorded = {}   # module -> {'tests': {test id: [entry]}, 'blobs': {sha: bytes}}
_current = None  # the running test's recorder: its run-specific strings are placeholders in digests
CLOCK = re.compile(r'\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})?')
CLOCK_BYTES = re.compile(CLOCK.pattern.encode())


def unclocked(value):
    """Clock-like text masked, in str or bytes."""
    if isinstance(value, str):
        if _current is not None:
            value = _current._to_placeholders(value)
        return CLOCK.sub('<clock>', value)
    if isinstance(value, bytes):
        return CLOCK_BYTES.sub(b'<clock>', value)
    return value


def quote(name):
    return '"' + name.replace('"', '""') + '"'


def table_digests(path):
    """Schema, user_version, every table (typed cells and rowids) and view (rows), each as one digest."""
    with closing(sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True)) as c:
        c.execute('BEGIN')
        schema = [tuple(r) for r in c.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name')]
        out = {'schema': _hash(schema), 'user_version': _hash(c.execute('PRAGMA user_version').fetchone()[0])}
        for kind, name, _, sql in schema:
            if kind not in ('table', 'view'):
                continue
            columns = '*' if kind == 'view' or 'WITHOUT ROWID' in (sql or '').upper() else 'rowid,*'
            cursor = c.execute('SELECT ' + columns + ' FROM ' + quote(name))
            # Text cells as compared: run-specific strings (stored temporary paths) as placeholders.
            out[name] = _hash(([d[0] for d in cursor.description],
                               sorted((tuple((type(v).__name__, unclocked(v)) for v in row) for row in cursor), key=repr)))
        return out


def _hash(value):
    return hashlib.sha256(repr(value).encode()).hexdigest()[:20]


def library_digests(lib):
    return {name: table_digests(lib.path / filename) for name, filename in
            (('app', 'library.sqlite'), ('source', 'source.sqlite'), ('provenance', 'provenance.sqlite'))}


class Library:
    """The parts of a library package the tests use: its directory and database paths."""
    def __init__(self, path):
        self.path = Path(path)
        self.db, self.source = self.path / 'library.sqlite', self.path / 'source.sqlite'
        self.thumbnails, self.backups, self.features = (self.path / sub for sub in ('thumbnails', 'backups', 'features'))
        for sub in ('features', 'thumbnails', 'backups'):
            (self.path / sub).mkdir(exist_ok=True)

    @property
    def config(self):
        """library.json as first read (then kept and changed in place, like the application's own object)."""
        if not hasattr(self, '_config'):
            self._config = json.loads((self.path / 'library.json').read_text(encoding='utf-8'))
        return self._config

    name = property(lambda self: self.config['name'])
    frozen = property(lambda self: bool(self.config.get('test') or self.config.get('frozen')))

    def save(self, config=None):
        if config is not None:
            self._config = config
        (self.path / 'library.json').write_text(json.dumps(self.config, ensure_ascii=False, indent=2) + '\n',
                                                encoding='utf-8', newline='\n')


class Recorder:
    def __init__(self, test):
        """Values of one test, or of a class's ``setUpClass`` when given the class."""
        global _current
        _current = self
        kind = test if isinstance(test, type) else type(test)
        self.module = kind.__module__.split('.')[-1]
        self.test = f'{kind.__module__}.{kind.__qualname__}.setUpClass' if isinstance(test, type) else test.id()
        self.aliases = []      # (actual, placeholder)
        self.position = 0
        if RECORD:
            store = _recorded.setdefault(self.module, {'tests': {}, 'blobs': {}})
            self.entries = store['tests'][self.test] = []
            self.blobs = store['blobs']
        else:
            data = _load(self.module)
            self.entries = data['tests'].get(self.test, [])   # a test may use no reference value
            self.blobs = data['blobs']

    def alias(self, actual, placeholder):
        """Register a run-specific string; the latest registration of a placeholder wins."""
        self.aliases = [(a, p) for a, p in self.aliases if p != placeholder] + [(str(actual), placeholder)]

    def _to_placeholders(self, text):
        for actual, placeholder in sorted(self.aliases, key=lambda x: -len(x[0])):
            text = text.replace(actual, placeholder)
        return text

    def _from_placeholders(self, text):
        for actual, placeholder in self.aliases:
            text = text.replace(placeholder, actual)
        return text

    def _encode(self, value):
        if isinstance(value, bytes):
            return {'$bytes': base64.b64encode(self._to_placeholders(value.decode('latin-1')).encode('latin-1')).decode()}
        if isinstance(value, str):
            return self._to_placeholders(value)
        if isinstance(value, tuple):
            return {'$tuple': [self._encode(v) for v in value]}
        if isinstance(value, list):
            return [self._encode(v) for v in value]
        if isinstance(value, dict):
            if any(not isinstance(k, str) for k in value):
                return {'$items': [[self._encode(k), self._encode(v)] for k, v in value.items()]}
            return {k: self._encode(v) for k, v in value.items()}
        return value

    def _decode(self, value):
        if isinstance(value, str):
            return self._from_placeholders(value)
        if isinstance(value, list):
            return [self._decode(v) for v in value]
        if isinstance(value, dict):
            if set(value) == {'$bytes'}:
                return self._from_placeholders(base64.b64decode(value['$bytes']).decode('latin-1')).encode('latin-1')
            if set(value) == {'$tuple'}:
                return tuple(self._decode(v) for v in value['$tuple'])
            if set(value) == {'$items'}:
                return {self._decode(k): self._decode(v) for k, v in value['$items']}
            return {k: self._decode(v) for k, v in value.items()}
        return value

    def _next(self, label):
        if self.position >= len(self.entries):
            raise AssertionError(f'{self.test}: no recorded value #{self.position} ({label})')
        entry = self.entries[self.position]
        if entry['label'] != label:
            raise AssertionError(f'{self.test}: recorded value #{self.position} is {entry["label"]!r}, test asks {label!r}')
        self.position += 1
        return entry

    def value(self, label, compute):
        """A value the reference produced: computed and saved when recording, read back otherwise."""
        if RECORD:
            value = compute()
            self.entries.append({'label': label, 'value': self._encode(value)})
            return value
        return self._decode(self._next(label)['value'])

    def state(self, label, lib_or_compute):
        """A reference library state, kept as digests; compares equal to a live ``library_digests``."""
        if RECORD:
            digests = library_digests(lib_or_compute) if hasattr(lib_or_compute, 'path') else lib_or_compute()
            self.entries.append({'label': label, 'value': digests})
            return Digests(digests)
        return Digests(self._next(label)['value'])

    def package(self, label, path, build=None, library=True, only=None):
        """Files under ``path`` (databases as consistent bytes, text with placeholders, anything
        else raw): saved after ``build`` when recording, restored into ``path`` otherwise. Returns a
        ``Library`` unless ``library`` is false. ``only`` limits it to the named relative paths."""
        path = Path(path)
        if RECORD:
            if build is not None:
                build()
            files = {}
            for f in sorted(path.rglob('*')):
                if only is not None and f.relative_to(path).as_posix() not in only:
                    continue
                if f.is_file() and not f.name.endswith(('-wal', '-shm', '-journal')) and 'thumbnails' not in f.parts:
                    data, kind = _file_bytes(f), 'raw'
                    if f.suffix in ('.sqlite', '.db'):
                        try:
                            data, kind = _sqlite_bytes(f), 'sqlite'
                        except sqlite3.DatabaseError:
                            pass
                    else:
                        try:
                            data, kind = self._to_placeholders(data.decode('utf-8')).encode(), 'text'
                        except UnicodeDecodeError:
                            pass
                    key = hashlib.sha256(data).hexdigest()
                    self.blobs[key] = data
                    files[f.relative_to(path).as_posix()] = key if kind != 'raw' else ['raw', key]
            # Databases keep the recording's run-specific strings (temporary roots in stored paths);
            # a replay rebases them on its own.
            self.entries.append({'label': label, 'value': files, 'package': True,
                                 'aliases': {placeholder: actual for actual, placeholder in self.aliases}})
            return Library(path) if library else path
        entry = self._next(label)
        files = entry['value']
        current = {placeholder: actual for actual, placeholder in self.aliases}
        rebase = [(old, current[p]) for p, old in entry.get('aliases', {}).items() if p in current and current[p] != old]
        path.mkdir(parents=True, exist_ok=True)
        for name, key in files.items():
            target = path / name
            target.parent.mkdir(parents=True, exist_ok=True)
            for suffix in ('-wal', '-shm', '-journal'):
                Path(str(target) + suffix).unlink(missing_ok=True)
            raw = isinstance(key, list)
            data = self.blobs[key[1] if raw else key]
            if not raw and not name.endswith(('.sqlite', '.db')):
                data = self._from_placeholders(data.decode('utf-8')).encode()
            target.write_bytes(data)
            if not raw and name.endswith(('.sqlite', '.db')) and rebase:
                _rebase(target, rebase)
        return Library(path) if library else path

    def finish(self, test):
        """Every recorded value must have been used."""
        if not RECORD and self.position != len(self.entries):
            test.fail(f'{self.test}: {len(self.entries) - self.position} recorded reference values not reached')


class Digests:
    """Per-table digests of a reference library state; equal to a live state with the same digests."""
    def __init__(self, tree):
        self.tree = tree

    def __getitem__(self, key):
        value = self.tree[key]
        return Digests(value) if isinstance(value, dict) else value

    def __eq__(self, other):
        if isinstance(other, Digests):
            return self.tree == other.tree
        if isinstance(other, dict):
            return self.tree == other
        return NotImplemented

    def __repr__(self):
        return f'Digests({self.tree!r})'


def _rebase(path, pairs):
    """Replace recorded run-specific strings in every text cell of the ordinary tables, rowids kept.
    Triggers are set aside meanwhile (they guard writes, they are not data)."""
    with closing(sqlite3.connect(path)) as c:
        tables = [(name, sql) for name, sql in c.execute("SELECT name,sql FROM sqlite_schema WHERE type='table'")
                  if sql and not name.startswith('sqlite_') and not sql.upper().startswith('CREATE VIRTUAL')]
        virtual = [name for (name,) in c.execute("SELECT name FROM sqlite_schema WHERE type='table' AND upper(sql) LIKE 'CREATE VIRTUAL%'")]
        updates = []
        for name, _ in tables:
            if any(name.startswith(v + '_') for v in virtual):
                continue
            for (column,) in c.execute('SELECT name FROM pragma_table_info(?)', (name,)).fetchall():
                for old, new in pairs:
                    if c.execute(f'SELECT 1 FROM {quote(name)} WHERE typeof({quote(column)})=\'text\' '
                                 f'AND instr({quote(column)},?) LIMIT 1', (old,)).fetchone():
                        updates.append((name, column, old, new))
        if not updates:
            return
        triggers = c.execute("SELECT name,sql FROM sqlite_schema WHERE type='trigger' ORDER BY rowid").fetchall()
        c.execute('BEGIN')
        for name, _ in triggers:
            c.execute(f'DROP TRIGGER {quote(name)}')
        for name, column, old, new in updates:
            c.execute(f'UPDATE {quote(name)} SET {quote(column)}=replace({quote(column)},?,?) '
                      f'WHERE typeof({quote(column)})=\'text\' AND instr({quote(column)},?)', (old, new, old))
        for _, sql in triggers:
            c.execute(sql)
        c.commit()


def _file_bytes(path):
    return Path(path).read_bytes()


def _sqlite_bytes(path):
    """A consistent single-file copy (WAL folded in), byte for byte including rowids."""
    with closing(sqlite3.connect(path)) as src, closing(sqlite3.connect(':memory:')) as mem:
        src.backup(mem)
        return mem.serialize()


@lru_cache(maxsize=None)
def _load(module):
    path = FIXTURES / f'{module}.tar.xz'
    if not path.exists():
        raise AssertionError(f'no recorded reference fixture {path.name}')
    data = {'tests': {}, 'blobs': {}}
    with tarfile.open(path, 'r:xz') as tar:
        for member in tar.getmembers():
            content = tar.extractfile(member).read()
            if member.name == 'expectations.json':
                data['tests'] = json.loads(content)
            elif member.name.startswith('blobs/'):
                data['blobs'][member.name[6:]] = content
    return data


def _save():
    for module, data in _recorded.items():
        tests = {k: v for k, v in data['tests'].items() if v}
        if not tests:
            continue
        FIXTURES.mkdir(parents=True, exist_ok=True)
        path = FIXTURES / f'{module}.tar.xz'
        if path.exists():
            # A partial re-recording keeps the other tests' values.
            old = _load(module)
            tests = {**old['tests'], **tests}
            data['blobs'] = {**old['blobs'], **data['blobs']}
        used = {key[1] if isinstance(key, list) else key for entries in tests.values() for e in entries
                if e.get('package') for key in e['value'].values()}
        with tarfile.open(path, 'w:xz', preset=9) as tar:
            def add(name, content):
                info = tarfile.TarInfo(name)
                info.size, info.mtime, info.mode = len(content), 0, 0o644
                tar.addfile(info, io.BytesIO(content))
            add('expectations.json', json.dumps(tests, ensure_ascii=False).encode())  # key order is part of recorded values
            for key in sorted(used):
                add('blobs/' + key, data['blobs'][key])


if RECORD:
    atexit.register(_save)
