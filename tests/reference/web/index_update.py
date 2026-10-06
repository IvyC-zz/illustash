"""定期检查外部角色索引：找出 Danbooru 上图够多、本地还没有的角色，用户挑选后补进索引。

检查只发文本查询（作品的 related_tag），每 CHECK_EVERY 天在启动时自动做一次，也可以在设置页手动做。
补角色要下载公开参考图（每个约 MB_PER_CHARACTER），所以只在用户选定并确认后做：
tools/char_patch.py apply 在子进程里备份 → 下载 → 清洗 → 重建锚点 → 重扫全库（写 pi.db labels 的 char_ext）；
之后比较重扫前后的认领，只把变了的图重新导入图库（Store.reindex_characters，与全量刷新等价）。
补进来的角色看过认领的图后觉得不行，可以放弃（char_patch.py drop --rebuild），以后检查也不再提它。

状态文件都在 review/char-patch/：watch.json（关注哪些作品）、check.json（最近一次检查）、history.json。
"""
import json
import os
import subprocess
import sys
import threading
import time
from collections import Counter
from pathlib import Path

from web.library import shared_dir

ROOT = Path(__file__).resolve().parents[3]
CHECK_EVERY = 7 * 86400
MB_PER_CHARACTER = 1.1          # 20 reference images at ~55 KB (720px webp)
DEFAULT_WORKS = ['blue_archive', 'arknights:_endfield', 'wuthering_waves', 'meitantei_precure!', 'genshin_impact',
                 'honkai:_star_rail', 'honkai_impact_3rd', 'zenless_zone_zero']
ACTIVE = ('checking', 'fetching', 'cleaning', 'building', 'scanning', 'importing')
PATCH_SOURCE = 'danbooru-patch'


def run_patch(args):
    """Run tools/char_patch.py in a child process; yield its @-progress messages."""
    proc = subprocess.Popen([sys.executable, '-u', str(ROOT / 'tools' / 'char_patch.py'), *args], cwd=ROOT,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, env={**os.environ, 'PYTHONUNBUFFERED': '1'})
    tail = []
    for line in proc.stdout:
        if line.startswith('@'):
            yield json.loads(line[1:])
        elif line.strip():
            tail = (tail + [line.strip()])[-6:]
    if proc.wait():
        raise RuntimeError('角色索引更新失败：' + ' / '.join(tail)[-300:])


def find_missing(works, min_posts, per_work):
    sys.path.insert(0, str(ROOT / 'tools')) if str(ROOT / 'tools') not in sys.path else None
    import char_patch
    return char_patch.candidates(works, min_posts, per_work)


class IndexUpdater:
    """One check or update at a time, in a background thread; `info()` is what the settings page shows."""
    def __init__(self, store, runner=run_patch, finder=find_missing, on_change=None, busy=None):
        self.store, self.runner, self.finder = store, runner, finder
        self.on_change = on_change   # e.g. let the learner reload the patched anchors and reclaim
        self.busy = busy             # e.g. a Photos sync is writing the same claims
        self.review = shared_dir(store.source)
        self.dir = self.review / 'char-patch'
        self.lock = threading.Lock()
        self.state = {'status': 'idle'}
        self.thread = None

    # ---------- files ----------
    def _read(self, name, default):
        try:
            return json.loads((self.dir / name).read_text(encoding='utf-8'))
        except (OSError, ValueError):
            return default

    def _write(self, name, value):
        self.dir.mkdir(parents=True, exist_ok=True)
        tmp = self.dir / (name + '.tmp')
        tmp.write_text(json.dumps(value, ensure_ascii=False, indent=1), encoding='utf-8')
        tmp.replace(self.dir / name)

    def watch(self):
        saved = self._read('watch.json', {'works': DEFAULT_WORKS, 'min_posts': 100})
        if saved.get('version') != 2:
            resolved, pending = [], []
            with self.store.connect() as c:
                for tag in saved.get('works', []):
                    row = c.execute("SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='danbooru' AND l.key=? AND e.kind='work'", (tag,)).fetchone()
                    (resolved if row else pending).append(row[0] if row else tag)
            # Preserve the original JSON before converting its identifiers.
            if (self.dir / 'watch.json').exists() and not (self.dir / 'watch-v1.json').exists():
                self._write('watch-v1.json', saved)
            saved = {'version': 2, 'works': list(dict.fromkeys(resolved)), 'pending_tags': pending, 'min_posts': saved.get('min_posts', 100)}
            self._write('watch.json', saved)
        if saved.get('pending_tags'):
            remaining = []
            with self.store.connect() as c:
                for tag in saved['pending_tags']:
                    row = c.execute("SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system='danbooru' AND l.key=? AND l.current=1 AND e.kind='work'", (tag,)).fetchone()
                    if row:
                        saved['works'] = list(dict.fromkeys([*saved['works'], row[0]]))
                    else:
                        remaining.append(tag)
            if remaining != saved['pending_tags']:
                saved['pending_tags'] = remaining
                self._write('watch.json', saved)
        return saved

    def set_watch(self, works, min_posts):
        if not isinstance(works, list) or len(works) > 40 or not all(isinstance(w, str) for w in works):
            raise ValueError('关注列表应为最多 40 个作品 ID')
        if not isinstance(min_posts, int) or not 20 <= min_posts <= 5000:
            raise ValueError('最少图片数应在 20–5000 之间')
        with self.store.connect() as c:
            for ident in works:
                if not c.execute("SELECT 1 FROM entities e JOIN links l ON l.entity_id=e.id WHERE e.id=? AND e.kind='work' AND l.system='danbooru' AND l.current=1", (ident,)).fetchone():
                    raise ValueError('只能关注已确认 Danbooru 链接的作品')
        saved = self.watch()
        watch = saved | {'works': list(dict.fromkeys(works)), 'min_posts': min_posts}
        self._write('watch.json', watch)
        return watch

    def progress(self):
        with self.lock:
            return {'state': dict(self.state)}

    def info(self):
        check = self._read('check.json', None)
        history = self._read('history.json', [])[:6]
        ids = {c['short_tag'] for h in history for c in h['characters']}
        found = self.store.index_entities(ids)
        for h in history:
            for c in h['characters']:
                c['entity'] = found.get(c['short_tag'])
        with self.lock:
            state = dict(self.state)
        return {'state': state, 'watch': self.watch(), 'check': check, 'history': history, 'mb_per_character': MB_PER_CHARACTER, 'works': self.store.works()}

    # ---------- jobs ----------
    def update(self, **fields):
        with self.lock:
            self.state.update(fields)

    def writing(self):
        """Rewriting the external-index claims (a Photos sync must not run meanwhile)."""
        return self.status_now() in ACTIVE[1:]

    def status_now(self):
        with self.lock:
            return self.state.get('status') if self.thread and self.thread.is_alive() else None

    def _start(self, status, target, *args):
        with self.lock:
            if self.thread and self.thread.is_alive():
                raise ValueError('角色索引正在更新，请稍后')
            if status != 'checking' and self.busy and self.busy():
                raise ValueError('正在与 Photos 同步，完成后再更新角色索引')
            self.state = {'status': status, 'started_at': time.strftime('%Y-%m-%d %H:%M:%S')}
            self.thread = threading.Thread(target=self._guard, args=(target, *args), name='index-update', daemon=True)
            self.thread.start()

    def _guard(self, target, *args):
        try:
            target(*args)
        except Exception as exc:
            self.update(status='error', message=f'{type(exc).__name__}: {exc}'[:300], finished_at=time.strftime('%Y-%m-%d %H:%M:%S'))

    def due(self):
        check = self._read('check.json', None)
        if not check or not check.get('checked_at'):
            return True
        return time.time() - time.mktime(time.strptime(check['checked_at'], '%Y-%m-%d %H:%M:%S')) >= CHECK_EVERY

    def maybe_check(self):
        """At startup: check again when the last check is a week old. Text queries only."""
        if self.due():
            try:
                self.start_check()
            except ValueError:
                pass

    def start_check(self):
        self._start('checking', self._check)

    def _check(self):
        watch, works = self.watch(), {}
        with self.store.connect() as c:
            tags = [r[0] for ident in watch['works'] for r in c.execute("SELECT key FROM links WHERE entity_id=? AND system='danbooru' AND current=1", (ident,))]
        missing = self.finder(tags, watch['min_posts'], lambda work, n: works.__setitem__(work, n))
        self._write('check.json', {'checked_at': time.strftime('%Y-%m-%d %H:%M:%S'), 'works': works,
                                   'min_posts': watch['min_posts'], 'missing': missing})
        self.update(status='checked', found=len(missing), finished_at=time.strftime('%Y-%m-%d %H:%M:%S'))

    def start_apply(self, tags):
        check = self._read('check.json', None) or {}
        offered = {r['short_tag']: r for r in check.get('missing', [])}
        if not isinstance(tags, list) or not tags or not all(t in offered for t in tags):
            raise ValueError('请从最近一次检查的结果里选择角色')
        rows = [offered[t] for t in dict.fromkeys(tags)]
        self._start('fetching', self._apply, rows)

    def start_drop(self, short_tag, reason):
        meta = self._read_meta(short_tag)
        if not meta or meta.get('source') != PATCH_SOURCE:
            raise ValueError('只能放弃从 Danbooru 补进来的角色')
        if not isinstance(reason, str) or not 0 < len(reason.strip()) <= 200:
            raise ValueError('请写一句放弃的原因')
        self._start('building', self._drop, meta, reason.strip())

    def _read_meta(self, short_tag):
        if not isinstance(short_tag, str) or '/' in short_tag or short_tag.startswith('.'):
            return None
        try:
            return json.loads((self.review / 'char-index' / (short_tag + '.json')).read_text(encoding='utf-8'))
        except (OSError, ValueError):
            return None

    def _claims(self):
        import sqlite3
        db = sqlite3.connect(f'file:{self.store.source}?mode=ro', uri=True, timeout=30)
        try:
            return {(u, v, round(c, 4)) for u, v, c in db.execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")}
        finally:
            db.close()

    def _run(self, args):
        steps = {'fetch': 'fetching', 'clean': 'cleaning', 'build': 'building', 'scan': 'scanning'}
        before = self._claims()
        for step in self.runner(args):
            if step['step'] in steps:
                self.update(status=steps[step['step']], **{k: step[k] for k in ('done', 'total') if k in step})
        after = self._claims()
        self.update(status='importing')
        changed = self.store.reindex_characters({u for u, *_ in before ^ after})
        claims = Counter(v for u, v in {(u, v) for u, v, _ in after})
        return changed, claims

    def _apply(self, rows):
        plan = self._read('plan.json', [])
        known = {r['short_tag'] for r in plan}
        self._write('plan.json', plan + [r for r in rows if r['short_tag'] not in known])
        changed, claims = self._run(['apply', '--only', ','.join(r['short_tag'] for r in rows)])
        characters = []
        for r in rows:
            meta = self._read_meta(r['short_tag'])
            if meta and meta.get('source') == PATCH_SOURCE:
                characters.append({**r, 'status': 'added', 'claims': claims.get(r['short_tag'], 0)})
            else:
                why = self._read(f"rejected/{r['short_tag']}.json", {}).get('rejected') or '没有可用的参考图'
                characters.append({**r, 'status': 'rejected', 'reason': why})
        check = self._read('check.json', None)
        if check:
            done = {r['short_tag'] for r in rows}
            check['missing'] = [r for r in check['missing'] if r['short_tag'] not in done]
            self._write('check.json', check)
        self._record('add', characters, changed)

    def _drop(self, meta, reason):
        changed, _ = self._run(['drop', meta['short_tag'], '--reason', reason, '--rebuild'])
        self._record('drop', [{'tag': meta.get('tag'), 'short_tag': meta['short_tag'], 'copyright': meta.get('copyright'),
                               'status': 'dropped', 'reason': reason}], changed)

    def _record(self, kind, characters, changed):
        at = time.strftime('%Y-%m-%d %H:%M:%S')
        self._write('history.json', [{'at': at, 'kind': kind, 'characters': characters, 'changed': changed}] + self._read('history.json', [])[:30])
        added = sum(c['status'] == 'added' for c in characters)
        self.update(status='done', kind=kind, added=added, changed=changed, finished_at=at)
        if self.on_change:
            self.on_change()
