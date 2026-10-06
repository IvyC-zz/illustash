"""仅供合成测试及对照工具；不属于生产应用宿主。"""
"""The Rust terms core (native/terms, docs/TERMS_SCHEMA.md) as seen from the Python store.

Both write the same `actions` history in library.sqlite, on their own connections. A command that writes (undo,
judge, import, cutover) runs the core's command line, and the caller must have closed its own connection first, or
the two would wait on each other's write lock. The core re-checks inside its own transaction whatever the caller saw
(an undo only undoes the step that is still the latest), so a step added in between is never skipped.

Reads go straight to the core's tables in library.sqlite (term_projection, term_links, ...): they are plain SQLite.
"""
import atexit
import json
import queue
import threading
import os
from pathlib import Path
import sqlite3
import subprocess

ROOT = Path(__file__).resolve().parents[3]
NAME = 'illustash-terms.exe' if os.name == 'nt' else 'illustash-terms'
ARTISTS_IN_CORE = 'terms_artists'   # meta key the core sets to 'core' when it takes over artists (legacy_artists.rs)
LABELS_IN_CORE = 'terms_labels'     # ... user tags, ratings and WD topics
CHARACTERS_IN_CORE = 'terms_characters'   # ... characters, skins and works (legacy_characters.rs) (legacy_labels.rs)
NAMES_IN_CORE = 'terms_names'
TAG_STEP = {'uuid', 'tag', 'had'}   # one item of an old user-tag step (Store.tag)


class Failed(Exception):
    """The core refused (history changed, schema mismatch) or is not available; the message is for people."""


_read_only = {}


def refuse_writes(db, reason):
    """This library opened read-only (Store.refuse_writes): the core's write commands refuse it with `reason`."""
    _read_only[str(Path(db).resolve())] = reason


def allow_writes(db):
    """A new start (a new Store for this library) tries the switch again: the refusal of the last one is forgotten."""
    _read_only.pop(str(Path(db).resolve()), None)


def _writable(db):
    reason = _read_only.get(str(Path(db).resolve()))
    if reason:
        raise Failed(reason)


class NeedId(Exception):
    """The command needs a new id from the caller (exit code 5), at this JSON pointer; nothing was written."""
    def __init__(self, pointer):
        super().__init__(pointer)
        self.pointer = pointer


def fill_id(command, pointer, value):
    """Put `value` at `pointer` (a JSON Pointer) in the command, where the caller left `false` (or, in an `ids` object
    the caller gave, nothing) for the core to ask for."""
    *path, last = [p.replace('~1', '/').replace('~0', '~') for p in pointer[1:].split('/')]
    slot = command
    for part in path:
        slot = slot[int(part)] if isinstance(slot, list) else slot[part]
    if not isinstance(slot, dict) or slot.get(last, False) is not False:
        raise RuntimeError(f'词条核心要的 id 不在命令里留的位置：{pointer}')
    slot[last] = value


def owns(before, artists=False, labels=False):
    """Same rule as `owns` in crates/core/src/store.rs: a non-empty list whose every item is exactly {"terms": ...};
    once artists live in the core, an old artist judgement ({"artist_verdict": ...}) counts too; once labels do, an
    old user-tag item ({"uuid", "tag", "had"}) too."""
    def mine(b):
        if not isinstance(b, dict):
            return False
        keys = set(b)
        return keys == {'terms'} or (artists and keys == {'artist_verdict'}) or (labels and keys == TAG_STEP)
    return isinstance(before, list) and bool(before) and all(mine(b) for b in before)


def _flag(c, key):
    try:
        row = c.execute('SELECT value FROM meta WHERE key=?', (key,)).fetchone()
    except sqlite3.OperationalError:
        return False
    return bool(row) and row[0] == 'core'


def artists_in_core(c):
    """On an open connection to library.sqlite."""
    return _flag(c, ARTISTS_IN_CORE)


def labels_in_core(c):
    """User tags, ratings and WD topics are the core's; general_hits, asset_rating and asset_tags are views."""
    return _flag(c, LABELS_IN_CORE)


def characters_in_core(c):
    """Characters, skins and works are the core's: their tables are write-protected, every write is a core command."""
    return _flag(c, CHARACTERS_IN_CORE)


def refuse_if_characters_in_core(library, tool):
    """Offline tools that write the character tables, or a copy they then write through the old code, stop before any
    side effect (backup, directory, source write) once characters are the core's: their writes would be refused half
    way. Read-only tools keep working."""
    path = Path(library)
    if not path.is_file():
        return
    c = sqlite3.connect(path.resolve().as_uri() + '?mode=ro', uri=True)
    try:
        moved = characters_in_core(c)
    finally:
        c.close()
    if moved:
        raise SystemExit(f'{tool}：角色已切到词条核心，这个离线工具会直接写角色表，已停止，没有做任何改动')


def cutover_characters(db):
    return run('cutover-characters', '--db', db, timeout=1800)


def names_in_core(c):
    """Names are the core's (docs/TERMS_NAMES_PLAN.md): names, display_names and name_aliases are write-protected, every
    write is a core command, and display reads come from the core's projection."""
    return _flag(c, NAMES_IN_CORE)


def cutover_names(db):
    return run('cutover-names', '--db', db, timeout=1800)


def names_command(db, command):
    """One name command or a batch (crates/core/src/names_commands.rs), one core transaction."""
    _writable(db)
    return run('names-command', '--db', db, stdin=json.dumps(command, ensure_ascii=False, allow_nan=False), timeout=600)


def names_search(db, query):
    """names.search_hits by the core (crates/core/src/names_search.rs): {identity: rank}."""
    return run('names-search', '--db', db, stdin=json.dumps({'query': query}, ensure_ascii=False))['hits']


def binary():
    """ILLUSTASH_TERMS_BIN, else the newest cargo build in this checkout (release or debug); None when there is none."""
    configured = os.environ.get('ILLUSTASH_TERMS_BIN')
    if configured:
        return Path(configured) if Path(configured).is_file() else None
    built = [p for p in (ROOT / 'native' / 'terms' / 'target' / profile / NAME for profile in ('release', 'debug')) if p.is_file()]
    return max(built, key=lambda p: p.stat().st_mtime) if built else None


def run(*args, stdin=None, timeout=120):
    """One core command; its JSON answer. Failed for refusals people should see, RuntimeError for crashes."""
    exe = binary()
    if exe is None:
        raise Failed('找不到词条核心程序')
    try:
        done = subprocess.run([str(exe), *map(str, args)], input=stdin, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise Failed(f'词条核心没有完成：{exc}') from exc
    return _answer(done.returncode, done.stdout, done.stderr)


def _answer(code, stdout, stderr):
    if code in (2, 3, 4):
        raise Failed(stderr.strip() or '词条核心拒绝了这次操作')
    if code == 5:
        raise NeedId(stderr.strip())
    if code != 0:
        raise RuntimeError(f'词条核心失败（退出码 {code}）：{stderr.strip()}')
    return json.loads(stdout)


class _TransportFailed(Failed):
    """通道或进程失效；业务拒绝不需要重启子进程。"""


class _Service:
    """一个 Python 进程拥有一个私有子进程，队列只有一条在途命令。管道读写也受超时覆盖。"""
    def __init__(self, exe):
        self.process = subprocess.Popen([str(exe), 'serve'], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                        text=True, encoding='utf-8')
        self.jobs = queue.Queue()
        self.thread = threading.Thread(target=self._transport, daemon=True)
        self.thread.start()

    def _transport(self):
        while True:
            job = self.jobs.get()
            if job is None:
                return
            payload, reply = job
            line = None
            try:
                self.process.stdin.write(payload + '\n')
                self.process.stdin.flush()
                line = self.process.stdout.readline()
                if not line:
                    raise OSError('常驻核心进程已退出；这条命令没有自动重放')
                reply.put((json.loads(line), None))
            except (OSError, ValueError) as exc:
                reply.put((None, exc))
            finally:
                # 队列空闲时不持有上条命令的大型输入、输出与应答队列。
                del job, payload, reply, line

    def close(self):
        if self.process.poll() is None:
            self.process.kill()
        self.process.wait()
        self.jobs.put(None)
        self.thread.join(timeout=5)
        self.process.stdin.close()
        self.process.stdout.close()

    def request(self, db, command, timeout):
        reply = queue.Queue(maxsize=1)
        self.jobs.put((json.dumps({'db': str(db), 'input': command}, ensure_ascii=False, allow_nan=False), reply))
        try:
            response, error = reply.get(timeout=timeout)
        except queue.Empty as exc:
            raise _TransportFailed('常驻核心超时；提交状态不确定，未自动重放，请检查历史') from exc
        if error:
            raise _TransportFailed(f'常驻核心没有完成：{error}；提交状态请检查历史') from error
        return _answer(response['code'], json.dumps(response.get('result')), response.get('error', ''))


_service_lock = threading.RLock()
_service = None
_service_key = None


def close_service():
    global _service, _service_key
    with _service_lock:
        if _service is not None and _service_key[0] == os.getpid():
            _service.close()
        _service = _service_key = None


atexit.register(close_service)


def _resident(db, command, timeout=600):
    global _service, _service_key
    exe = binary()
    if exe is None:
        raise Failed('找不到词条核心程序')
    stat = exe.stat()
    key = (os.getpid(), str(exe.resolve()), stat.st_mtime_ns, stat.st_size)
    with _service_lock:
        if _service_key != key or _service is None or _service.process.poll() is not None:
            close_service()
            try:
                _service = _Service(exe)
            except OSError as exc:
                raise Failed(f'无法启动常驻核心：{exc}') from exc
            _service_key = key
        try:
            return _service.request(db, command, timeout)
        except _TransportFailed:
            close_service()
            raise


def undo(db, action_id):
    """Undo `action_id`, which must be the latest step and one the core owns."""
    _writable(db)
    if binary() is None:
        raise Failed('这一步是词条核心记录的，但找不到词条核心程序，暂时不能撤销')
    return run('undo', '--db', db, '--action', int(action_id), timeout=60)


def judge(db, term, verdict, label, uuids):
    """是 / 不是 / 清除 on these pictures, as one undoable step. verdict: 'yes' | 'no' | None."""
    _writable(db)
    return run('judge', '--db', db, '--term', term, '--verdict', verdict or 'clear', '--label', label,
               stdin=json.dumps(list(uuids)))


def cutover_artists(db, provenance=None):
    """Hand artists to the core (last sync from the old tables, then the switch, in one transaction)."""
    args = ['cutover-artists', '--db', db]
    if provenance and Path(provenance).is_file():
        args += ['--provenance', provenance]
    return run(*args, timeout=600)


def import_package(db, library_id, package, new_assets=False):
    """new_assets: this library's pictures not in `assets` yet may be registered (WD evidence ahead of a sync's insert)."""
    _writable(db)
    return run('import', '--db', db, '--library-id', library_id, '--package', package, *(['--new-assets'] if new_assets else []),
               timeout=600)


def tag(db, name, add, label, uuids):
    """Put a user tag on these pictures (or take it off), as one undoable step."""
    _writable(db)
    return run('tag', '--db', db, '--name', name, '--verdict', 'yes' if add else 'clear', '--label', label,
               stdin=json.dumps(list(uuids)))


def cutover_labels(db):
    return run('cutover-labels', '--db', db, timeout=600)


class LearningPublisher:
    """The learner's batch goes to the core (web/learn.py learn()): used automatically once characters are there, and
    in the S3 double run. With the store's write lock, publishing waits for other core commands."""
    def __init__(self, db, lock=None):
        self.db, self.lock = db, lock

    def snapshot(self):
        return run('learning-snapshot', '--db', self.db)['token']

    def publish(self, batch):
        _writable(self.db)
        if self.lock is None:
            return run('publish-learning', '--db', self.db, stdin=json.dumps(batch, allow_nan=False), timeout=1800)
        with self.lock:
            return run('publish-learning', '--db', self.db, stdin=json.dumps(batch, allow_nan=False), timeout=1800)


CHARACTER_SHAPES = ({'uuid', 'entity', 'assertion', 'deferred'}, {'uuid', 'entity', 'assertion', 'deferred', 'identity'})
CHARACTER_ONE_KEY = ('merge', 'identity_relation', 'identity_anchor', 'profile')


def character_step(b):
    """One item of an old character step (store.rs `character_commands::shape`)."""
    if not isinstance(b, dict):
        return False
    keys = set(b)
    return keys in CHARACTER_SHAPES or (len(keys) == 1 and next(iter(keys)) in CHARACTER_ONE_KEY)


def character_owns(before):
    """Same rule as `character_commands::owns`: a core character step ([{"terms": {"v": 2, ...}}]), or an old step
    whose items are all character shapes. Mixed old steps count as the core's too: it refuses them."""
    if isinstance(before, list) and len(before) == 1 and isinstance(before[0], dict) and set(before[0]) == {'terms'} \
            and isinstance(before[0]['terms'], dict) and 'v' in before[0]['terms']:
        return True
    return isinstance(before, list) and any(character_step(b) for b in before)


def legacy_before(before):
    """The old-shaped snapshot list inside a core character step (names.action_label renders history from it)."""
    if isinstance(before, list) and len(before) == 1 and isinstance(before[0], dict) and set(before[0]) == {'terms'} \
            and isinstance(before[0]['terms'], dict) and before[0]['terms'].get('v') == 2:
        return before[0]['terms'].get('before', [])
    return before


def character_command(db, command):
    """One character business command (crates/core/src/character_commands.rs), one core transaction."""
    _writable(db)
    if os.environ.get('ILLUSTASH_TERMS_DAEMON', '1') != '0':
        return _resident(db, command)
    return run('character-command', '--db', db, stdin=json.dumps(command, ensure_ascii=False, allow_nan=False), timeout=600)
