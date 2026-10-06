"""仅供历史合成对照；生产识别导入使用原生 recognition-import。"""
"""Validated, transactional offline recognition imports. See docs/RESULT_PACKAGES.md."""
import argparse
import csv
import hashlib
import io
import json
import math
import sqlite3
from pathlib import Path
import numpy as np
from web import effects, library
from web.store import Store, Invalid

ROOT = Path(__file__).resolve().parents[3]
MODELS = {'wd': ('swinv2_448', 1024, ROOT / 'models/wd_swinv2_v3_emb.onnx'),
          'ccip': ('ccip-model_feat', 768, ROOT / 'models/ccip/model_feat.onnx')}
FILES = ('labels.jsonl', 'wd.npy', 'ccip.npy')
SCHEMA = """CREATE TABLE IF NOT EXISTS recognition_imports(
 digest TEXT PRIMARY KEY, manifest TEXT NOT NULL, assets TEXT NOT NULL, status TEXT NOT NULL,
 error TEXT NOT NULL DEFAULT '', imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
 CREATE TABLE IF NOT EXISTS recognition_assets(
 asset TEXT PRIMARY KEY, digest TEXT NOT NULL, package TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model));"""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def model_specs():
    out = {}
    for key, (version, dimension, path) in MODELS.items():
        with path.open('rb') as f:
            digest = hashlib.file_digest(f, 'sha256').hexdigest()
        out[key] = dict(version=version, dimension=dimension, sha256=digest)
    with (ROOT / 'tags/tags_v3.csv').open('rb') as f:
        out['wd']['tags_sha256'] = hashlib.file_digest(f, 'sha256').hexdigest()
    return out


def validate(package, lib, specs=None):
    package = Path(package)
    raw = (package / 'manifest.json').read_bytes()
    if len(raw) > 4 * 1024 * 1024:
        raise Invalid('manifest 过大')
    m = json.loads(raw)
    if m.get('format') != 'illustash-recognition/1' or m.get('library_id') != lib.config['id']:
        raise Invalid('结果包格式或图库 ID 不匹配')
    ids = m.get('assets')
    if not isinstance(ids, list) or not ids or len(ids) > 100000 or any(not isinstance(u, str) or not u for u in ids) or len(ids) != len(set(ids)):
        raise Invalid('资产 ID 列表不合法或重复')
    specs = model_specs() if specs is None else specs
    if m.get('models') != specs:
        raise Invalid('模型版本、文件指纹或向量维度不匹配')
    if set(m.get('files', {})) != set(FILES):
        raise Invalid('结果包文件清单不完整')
    payload = {}
    for name in FILES:
        path = package / name
        if path.is_symlink() or path.stat().st_size > 1024 ** 3:
            raise Invalid('结果文件不可为符号链接且不得超过 1 GiB')
        data = path.read_bytes()
        if m['files'][name] != {'sha256': sha(data), 'bytes': len(data)}:
            raise Invalid('结果文件完整性校验失败：' + name)
        payload[name] = data
    vectors = {}
    for model in ('wd', 'ccip'):
        raw_vector = payload[model + '.npy']
        stream = io.BytesIO(raw_vector)
        version = np.lib.format.read_magic(stream)
        readers = {(1, 0): np.lib.format.read_array_header_1_0, (2, 0): np.lib.format.read_array_header_2_0}
        if version not in readers:
            raise Invalid('不支持的 NPY 格式版本')
        shape, fortran, dtype = readers[version](stream)
        expected = (len(ids), specs[model]['dimension'])
        # Validate the header and exact byte count BEFORE allocating or interpreting any array.
        if shape != expected or fortran or dtype != np.dtype('<f4') or len(raw_vector) - stream.tell() != expected[0] * expected[1] * 4:
            raise Invalid('非法向量文件头或长度：' + model)
        x = np.frombuffer(raw_vector, dtype='<f4', offset=stream.tell()).reshape(expected)
        if not np.isfinite(x).all() or (np.linalg.norm(x, axis=1) <= 1e-12).any():
            raise Invalid('非法向量：' + model)
        vectors[model] = x
    categories = {'0': 'general', '4': 'character', '9': 'rating'}
    with (ROOT / 'tags/tags_v3.csv').open() as f:
        vocab = {(categories.get(x['category']), x['name']) for x in csv.DictReader(f)}
    labels, keys, rated = {u: [] for u in ids}, set(), set()
    for line in payload['labels.jsonl'].splitlines():
        row = json.loads(line)
        u, dim, value, conf = (row.get(k) for k in ('asset', 'dim', 'value', 'confidence'))
        if u not in labels or (dim, value) not in vocab or isinstance(conf, bool) or not isinstance(conf, (int, float)) or not math.isfinite(conf) or not 0 <= conf <= 1:
            raise Invalid('非法标签、资产 ID 或置信度')
        key = (u, dim, value)
        if key in keys:
            raise Invalid('包内重复标签')
        keys.add(key)
        labels[u].append((dim, value, float(conf)))
        if dim == 'rating':
            rated.add(u)
    if rated != set(ids):
        raise Invalid('每个资产必须包含 rating 结果')
    digest = sha(json.dumps(m, sort_keys=True, separators=(',', ':')).encode())
    return m, digest, labels, vectors


def import_package(lib, package, store=None, specs=None, process=True):
    """Input failures roll back all source writes. Derived work is durable/retryable after source commit."""
    m, digest, labels, vectors = validate(package, lib, specs)
    ids = m['assets']
    c = sqlite3.connect(lib.source.resolve().as_uri() + '?mode=rw', uri=True, timeout=30)
    duplicate = False
    try:
        with c:
            c.execute('BEGIN IMMEDIATE')
            for sql in SCHEMA.split(';'):
                if sql.strip():
                    c.execute(sql)
            known = {r[0] for r in c.execute('SELECT uuid FROM assets WHERE in_scope=1 AND coalesce(trashed,0)=0')}
            if set(ids) - known:
                raise Invalid('结果包含不存在、不在范围内或已移除的资产 ID')
            old = c.execute('SELECT manifest FROM recognition_imports WHERE digest=?', (digest,)).fetchone()
            duplicate = old is not None
            if not duplicate:
                added = []
                for i, u in enumerate(ids):
                    fingerprint = sha(json.dumps(m['models'], sort_keys=True).encode() + json.dumps(sorted(labels[u]), separators=(',', ':')).encode() + vectors['wd'][i].tobytes() + vectors['ccip'][i].tobytes())
                    previous = c.execute('SELECT digest FROM recognition_assets WHERE asset=?', (u,)).fetchone()
                    if previous:
                        if previous[0] != fingerprint:
                            raise Invalid('资产已有不同识别结果，拒绝静默覆盖：' + u)
                        continue
                    c.execute("DELETE FROM labels WHERE uuid=? AND source='wd_v3' AND dim IN ('general','character','rating')", (u,))
                    c.executemany('INSERT INTO labels(uuid,dim,value,conf,source,model_ver,run_id) VALUES (?,?,?,?,?,?,?)',
                                  [(u, d, v, f, 'wd_v3', m['models']['wd']['version'], 'import:' + digest) for d, v, f in labels[u]])
                    for key in vectors:
                        c.execute('INSERT OR REPLACE INTO embeddings VALUES (?,?,?)', (u, key, vectors[key][i].tobytes()))
                    c.execute('INSERT INTO recognition_assets VALUES (?,?,?)', (u, fingerprint, digest))
                    added.append(u)
                c.execute('INSERT INTO recognition_imports(digest,manifest,assets,status) VALUES (?,?,?,?)',
                          (digest, json.dumps(m, sort_keys=True), json.dumps(added), 'pending'))
    finally:
        c.close()
    state = resume(lib, store) if process else {'status': 'pending'}
    return {'digest': digest, 'duplicate': duplicate, **state}


def resume(lib, store=None):
    store = store or Store(lib.source, lib.db, initialize=False).initialize(build_general=False)
    c = sqlite3.connect(lib.source, timeout=30)
    try:
        for digest, raw in c.execute("SELECT digest,assets FROM recognition_imports WHERE status<>'done'").fetchall():
            try:
                ids = json.loads(raw)
                if ids:
                    store.import_changes(ids)
                # CLI completes local followups synchronously; the server uses its existing workers.
                effects.drain(store, store.on_effect or effects.local_handler(store))
                with store.connect() as app:
                    pending = app.execute("SELECT error FROM backend_effects WHERE status='pending' LIMIT 1").fetchone()
                if pending:
                    raise RuntimeError(pending[0] or '后处理待完成')
                with c:
                    c.execute("UPDATE recognition_imports SET status='done',error='' WHERE digest=?", (digest,))
            except Exception as exc:
                with c:
                    c.execute("UPDATE recognition_imports SET status='pending',error=? WHERE digest=?", (str(exc)[:500], digest))
                return {'status': 'pending', 'error': str(exc)}
        return {'status': 'done'}
    finally:
        c.close()


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--library', required=True, type=Path)
    ap.add_argument('--package', type=Path)
    ap.add_argument('--resume', action='store_true')
    ap.add_argument('--model-specs', action='store_true')
    args = ap.parse_args()
    # Compatibility CLI forwards the stable application work to Rust; Python functions above remain test/reference APIs.
    import subprocess
    from web.terms_core import binary
    exe = binary()
    if exe is None:
        ap.error('找不到原生词条核心程序')
    command = [str(exe), 'recognition-import', '--library', str(args.library), '--root', str(ROOT)]
    if args.model_specs:
        command += ['--model-specs']
    elif args.resume:
        command += ['--resume']
    elif args.package:
        command += ['--package', str(args.package)]
    else:
        ap.error('需要 --package 或 --resume')
    return subprocess.run(command).returncode


if __name__ == '__main__':
    raise SystemExit(main())
