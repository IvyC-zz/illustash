"""只读的历史固定期望，按顺序恢复值、摘要和合成包。

夹具无法录制或保存。临时路径、令牌、服务地址按登记占位符还原，时钟按 <clock> 比较。
其他类型、列、rowid、响应字节与固定录制保持精确一致。
"""
import base64
import gzip
import hashlib
import json
import lzma
import os
import re
import sqlite3
import struct
import subprocess
import sys
import tarfile
import unittest
from contextlib import closing
from functools import lru_cache
from pathlib import Path

WINDOWS = os.name == 'nt'
# A recorded path continues after its placeholder with /-separated segments.
_TAIL = r'((?:/[^/"\'\s,;:}\]\)\\]+)*)'
FIXTURES = Path(__file__).resolve().parent / 'fixtures' / 'reference_recordings'
_current = None  # the running test's recorder: its run-specific strings are placeholders in digests
CLOCK = re.compile(r'\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:\d{2})?')
CLOCK_BYTES = re.compile(CLOCK.pattern.encode())


def fixed_values(name):
    """A frozen value file under tests/fixtures. Values the old reference computed differently on
    Windows (float summation order, image codecs) were recorded there too and are kept in
    tests/fixtures/windows/ as replacements at the listed paths; they apply on Windows only."""
    opener = lzma.open if name.endswith('.xz') else gzip.open
    with opener(FIXTURES.parent / name, 'rt', encoding='utf-8') as f:
        values = json.load(f)
    overlay = FIXTURES.parent / 'windows' / name
    if WINDOWS and overlay.exists():
        with opener(overlay, 'rt', encoding='utf-8') as f:
            for path, value in json.load(f)['replace']:
                node = values
                for key in path[:-1]:
                    node = node.setdefault(key, {})
                node[path[-1]] = value
    return values


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
    return Live({name: table_digests(lib.path / filename) for name, filename in
                 (('app', 'library.sqlite'), ('source', 'source.sqlite'), ('provenance', 'provenance.sqlite'))},
                baseline_state(lib.path))


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


class Expectations:
    def __init__(self, test):
        """Values of one test, or of a class's ``setUpClass`` when given the class."""
        global _current
        _current = self
        kind = test if isinstance(test, type) else type(test)
        self.module = kind.__module__.split('.')[-1]
        # Keyed by the id under the tests package, however the tests were discovered.
        test_id = f'{kind.__module__}.{kind.__qualname__}.setUpClass' if isinstance(test, type) else test.id()
        self.test = 'tests.' + test_id.removeprefix('tests.')
        self.aliases = []      # (actual, placeholder)
        self.position = 0
        self.states = 0        # state() calls so far: the #201 baseline keys a state by its number
        try:
            data = _load(self.module)
        except AssertionError:
            if isinstance(test, type):
                raise
            data = {'tests': {}, 'blobs': {}}   # a module of platform-only subclasses only
        self.entries = data['tests'].get(self.test)
        if self.entries is None and not isinstance(test, type):
            # A platform-only subclass (e.g. the Windows OpenBLAS runs) replays its parent's recording.
            for parent in kind.__mro__[1:]:
                if not (isinstance(parent, type) and issubclass(parent, unittest.TestCase)) \
                        or parent is unittest.TestCase:
                    continue
                name = parent.__module__.split('.')[-1]
                key = f"tests.{parent.__module__.removeprefix('tests.')}.{parent.__qualname__}.{test._testMethodName}"
                try:
                    found = _load(name)
                except AssertionError:
                    continue
                if key in found['tests']:
                    self.module, self.test, data = name, key, found
                    self.entries = found['tests'][key]
                    break
        if self.entries is None:
            # Never recorded: a test that only runs on another platform than the recording's, or
            # one that uses no reference value. Its shared setup values come from the module's
            # first recorded test (the same synthetic packages and seeds).
            donors = [entries for _, entries in sorted(data['tests'].items())]
            self.entries, self.donor = [], donors[0] if donors else []
        self.blobs = data['blobs']

    def alias(self, actual, placeholder):
        """Register a run-specific string; the latest registration of a placeholder wins. A macOS
        temporary path is also known by its unresolved /var spelling."""
        actual = str(actual)
        pairs = [(actual, placeholder)]
        if actual.startswith('/private/'):
            pairs.append((actual[len('/private'):], placeholder[:-1] + '_SHORT@'))
        names = {p for _, p in pairs}
        self.aliases = [(a, p) for a, p in self.aliases if p not in names] + pairs

    def _ordered(self):
        return sorted(self.aliases, key=lambda x: -len(x[0]))

    def _to_placeholders(self, text):
        for actual, placeholder in self._ordered():
            if WINDOWS and _is_path(actual):
                # Recordings spell paths with /: a Windows path (also JSON-escaped) is normalized.
                escaped = json.dumps(actual, ensure_ascii=False)[1:-1]
                text = re.sub(re.escape(escaped) + r'((?:\\\\[^\\"]+)*)',
                              lambda m: placeholder + m.group(1).replace('\\\\', '/'), text)
                text = re.sub(re.escape(actual) + r'((?:\\[^\\"\s,;:}\]\)]+)*)',
                              lambda m: placeholder + m.group(1).replace('\\', '/'), text)
            text = text.replace(actual, placeholder)
        return text

    def _from_placeholders(self, text):
        """This run's strings back in place; on Windows a path takes this platform's separators,
        JSON-escaped when the text is a JSON document."""
        if not WINDOWS:
            for actual, placeholder in self.aliases:
                text = text.replace(placeholder, actual)
            return text
        as_json = _is_json(text)
        for actual, placeholder in sorted(self.aliases, key=lambda x: -len(x[1])):
            if placeholder not in text:
                continue
            if not _is_path(actual):
                text = text.replace(placeholder, actual)
                continue
            def path(m, actual=actual):
                value = actual + m.group(1).replace('/', '\\')
                return json.dumps(value, ensure_ascii=False)[1:-1] if as_json else value
            text = re.sub(re.escape(placeholder) + _TAIL, path, text)
        return text


    def _decode(self, value):
        if isinstance(value, str):
            return self._from_placeholders(value)
        if isinstance(value, list):
            return [self._decode(v) for v in value]
        if isinstance(value, dict):
            if set(value) == {'$bytes'}:
                return self._from_placeholders(base64.b64decode(value['$bytes']).decode('utf-8', 'surrogateescape')).encode('utf-8', 'surrogateescape')
            if set(value) == {'$tuple'}:
                return tuple(self._decode(v) for v in value['$tuple'])
            if set(value) == {'$items'}:
                return {self._decode(k): self._decode(v) for k, v in value['$items']}
            out = {k: self._decode(v) for k, v in value.items()}
            # A response whose body took this run's strings keeps a consistent length header.
            if isinstance(out.get('body'), bytes) and isinstance(out.get('headers'), dict) and 'Content-Length' in out['headers']:
                out['headers']['Content-Length'] = str(len(out['body']))
            return out
        return value

    def _next(self, label):
        donor = getattr(self, 'donor', None)
        if donor is not None and self.position < len(donor) and donor[self.position]['label'] == label:
            self.position += 1
            return donor[self.position - 1]
        if self.position >= len(self.entries):
            raise AssertionError(f'{self.test}: no recorded value #{self.position} ({label})')
        entry = self.entries[self.position]
        if entry['label'] != label:
            raise AssertionError(f'{self.test}: recorded value #{self.position} is {entry["label"]!r}, test asks {label!r}')
        self.position += 1
        return entry

    def value(self, label):
        """读取一个已冻结的历史返回值。"""
        return self._decode(self._next(label)['value'])

    def state(self, label):
        """A reference library state, kept as digests; compares equal to a live ``library_digests``.
        Compared with a live state, it also holds the #201 baseline recorded for this state (see ``Live``)."""
        value = self._next(label)['value']
        slot = (self.module, self.test, f'{self.states}:{label}')
        self.states += 1
        return Digests(value, slot)

    def baseline(self, label, package, rename=None):
        """The #201 baseline of a library package at a named point (fresh libraries the tests compare whole):
        recorded when recording, otherwise compared with what was recorded, if anything was. ``rename`` maps
        values made at random (generated keys) to stable names first."""
        return _check_baseline((self.module, self.test, f'@{label}'), baseline_state(Path(package), rename))

    def package(self, label, path, library=True):
        """恢复冻结的合成输入包，数据库文本中的临时路径按本轮根目录还原。"""
        path = Path(path)
        files = self._next(label)['value']
        # Run-specific strings stored in database text (temporary roots in paths) come back as this run's.
        rebase = [(p, a) for a, p in sorted(self.aliases, key=lambda x: -len(x[1]))]
        path.mkdir(parents=True, exist_ok=True)
        for name, key in files.items():
            if WINDOWS and any(c in name for c in '<>:"|?*'):
                continue   # a name this platform cannot hold; its tests are POSIX-only
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
                _rebase(target, rebase, self._from_placeholders)
            # Libraries (not their backups) are upgraded to this core version (#201).
            if Path(name).name in ('library.sqlite', 'app.sqlite') and not {'backups', 'library-backups'} & set(Path(name).parts):
                upgrade_recorded(target)
        return Library(path) if library else path

    def skip(self, label):
        """A recorded value this platform has no counterpart for (the test documents why)."""
        self._next(label)

    def finish(self, test):
        """Every recorded value must have been used."""
        if getattr(self, 'donor', None) is None and self.position != len(self.entries):
            test.fail(f'{self.test}: {len(self.entries) - self.position} recorded reference values not reached')


class Digests:
    """Per-table digests of a reference library state; equal to a live state with the same digests."""
    def __init__(self, tree, slot=None):
        self.tree = tree
        self.slot = slot

    def __getitem__(self, key):
        value = self.tree[key]
        return Digests(value) if isinstance(value, dict) else value

    def __eq__(self, other):
        if isinstance(other, Digests):
            return self.tree == other.tree
        if isinstance(other, Live):
            if RECORDING:
                # Recording (core version 7 only, PR #239): equal by the recordings above, the live state by the
                # #201 rules is that state's baseline.
                equal = self.tree == dict(other)
                return equal and (self.slot is None or _check_baseline(self.slot, other.baseline))
            if self.slot is None:
                return self.tree == dict(other)
            # Core v8 changed the schema and retired tables these digests cover: the #201 baseline is the reference.
            return _check_baseline(self.slot, other.baseline)
        if isinstance(other, dict):
            return self.tree == other
        return NotImplemented

    def __repr__(self):
        return f'Digests({self.tree!r})'


def _rebase(path, pairs, convert=None):
    """Replace recorded run-specific strings (``pairs`` of old, new) in every text cell of the
    ordinary tables with ``convert`` (plain replacement by default), rowids kept. Triggers are set
    aside meanwhile (they guard writes, they are not data)."""
    if convert is None:
        def convert(text):
            for old, new in pairs:
                text = text.replace(old, new)
            return text
    with closing(sqlite3.connect(path)) as c:
        c.create_function('convert_text', 1, lambda v: convert(v) if isinstance(v, str) else v, deterministic=True)
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
        for name, column, old in dict.fromkeys((n, col, o) for n, col, o, _ in updates):
            c.execute(f'UPDATE {quote(name)} SET {quote(column)}=convert_text({quote(column)}) '
                      f'WHERE typeof({quote(column)})=\'text\' AND instr({quote(column)},?)', (old,))
        for _, sql in triggers:
            c.execute(sql)
        c.commit()


def upgrade_recorded(db):
    """A recorded library at an older core version (7) is upgraded as the user's libraries are before a test uses it
    (#201), with the core's own command; the backup it makes is removed, so the directory is as it was."""
    with closing(sqlite3.connect(db)) as c:
        has_meta = c.execute("SELECT 1 FROM sqlite_schema WHERE type='table' AND name='meta'").fetchone()
        row = c.execute("SELECT value FROM meta WHERE key='terms_schema'").fetchone() if has_meta else None
    if row is None or not 0 < int(row[0]) < TERMS_VERSION:
        return
    binary = os.environ.get('ILLUSTASH_TERMS_BIN')
    if not binary:
        raise unittest.SkipTest('set ILLUSTASH_TERMS_BIN: recorded packages are upgraded to this core version first')
    backups = Path(db).parent / 'library-backups'
    existing = set(backups.iterdir()) if backups.exists() else None
    done = subprocess.run([binary, 'migrate', '--db', str(db)], capture_output=True)
    if done.returncode:
        raise AssertionError(f'upgrading {db}: {done.stderr.decode(errors="replace")}')
    for made in set(backups.iterdir()) - (existing or set()):
        made.unlink()
    if existing is None:
        backups.rmdir()


def _is_path(text):
    return '\\' in text or text.startswith('/')


def _is_json(text):
    stripped = text.strip()
    if not stripped[:1] in ('{', '['):
        return False
    try:
        json.loads(stripped)
        return True
    except ValueError:
        return False






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


# ---- The comparison baseline of the first public test release (#201) ----
#
# Core v8 drops the legacy tables, so the digests above (whole schema, every table, rowids) stop matching. Where a
# recorded state is compared with a live one, the live state is also taken by the rules of
# native/terms/tests/common/baseline.rs and compared with a baseline recorded from main on core version 7:
# every table except RETIRED, typed cells, rows as a multiset (implicit rowids not compared), run-specific strings
# and clocks as above, learning stamps masked, a few meta values by what they mean. Recorded with
# ILLUSTASH_RECORD_BASELINE=1 into fixtures/baseline_states/<module>.json.gz.

BASELINE_STATES = FIXTURES.parent / 'baseline_states'
RETIRED = frozenset({'entities', 'links', 'work_review', 'names', 'display_names', 'name_aliases', 'identity_baseline'})
# The last two were added by core v8 (#201): what the history shows from them is compared in the replies.
SKIP_COLUMNS = frozenset({('term_packages', 'manifest'), ('term_evidence', 'fingerprint'),
                          ('actions', 'label_kind'), ('actions', 'label_count')})
STAMP = re.compile(r'learning-v1:[0-9a-f]{64}')
TERMS_VERSION = int(re.search(r'pub const SCHEMA_VERSION: i64 = (\d+);',
                              (Path(__file__).resolve().parents[1] / 'crates/core/src/schema.rs').read_text()).group(1))
RECORDING = bool(os.environ.get('ILLUSTASH_RECORD_BASELINE'))


class Live(dict):
    """A live ``library_digests``: the digests, and the same state by the #201 rules (``baseline``)."""
    def __init__(self, digests, baseline):
        super().__init__(digests)
        self.baseline = baseline


def _meta_value(key, value, names_version):
    if key == 'terms_schema' and value == str(TERMS_VERSION):
        return '<terms-version>'
    if key == 'names_version':
        return '<names_version>'
    if key in ('names_projected', 'names_display_projected'):
        return '<names_version>' if value == names_version else '<stale>'
    if key == 'identity_graph_version':
        return '<derived>'
    return value


def _cell(value, rename):
    if isinstance(value, float):
        return {'real': struct.pack('>d', value).hex()}
    if isinstance(value, bytes):
        return {'blob': value.hex()}
    if isinstance(value, str):
        for old, new in rename.items():
            value = value.replace(old, new)
        return STAMP.sub('<stamp>', unclocked(value))
    return value


def _database_state(path, rename):
    with closing(sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True)) as c:
        c.execute('BEGIN')
        names = [n for (n,) in c.execute("SELECT name FROM sqlite_schema WHERE type='table' "
                                         "AND name NOT LIKE 'sqlite_%' ORDER BY name")]
        names_version = None
        if 'meta' in names:
            row = c.execute("SELECT value FROM meta WHERE key='names_version'").fetchone()
            names_version = row[0] if row else None
        out = {}
        for name in names:
            if name in RETIRED:
                continue
            columns = [r[1] for r in c.execute('PRAGMA table_info(' + quote(name) + ')')]
            keep = [i for i, col in enumerate(columns) if (name, col) not in SKIP_COLUMNS]
            rows = []
            for raw in c.execute('SELECT * FROM ' + quote(name)):
                cells = [_cell(raw[i], rename) for i in keep]
                if name == 'meta' and isinstance(raw[0], str) and isinstance(raw[1], str):
                    cells[1] = _cell(_meta_value(raw[0], raw[1], names_version), rename)
                rows.append(json.dumps(cells, ensure_ascii=False, separators=(',', ':')))
            out[name] = {'columns': [columns[i] for i in keep], 'rows': sorted(rows)}
        return out


def baseline_state(package, rename=None):
    package = Path(package)
    return {name: _database_state(package / filename, rename or {}) for name, filename in
            (('app', 'library.sqlite'), ('source', 'source.sqlite'), ('provenance', 'provenance.sqlite'))
            if (package / filename).exists()}


@lru_cache(maxsize=None)
def _baseline_file(module):
    path = BASELINE_STATES / f'{module}.json.gz'
    if not path.exists():
        return {}
    with gzip.open(path, 'rt', encoding='utf-8') as f:
        return json.load(f)


def _check_baseline(slot, state):
    """Record ``state`` for ``slot`` (module, test, key) when recording; otherwise True unless a baseline was
    recorded for it and differs (the difference is printed)."""
    module, test, key = slot
    if RECORDING:
        _record_baseline(module, test, key, state)
        return True
    want = _baseline_file(module).get(test, {}).get(key)
    if want is not None:
        want, state = _without_volatile(want), _without_volatile(state)
    if want == state:
        return True
    if want is None:
        print(f'#201 baseline {test} {key}: nothing recorded for this state', file=sys.stderr)
        return False
    for db in sorted(set(want) | set(state)):
        for table in sorted(set(want.get(db, {})) | set(state.get(db, {}))):
            w, g = want.get(db, {}).get(table), state.get(db, {}).get(table)
            if w != g:
                print(f'#201 baseline {test} {key} {db}.{table}: baseline {w!r:.400}, live {g!r:.400}', file=sys.stderr)
    return False


# Python side only: evidence packages the host builds from source labels carry the time they were made, and the package
# fingerprint hashes it (the manifest itself is left out on both sides). The core replays have no such packages.
VOLATILE = frozenset({('term_packages', 'fingerprint')})


def _without_volatile(state):
    out = {}
    for db, tables in state.items():
        out[db] = {}
        for name, table in tables.items():
            drop = [i for i, column in enumerate(table['columns']) if (name, column) in VOLATILE]
            if not drop:
                out[db][name] = table
                continue
            keep = [i for i in range(len(table['columns'])) if i not in drop]
            rows = []
            for row in table['rows']:
                cells = json.loads(row)
                rows.append(json.dumps([cells[i] for i in keep], ensure_ascii=False, separators=(',', ':')))
            out[db][name] = {'columns': [table['columns'][i] for i in keep], 'rows': sorted(rows)}
    return out


def _record_baseline(module, test, key, state):
    """Merged into the module's file under a lock: the fixed suite runs test classes in several processes."""
    import fcntl
    BASELINE_STATES.mkdir(exist_ok=True)
    path = BASELINE_STATES / f'{module}.json.gz'
    import tempfile
    with open(Path(tempfile.gettempdir()) / f'illustash-baseline-{module}.lock', 'w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        data = {}
        if path.exists():
            with gzip.open(path, 'rt', encoding='utf-8') as f:
                data = json.load(f)
        recorded = data.setdefault(test, {})
        if key in recorded and recorded[key] != state:
            raise AssertionError(f'#201 baseline {test} {key}: two different live states compared equal to it')
        recorded[key] = state
        with gzip.GzipFile(path, 'wb', mtime=0) as f:
            f.write(json.dumps(data, ensure_ascii=False, sort_keys=True, separators=(',', ':')).encode('utf-8'))
