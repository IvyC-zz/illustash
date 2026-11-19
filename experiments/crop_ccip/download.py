"""从两个公开数据集按索引取选定图片，原文件核对 MD5，高分辨率 WebP 单独记来源。"""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
from io import BytesIO
import json
from pathlib import Path
from threading import local

from PIL import Image
import requests
from requests.adapters import HTTPAdapter
from urllib3.util.retry import Retry

from sample import load, save

ORIGINAL = 'https://huggingface.co/datasets/picollect/danbooru/resolve/45ddd4d52b3cc5554d529703e8ea7bcd6f7fbe30'
MIRROR = 'https://huggingface.co/datasets/nyanko-devs/danbooru2026/resolve/ebb02a630201c7b51487e45fb90b3fcf4cbedc20'
_sockets = local()


def session():
    if not hasattr(_sockets, 'client'):
        client = requests.Session()
        retry = Retry(total=3, backoff_factor=1, status_forcelist=(429, 500, 502, 503, 504))
        client.mount('https://', HTTPAdapter(max_retries=retry))
        _sockets.client = client
    return _sockets.client


def ranged(url, offset, length):
    last = offset + length - 1
    with session().get(url, params={'range_start': offset}, headers={'Range': f'bytes={offset}-{last}'},
                       timeout=(15, 40), stream=True) as r:
        r.raise_for_status()
        if r.status_code != 206 or r.headers.get('Content-Range', '').split('/')[0] != f'bytes {offset}-{last}':
            raise ValueError('镜像未按索引返回指定字节范围')
        data = r.content
    if len(data) != length:
        raise ValueError('返回数据长度与索引不一致')
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--jobs', default=5, type=int)
    args = parser.parse_args()
    work = args.directory
    rows = load(work / 'sample.json')
    dest = work / 'images'
    dest.mkdir(exist_ok=True)
    index_dir = work / 'indices'
    index_dir.mkdir(exist_ok=True)
    buckets = sorted({r['id'] // 1000 for r in rows if r['id'] // 1000 <= 8049})

    def old_index(bucket):
        file = index_dir / f'{bucket}.json'
        if not file.is_file():
            resp = session().get(f'{ORIGINAL}/index/{bucket:04}.json', timeout=25)
            if resp.status_code == 404:
                return {}
            resp.raise_for_status()
            save(file, resp.json()['files'])
        return load(file)

    with ThreadPoolExecutor(args.jobs) as executor:
        indices = dict(zip(buckets, executor.map(old_index, buckets)))

    def retrieve(row):
        i = row['id']
        bucket = i // 1000
        filename = dest / str(i)
        receipt = dest / f'{i}.json'
        if receipt.exists() and filename.exists():
            rec = load(receipt)
            if hashlib.sha256(filename.read_bytes()).hexdigest() != rec['sha256']:
                raise ValueError('缓存数据校验失败')
            return rec
        original = next(((name, item) for name, item in indices.get(bucket, {}).items()
                         if name.partition('.')[0] == str(i)), None)
        data = None
        original_failure = None
        if original:
            _, item = original
            try:
                data = ranged(f'{ORIGINAL}/images/{bucket:04}.tar', item['offset'], item['size'])
            except requests.RequestException as error:
                if 'mirror' not in row:
                    raise
                original_failure = type(error).__name__
            if data is not None:
                if hashlib.md5(data).hexdigest() != row['md5'] or hashlib.sha256(data).hexdigest() != item['sha256']:
                    raise ValueError('原文件与冻结或镜像清单不一致')
                source = 'original'
        if data is None:
            if 'mirror' not in row:
                return {'id': i, 'source': 'unavailable'}
            shard, offset, length = row['mirror']
            data = ranged(f'{MIRROR}/original/data-{shard:04}.tar', offset, length)
            source = 'webp_4m'
        with Image.open(BytesIO(data)) as im:
            size = list(im.size)
            im.verify()
        temp = filename.with_suffix('.partial')
        temp.write_bytes(data)
        temp.replace(filename)
        rec = {'id': i, 'source': source, 'size': size, 'sha256': hashlib.sha256(data).hexdigest()}
        if original_failure:
            rec['original_network_error'] = original_failure
        save(receipt, rec)
        return rec

    # 网络失败单列；收到的字节与索引不符则立即报错，不能当作正常缺图。
    def attempt(row):
        try:
            return retrieve(row)
        except requests.RequestException as error:
            return {'id': row['id'], 'source': 'network_error', 'kind': type(error).__name__}

    with ThreadPoolExecutor(args.jobs) as executor:
        result = list(executor.map(attempt, rows))
    save(work / 'downloads.json', result)
    from collections import Counter
    print(dict(Counter(r['source'] for r in result)), flush=True)


if __name__ == '__main__':
    main()
