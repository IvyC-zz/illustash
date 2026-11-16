"""从 #534 固定实验产物生成待验收的 228 个 WD 扩展头。"""
import argparse
import hashlib
import json
from pathlib import Path

import numpy as np


def sha(data):
    return hashlib.sha256(data).hexdigest()


def expect(data, digest):
    if sha(data) != digest:
        raise ValueError("输入与 #534 冻结产物不符")
    return data


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("experiment", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()

    evaluation = args.experiment / "evaluation"
    coverage = json.loads((evaluation / "A-coverage.json").read_bytes())
    roster = json.loads((args.experiment / "sample/roster.json").read_bytes())
    if set(coverage) != {row["name"] for row in roster} or len(coverage) != 500:
        raise ValueError("目标名单与实验不符")
    outside = {tag for tag, status in coverage.items() if status["state"] != "covered_by_loaded_curated_reference"}
    thresholds = expect((evaluation / "thresholds.json").read_bytes(),
                        "3ece000d79d0f620beb68f3a2a40a97e850d3b70e710aa37485a43affb8b5ff5")
    threshold = json.loads(thresholds)["B_calibration_1pct"]

    heads = evaluation / "B-heads.npz"
    expect(heads.read_bytes(), "5bc45fcc05e039270aa0261bc1e6a4687c707abe743f00e63ed77fe57b620028")
    with np.load(heads, allow_pickle=False) as archive:
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
    header = json.dumps(manifest, ensure_ascii=False, separators=(",", ":")).encode()
    data = header + b"\n" + weight + bias
    with args.output.open("xb") as output:
        output.write(data)
    print(json.dumps({"package_sha256": sha(data),
                      "labels_sha256": sha(json.dumps(names, ensure_ascii=False, separators=(",", ":")).encode()),
                      "weight_sha256": sha(weight), "bias_sha256": sha(bias), "bytes": len(data)}))


if __name__ == "__main__":
    main()
