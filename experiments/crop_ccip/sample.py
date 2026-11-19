"""用 #557 冻结的参考图和共用评测集 v1 选取独立的 CCIP 裁图对照。"""
import argparse
from collections import defaultdict
import hashlib
import json
from pathlib import Path



def load(path):
    return json.loads(path.read_bytes())


def save(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, separators=(',', ':')))


def sample(manifest, sources):
    anchors = {}
    refs = {}
    for folder in sources:
        for entry in load(folder / 'A-metadata.json'):
            label = entry['tag']
            if label in anchors:
                raise ValueError(f'重复锚点 {label}')
            anchors[label] = entry['posts']
        refs.update({r['id']: r for r in load(folder / 'development-accepted.json')})
    gathered = {i for ids in anchors.values() for i in ids}
    reference = [dict(refs[i], cohort='reference') for i in sorted(gathered)]
    works = {work for r in reference for work in r['copyrights'] if work != 'original'}
    controls = defaultdict(list)
    selected = []
    for row in load(manifest):
        if row['id'] in gathered:
            raise ValueError('参考图进入评测集')
        if anchors.keys() & set(row['characters']):
            selected.append(dict(row, cohort='positive'))
        elif row['solo'] and len(row['characters']) == 1:
            controls['single_control'].append(row)
        elif len(row['characters']) >= 2:
            group = 'same_work_control' if works.intersection(row['copyrights']) else 'other_control'
            controls[group].append(row)
    for group, rows in sorted(controls.items()):
        def rank(row):
            return hashlib.sha256(f"issue456-ccip-v1:{row['id']}".encode()).digest()
        selected.extend(dict(r, cohort=group) for r in sorted(rows, key=rank)[:192])
    return anchors, reference + selected


def main():
    import duckdb

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--manifest', type=Path, required=True)
    parser.add_argument('--references', type=Path, nargs='+', required=True)
    parser.add_argument('--index', type=Path, required=True)
    parser.add_argument('--metadata', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    anchors, rows = sample(args.manifest, args.references)
    db = duckdb.connect()
    ids = [r['id'] for r in rows]
    ranges = db.execute('SELECT id, shard, offset_bytes, length_bytes FROM read_parquet(?) '
                        'WHERE id IN (SELECT unnest(?))', [str(args.index), ids]).fetchall()
    urls = db.execute('SELECT id, image_width, image_height, file_ext FROM read_parquet(?) '
                      'WHERE id IN (SELECT unnest(?))', [str(args.metadata), ids]).fetchall()
    by_id = {r['id']: r for r in rows}
    for i, shard, offset, length in ranges:
        by_id[i]['mirror'] = [shard, offset, length]
    for i, width, height, ext in urls:
        by_id[i]['original_size'] = [width, height]
        by_id[i]['ext'] = ext
    args.output.mkdir(parents=True, exist_ok=True)
    save(args.output / 'anchors.json', anchors)
    save(args.output / 'sample.json', rows)
    print(f'{len(anchors)} 个参考锚点，{len(rows)} 张图，镜像有 {len(ranges)} 张')


if __name__ == '__main__':
    main()
