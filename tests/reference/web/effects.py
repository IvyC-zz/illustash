"""Post-commit work shared by HTTP, CLI and workers; pending records survive failed dispatch."""
from contextvars import ContextVar
from functools import wraps
import threading

CURRENT = ContextVar('library_operation', default=None)
SCHEMA = """CREATE TABLE IF NOT EXISTS backend_effects(
    id INTEGER PRIMARY KEY, kind TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending',
    error TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)"""


def operation(kind):
    def decorate(fn):
        @wraps(fn)
        def call(self, *args, **kwargs):
            self.require_writable()
            if CURRENT.get() is not None:
                return fn(self, *args, **kwargs)
            token = CURRENT.set((self, kind))
            try:
                result = fn(self, *args, **kwargs)
            finally:
                CURRENT.reset(token)
            if getattr(self, 'on_effect', None):
                drain(self, self.on_effect)
            return result
        return call
    return decorate


def drain(store, handler):
    """Handlers must be idempotent. A failed handler leaves its event pending, with the actual error."""
    lock, requested = store._effects_lock, store._effects_requested
    requested.set()
    while lock.acquire(blocking=False):
        failed = False
        try:
            requested.clear()
            with store.connect() as c:
                # Native profile handoffs belong to M4 names coordination, never to the learner.
                rows = c.execute("SELECT id,kind FROM backend_effects WHERE status='pending' AND kind NOT LIKE 'names:%' ORDER BY id").fetchall()
            for ident, kind in rows:
                try:
                    handler(kind)
                except Exception as exc:
                    with store.connect() as c:
                        c.execute('UPDATE backend_effects SET error=? WHERE id=?', (str(exc)[:500], ident))
                    failed = True
                    break
                else:
                    with store.connect() as c:
                        c.execute("UPDATE backend_effects SET status='done',error='' WHERE id=?", (ident,))
        finally:
            lock.release()
        # 先释放锁再检查：并发请求要么被本轮接续，要么自己拿锁处理，释放边界不丢唤醒。
        # 真正失败仍等待下次显式重试，避免失败事件在这里忙循环。
        if failed or not requested.is_set():
            return


def local_handler(store):
    """Synchronous CLI dispatcher. Never fetches names, models, or any other remote content."""
    from web.features import Features
    from web.learn import learn, cluster, patch_anchors
    from web.similar import SimilarScanner
    def handle(kind):
        features = Features(store.source)
        features.load()
        learn(store, features, reclaim=kind == 'imported', anchors=patch_anchors(store) or {})
        if kind == 'imported':
            cluster(store, features)
            SimilarScanner(store, features).scan()
    return handle
