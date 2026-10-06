"""Cross-process ownership and recovery. Never unlink a lock file (waiters share its inode)."""
import os
from provenance import db

class Lease:
    def __init__(self, library):
        self.path = library.path / '.provenance.lock'
        self.file = None

    def acquire(self):
        f = open(self.path, 'a+b')
        try:
            if os.name == 'nt':
                import msvcrt
                f.seek(0)
                if not f.read(1):
                    f.write(b'0'); f.flush()
                f.seek(0)
                msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
            else:
                import fcntl
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except (OSError, BlockingIOError):
            f.close()
            return False
        self.file = f
        return True

    def close(self):
        if self.file:
            self.file.close()
            self.file = None


def recover(library):
    """Caller MUST hold the exclusive lease; an active owner's rows are never touched."""
    with db.connect(library) as c:
        c.execute("UPDATE runs SET status='failed', reason='进程异常退出，任务已中断', finished_at=datetime('now') WHERE status='running'")


def finish(library, run_id, status, reason=''):
    with db.connect(library) as c:
        c.execute("UPDATE runs SET status=?,reason=?,finished_at=datetime('now') WHERE id=? AND status='running'",
                  (status, reason, run_id))
