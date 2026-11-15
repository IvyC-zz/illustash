"""#347 测试专用本地溯源参考，不调用原生实现或冻结输出。

算法来源：c6762ff65ecd06ca98a0b266bb49f9e04eeaa118 的
provenance/verify.py（Library、_normalise、local_run、_record）及 web/similar.py
的 signature。缓存 JSON 沿用现行紧凑格式；只写测试传入的参考缓存。
"""
from contextlib import closing
import json
import sqlite3

import numpy as np
from PIL import Image, ImageOps


def signature(path):
    """独立读取缺失的签名；该用例只包含生成 PNG 和不存在的路径。"""
    with Image.open(path) as image:
        image.draft('RGB', (128, 128))
        image = ImageOps.exif_transpose(image)
        if 'A' in image.getbands() or image.mode == 'P':
            rgba = image.convert('RGBA')
            image = Image.new('RGB', rgba.size, '#f4f4f2')
            image.paste(rgba, mask=rgba.getchannel('A'))
        grey = image.convert('L')
        return grey.width / grey.height, grey.resize((16, 16), Image.Resampling.BOX).tobytes()


def colour(path):
    with Image.open(path) as image:
        image.draft('RGB', (96, 96))
        return np.asarray(image.convert('RGB').resize((24, 24), Image.Resampling.BOX), np.float32)


def normalise(grids):
    values = np.frombuffer(b''.join(grids), np.uint8).reshape(len(grids), 256).astype(np.float32)
    values -= values.mean(1, keepdims=True)
    spread = np.sqrt((values * values).mean(1))
    return values / (np.linalg.norm(values, axis=1, keepdims=True) + 1e-6), spread


def local_run(library):
    """从输入签名独立推导命中、计数及全部缓存行，保留冲突不覆盖规则。"""
    with closing(sqlite3.connect(library.db.resolve().as_uri() + '?mode=ro', uri=True)) as core:
        paths = dict(core.execute('SELECT uuid,src_path FROM assets'))
        sigs = {u: (r, g) for u, r, g in core.execute('SELECT uuid,ratio,grid FROM image_sigs WHERE ok=1') if u in paths}
    for asset in paths.keys() - sigs.keys():
        try:
            sigs[asset] = signature(paths[asset])
        except (OSError, ValueError, TypeError):
            pass
    ids = np.array(list(sigs))
    logr = np.log(np.array([sigs[u][0] for u in ids], np.float32))
    vectors, spread = normalise([sigs[u][1] for u in ids])
    result = dict(pictures=0, matches=0, stopped='')
    colours = {}
    with closing(sqlite3.connect(library.path / 'provenance.sqlite')) as cache:
        cache.row_factory = sqlite3.Row
        seen = {r[0] for r in cache.execute("SELECT target FROM lookups WHERE backend='ref_compare'")}
        new = np.array([i for i, asset in enumerate(ids) if str(asset) not in seen], dtype=np.int64)
        if not len(new):
            return result
        vectors, logr, spread = vectors[new], logr[new], spread[new]
        rows = cache.execute('SELECT site,post_id,artist_id,ratio,grid,colour FROM ref_sigs WHERE colour IS NOT NULL')
        while batch := rows.fetchmany(4096):
            refs, ref_spread = normalise([r['grid'] for r in batch])
            ref_ratio = np.log(np.array([r['ratio'] for r in batch], np.float32))
            for start in range(0, len(new), 4096):
                scores = refs @ vectors[start:start + 4096].T
                matches = ((scores >= .9)
                           & (np.abs(ref_ratio[:, None] - logr[None, start:start + 4096]) <= .03)
                           & (ref_spread[:, None] >= 6) & (spread[None, start:start + 4096] >= 6))
                for i, j in zip(*np.nonzero(matches)):
                    ref, asset = batch[i], str(ids[new[start + j]])
                    try:
                        if asset not in colours:
                            colours[asset] = colour(paths[asset])
                        pixels = np.frombuffer(ref['colour'], np.uint8).reshape(24, 24, 3).astype(np.float32)
                        diff = float(np.abs(pixels - colours[asset]).mean())
                    except (OSError, ValueError):
                        continue
                    if diff >= 12 or not ref['artist_id']:
                        continue
                    site, post, artist = ref['site'], ref['post_id'], ref['artist_id']
                    url = (f'https://danbooru.donmai.us/posts/{post}' if site == 'danbooru'
                           else f'https://www.pixiv.net/artworks/{post}')
                    score = float(scores[i, j])
                    before = cache.total_changes
                    with cache:
                        cache.execute("""INSERT OR IGNORE INTO image_sources
                            (asset,site,post_id,url,artist_id,method,similarity)
                            VALUES (?,?,?,?,?,'gallery_match',?)""", (asset, site, post, url, artist, round(score * 100, 1)))
                        cache.execute("""INSERT INTO image_artists(asset,artist_id,method,score,evidence,status)
                            VALUES (?,?,'gallery_match',?,?,'auto') ON CONFLICT(asset,artist_id) DO NOTHING""",
                                      (asset, artist, score, json.dumps({site: post}, separators=(',', ':'))))
                    result['matches'] += cache.total_changes > before
        with cache:
            cache.executemany("""INSERT INTO lookups(target,backend,attempts,last_at,last_result)
                VALUES (?,'ref_compare',1,datetime('now'),'miss') ON CONFLICT(target,backend) DO NOTHING""",
                              [(str(ids[k]),) for k in new])
        result['pictures'] = len(new)
    return result
