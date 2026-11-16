"""从 #534 的冻结产物生成 WD 角色扩展 v1（228 个高阈值角色头）。

仅用于构建模型 Release。用法为 python tools/build_wd_extension.py 实验目录 b-534.bin。
使用 Python 3.12 和 NumPy 2.5.3 复封。实验目录包含 FROZEN_INPUTS 中登记的四个文件。
输入来自 2026-11-16 冻结的公开角色库存和评测。Danbooru 帖子 Parquet 固定在
ebb02a630201c7b51487e45fb90b3fcf4cbedc20（截至 2026-07-13 UTC），API 补查限定到
2026-11-15，DeepGHS Character Index 固定在 5ee6075ff09f82c27e23f33044b9f9cabaf1e8c3。
WD 向量来自 DeepGHS 的 4ec31f2aeabf66353f3ee3ccd20d2f07e716851b 导出版本。
脚本筛选并封装已训练好的分类头，逐一核验输入的 SHA-256，核验输出大小和 SHA-256 后写出。
"""
import argparse
import hashlib
import io
import json
from pathlib import Path

import numpy as np


FROZEN_INPUTS = {
    "evaluation/A-coverage.json": "3531cddeaf0148cbaa93ef0c3643d6cd5ade00adda04f09de5def27807a95040",
    "sample/roster.json": "38ee7dda68563b3800e76d791905c19e844db4eecf43fbc8c8821704484d1344",
    "evaluation/thresholds.json": "3ece000d79d0f620beb68f3a2a40a97e850d3b70e710aa37485a43affb8b5ff5",
    "evaluation/B-heads.npz": "5bc45fcc05e039270aa0261bc1e6a4687c707abe743f00e63ed77fe57b620028",
}
OUTPUT_BYTES = 940_701
OUTPUT_SHA256 = "9da0365457e479eff189150926289c6cf8325cd9bb98c0bd2504c26d7dcae471"


def sha(data):
    return hashlib.sha256(data).hexdigest()


def frozen_input(experiment, relative_path):
    data = (experiment / relative_path).read_bytes()
    if sha(data) != FROZEN_INPUTS[relative_path]:
        raise ValueError(f"输入与 #534 冻结产物不符：{relative_path}")
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("experiment", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    coverage = json.loads(frozen_input(args.experiment, "evaluation/A-coverage.json"))
    roster = json.loads(frozen_input(args.experiment, "sample/roster.json"))
    if set(coverage) != {row["name"] for row in roster} or len(coverage) != 500:
        raise ValueError("目标名单与实验不符")
    outside = {tag for tag, status in coverage.items() if status["state"] != "covered_by_loaded_curated_reference"}
    thresholds = frozen_input(args.experiment, "evaluation/thresholds.json")
    threshold = json.loads(thresholds)["B_calibration_1pct"]

    heads = frozen_input(args.experiment, "evaluation/B-heads.npz")
    with np.load(io.BytesIO(heads), allow_pickle=False) as archive:
        labels = archive["labels"].tolist()
        keep = [idx for idx, name in enumerate(labels) if name in outside]
        if (len(outside), len(labels), len(keep)) != (251, 433, 228):
            raise ValueError("扩展名单未得到 228 个头")
        names = [labels[idx] for idx in keep]
        weight = archive["weight"][keep].astype("<f4").tobytes()
        bias = archive["bias"][keep].astype("<f4").tobytes()
    manifest = {
        "format": "illustash-wd-extension/1",
        "wd_model_sha256": "ae25f5f12b2d5891728fb7768fa63d65bf13a83d6d4823e3a1bf7b42875c74ff",
        "wd_tags_sha256": "298633d94d0031d2081c0893f29c82eab7f0df00b08483ba8f29d1e979441217",
        "threshold": threshold,
        "labels": names,
    }
    encoded_names = json.dumps(names, ensure_ascii=False, separators=(",", ":")).encode()
    header = json.dumps(manifest, ensure_ascii=False, separators=(",", ":")).encode()
    data = header + b"\n" + weight + bias
    digest = sha(data)
    if len(data) != OUTPUT_BYTES or digest != OUTPUT_SHA256:
        raise ValueError("输出大小或 SHA-256 与 WD 角色扩展 v1 不符")
    with args.output.open("xb") as output:
        output.write(data)
    print(json.dumps({"package_sha256": digest, "labels_sha256": sha(encoded_names),
                      "weight_sha256": sha(weight), "bias_sha256": sha(bias), "bytes": len(data)}))


if __name__ == "__main__":
    main()
