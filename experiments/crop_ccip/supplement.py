"""按冻结原图 MD5 从另一份独立高分辨率镜像补图，核对 tar 条目、WebP 摘要和尺寸。"""
import argparse
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
import hashlib
from io import BytesIO
from pathlib import Path
import tarfile

import duckdb
from PIL import Image
import requests

from download import ranged
from sample import load, save


MIRROR = 'https://huggingface.co/datasets/nyanko-devs/gelbooru2026/resolve/3dd3a561e35cca9fc2603b84c0c854b5a029cf5e'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--metadata', required=True, type=Path, help='同一镜像发布的 metadata/posts.parquet')
    parser.add_argument('--jobs', type=int, default=6)
    args = parser.parse_args()
    work = args.directory
    results = {r['id']: r for r in load(work / 'downloads.json')}
    missing = {r['md5']: r['id'] for r in load(work / 'sample.json')
               if results[r['id']]['source'] in ('unavailable', 'network_error')}
    db = duckdb.connect()
    # 文件按各桶中的原始帖子编号递增排列。每个 tar 从目录头开始，每个文件占一条头记录和整块的载荷。
    # 只在外层过滤 MD5，否则会少计同桶未选文件的占用；实际读出后再验证独立的文件路径、长度和摘要。
    query = """
        SELECT extra.md5, image_id, bucket, path, width, height, file_size, sha256, header
        FROM (
            SELECT *, 512 + COALESCE(SUM(512 + 512 * ((file_size + 511) // 512))
                OVER (PARTITION BY bucket ORDER BY image_id ROWS BETWEEN UNBOUNDED PRECEDING AND 1 PRECEDING), 0) AS header
            FROM read_parquet(?)
        ) WHERE extra.md5 IN (SELECT UNNEST(?)) ORDER BY image_id
    """
    unique = {}
    for row in db.execute(query, [str(args.metadata), list(missing)]).fetchall():
        unique.setdefault(row[0], row)

    def get(row):
        md5, other, bucket, name, width, height, size, digest, header = row
        i = missing[md5]
        url = f'{MIRROR}/original/data-{bucket:04}.tar'
        dest = work / 'images' / str(i)
        receipt = dest.with_suffix('.json')
        if dest.is_file() and receipt.is_file():
            old = load(receipt)
            if old['sha256'] == digest and hashlib.sha256(dest.read_bytes()).hexdigest() == digest:
                return old
        try:
            raw = ranged(url, int(header), 512 + size)
        except requests.RequestException as error:
            return dict(id=i, source='network_error', kind=type(error).__name__)
        member = tarfile.TarInfo.frombuf(raw[:512], 'utf-8', 'strict')
        data = raw[512:]
        if not member.isfile() or member.name != name or member.size != size or hashlib.sha256(data).hexdigest() != digest:
            raise ValueError('补充镜像的条目或摘要与清单不一致')
        with Image.open(BytesIO(data)) as im:
            if im.format != 'WEBP' or im.size != (width, height):
                raise ValueError('解码尺寸与镜像清单不一致')
            im.verify()
        temp = dest.with_suffix('.partial')
        temp.write_bytes(data)
        temp.replace(dest)
        record = dict(id=i, source='gel_webp_4m', size=[width, height], sha256=digest, mirror_id=other)
        save(receipt, record)
        return record

    with ThreadPoolExecutor(args.jobs) as pool:
        for record in pool.map(get, unique.values()):
            results[record['id']] = record
    save(work / 'downloads.json', list(results.values()))
    print(dict(Counter(r['source'] for r in results.values())), flush=True)


if __name__ == '__main__':
    main()
