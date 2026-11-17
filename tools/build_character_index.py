"""从固定版本的 DeepGHS Character Index 制作基础角色索引包（#607）。

仅用于构建模型 Release。用法：

    python tools/build_character_index.py 上游目录 character-index-v1.bin [--compare-534 index.npz]

上游目录放固定提交 5ee6075ff09f82c27e23f33044b9f9cabaf1e8c3 的 `characters.json`、文件树 `tree.json`
（Hugging Face 的 `api/datasets/deepghs/character_index/revision/<提交>?blobs=true`）和
`data/<hprefix>/<short_tag>/feat.npy`。脚本逐一核对元数据、文件树和每个特征文件的 SHA-256，不联网。

公式与 #534 的装载和生产一致：每个角色把参考向量归一化后等权平均、再乘回平均模长，得到锚点；参考到锚点的
CCIP 距离取 p50、p75、p90。共用 short_tag 的四对按 (short_tag, -post_count, tag) 排序取第一项。
包内只放锚点、阈值和角色元数据，不放原始参考向量。

包是不压缩的 ZIP，成员顺序、时间戳和属性固定，同一输入和同一 NumPy 版本重复运行逐字节相同。
用 Python 3.12 和 NumPy 2.5.3 制作。`--compare-534` 与 #534 留下的装载结果比较锚点和阈值（ID 和顺序一致，数值差不超过 1e-6）。
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
import platform
import zipfile

import numpy as np

REVISION = "5ee6075ff09f82c27e23f33044b9f9cabaf1e8c3"
VERSION = "character_index@2025-09-25"
FORMAT = "illustash-character-index/1"
METADATA_SHA256 = "77aa16e88b463473843e92fa44c8f3146cb0ef996caaaee9a38c8f762ccd3339"
METADATA_COUNT = 7371
CHARACTERS = 7367
DIMENSION = 768
# 应用识别用的 CCIP 特征模型（native/terms/models.json 的 ccip/model_feat.onnx）。
CCIP_SHA256 = "4ea118d16496274f4f6e08d3afc768cc592389e8f7f32f8732ce2215c228ac5f"
LICENSE = Path(__file__).resolve().parent.parent / "packaging/model-licenses/wd-extension/Character-Index-MIT.txt"
NOTICE = """Illustash 基础角色索引 v1（character-index-v1）

由 DeepGHS Character Index（https://huggingface.co/datasets/deepghs/character_index，
固定提交 5ee6075ff09f82c27e23f33044b9f9cabaf1e8c3，MIT）的角色元数据和 CCIP 参考特征计算得到：
每个角色一个锚点和三档距离阈值。不含原始参考向量和图片。原图版权归各原作者；
Character Index 的许可与归属见 LICENSE-Character-Index.txt。制作脚本：tools/build_character_index.py。
"""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def unit(matrix):
    norms = np.sqrt(np.sum(matrix * matrix, axis=1, keepdims=True))
    return matrix / np.maximum(norms, 1e-12)


def reference_stats(rows):
    """生产公式：等权单位向量中心、平均模长和距离分位数（与 #534 的 index.py 相同）。"""
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


def feature_path(row):
    return f'{row["hprefix"]}/{row["short_tag"]}/feat.npy'


def load_inputs(source):
    metadata = (source / "characters.json").read_bytes()
    if sha(metadata) != METADATA_SHA256:
        raise ValueError("characters.json 与固定版本不符")
    tree_bytes = (source / "tree.json").read_bytes()
    tree = json.loads(tree_bytes)
    if tree.get("sha") != REVISION:
        raise ValueError("文件树不是固定提交")
    lfs = {row["rfilename"]: row["lfs"] for row in tree["siblings"] if "lfs" in row}
    rows = sorted(json.loads(metadata), key=lambda row: (row["short_tag"], -row["post_count"], row["tag"]))
    if len(rows) != METADATA_COUNT or len({feature_path(row) for row in rows}) != len(rows):
        raise ValueError("上游角色清单条数不符")
    for row in rows:
        short = row["short_tag"]
        if not short or not all(c.isascii() and (c.isalnum() or c == "_") for c in short):
            raise ValueError(f"short_tag 不能作为索引角色 ID：{short!r}")
        if feature_path(row) not in lfs:
            raise ValueError(f"文件树缺少特征文件：{feature_path(row)}")
    return metadata, tree_bytes, lfs, rows


def build(source):
    metadata, tree_bytes, lfs, rows = load_inputs(source)
    entries, anchors, seen, reference_rows, feature_bytes = [], [], set(), 0, 0
    for row in rows:
        name = feature_path(row)
        content = (source / "data" / name).read_bytes()
        if len(content) != lfs[name]["size"] or sha(content) != lfs[name]["sha256"]:
            raise ValueError(f"特征文件与固定版本不符：{name}")
        features = np.load(io.BytesIO(content), allow_pickle=False)
        if (features.dtype.kind != "f" or features.ndim != 2 or features.shape[1] != DIMENSION
                or len(features) < 3 or not np.isfinite(features).all()):
            raise ValueError(f"特征数组不符合参考契约：{name}")
        reference_rows += len(features)
        feature_bytes += len(content)
        if row["short_tag"] in seen:
            continue
        seen.add(row["short_tag"])
        anchor, limits = reference_stats(features)
        anchors.append(anchor)
        entries.append({
            "short_tag": row["short_tag"],
            "tag": row["tag"],
            "copyright": row["copyright"],
            "copyrights": row["copyrights"],
            "core_tags": row["core_tags"],
            "gender": row["gender"],
            "post_count": row["post_count"],
            "n_ref": len(features),
            "anchor_p50": float(limits[0]),
            "anchor_p75": float(limits[1]),
            "anchor_p90": float(limits[2]),
        })
    if len(entries) != CHARACTERS:
        raise ValueError(f"唯一角色数应为 {CHARACTERS}，实际 {len(entries)}")
    matrix = np.ascontiguousarray(np.stack(anchors), dtype="<f4")
    characters = (json.dumps(entries, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
    packed = matrix.tobytes()
    license_text = LICENSE.read_bytes()
    notice = NOTICE.encode()
    files = {"characters.json": characters, "anchors.f32": packed,
             "LICENSE-Character-Index.txt": license_text, "NOTICE.txt": notice}
    provenance = {
        "format": FORMAT,
        "version": VERSION,
        "characters": len(entries),
        "dimension": DIMENSION,
        "ccip_sha256": CCIP_SHA256,
        "upstream": {
            "dataset": "deepghs/character_index",
            "revision": REVISION,
            "license": "MIT",
            "characters_sha256": sha(metadata),
            "tree_sha256": sha(tree_bytes),
            "feature_files": len(rows),
            "feature_bytes": feature_bytes,
            "reference_rows": reference_rows,
        },
        "formula": "anchor = mean(unit(refs)) / |mean(unit(refs))| * mean(|refs|); "
                   "thresholds = percentile((1 - unit(refs) . unit(anchor)) / 2, [50, 75, 90]); "
                   "duplicate short_tag: first by (short_tag, -post_count, tag)",
        "tool": {"script": "tools/build_character_index.py", "python": platform.python_version(),
                 "numpy": np.__version__},
        "files": {name: sha(data) for name, data in sorted(files.items())},
    }
    files["provenance.json"] = (json.dumps(provenance, ensure_ascii=False, indent=2) + "\n").encode()
    return files, entries, matrix, provenance


def archive(files):
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", zipfile.ZIP_STORED) as package:
        for name in ("provenance.json", "characters.json", "anchors.f32", "LICENSE-Character-Index.txt", "NOTICE.txt"):
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_STORED
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            package.writestr(info, files[name])
    return output.getvalue()


def compare_534(path, entries, matrix):
    """与 #534 的装载结果比较。#534 在另一台机器（Colab）上算，NumPy 和 BLAS 不同，末位可能不同：
    角色 ID 和顺序必须一致，锚点和阈值报告最大差值，超过 1e-6 视为不一致。"""
    with np.load(path, allow_pickle=False) as frozen:
        ids, anchors, limits = frozen["ids"].tolist(), frozen["anchors"], frozen["limits"]
    if ids != [entry["short_tag"] for entry in entries]:
        raise ValueError("角色 ID 与 #534 装载结果不一致")
    ours = np.array([[e["anchor_p50"], e["anchor_p75"], e["anchor_p90"]] for e in entries], np.float32)
    anchor_diff = float(np.max(np.abs(anchors.astype(np.float64) - matrix)))
    limit_diff = float(np.max(np.abs(limits.astype(np.float64) - ours)))
    if anchor_diff > 1e-6 or limit_diff > 1e-6:
        raise ValueError(f"与 #534 装载结果相差过大：锚点 {anchor_diff}，阈值 {limit_diff}")
    return {"characters": len(ids), "anchors_identical": bool(np.array_equal(anchors, matrix)),
            "limits_identical": bool(np.array_equal(limits, ours)),
            "anchor_max_abs_diff": anchor_diff, "limit_max_abs_diff": limit_diff}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--compare-534", type=Path, help="#534 装载结果 index.npz")
    args = parser.parse_args()
    files, entries, matrix, provenance = build(args.source)
    data = archive(files)
    report = {"package": args.output.name, "bytes": len(data), "sha256": sha(data), "provenance": provenance}
    if args.compare_534:
        report["compare_534"] = compare_534(args.compare_534, entries, matrix)
    temporary = args.output.with_name(args.output.name + ".tmp")
    temporary.write_bytes(data)
    temporary.replace(args.output)
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
