"""复用 #380 与 #534 冻结的别名、分位数和原生扫描接口，仅依赖 NumPy。"""


import re
import subprocess

import numpy as np

from sample import load, save


def canonical(tag, aliases):
    seen = set()
    while tag in aliases:
        if tag in seen:
            raise ValueError(f"别名成环：{tag}")
        seen.add(tag)
        tag = aliases[tag]
    return tag


def unit(matrix):
    norms = np.sqrt(np.sum(matrix * matrix, axis=1, keepdims=True))
    return matrix / np.maximum(norms, 1e-12)


def reference_stats(rows):
    """复现生产的等权单位向量中心、平均模长和距离分位数。"""
    lens = np.sqrt(np.sum(rows * rows, axis=1, keepdims=True))
    if not np.isfinite(lens).all() or (lens == 0).any():
        raise ValueError("参考模长为零或非有限数")
    units = rows / lens
    center = units.mean(axis=0)
    normalized = unit(center[None, :])[0]
    if not normalized.any():
        raise ValueError("中心模长为零")
    anchor = normalized * lens.mean()
    distances = (1 - units @ unit(anchor[None, :])[0]) / 2
    limits = np.percentile(distances, np.array([50, 75, 90], dtype=rows.dtype))
    return anchor.astype(np.float32), limits.astype(np.float32)


def skin(tag, source_tags):
    base = re.sub(r"_?\([^()]+\)", "", tag).strip("_")
    parts = re.findall(r"\(([^()]+)\)", tag)
    return len(parts) > 1 or (len(parts) == 1 and base in source_tags and base != tag)


def baseline_entries(meta, packed):
    loaded = {row["short_tag"]: row for row in meta}
    source_tags = {row["tag"] for row in meta if "(" not in row["tag"]}
    entries = []
    for name, limits in zip(packed["ids"].tolist(), packed["limits"]):
        row = loaded[name]
        entries.append(
            dict(id=name, tag=row["tag"], skin=skin(row["tag"], source_tags),
                 post_count=row["post_count"], gender_conf=max(row.get("gender", {}).values(), default=0),
                 p50=float(limits[0]), p75=float(limits[1]), p90=float(limits[2]), patched=False)
        )
    if len(loaded) != len(entries):
        raise ValueError("载入的上游角色与锚点不匹配")
    return entries


def matrix_file(path, vectors):
    np.asarray(vectors, dtype="<f4", order="C").tofile(path)


def native_scan(native, folder, entries, vectors, features_file, out_file):
    location = folder / out_file.stem
    location.mkdir(exist_ok=True)
    order = sorted(range(len(entries)), key=lambda i: entries[i]["id"])
    save(location / "entries.json", [entries[i] for i in order])
    matrix_file(location / "anchors.f32", vectors[order])
    subprocess.run([str(native.resolve()), str(location), str(features_file), str(out_file)], check=True)
    return load(out_file.with_suffix(".json"))
