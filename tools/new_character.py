#!/usr/bin/env python3
"""新增一个角色标签 —— 给一小撮种子图，把全库里同一个角色捞出来。

什么时候用这个
--------------
`char_scan.py` 用 character_index 的 7,373 个角色覆盖了绝大部分情况。
剩下三类只能走这条路：
  · OC / 同人原创角色
  · danbooru 还没收录的角色（当时由已删除的 `album_probe.py` 判读为「候选集里没有」）
  · 主人自己定义的分类（不是角色，例如某种构图、某个梗）

种子要几张
----------
实测（mika / ニヤニヤ教授 / miyo 三个角色的留出实验）：**建原型 10~20 张就够**，
10 张和 200 张的检索质量几乎没差别。种子多了主要是让阈值分位估得稳，
而阈值这里是从种子自身的距离分布取的，所以 20~50 张是舒服区间。

用法
----
    # 用人工相册当种子
    .venv/bin/python -u tools/new_character.py --album miyo --name sakurai_miyo
    # 或者直接给 uuid 清单
    .venv/bin/python -u tools/new_character.py --uuids seeds.txt --name my_oc

产出
    review/new-<name>.csv    候选清单（按距离排序，标了是否已被别的角色认领）
    review/new-<name>.jpg    分档拼图（紧/中/松三档各抽样），直接拿去人工核
"""
import argparse
import collections
import csv
import os
import sqlite3
import subprocess
import sys

import numpy as np
from numpy.linalg import norm

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
sys.path.insert(0, os.path.join(ROOT, "tools"))
from ccip import cross, threshold  # noqa: E402
from strata_report import sheet  # noqa: E402


def merge(e):
    L = norm(e, axis=-1)
    u = e / L.reshape(-1, 1)
    c = u.mean(axis=0)
    return c / norm(c) * L.mean()


def main():
    ap = argparse.ArgumentParser()
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--album", help="decisions 里 dim='character' 的 value，拿它当种子")
    g.add_argument("--uuids", help="一行一个 uuid 的文件")
    ap.add_argument("--name", required=True, help="给这个角色起的 id")
    ap.add_argument("--seeds", type=int, default=0, help="只用前 N 张种子（0=全用）")
    ap.add_argument("--per-tier", type=int, default=12, help="拼图每档抽几张")
    args = ap.parse_args()

    man = list(csv.DictReader(open(os.path.join(REVIEW, "full-manifest.csv"))))
    idx = {r["uuid"].strip().upper(): i for i, r in enumerate(man)}
    F = np.load(os.path.join(REVIEW, "full-features.npy"))

    db = sqlite3.connect("file:%s?mode=ro" % os.path.join(REVIEW, "pi.db"), uri=True)
    if args.album:
        assets = {u for (u,) in db.execute("SELECT uuid FROM assets")}
        us = [u for (u,) in db.execute(
            "SELECT uuid FROM decisions WHERE dim='character' AND value=?", (args.album,))
            if u in assets]
    else:
        us = [l.strip() for l in open(args.uuids) if l.strip()]
    seed = np.array(sorted(idx[u.upper()] for u in us if u.upper() in idx))
    if args.seeds:
        seed = seed[:args.seeds]
    if len(seed) < 5:
        print("种子只有 %d 张，太少" % len(seed))
        return 1

    # 已经被 char_scan 认领的图：新角色的候选里出现它们，多半是撞了已知角色
    owned = {}
    for r in csv.DictReader(open(os.path.join(REVIEW, "char-scan.csv"))):
        if r["within"] == "1" and r["rank"] == "0":
            owned[r["uuid"]] = r["entity"]
    db.close()

    # 建锚点：迭代自剪枝，剔掉种子里的离群（多角色图、误放的图）
    A = F[seed]
    th = threshold()
    keep = np.ones(len(A), bool)
    prev = -1
    for rnd in range(1, 11):
        a = merge(A[keep])
        d = cross(a[None], A[keep], check=(rnd == 1))[0]
        keep[keep] = d <= th
        n = int(keep.sum())
        print("  round %d: 保留 %d/%d" % (rnd, n, len(A)))
        if n == prev or n < 5:
            break
        prev = n
    a = merge(A[keep])

    dk = cross(a[None], A, check=False)[0]           # 种子到锚点的距离分布
    p50, p75, p90 = (float(np.percentile(dk, q)) for q in (50, 75, 90))
    print("  种子内距离分位 p50 %.4f  p75 %.4f  p90 %.4f  （全局阈值 %.4f 仅供参考）"
          % (p50, p75, p90, th))

    d = cross(a[None], F, check=False)[0]
    seedset = set(seed.tolist())
    order = [i for i in np.argsort(d) if i not in seedset]

    out = os.path.join(REVIEW, "new-%s.csv" % args.name)
    with open(out, "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["uuid", "file", "dist", "tier", "owned_by"])
        for i in order:
            if d[i] > p90:
                break
            tier = "p50" if d[i] <= p50 else ("p75" if d[i] <= p75 else "p90")
            u = man[i]["uuid"].strip()
            w.writerow([u, man[i]["file"].strip(), "%.4f" % d[i], tier, owned.get(u, "")])

    rows = list(csv.DictReader(open(out)))
    c = collections.Counter(r["tier"] for r in rows)
    print("\n相册外候选（种子已排除）：p50 %d / p75 累计 %d / p90 累计 %d"
          % (c["p50"], c["p50"] + c["p75"], len(rows)))
    own = collections.Counter(r["owned_by"] for r in rows if r["owned_by"])
    print("  其中 %d 张已被 char_scan 认作别的角色，最多的："
          % sum(own.values()), own.most_common(5))

    # 分档拼图
    rng = np.random.default_rng(0)
    items = []
    for tier in ("p50", "p75", "p90"):
        rs = [r for r in rows if r["tier"] == tier]
        if not rs:
            continue
        sel = rng.choice(len(rs), size=min(args.per_tier, len(rs)), replace=False)
        for r in sorted([rs[i] for i in sel], key=lambda x: float(x["dist"])):
            items.append((tier, r))
    files = "\n".join(r["file"] for _, r in items)
    subprocess.run(["bash", os.path.join(ROOT, "tools", "fetch_imgs.sh")],
                   input=files, text=True, check=True)
    sheet([(os.path.join(REVIEW, "imgcache", r["file"]),
            "%s d=%s|%s" % (t, r["dist"], (r["owned_by"] or "")[:26]))
           for t, r in items],
          os.path.join(REVIEW, "new-%s.jpg" % args.name),
          cols=args.per_tier, cell=260)
    print("  -> %s" % out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
