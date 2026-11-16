#!/usr/bin/env python3
"""从固定版本的上游词表生成随原生宿主分发的 WD 通用标签对照表。"""
import argparse
import csv
import hashlib
import io
import json
from pathlib import Path
from urllib.request import urlopen


ROOT = Path(__file__).resolve().parents[1]
WD = ROOT / 'tags/tags_v3.csv'
OUTPUT = ROOT / 'tags/general_names.json'
WD_SHA256 = '298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217'
ZH_URL = ('https://raw.githubusercontent.com/amenorira/danbooru-tags-data-zh/'
          '8e73f3b1c15b30f873c11a8c65d2d8264cdf0224/tags/general.csv')
ZH_SHA256 = '4df46d24d61ee32b96d9aa79ae390b0c35bdd99ce15d591b617dc949e92c4240'
JA_URL = ('https://raw.githubusercontent.com/taki-ttt/charadb/'
          '7fe2e4b009ad6d85496101e929ac8528cb21d357/data/items_public.json')
JA_SHA256 = '7d3afccb974dbec08b03b33cd8ba922849903af89ce51eb6428d63f0158f5e9e'


def verify(data, expected, source):
    digest = hashlib.sha256(data).hexdigest()
    if digest != expected:
        raise ValueError(f'{source}: SHA-256 {digest} != {expected}')
    return data


def read_source(path, url, expected):
    if path is None:
        with urlopen(url, timeout=60) as response:
            data = response.read()
    else:
        data = path.read_bytes()
    return verify(data, expected, path or url)


def build(wd, chinese, japanese):
    general = {row['name'] for row in csv.DictReader(io.StringIO(wd, newline='')) if row['category'] == '0'}
    names = {}
    for row in csv.DictReader(io.StringIO(chinese, newline='')):
        tag = row['tag']
        if row['category'] != '0' or tag not in general:
            continue
        if tag in names:
            raise ValueError(f'上游有重复的通用标签: {tag}')
        zh = row['zh'].strip()
        aliases = [alias.strip() for alias in row['aliases'].split('|')]
        aliases = list(dict.fromkeys(alias for alias in aliases if alias and alias not in (tag, zh)))
        names[tag] = {}
        if zh and zh != tag:
            names[tag]['zh'] = zh
        if aliases:
            names[tag]['aliases'] = aliases

    for row in json.loads(japanese):
        # 多标签提示词和带 parent 的组合卡片都不是一个标签的日文名。
        tag = row['tags'].strip().replace(' ', '_')
        if row.get('parent') or tag not in general:
            continue
        ja = row['jp'].strip()
        if ja:
            entry = names.setdefault(tag, {})
            if 'ja' in entry and entry['ja'] != ja:
                raise ValueError(f'上游有多个首选日文名: {tag}')
            entry['ja'] = ja

    return {tag: entry for tag, entry in names.items() if entry}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--zh', type=Path, help='已下载的固定版 general.csv')
    parser.add_argument('--ja', type=Path, help='已下载的固定版 items_public.json')
    parser.add_argument('--output', type=Path, default=OUTPUT)
    args = parser.parse_args()
    wd = verify(WD.read_bytes(), WD_SHA256, WD).decode('utf-8')
    zh = read_source(args.zh, ZH_URL, ZH_SHA256).decode('utf-8-sig')
    ja = read_source(args.ja, JA_URL, JA_SHA256)
    names = build(wd, zh, ja)
    # 每个标签一行，便于版本更新时 review。
    lines = [json.dumps(tag) + ':' + json.dumps(names[tag], ensure_ascii=False, separators=(',', ':'))
             for tag in sorted(names)]
    result = ('{\n' + ',\n'.join(lines) + '\n}\n').encode('utf-8')
    args.output.write_bytes(result)
    print(f'{len(names)} 标签，{sum("zh" in row for row in names.values())} 中文，'
          f'{sum("ja" in row for row in names.values())} 日文，{len(result)} 字节，'
          f'SHA-256 {hashlib.sha256(result).hexdigest()}')


if __name__ == '__main__':
    main()
