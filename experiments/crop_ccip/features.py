"""在本机从整图、人物框和头部框提取同一组图片的两种 CCIP 特征。"""
import argparse
from collections import Counter
import hashlib
import json
import os
from pathlib import Path
import platform
import time

import numpy as np
import onnxruntime as ort
from PIL import Image

MODELS = {
    'current': '4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f',
    'b36': 'c1e7333a55c2ad9e03cd340c635e96c9c1d86f0836fa7eae65720e7c9c94ee51',
    'person': 'c80365a98ca8388fd2f01fd5abea0439e92265bceceb048bef38b0849303467c',
    'head': '6679f9b71192298bbf174d82e9e5581c3237b0c3dc67deace7cdbf686b070a00',
}


PROVIDERS = {'cpu': 'CPUExecutionProvider', 'coreml': 'CoreMLExecutionProvider', 'dml': 'DmlExecutionProvider'}


def load(path):
    return json.loads(path.read_bytes())


def save(path, value):
    partial = path.with_suffix('.partial.json')
    partial.write_text(json.dumps(value, ensure_ascii=False, separators=(',', ':')), encoding='utf-8')
    partial.replace(path)


def session(path, device, adapter, profile):
    options = ort.SessionOptions()
    options.intra_op_num_threads = 2
    options.inter_op_num_threads = 1
    if profile is not None:
        options.enable_profiling = True
        options.profile_file_prefix = str(profile / path.stem)
    provider = PROVIDERS[device]
    providers = ['CPUExecutionProvider']
    if device != 'cpu':
        if provider not in ort.get_available_providers():
            raise RuntimeError(f'{provider} 未安装，当前可用：{ort.get_available_providers()}')
        options.add_free_dimension_override_by_name('batch', 1)
        if path.stem in ('person', 'head'):
            options.add_free_dimension_override_by_name('height', 640)
            options.add_free_dimension_override_by_name('width', 640)
        if device == 'coreml':
            cache = path.parent / 'cache' / path.stem
            cache.mkdir(parents=True, exist_ok=True)
            flags = {'ModelFormat': 'MLProgram', 'RequireStaticInputShapes': '1',
                     'MLComputeUnits': 'ALL', 'ModelCacheDirectory': str(cache)}
        else:
            options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
            options.enable_mem_pattern = False
            flags = {'device_id': str(adapter)}
        providers.insert(0, (provider, flags))
    start = time.perf_counter()
    model = ort.InferenceSession(str(path), options, providers=providers)
    if model.get_providers()[0] != provider:
        raise RuntimeError(f'{path.stem} 未启用 {provider}')
    model.disable_fallback()
    print(f'{path.stem} {model.get_providers()} 初始化 {time.perf_counter()-start:.2f}s', flush=True)
    return model


class Detector:
    def __init__(self, model, iou):
        self.model = model
        meta = self.model.get_modelmeta().custom_metadata_map
        self.size = tuple(json.loads(meta.get('imgsz', '[640, 640]')))
        self.iou = iou

    def __call__(self, image):
        # 沿用 #456 粗测：拉伸到模型输入尺寸，CHW、0–1 浮点，NMS 后换回原图坐标。
        width, height = self.size
        data = np.asarray(image.resize((width, height)), dtype=np.float32).transpose(2, 0, 1)[None] / 255
        output, = self.model.run(['output0'], {'images': np.ascontiguousarray(data)})
        output = output[0]
        if output.shape[0] != 5:
            raise ValueError(f'检测输出形状不符：{output.shape}')
        keep = output[4] >= 0.348
        boxes, scores = output[:4, keep].T, output[4, keep]
        if not len(scores):
            return []
        xyxy = np.stack([boxes[:, 0] - boxes[:, 2] / 2, boxes[:, 1] - boxes[:, 3] / 2,
                         boxes[:, 0] + boxes[:, 2] / 2, boxes[:, 1] + boxes[:, 3] / 2], axis=1)
        chosen = nms(xyxy, scores, self.iou)
        sx, sy = image.width / width, image.height / height
        result = []
        for i in chosen:
            x0, y0, x1, y1 = xyxy[i]
            box = (int(np.clip(x0 * sx, 0, image.width).round()), int(np.clip(y0 * sy, 0, image.height).round()),
                   int(np.clip(x1 * sx, 0, image.width).round()), int(np.clip(y1 * sy, 0, image.height).round()))
            result.append((box, float(scores[i])))
        return result


def nms(boxes, scores, threshold):
    x1, y1, x2, y2 = boxes.T
    areas = (x2 - x1 + 1) * (y2 - y1 + 1)
    order = scores.argsort()[::-1]
    keep = []
    while order.size:
        i = order[0]
        keep.append(int(i))
        w = np.maximum(0.0, np.minimum(x2[i], x2[order[1:]]) - np.maximum(x1[i], x1[order[1:]]) + 1)
        h = np.maximum(0.0, np.minimum(y2[i], y2[order[1:]]) - np.maximum(y1[i], y1[order[1:]]) + 1)
        inter = w * h
        iou = inter / (areas[i] + areas[order[1:]] - inter)
        order = order[np.flatnonzero(iou <= threshold) + 1]
    return keep


def expand(box, ratio, width, height):
    x0, y0, x1, y1 = box
    dx, dy = (x1 - x0) * ratio, (y1 - y0) * ratio
    return (max(0, int(round(x0 - dx))), max(0, int(round(y0 - dy))),
            min(width, int(round(x1 + dx))), min(height, int(round(y1 + dy))))



def ccip_input(image):
    mean = np.array([0.48145466, 0.4578275, 0.40821073], dtype=np.float32)
    std = np.array([0.26862954, 0.26130258, 0.27577711], dtype=np.float32)
    resized = image.resize((384, 384), Image.Resampling.BILINEAR)
    values = (np.asarray(resized, dtype=np.float32) / 255 - mean) / std
    return np.ascontiguousarray(values.transpose(2, 0, 1)[None])


def crop_boxes(detector, image, ratio):
    boxes = []
    for box, _ in detector(image):
        x0, y0, x1, y1 = expand(box, ratio, *image.size)
        if min(x1 - x0, y1 - y0) >= 64:
            boxes.append((x0, y0, x1, y1))
        if len(boxes) == 6:
            break
    return boxes


def infer(image, detectors, encoders):
    views = [image]
    timing = {}
    saved = {}
    for name, detector, ratio in detectors:
        start = time.perf_counter()
        boxes = crop_boxes(detector, image, ratio)
        timing[name] = time.perf_counter() - start
        views.extend(image.crop(rect) for rect in boxes)
        saved[name] = np.asarray(boxes, dtype=np.int32).reshape(-1, 4)
    prepared = []
    for i, view in enumerate(views):
        kind = 'whole' if i == 0 else 'person' if i <= len(saved['person']) else 'head'
        start = time.perf_counter()
        prepared.append((kind, ccip_input(view)))
        timing[f'preprocess_{kind}'] = timing.get(f'preprocess_{kind}', 0.0) + time.perf_counter() - start
    for name, encoder in encoders.items():
        output = []
        input_name = encoder.get_inputs()[0].name
        for kind, inputs in prepared:
            start = time.perf_counter()
            result, = encoder.run(None, {input_name: inputs})
            duration = time.perf_counter() - start
            timing[f'{name}_{kind}'] = timing.get(f'{name}_{kind}', 0.0) + duration
            output.append(result[0])
        values = np.asarray(output, dtype=np.float32)
        if values.shape != (len(views), 768) or not np.isfinite(values).all():
            raise ValueError(f'{name} 特征形状或数值不正确：{values.shape}')
        saved[name] = values
    return saved, timing


def timing_sample(samples, source, per_group):
    groups = {'person': Counter(), 'head': Counter()}
    picked = []
    # 固定散列顺序，避免全按最早的帖子挑图。
    rows = sorted(samples, key=lambda row: hashlib.sha256(f"issue456-timing:{row['id']}".encode()).digest())
    for row in rows:
        feature = source / f"{row['id']}.npz"
        if not feature.is_file():
            continue
        with np.load(feature, allow_pickle=False) as old:
            counts = {name: len(old[name]) for name in groups}
        if any(groups[name][count] < per_group for name, count in counts.items()):
            picked.append(row)
            for name, count in counts.items():
                groups[name][count] += 1
    return picked


def image_for(images, i):
    receipt = load(images / f'{i}.json')
    with (images / str(i)).open('rb') as source:
        if hashlib.file_digest(source, 'sha256').hexdigest() != receipt['sha256']:
            raise ValueError('图片与下载收据不符')
        source.seek(0)
        with Image.open(source) as decoded:
            if list(decoded.size) != receipt['size']:
                raise ValueError('图片尺寸与下载收据不符')
            return decoded.convert('RGB')


def summarize_profiles(sessions):
    profiles = {}
    for name, model in sessions.items():
        path = Path(model.end_profiling())
        events = load(path)
        counters = Counter(x['args']['provider'] for x in events
                           if x.get('cat') == 'Node' and 'provider' in x.get('args', {}))
        micros = Counter()
        for event in events:
            if event.get('cat') == 'Node' and 'provider' in event.get('args', {}):
                micros[event['args']['provider']] += event.get('dur', 0)
        profiles[name] = {'kernel_calls': dict(counters), 'kernel_us': dict(micros)}
        path.unlink()
    return profiles


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--device', choices=PROVIDERS, default='cpu')
    parser.add_argument('--adapter', type=int, default=0, help='DirectML 的 DXGI 适配器序号')
    parser.add_argument('--limit', type=int)
    parser.add_argument('--target', type=Path)
    parser.add_argument('--ids', type=int, nargs='+')
    parser.add_argument('--timing', action='store_true', help='按两种检测框数分层，从已有特征的图片中挑图重测')
    parser.add_argument('--boxes-from', type=Path)
    parser.add_argument('--timing-per-group', type=int, default=6)
    parser.add_argument('--profile', action='store_true', help='汇总各模型实际执行的 ONNX Runtime 后端')
    args = parser.parse_args()
    if args.adapter < 0 or args.timing_per_group < 1 or (args.limit is not None and args.limit < 1):
        parser.error("适配器序号至少为 0，数量限制至少为 1")
    work = args.directory.resolve()
    images = work / 'images'
    target = args.target or work / ('features' if args.device == 'cpu' else f'features-{args.device}')
    target.mkdir(parents=True, exist_ok=True)
    samples = sorted(load(work / 'sample.json'), key=lambda r: (r['cohort'] != 'positive', r['id']))
    if args.ids:
        ids = set(args.ids)
        if unknown := ids - {row['id'] for row in samples}:
            raise ValueError(f'样本清单没有 {len(unknown)} 个指定 ID')
        samples = [row for row in samples if row['id'] in ids]
    eligible = [row for row in samples if (images / f"{row['id']}.json").is_file()]
    rows = timing_sample(eligible, args.boxes_from or target, args.timing_per_group) if args.timing else eligible
    if not rows:
        raise ValueError('没有可提取或可计时的图片')
    profiles = target if args.profile else None
    for name, expected in MODELS.items():
        with (work / 'models' / f'{name}.onnx').open('rb') as f:
            if hashlib.file_digest(f, 'sha256').hexdigest() != expected:
                raise ValueError(f'模型身份不符：{name}')
    sessions = {name: session(work / 'models' / f'{name}.onnx', args.device, args.adapter, profiles)
                for name in MODELS}
    detectors = [(name, Detector(sessions[name], iou), pad)
                 for name, iou, pad in [('person', 0.5, 0.10), ('head', 0.7, 0.50)]]
    encoders = {name: sessions[name] for name in ['current', 'b36']}
    timed, failures, finished = [], [], set()
    started = time.perf_counter()
    if args.timing:
        # 完整走过一张图、每种裁图和编码会话，预热期单独计时。
        warm = max(rows, key=lambda row: (args.boxes_from or target).joinpath(f"{row['id']}.npz").stat().st_size)
        infer(image_for(images, warm['id']), detectors, encoders)
    for row in rows:
        if args.limit is not None and len(timed) >= args.limit:
            break
        i = row['id']
        output = target / f'{i}.npz'
        if not args.timing and output.is_file():
            finished.add(i)
            continue
        before = time.perf_counter()
        try:
            image = image_for(images, i)
        except (OSError, ValueError, KeyError) as error:
            failures.append(dict(id=i, stage='decode', kind=type(error).__name__))
            continue
        decode = time.perf_counter() - before
        data, seconds = infer(image, detectors, encoders)
        timed.append(dict(id=i, persons=len(data['person']), heads=len(data['head']),
                          decode_verify=decode, wall=time.perf_counter()-before, **seconds))
        if not args.timing:
            partial = output.with_suffix('.partial.npz')
            np.savez(partial, **data)
            partial.replace(output)
        finished.add(i)
        if len(timed) <= 5 or len(timed) % 10 == 0:
            print(f'{len(timed)} 张新结果，耗时 {time.perf_counter()-started:.1f}s，最近一张 {timed[-1]["wall"]:.2f}s',
                  flush=True)
    metadata = dict(device=args.device, adapter=args.adapter, os=platform.system(), architecture=platform.machine(),
                    runtime=ort.__version__, timings=timed, failures=failures, eligible=len(eligible),
                    coverage=dict(Counter(row['cohort'] for row in eligible if row['id'] in finished)))
    if args.profile:
        metadata['profile'] = summarize_profiles(sessions)
    report = work / f'timing-{args.device}.json' if args.timing else target / 'run.json'
    save(report, metadata)
    print(f'{args.device} 已提取或计时 {len(timed)} 张，跳过 {len(failures)} 张读取失败', flush=True)
    if args.device == 'coreml':
        # 此版运行库销毁 Core ML 会话时可能触发 recursive_mutex 异常；文件已提交，直接回收进程。
        os._exit(0)


if __name__ == '__main__':
    main()
