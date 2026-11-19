"""在同一批图片上比较已发布长尾索引与用独立参考重算的整图、人体、头部锚点。"""
import argparse
from collections import Counter, defaultdict
import csv
from pathlib import Path

import numpy as np

from frozen import baseline_entries, canonical, matrix_file, native_scan, reference_stats
from sample import load, save


KINDS = ('whole', 'person', 'head')
MODELS = ('current', 'b36')


def slices(data):
    split = 1 + len(data['person'])
    return {'whole': slice(0, 1), 'person': slice(1, split), 'head': slice(split, split + len(data['head']))}


def group(row):
    if row['cohort'] != 'positive':
        return row['cohort']
    if row['solo'] and len(row['characters']) == 1:
        return 'positive_solo'
    if not row['solo'] and len(row['characters']) >= 2:
        return 'positive_multitag'
    return 'positive_ambiguous'


def read_features(rows, directory, receipts):
    data = {}
    for row in rows:
        i = row['id']
        if receipts[i]['source'] not in ('original', 'webp_4m', 'gel_webp_4m'):
            continue
        path = directory / f'{i}.npz'
        if not path.is_file():
            continue
        with np.load(path, allow_pickle=False) as archive:
            record = {k: archive[k] for k in ('person', 'head', *MODELS)}
        count = 1 + len(record['person']) + len(record['head'])
        for m in MODELS:
            features = record[m]
            if features.shape != (count, 768) or not np.isfinite(features).all() or (np.linalg.norm(features, axis=1) == 0).any():
                raise ValueError(f'{i} {m} 特征有误')
        data[i] = record
    return data


def check_disjoint(reference, evaluation):
    for field in ('id', 'artist', 'md5', 'pixel_hash'):
        train = {r[field] for r in reference if r.get(field)}
        if any(r.get(field) in train for r in evaluation):
            raise ValueError(f'参考与评测重合：{field}')
    families = {r['parent_id'] or r['id'] for r in reference}
    if any((r['parent_id'] or r['id']) in families for r in evaluation):
        raise ValueError('参考与评测存在同组帖子')


def strata(row, receipt):
    width, height = receipt['size']
    pixels = width * height
    size = 'under_1mp' if pixels < 1_000_000 else '1_to_4mp' if pixels <= 4_000_000 else 'over_4mp'
    source = receipt['source']
    flags = set(row.get('flags', [])) | set(row.get('gender', []))
    multiple = bool(flags & {'multiple_girls', 'multiple_boys', 'multiple_others'}) or {'1girl', '1boy'} <= flags
    people = 'multiple_count' if multiple and not row['solo'] else 'single_marked' if row['solo'] else 'count_unknown'
    cohort = group(row)
    return ['all', cohort, people, f'{people}/{cohort}', source, f'{source}/{size}', f'{source}/{size}/{cohort}']


def measure(rows, receipts, predictions, roster, baseline):
    counts = defaultdict(Counter)
    role_counts = defaultdict(Counter)
    for row in rows:
        i = row['id']
        identified = predictions[i] & roster
        expected = set(row['characters']) & roster
        hit, wrong = identified & expected, identified - expected
        original = baseline[i] & roster
        gained = hit - original
        lost = (original & expected) - hit
        added_wrong = wrong - original
        removed_wrong = (original - expected) - wrong
        for name in strata(row, receipts[i]):
            counts[name].update(images=1, targets=len(expected), true_pairs=len(hit), false_pairs=len(wrong),
                                false_images=int(bool(wrong)), complete_target_images=int(bool(expected) and hit == expected),
                                target_images=int(bool(expected)), annotated_pairs=len(set(row['characters'])),
                                gained_pairs=len(gained), lost_pairs=len(lost),
                                added_false_pairs=len(added_wrong), removed_false_pairs=len(removed_wrong),
                                newly_false_images=int(bool(wrong) and not original - expected),
                                cleared_false_images=int(bool(original - expected) and not wrong))
        for role in expected | identified:
            role_counts[role].update(targets=int(role in expected), correct=int(role in hit), wrong=int(role in wrong))
    return {'groups': dict(counts), 'roles': dict(role_counts)}


def predict_views(rows, data, predictor):
    """给每张图分别记整图、仅裁图和整图加裁图的结果，同图同角色只记一次。"""
    result = defaultdict(dict)
    for row in rows:
        i = row['id']
        layers = slices(data[i])
        found = {kind: predictor(i, kind, layer) for kind, layer in layers.items()}
        for kind in KINDS:
            result[kind][i] = found[kind]
        for kind in ('person', 'head'):
            result['whole+' + kind][i] = found['whole'] | found[kind]
            result['whole+' + kind + '_if_multiple'][i] = found['whole'] | (found[kind] if len(data[i][kind]) >= 2 else set())
    return result


def shared_reference_ids(anchors, references, data, crop):
    """整图和裁图、两个模型共用参考帖子。只选冻结的单角色单人图，且该检测器恰好给出一个框。"""
    refs = {r['id']: r for r in references}
    shared = {}
    for role, ids in anchors.items():
        usable = [i for i in ids if i in data and refs[i]['solo'] and refs[i]['characters'] == [role]
                  and len(data[i][crop]) == 1]
        if len(usable) >= 5:
            shared[role] = usable
    return shared


def fit_panel(model, kind, refs, data):
    vectors = []
    thresholds = []
    for ids in refs.values():
        training = [data[i][model][slices(data[i])[kind]][0] for i in ids]
        anchor, percentiles = reference_stats(np.stack(training))
        vectors.append(anchor / np.linalg.norm(anchor))
        thresholds.append(percentiles[1])
    return np.stack(vectors), np.asarray(thresholds)


def panel_predictions(rows, model, crop, refs, data):
    # 只比较本组共有的长尾角色。不能用旧模型的向量或阈值给新模型打分。
    names = np.array(list(refs))
    panels = {kind: fit_panel(model, kind, refs, data) for kind in ('whole', crop)}

    def selected(i, kind, layer):
        if kind not in panels:
            return set()
        anchors, limits = panels[kind]
        features = data[i][model][layer]
        normalized = features / np.linalg.norm(features, axis=1, keepdims=True)
        distances = (1 - normalized @ anchors.T) / 2
        claims = set()
        for values in distances:
            for j in np.argsort(values, kind='stable')[:9]:
                if 0 < limits[j] and values[j] <= limits[j]:
                    claims.add(names[j])
        return claims

    predictions = predict_views(rows, data, selected)
    keys = ('whole', crop, 'whole+' + crop, 'whole+' + crop + '_if_multiple')
    return {key: predictions[key] for key in keys}


def published_predictions(args, rows, data, aliases):
    with np.load(args.index / 'index.npz', allow_pickle=False) as packed:
        entries = baseline_entries(load(args.index / 'loaded-characters.json'), packed)
        base = packed['anchors']
    extra = load(args.pack / 'A-metadata.json')
    more = np.fromfile(args.pack / 'A-anchors.f32', dtype='<f4').reshape(-1, 768)
    if len(extra) != len(more) or len({e['id'] for e in entries + extra}) != len(entries) + len(extra):
        raise ValueError('索引与长尾包条目数或身份不符')
    entries += extra
    tag = {e['id']: canonical(e['tag'], aliases) for e in entries}
    directory = args.directory / ('score-' + args.features.name)
    directory.mkdir(exist_ok=True)
    offsets, vectors = {}, []
    start = 0
    for row in rows:
        i = row['id']
        offsets[i] = start
        matrix = data[i]['current']
        vectors.append(matrix)
        start += len(matrix)
    matrix_file(directory / 'views.f32', np.concatenate(vectors))
    output = directory / 'claims.tsv'
    receipt = native_scan(args.native, directory, entries, np.concatenate((base, more)), directory / 'views.f32', output)
    if receipt['live_claim_difference_images']:
        raise ValueError('批量扫描与生产单图认领不一致')
    found = [set() for _ in range(start)]
    with output.open() as stream:
        for i, _, entity, distance, threshold, _ in csv.reader(stream, delimiter='\t'):
            if np.float32(threshold) > 0 and np.float32(distance) <= np.float32(threshold):
                found[int(i)].add(tag[entity])

    def select(i, _, layer):
        offset = offsets[i]
        return set().union(*found[offset + layer.start:offset + layer.stop])

    return predict_views(rows, data, select), receipt, set(tag.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--features', type=Path, required=True)
    parser.add_argument('--native', type=Path, required=True, help='#534 生产锚点扫描驱动')
    parser.add_argument('--aliases', type=Path, required=True)
    parser.add_argument('--index', type=Path, required=True)
    parser.add_argument('--pack', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--allow-partial', action='store_true', help='仅用于验证未完成的提取流水线')
    args = parser.parse_args()
    sample = load(args.directory / 'sample.json')
    references = [r for r in sample if r['cohort'] == 'reference']
    rows = [r for r in sample if r['cohort'] != 'reference']
    check_disjoint(references, rows)
    anchors = load(args.directory / 'anchors.json')
    receipts = {r['id']: r for r in load(args.directory / 'downloads.json')}
    data = read_features(sample, args.features, receipts)
    available = {i for i, receipt in receipts.items() if receipt['source'] not in ('unavailable', 'network_error')}
    if not args.allow_partial and set(data) != available:
        raise ValueError(f'仍有 {len(available - data.keys())} 张可用图片缺特征')
    rows = [r for r in rows if r['id'] in data]
    if not rows:
        raise ValueError('没有可评分的评测图')
    aliases = load(args.aliases)
    predictions, receipt, all_roles = published_predictions(args, rows, data, aliases)
    result = {'native': receipt, 'available': len(data), 'evaluation': len(rows), 'shared_panels': {}}
    tail = set(anchors)
    for name, roster in [('published_all', all_roles), ('published_longtail', set(anchors))]:
        result[name] = {k: measure(rows, receipts, p, roster, predictions['whole']) for k, p in predictions.items()}
    # 带完整精选索引的实际候选只从整图取，额外裁图只补长尾角色。
    result['whole_index_with_crop_longtail'] = {}
    for kind in ('whole', 'whole+person', 'whole+head', 'whole+person_if_multiple', 'whole+head_if_multiple'):
        rollout = {i: original | (predictions[kind][i] & tail) for i, original in predictions['whole'].items()}
        result['whole_index_with_crop_longtail'][kind] = measure(rows, receipts, rollout, all_roles, predictions['whole'])
    for crop in ('person', 'head'):
        training = shared_reference_ids(anchors, references, data, crop)
        panel = {'reference_counts': {k: len(v) for k, v in training.items()}}
        baseline = None
        for model in MODELS:
            if training:
                preds = panel_predictions(rows, model, crop, training, data)
                if model == 'current':
                    baseline = preds['whole']
                panel[model] = {k: measure(rows, receipts, p, set(training), baseline) for k, p in preds.items()}
        result['shared_panels'][crop] = panel
    save(args.output, result)
    print(f'已评分 {len(rows)} 张，生产索引扫描 {receipt["features"]} 个视图')


if __name__ == '__main__':
    main()
