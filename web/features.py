"""CCIP features of every library image, held in memory for clustering and learned prototypes.

The 46,980 images of the original run live in review/full-features.npy (row order: review/full-manifest.csv);
images added by the Photos sync keep theirs in pi.db `embeddings` (web/infer.py). Rows are L2-normalised,
so the CCIP difference used everywhere else, (1 - cos) / 2, is one matrix product (tools/ccip.py cross()).
"""
import csv
import hashlib
import sqlite3
import threading
from pathlib import Path

import numpy as np

from web.library import features_dir

ROOT = Path(__file__).resolve().parents[1]
PAIR_BLOCK_BYTES = 16 * 1024 * 1024


def pair_block_rows(columns):
    """Bound a float32 pairwise working matrix to 16 MiB (at least one row)."""
    return max(1, min(512, PAIR_BLOCK_BYTES // max(4 * columns, 1)))


class Features:
    def __init__(self, source, matrix=None, uuids=None, *, base_dir=None):
        """`matrix`/`uuids` let tests supply features without the files."""
        self.source = Path(source)
        self.base_dir = Path(base_dir) if base_dir is not None else None
        self.lock = threading.Lock()
        self.uuids, self.row, self.X = [], {}, np.zeros((0, 768), np.float32)
        self.versions = {}
        self._file_stamp, self._extra_raw = None, {}
        self._injected = (list(uuids), np.asarray(matrix, np.float32)) if matrix is not None else None
        if matrix is not None:
            self._set(list(uuids), np.asarray(matrix, np.float32))

    def _set(self, uuids, X):
        if X.ndim != 2 or len(uuids) != len(X) or len(set(uuids)) != len(uuids):
            raise ValueError('Feature manifest/matrix mismatch or duplicate UUID')
        X = self._normalise(X)
        self.uuids, self.X = uuids, X
        self.row = {u: i for i, u in enumerate(uuids)}
        self.versions = {u: hashlib.sha256(X[i].tobytes()).digest() for i, u in enumerate(uuids)}

    @staticmethod
    def _normalise(X):
        norm = np.linalg.norm(X, axis=1, keepdims=True)
        if not np.isfinite(X).all() or (norm <= 1e-12).any():
            raise ValueError('Invalid CCIP feature vector')
        return (X / norm).astype(np.float32)

    def load(self):
        """Reload changed base files and changed/new DB vectors; unchanged calls retain the matrix."""
        with self.lock:
            folder = self.base_dir if self.base_dir is not None else features_dir(self.source)
            manifest = folder / 'full-manifest.csv'
            features = folder / 'full-features.npy'
            stamp = tuple((p.stat().st_mtime_ns, p.stat().st_size) if p.exists() else None
                          for p in (manifest, features))
            db = sqlite3.connect(self.source.resolve().as_uri() + '?mode=ro', uri=True, timeout=30)
            try:
                extra = dict(db.execute("SELECT uuid,vec FROM embeddings WHERE model='ccip'"))
            except sqlite3.OperationalError as exc:
                if 'no such table: embeddings' not in str(exc):
                    raise
                extra = {}
            finally:
                db.close()
            previous = self.uuids, self.row, self.X, self.versions
            try:
                rebuild = stamp != self._file_stamp or bool(self._extra_raw.keys() - extra.keys())
                if rebuild:
                    if all(stamp):
                        with manifest.open() as f:
                            uuids = [r['uuid'].strip().upper() for r in csv.DictReader(f)]
                        self._set(uuids, np.load(features))
                    elif self._injected is not None:
                        self._set(*self._injected)
                    elif not any(stamp):
                        self._set([], np.zeros((0, self.X.shape[1]), np.float32))
                    else:
                        raise ValueError('Both feature matrix and manifest are required')
                changed = {u: v for u, v in extra.items() if rebuild or self._extra_raw.get(u) != v}
                if changed:
                    vectors = self._normalise(np.stack([np.frombuffer(v, np.float32) for v in changed.values()]))
                    if vectors.shape[1] != self.X.shape[1]:
                        raise ValueError('CCIP feature dimension mismatch')
                    added = [u for u in changed if u not in self.row]
                    uuids = self.uuids + added
                    row = {u: i for i, u in enumerate(uuids)}
                    X = np.empty((len(uuids), self.X.shape[1]), np.float32)
                    X[:len(self.X)] = self.X
                    versions = self.versions.copy()
                    for u, v in zip(changed, vectors):
                        X[row[u]] = v
                        versions[u] = hashlib.sha256(v.tobytes()).digest()
                    self.uuids, self.row, self.X, self.versions = uuids, row, X, versions
                self._file_stamp, self._extra_raw = stamp, extra
            except BaseException:
                self.uuids, self.row, self.X, self.versions = previous
                raise
        return self

    def matrix(self, uuids):
        """(known uuids, their normalised rows), from one coherent cache snapshot."""
        with self.lock:
            known = [u for u in uuids if u in self.row]
            return known, (self.X[[self.row[u] for u in known]] if known else np.zeros((0, self.X.shape[1]), np.float32))

    @staticmethod
    def distance(a, b):
        """CCIP difference between normalised rows: (1 - cos) / 2."""
        return (1.0 - a @ b.T) / 2.0
