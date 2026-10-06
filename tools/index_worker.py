#!/usr/bin/env python3
"""可选索引计算 worker。只写 Rust 准备的隔离 review，不联网、不访问正式数据库。
argv: request.json response.json；协议见 docs/ARCHITECTURE_DIRECTION.md。
"""
import collections
import json
import os
from pathlib import Path
import sqlite3
import sys


def main(request, response):
    import numpy as np
    from PIL import Image, UnidentifiedImageError
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    import ccip
    import char_patch
    import build_characters as build
    import char_scan as scan
    req = json.loads(Path(request).read_text())
    if req['format'] != 'illustash-index/1':
        raise ValueError('unsupported index request')
    review = Path(req['review']).resolve()
    if review != Path(request).resolve().parent or review / 'response.json' != Path(response).resolve():
        raise ValueError('worker output outside isolated job')
    idx, patch = review / 'char-index', review / 'char-patch'
    for name, value in {'REVIEW': review, 'IDX': idx, 'PATCH': patch, 'PLAN': patch / 'plan.json'}.items():
        setattr(char_patch, name, str(value))
    build.REVIEW, build.DB, build.IDX, build.GAMES = map(str, (review, review / 'pi.db', idx, review / 'game-chars'))
    scan.REVIEW = str(review)
    def progress(step, **fields):
        tmp=review/'progress.tmp'
        tmp.write_text(json.dumps(dict(identity=req['identity'],step=step,**fields)))
        tmp.replace(review/'progress.json')
    rows = req['rows']
    selected = {r['short_tag'] for r in rows}
    if req['action'] == 'apply':
        for i,row in enumerate(rows):
            progress('fetch',done=i,total=len(rows))
            cid = row['short_tag']
            if any((p / (cid + ext)).exists() for p, ext in ((idx, '.npy'), (patch / 'raw', '.npy'), (patch / 'rejected', '.json'))):
                continue
            refs = json.loads((patch / (cid + '-refs.json')).read_text())
            feats, posts, general = [], [], collections.Counter()
            for ref in refs:
                try:
                    with Image.open(ref['path']) as im:
                        image = im.convert('RGB')
                except (OSError, UnidentifiedImageError):
                    continue
                # Model/runtime failures must fail the task, never masquerade as bad references.
                feature = ccip.features([image])[0]
                feats.append(feature)
                posts.append(ref['id'])
                general.update(set((ref['general'] or '').split()))
            if len(posts) < 3:
                char_patch.reject(dict(row), 'Danbooru 上可用的全年龄单人图只有 %d 张' % len(posts))
                continue
            n = len(posts)
            core = [t for t, c in general.most_common() if c / n >= .6 and t not in ('solo', '1girl', '1boy', 'looking_at_viewer')]
            meta = dict(row, copyrights=[row['copyright']], core_tags=core[:24],
                        gender={'girl': general['1girl'] / n, 'boy': general['1boy'] / n},
                        source='danbooru-patch', fetched_at=__import__('time').strftime('%Y-%m-%d'), posts=posts)
            np.save(idx / (cid + '.npy'), np.stack(feats).astype(np.float32))
            (idx / (cid + '.json')).write_text(json.dumps(meta, ensure_ascii=False))
        progress('clean')
        char_patch.clean(selected)
    elif req['action'] == 'drop':
        char_patch.drop(selected, req['reason'].strip())
    else:
        raise ValueError('unsupported index action')
    progress('build')
    sys.argv = ['index-worker', '--db', str(review / 'pi.db')]
    if build.main():
        raise RuntimeError('anchor calculation failed')
    if not (review / 'full-features.npy').exists():
        np.save(review / 'full-features.npy', np.empty((0, 768), dtype=np.float32))
        (review / 'full-manifest.csv').write_text('uuid\n')
    with sqlite3.connect(review / 'pi.db') as db:
        count = len(np.load(review / 'full-features.npy')) + db.execute("SELECT count(*) FROM embeddings WHERE model='ccip'").fetchone()[0]
    if count:
        progress('scan')
        sys.argv = ['index-worker', '--load', '--run', 'char-scan-' + req['identity']['task_id'], '--index', 'character_index@2025-09-25+danbooru@' + __import__('time').strftime('%Y-%m-%d')]
        scan.main()
    else:
        with sqlite3.connect(review / 'pi.db') as db:
            db.execute("DELETE FROM labels WHERE dim='char_ext'")
    with sqlite3.connect(review / 'pi.db') as db:
        db.row_factory = sqlite3.Row
        result = {'format': req['format'], 'identity': req['identity'], 'before': req['before'],
                  'characters': [dict(r) for r in db.execute('SELECT * FROM characters')],
                  'aliases': [list(r) for r in db.execute('SELECT char_id,alias,kind FROM character_aliases')],
                  'labels': [list(r) for r in db.execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")]}
    Path(response).write_text(json.dumps(result, ensure_ascii=False))


if __name__ == '__main__':
    main(*sys.argv[1:])
