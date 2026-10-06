#!/usr/bin/env python3
"""按「mika 概率 × 是否有第二个角色」重新分层。

前一次分组用 mika>=0.85 一刀切，把「多角色图里 mika 概率被分流到 0.6~0.85」
的那一批错划进了假阳性池。实际观察：多人同框时 WD 会同时给两个角色高分，
mika 自己的分被摊薄，但它仍然是 mika。

分层（相册外）：
    L1  mika>=0.95 且无他角色     最高置信单人
    L2  mika>=0.85 且无他角色     高置信单人
    L3  mika>=0.85 且有他角色     高置信多角色
    L4  0.5<=mika<0.85 且有他角色 中置信多角色
    L5  0.5<=mika<0.85 且无他角色 中置信单人
    L6  mika<0.5                  不认
"""
import argparse
import collections
import csv
import glob
import os
import sqlite3
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
BA = "_(blue_archive)"
MIKA = "mika" + BA
OTH = 0.5      # 第二个角色的判定阈值


def font(size):
    try:
        return ImageFont.load_default(size=size)
    except TypeError:
        return ImageFont.load_default()


def sheet(items, out, cols=6, cell=200):
    if not items:
        return
    rows = int(np.ceil(len(items) / cols))
    im = Image.new("RGB", (cols * cell, rows * cell), (22, 22, 22))
    d = ImageDraw.Draw(im)
    f = font(max(11, cell // 13))
    for i, (p, lab) in enumerate(items):
        try:
            t = Image.open(p).convert("RGB")
        except Exception:
            continue
        t.thumbnail((cell - 4, cell - 4))
        x = (i % cols) * cell + (cell - t.width) // 2
        y = (i // cols) * cell + (cell - t.height) // 2
        im.paste(t, (x, y))
        for j, line in enumerate(lab.split("|")):
            b = d.textbbox((0, 0), line, font=f)
            d.rectangle([(i % cols) * cell + 3, (i // cols) * cell + 3 + j * (b[3] + 4),
                         (i % cols) * cell + 6 + (b[2] - b[0]),
                         (i // cols) * cell + 6 + j * (b[3] + 4) + (b[3] - b[1])],
                        fill=(0, 0, 0))
            d.text(((i % cols) * cell + 5, (i // cols) * cell + 4 + j * (b[3] + 4)),
                   line, fill=(255, 255, 255), font=f)
    im.save(out, quality=88)
    print(f"  wrote {out} ({len(items)} 张)")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--tag", default="mika")
    ap.add_argument("--wd", default=os.path.join(REVIEW, "wd"))
    ap.add_argument("--top", type=int, default=36)
    args = ap.parse_args()

    u2src, f2u = {}, {}
    with open(os.path.join(REVIEW, "full-manifest.csv")) as fh:
        for r in csv.DictReader(fh):
            u2src[r["uuid"].upper()] = r["src"]
            f2u[r["file"]] = r["uuid"].upper()
    u2f = {v: k for k, v in f2u.items()}
    dA = {}
    with open(os.path.join(REVIEW, f"full-{args.tag}-ranked.csv")) as fh:
        for r in csv.DictReader(fh):
            dA[r["uuid"].upper()] = float(r["d_to_anchor"])

    ba = collections.defaultdict(dict)
    for p in glob.glob(os.path.join(args.wd, "wd-char-*.csv")):
        with open(p, encoding="utf-8") as fh:
            for r in csv.DictReader(fh):
                if r["tag"].endswith(BA):
                    ba[r["file"]][r["tag"]] = float(r["prob"])
    print(f"files with BA tags: {len(ba)}")

    LIB = os.environ.get("PI_PHOTOS_LIBRARY", os.path.expanduser("~/Pictures/Photos Library.photoslibrary"))
    db = sqlite3.connect(f"file:{LIB}/database/Photos.sqlite?mode=ro", uri=True)
    db.execute("PRAGMA query_only=ON")

    def alb(pk):
        return {r[0].upper() for r in db.execute(
            "SELECT ZUUID FROM ZASSET WHERE Z_PK IN "
            "(SELECT Z_3ASSETS FROM Z_33ASSETS WHERE Z_33ALBUMS=?) AND ZKIND=0", (pk,))}
    pos = alb(1341); neg = alb(1344) | alb(1342) - pos
    db.close()

    def lay(fn):
        t = ba.get(fn, {})
        mk = t.get(MIKA, 0.0)
        oth = sorted(((k, v) for k, v in t.items() if k != MIKA and v >= OTH),
                     key=lambda x: -x[1])
        has = bool(oth)
        if mk < 0.5:
            L = "L6_不认"
        elif mk >= 0.95 and not has:
            L = "L1_极高置信单人"
        elif mk >= 0.85 and not has:
            L = "L2_高置信单人"
        elif mk >= 0.85 and has:
            L = "L3_高置信多角色"
        elif has:
            L = "L4_中置信多角色"
        else:
            L = "L5_中置信单人"
        return L, mk, oth, has

    # ---- 相册内（真值）----
    print(f"\n=== 相册内分层（n={len(pos)}）===")
    cnt = collections.Counter()
    for u in pos:
        fn = u2f.get(u)
        if not fn:
            continue
        cnt[lay(fn)[0]] += 1
    n = sum(cnt.values())
    for k in sorted(cnt):
        print(f"  {k:<18} {cnt[k]:>5}  ({cnt[k]/n:.1%})")

    print(f"\n=== 其它角色相册（n={len(neg)}）对照 ===")
    cnt2 = collections.Counter()
    for u in neg:
        fn = u2f.get(u)
        if not fn:
            continue
        cnt2[lay(fn)[0]] += 1
    n2 = sum(cnt2.values())
    for k in sorted(set(cnt) | set(cnt2)):
        print(f"  {k:<18} {cnt2[k]:>5}  ({cnt2[k]/n2:.1%})")

    # ---- 相册外分层 ----
    pools = collections.defaultdict(list)
    for fn, u in f2u.items():
        if u in pos:
            continue
        L, mk, oth, _ = lay(fn)
        pools[L].append((u, fn, mk, oth))
    print(f"\n=== 相册外分层（n={sum(len(v) for v in pools.values())}）===")
    for k in sorted(pools):
        print(f"  {k:<18} {len(pools[k]):>6}")

    # ---- 多角色：第二个角色是谁 ----
    print(f"\n=== 相册外「带第二个角色」的图里，第二个角色 top20 ===")
    c = collections.Counter()
    for k in ("L3_高置信多角色", "L4_中置信多角色"):
        for u, fn, mk, oth in pools[k]:
            if oth:
                c[oth[0][0].replace(BA, "")] += 1
    for t, k in c.most_common(20):
        print(f"    {t:<22} {k}")

    # ---- 导出 + 拼图 ----
    out = os.path.join(REVIEW, f"{args.tag}-strata.csv")
    with open(out, "w", newline="") as fh:
        w = csv.writer(fh)
        w.writerow(["layer", "uuid", "file", "wd_mika", "d_anchor", "others", "src"])
        for k in sorted(pools):
            for u, fn, mk, oth in sorted(pools[k], key=lambda x: -x[2]):
                w.writerow([k, u, fn, f"{mk:.4f}", f"{dA.get(u,9):.6f}",
                            "|".join(f"{t.replace(BA,'')}:{v:.2f}" for t, v in oth[:3]),
                            u2src[u]])
    print(f"\nwrote {out}")

    print(f"\n=== 分层拼图 ===")
    for k in sorted(pools):
        v = sorted(pools[k], key=lambda x: -x[2])[:args.top]
        items = []
        for u, fn, mk, oth in v:
            o = (" +" + oth[0][0].replace(BA, "")) if oth else ""
            items.append((u2src[u], f"m{mk:.2f} c{dA.get(u,9):.3f}{o}"))
        sheet(items, os.path.join(REVIEW, f"{args.tag}-strata-{k}.jpg"))
    return 0


if __name__ == "__main__":
    sys.exit(main())
