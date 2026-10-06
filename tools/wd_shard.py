#!/usr/bin/env python3
"""WD v3 tagger 分片推理 —— 可中断、可跨机续跑。

设计要点（为了「Mac 跑一半 → 停下 → Windows 接着跑」）：
    · 按 manifest 的文件名排序后切固定大小的分片，两台机器看到的分片完全一致
    · 每片独立产出 shards/shard-NNNN.csv，完成后写 .done 标记
    · 已存在 .done 的分片直接跳过 —— 换机器、重跑都安全
    · 先写 .tmp 再 rename，中途被杀不会留下半截文件

输出统一格式（两台机器完全一致，可直接 cat 合并）：
    file,dim,value,conf

预处理严格照 imgutils 源码：
    白边补方 -> BICUBIC 448 -> float32 [0,255] 不归一化 -> RGB->BGR -> NHWC
    output 已经是 sigmoid 后的概率，不要再套 sigmoid（见 ERRATA.md 第 4 条）

用法：
    # Mac
    .venv/bin/python tools/wd_shard.py --shard 0 --out review/shards
    # Windows
    python wd_shard.py --shard 0 --out E:\\photo-intel\\shards --img-dir E:\\photo-intel\\full\\imgs
    # 看进度
    .venv/bin/python tools/wd_shard.py --status --out review/shards
"""
import argparse
import csv
import os
import sys
import time

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DEFAULT_MANIFEST = os.path.join(ROOT, "review", "full-manifest.csv")
DEFAULT_TAGS = os.path.join(ROOT, "tags", "tags_v3.csv")
DEFAULT_MODEL = os.path.join(ROOT, "models", "wd_swinv2_v3_emb.onnx")

CHAR_TH = 0.001     # 角色标签存盘阈值（全存策略，2026-09-21 主人定）
RATING_TH = 0.001   # rating 存盘阈值（全存）
GEN_TH = 0.001      # general 存盘阈值（全存）


def load_tags(path):
    names, cats = [], []
    with open(path, encoding="utf-8") as fh:
        for r in csv.DictReader(fh):
            names.append(r["name"])
            cats.append(int(r["category"]))
    return names, np.asarray(cats, dtype=np.int32)


def all_files(manifest):
    """按文件名排序 —— 两台机器必须得到同一个顺序"""
    with open(manifest, encoding="utf-8") as fh:
        return sorted(r["file"] for r in csv.DictReader(fh))


def src_map(manifest):
    with open(manifest, encoding="utf-8") as fh:
        return {r["file"]: r["src"] for r in csv.DictReader(fh)}


def prep(path, size=448):
    from PIL import Image
    with Image.open(path) as im:
        im = im.convert("RGB")
        w, h = im.size
        m = max(w, h)
        if m != size or (w, h) != (size, size):
            canvas = Image.new("RGB", (m, m), (255, 255, 255))
            canvas.paste(im, ((m - w) // 2, (m - h) // 2))
            if m != size:
                canvas = canvas.resize((size, size), Image.BICUBIC)
            im = canvas
        a = np.asarray(im, dtype=np.float32)[:, :, ::-1]   # RGB -> BGR
    return np.ascontiguousarray(a[None])


def show_status(out):
    files = all_files(DEFAULT_MANIFEST)
    dones = [f for f in os.listdir(out) if f.endswith(".done")] if os.path.isdir(out) else []
    n = 0
    for f in dones:
        try:
            n += int(open(os.path.join(out, f)).read().split()[0])
        except Exception:
            pass
    print(f"分片目录 {out}")
    print(f"  完成分片 {len(dones)}  已处理 {n} / {len(files)}  ({n/len(files):.1%})")
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--shard", type=int, default=-1)
    ap.add_argument("--size", type=int, default=1000, help="每个分片多少张")
    ap.add_argument("--out", default=os.path.join(ROOT, "review", "shards"))
    ap.add_argument("--manifest", default=DEFAULT_MANIFEST)
    ap.add_argument("--tags", default=DEFAULT_TAGS)
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--img-dir", default="", help="给了就用 目录/file，否则用 manifest 的 src 列")
    ap.add_argument("--providers", default="auto")
    ap.add_argument("--status", action="store_true")
    args = ap.parse_args()

    if args.status:
        return show_status(args.out)

    os.makedirs(args.out, exist_ok=True)
    files = all_files(args.manifest)
    nshard = (len(files) + args.size - 1) // args.size
    if args.shard < 0 or args.shard >= nshard:
        print(f"--shard 必须在 0..{nshard-1}", file=sys.stderr)
        return 2

    tag = f"shard-{args.shard:04d}"
    done = os.path.join(args.out, tag + ".done")
    outp = os.path.join(args.out, tag + ".csv")
    if os.path.exists(done):
        print(f"{tag} 已完成，跳过")
        return 0

    mine = files[args.shard * args.size:(args.shard + 1) * args.size]
    src = None if args.img_dir else src_map(args.manifest)

    names, cats = load_tags(args.tags)
    char_idx = np.nonzero(cats == 4)[0]
    rate_idx = np.nonzero(cats == 9)[0]
    gen_idx = np.nonzero(cats == 0)[0]

    import onnxruntime as ort
    if args.providers == "cpu":
        providers = ["CPUExecutionProvider"]
    else:
        avail = ort.get_available_providers()
        providers = (["DmlExecutionProvider"] if "DmlExecutionProvider" in avail
                     else ["CPUExecutionProvider"])
    so = ort.SessionOptions()
    so.log_severity_level = 3
    sess = ort.InferenceSession(args.model, so, providers=providers)
    iname = sess.get_inputs()[0].name
    onames = [o.name for o in sess.get_outputs()]

    print(f"{tag}: {len(mine)} 张 | providers {providers}", flush=True)
    tmp = outp + ".tmp"
    t0 = time.time()
    nfail = 0
    with open(tmp, "w", newline="", encoding="utf-8") as fh:
        w = csv.writer(fh)
        w.writerow(["file", "dim", "value", "conf"])
        for i, fn in enumerate(mine):
            p = os.path.join(args.img_dir, fn) if args.img_dir else src.get(fn, "")
            if not p or not os.path.exists(p):
                nfail += 1
                continue
            try:
                x = prep(p)
                out, _ = sess.run(onames, {iname: x})
            except Exception:
                nfail += 1
                continue
            pr = np.asarray(out).reshape(-1).astype(np.float64)
            for j in char_idx:
                if pr[j] >= CHAR_TH:
                    w.writerow([fn, "character", names[j], f"{pr[j]:.4f}"])
            for j in rate_idx:
                if pr[j] >= RATING_TH:
                    w.writerow([fn, "rating", names[j], f"{pr[j]:.4f}"])
            for j in gen_idx:
                if pr[j] >= GEN_TH:
                    w.writerow([fn, "general", names[j], f"{pr[j]:.4f}"])
            if (i + 1) % 200 == 0:
                el = time.time() - t0
                print(f"  {i+1}/{len(mine)}  {el/(i+1)*1000:.0f} ms/img  "
                      f"eta {(len(mine)-i-1)*el/(i+1)/60:.1f} min", flush=True)
    os.replace(tmp, outp)
    el = time.time() - t0
    with open(done, "w") as fh:
        fh.write(f"{len(mine)} {el:.1f} {nfail}\n")
    print(f"{tag} DONE {len(mine)} in {el/60:.1f} min "
          f"({el/max(1,len(mine))*1000:.0f} ms/img)  失败 {nfail}", flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
