"""Recognise newly synced images on this Mac and write the results into the source database.

Runs as a child of the web server, which holds the Photos access permission. Same models,
preprocessing and storage as the full runs, so new rows are indistinguishable from old ones:
    WD v3 (wd_swinv2_v3_emb)   general / character / rating, every tag >= 0.001   (tools/wd_shard.py)
    CCIP + character_index      claims within each anchor's adaptive threshold    (tools/char_scan.py)
Embeddings (WD 1024-d, CCIP 768-d) are kept for later clustering of unknown characters.

Protocol: one JSON object per line on stdout, e.g. {"batch": [...], "failed": [...], "done": 25, "total": 993}.
A failed image gets no labels and is retried by the next sync.

    python -m web.infer review/pi.db uuids.txt
"""
import csv
import json
import os
import sys
import time
from pathlib import Path

import numpy as np
import onnxruntime as ort
from PIL import Image

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'tools'))
import ccip  # noqa: E402
from char_scan import anchor_index, claim_thresholds  # noqa: E402
from web.library import SHARED  # noqa: E402

WD_MODEL = Path(os.environ.get('ILLUSTASH_MODELS', ROOT / 'models')) / 'wd_swinv2_v3_emb.onnx'
TAGS = ROOT / 'tags' / 'tags_v3.csv'
ANCHORS = SHARED / 'char-anchors.npy'
ANCHOR_IDS = SHARED / 'char-anchors.txt'
KEEP = 0.001          # store every tag at or above this, like the full run
TOP = 9               # nearest anchors examined per image: the stored char_ext run used --top-k 3 (x3);
                      # k=9 reproduces its claims on 500/500 sampled images, the script default 15 only 95%
BATCH = 25
WD_RUN = ('wd_v3', 'swinv2_448', 'wd-sync')
CCIP_RUN = ('char_index', 'character_index@2025-09-25', 'char-scan-alias-v3-sync')
DIMS = {0: 'general', 4: 'character', 9: 'rating'}


def wd_input(image, size=448):
    """White-pad to square, bicubic 448, float32 0-255, BGR, NHWC (imgutils wd14; tools/wd_extract.py)."""
    im = image.convert('RGB')
    w, h = im.size
    m = max(w, h)
    if (w, h) != (size, size):
        canvas = Image.new('RGB', (m, m), (255, 255, 255))
        canvas.paste(im, ((m - w) // 2, (m - h) // 2))
        im = canvas.resize((size, size), Image.BICUBIC) if m != size else canvas
    return np.ascontiguousarray(np.asarray(im, dtype=np.float32)[:, :, ::-1][None])


class Recogniser:
    def __init__(self, pi_db, *, with_anchors=True):
        with TAGS.open(encoding='utf-8') as f:
            rows = list(csv.DictReader(f))
        self.names = [r['name'] for r in rows]
        self.dims = [DIMS.get(int(r['category'])) for r in rows]
        opts = ort.SessionOptions()
        opts.log_severity_level = 3
        opts.intra_op_num_threads = 4   # leave cores for the library UI
        self.wd = ort.InferenceSession(str(WD_MODEL), opts, providers=['CPUExecutionProvider'])
        self.wd_in = self.wd.get_inputs()[0].name
        self.wd_out = [o.name for o in self.wd.get_outputs()]
        if with_anchors:
            self.anchors = np.load(ANCHORS)
            ids = [line.strip() for line in ANCHOR_IDS.open() if line.strip()]
            _, th, _, th75, self.entity = anchor_index(self.anchors, ids, pi_db)
            self.th = claim_thresholds(ids, th, th75)
        else:
            self.anchors = np.empty((0, 768), dtype=np.float32)
            self.entity, self.th = [], np.empty(0, dtype=np.float32)

    def labels(self, image):
        """Label rows (dim, value, conf, source, model_ver, run_id) and embeddings for one image."""
        out, emb = self.wd.run(self.wd_out, {self.wd_in: wd_input(image)})
        probs = np.asarray(out, dtype=np.float64).reshape(-1)   # the model already outputs probabilities
        rows = [(self.dims[j], self.names[j], round(float(probs[j]), 4), *WD_RUN)
                for j in np.nonzero(probs >= KEEP)[0] if self.dims[j]]
        feat = ccip.features([image])
        dist = ccip.cross(self.anchors, feat, check=False)[:, 0]
        seen = set()
        for a in np.argsort(dist)[:TOP]:
            name = self.entity[a]
            if name in seen:
                continue
            seen.add(name)
            if self.th[a] > 0 and dist[a] <= self.th[a]:
                rows.append(('char_ext', name, 1.0 - float(dist[a]), *CCIP_RUN))
        return rows, {'wd': np.asarray(emb, dtype=np.float32).reshape(-1), 'ccip': feat[0]}


def save(db, done):
    with db:
        db.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model))')
        for uuid, rows, embs in done:
            db.execute("DELETE FROM labels WHERE uuid=? AND dim IN ('general','character','rating','char_ext')", (uuid,))
            db.executemany('INSERT OR REPLACE INTO labels(uuid,dim,value,conf,source,model_ver,run_id) VALUES (?,?,?,?,?,?,?)',
                           [(uuid, *r) for r in rows])
            db.executemany('INSERT OR REPLACE INTO embeddings VALUES (?,?,?)', [(uuid, k, v.tobytes()) for k, v in embs.items()])


def main():
    import sqlite3
    pi_db, listing = sys.argv[1], sys.argv[2]
    uuids = [line.strip() for line in open(listing) if line.strip()]
    db = sqlite3.connect(pi_db, timeout=30)
    paths = dict(db.execute(f"SELECT uuid,src_path FROM assets WHERE uuid IN ({','.join('?' for _ in uuids)})", uuids)) if uuids else {}
    model = Recogniser(pi_db)
    print(json.dumps({'ready': True, 'total': len(uuids)}), flush=True)
    done, failed, count, started = [], [], 0, time.monotonic()
    for i, uuid in enumerate(uuids, 1):
        try:
            with Image.open(paths[uuid]) as im:
                im.load()
                done.append((uuid, *model.labels(im)))
        except Exception as exc:   # unreadable (e.g. not downloaded from iCloud): retried next sync
            failed.append({'uuid': uuid, 'error': f'{type(exc).__name__}: {exc}'[:200]})
        if len(done) + len(failed) >= BATCH or i == len(uuids):
            save(db, done)
            count += len(done) + len(failed)
            print(json.dumps({'batch': [d[0] for d in done], 'failed': failed, 'done': count, 'total': len(uuids),
                              'ms_per_image': round((time.monotonic() - started) * 1000 / count)}), flush=True)
            done, failed = [], []
    db.close()


if __name__ == '__main__':
    main()
