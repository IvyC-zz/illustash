#!/usr/bin/env python3
"""WD v3 tagger 全库推理（Windows / DirectML）。

第二个独立信号源：CCIP 给的是「长得像不像」，WD 给的是「模型认不认为是这个角色」。
character 类标签可以直接回答主人问的「多角色图能不能筛出去」——
一张图同时高概率命中 mika 和小春，它就是双人图。

预处理严格照 imgutils 源码（reference/src/imgutils_tagging_wd14.py:151）：
    白边补方 -> BICUBIC 448 -> float32 [0,255] 不归一化 -> RGB->BGR -> NHWC

输出（每个 worker 一份分片，最后合并）：
    wd-char-<rank>.csv     file, tag, prob   (只存 character(4) 与 rating(9) 类, prob>=0.30)
    wd-top-<rank>.csv      file, tag, prob   (general(0) 类 top-20)
    wd-emb-<rank>.npy      (n, 1024) float32
    wd-order-<rank>.txt    embedding 行序对应的文件名

用法:
    python wd_extract.py --rank 0 --nproc 4
    python wd_extract.py --rank 0 --nproc 1 --limit 200 --out test
"""
import argparse
import csv
import os
import sys
import time

import numpy as np
import onnxruntime as ort
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = r"E:\photo-intel\wd"          # 模型与标签表所在目录
IMGS = r"E:\photo-intel\full\imgs"   # 待推理图片目录

CHAR_TH = 0.30          # 存盘阈值，比默认 0.85 松，便于事后调
GEN_TOP = 20


def load_tag_table():
    names, cats = [], []
    with open(os.path.join(BASE, "tags", "tags_v3.csv"), encoding="utf-8") as fh:
        for r in csv.DictReader(fh):
            names.append(r["name"])
            cats.append(int(r["category"]))
    return names, np.asarray(cats, dtype=np.int32)


def prep(path, size=448):
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rank", type=int, default=0)
    ap.add_argument("--nproc", type=int, default=1)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default="")
    ap.add_argument("--providers", default="auto")
    args = ap.parse_args()

    outdir = os.path.abspath(args.out) if args.out else os.getcwd()
    os.makedirs(outdir, exist_ok=True)

    names, cats = load_tag_table()
    char_idx = np.nonzero(cats == 4)[0]
    rating_idx = np.nonzero(cats == 9)[0]
    gen_idx = np.nonzero(cats == 0)[0]
    print(f"[{args.rank}] tags {len(names)} | char {len(char_idx)} rating {len(rating_idx)} "
          f"general {len(gen_idx)}", flush=True)

    files = sorted(os.listdir(IMGS))
    if args.limit:
        files = files[:args.limit]
    mine = files[args.rank::args.nproc]
    print(f"[{args.rank}] {len(mine)} / {len(files)} files", flush=True)

    if args.providers == "cpu":
        providers = ["CPUExecutionProvider"]
    else:
        avail = ort.get_available_providers()
        providers = ["DmlExecutionProvider"] if "DmlExecutionProvider" in avail \
            else ["CPUExecutionProvider"]
    print(f"[{args.rank}] providers {providers}", flush=True)
    so = ort.SessionOptions()
    so.log_severity_level = 3
    sess = ort.InferenceSession(os.path.join(BASE, "models", "wd_swinv2_v3_emb.onnx"),
                                so, providers=providers)
    iname = sess.get_inputs()[0].name
    onames = [o.name for o in sess.get_outputs()]
    print(f"[{args.rank}] in={iname} out={onames}", flush=True)

    cf = open(os.path.join(outdir, f"wd-char-{args.rank}.csv"), "w", newline="",
              encoding="utf-8")
    tf = open(os.path.join(outdir, f"wd-top-{args.rank}.csv"), "w", newline="",
              encoding="utf-8")
    of = open(os.path.join(outdir, f"wd-order-{args.rank}.txt"), "w", encoding="utf-8")
    cw, tw = csv.writer(cf), csv.writer(tf)
    cw.writerow(["file", "tag", "prob"])
    tw.writerow(["file", "tag", "prob"])

    embs = np.empty((len(mine), 1024), dtype=np.float32)
    t0 = time.time()
    done = 0
    for i, fn in enumerate(mine):
        p = os.path.join(IMGS, fn)
        try:
            x = prep(p)
            out, emb = sess.run(onames, {iname: x})
        except Exception as e:
            print(f"[{args.rank}] FAIL {fn}: {type(e).__name__} {e}", flush=True)
            embs[i] = 0.0
            of.write(fn + "\n")
            continue
        logits = np.asarray(out).reshape(-1)
        # 本模型的 output 已经是 sigmoid 之后的概率（实测 min 0 / max 0.999 / p50 0），
        # 再套一次 sigmoid 会把所有值压进 (0.5, 0.731)，2751 个角色标签全部越线。
        probs = logits.astype(np.float64)
        embs[i] = np.asarray(emb).reshape(-1)
        of.write(fn + "\n")

        for j in char_idx:
            if probs[j] >= CHAR_TH:
                cw.writerow([fn, names[j], f"{probs[j]:.4f}"])
        for j in rating_idx:
            if probs[j] >= CHAR_TH:
                cw.writerow([fn, names[j], f"{probs[j]:.4f}"])
        top = gen_idx[np.argsort(-probs[gen_idx])[:GEN_TOP]]
        for j in top:
            if probs[j] >= CHAR_TH:
                tw.writerow([fn, names[j], f"{probs[j]:.4f}"])

        done += 1
        if done % 500 == 0:
            el = time.time() - t0
            print(f"[{args.rank}] {done}/{len(mine)}  {el/done*1000:.0f} ms/img  "
                  f"eta {(len(mine)-done)*el/done/60:.1f} min", flush=True)

    np.save(os.path.join(outdir, f"wd-emb-{args.rank}.npy"), embs)
    cf.close(); tf.close(); of.close()
    el = time.time() - t0
    print(f"[{args.rank}] DONE {done} in {el/60:.1f} min ({el/max(1,done)*1000:.0f} ms/img)",
          flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
