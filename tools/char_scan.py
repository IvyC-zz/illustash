#!/usr/bin/env python3
"""A+B 层：用 character_index 的全部角色锚点扫全库，给 WD 认不出的角色补标签。

它解决什么
----------
WD v3 的 character 头只有 2,751 个角色。全库 46,980 张里有 20,827 张（44.3%）
WD top-1 < 0.30（match-status.csv 的 unmatched）。而实测这个池子里 85% 是 `1girl`、
65% 是 `solo`，非人物只占 2.6% —— **不是垃圾池，是词表不够**。

character_index 有 7,380 个角色，每个自带 11~50 张参考图的 CCIP 特征。
本地全库的 CCIP 特征（review/full-features.npy）早就提好了，所以这一步
**不需要跑任何模型**，纯矩阵运算。

判据
----
距离用 `(1 - cos)/2`（与 model_metrics.onnx 等价，见 tools/ccip.cross）。
阈值**按角色自适应**：取该角色在 character_index 内部到自家锚点的 p90
（characters.anchor_p90）。不用 CCIP 的全局阈值 0.178475 —— 实测 234 个 BA 角色的
组内 p90 中位只有 0.0741，全局值松了一倍多，会灌进大量噪声（WORKFLOW §2.4）。

产出
----
review/char-scan.csv   uuid, char_id, dist, thresh, rank, within
                       rank=0 是最近的角色；within=1 表示进了该角色的自适应阈值
`--load` 另外灌进 pi.db 的 labels，dim='char_ext'，source='char_index'。

用法
----
    .venv/bin/python -u tools/char_scan.py                 # 只出 CSV
    .venv/bin/python -u tools/char_scan.py --load          # 顺便灌库
"""
import argparse
import collections
import csv
import os
import sqlite3
import sys
import time

import numpy as np

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
sys.path.insert(0, os.path.join(ROOT, "tools"))
from ccip import cross  # noqa: E402

def log(m):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), m), flush=True)


# 两个锚点距离 ≤ 这个值，CCIP 就分不开它们：参考图画的是同一张脸。
# 依据（234 个 BA 锚点实测）：highlander_sidelocks_conductor × hikari = 0.0005，akane × akane_bunny = 0.0007；
# 全库 7,777 个锚点里的 1,406 对几乎都是「皮肤 / 同角色多标签 / 吉祥物、宠物条目的参考图其实是主人」
# （oz × fischl、puck × emilia、itzpapa × citlali）。真正不同的角色，锚点两两距离中位 0.3376。
ALIAS_TH = 0.01


def anchor_alias(A, ids, meta):
    """The char_id each anchor's claims are filed under.

    Anchors within ALIAS_TH are indistinguishable, so a claim goes to the most plausible of them:
    base over skin, real character over mascot entry, then Danbooru popularity. Only direct
    neighbours count, never chains: 2026-09-25 single-linkage chained luo_xiaohei (0.0124 away)
    into suzuran_arknights through a collab skin. Identity is untouched; this only files evidence.
    """
    def score(c):
        m = meta.get(c) or (None,) * 9
        return (m[6] or 0) >= 0.9, not m[4], m[5] or 0, c

    alias = list(ids)
    for i in range(0, len(A), 1024):
        D = cross(A[i:i + 1024], A, check=False)
        for r, j in zip(*np.nonzero(D <= ALIAS_TH)):
            a = i + int(r)
            if score(ids[j]) > score(alias[a]):
                alias[a] = ids[j]
    # Follow uphill hops (sleepwear -> death_sensei -> mori_calliope). Each hop strictly raises the score,
    # so a low-scoring hub can never bridge two better characters the way single-linkage did.
    step = dict(zip(ids, alias))
    for a, c in enumerate(alias):
        while step[c] != c:
            c = step[c]
        alias[a] = c
    return alias


def anchor_index(A, ids, pi_db):
    """Thresholds per anchor and the char_id its claims are filed under (see anchor_alias).

    The scan CSV keeps the matched char_id beside it, so provenance survives.
    """
    if len(A) != len(ids):
        raise ValueError("anchor vectors and ids differ in length")
    db = sqlite3.connect("file:%s?mode=ro" % pi_db, uri=True)
    meta = {r[0]: r[1:] for r in db.execute(
        "SELECT char_id, anchor_p90, copyright, wd_tag, base_tag, skin, post_count, gender_conf,"
        " anchor_p50, anchor_p75 FROM characters")}
    th = np.array([(meta.get(c) or [None])[0] or 0.0 for c in ids], dtype=np.float32)
    th50 = np.array([(meta.get(c) or [None] * 9)[7] or 0.0 for c in ids], dtype=np.float32)
    th75 = np.array([(meta.get(c) or [None] * 9)[8] or 0.0 for c in ids], dtype=np.float32)
    n_noth = int((th <= 0).sum())
    if n_noth:
        log("  ⚠️ %d 个角色没有 anchor_p90（参考图 <3 张），这些锚点只排序不判定" % n_noth)
    db.close()

    return meta, th, th50, th75, anchor_alias(A, ids, meta)


def claim_thresholds(ids, th, th75):
    """Per-anchor claim limit: p90 for curated upstream anchors, p75 for anchors patched in from Danbooru.

    Patched references are cleaned but not curated; on 2026-09-24 their loose tier held most false claims
    (aemeath_wuthering_waves: 205 of 230 claims at p90, mostly other pink-haired characters)."""
    import glob, json
    patched = set()
    for path in glob.glob(os.path.join(REVIEW, "char-index", "*.json")):
        try:
            with open(path, encoding="utf-8") as f:
                if json.load(f).get("source") == "danbooru-patch":
                    patched.add(os.path.basename(path)[:-5])
        except ValueError:
            continue
    return np.array([th75[i] if c in patched else th[i] for i, c in enumerate(ids)], dtype=np.float32)


def main():
    ap = argparse.ArgumentParser()
    # 3: the stored char_ext run used --top-k 3 (9 nearest anchors); web/infer.py uses the same for synced images.
    ap.add_argument("--top-k", type=int, default=3)
    ap.add_argument("--run", default="char-scan-alias-v3", help="labels.run_id for --load")
    ap.add_argument("--index", default="character_index@2025-09-25", help="labels.model_ver for --load")
    ap.add_argument("--chunk", type=int, default=4000)
    ap.add_argument("--output", default=os.path.join(REVIEW, "char-scan.csv"), help="CSV output; without --load the source database stays read-only")
    ap.add_argument("--load", action="store_true", help="把 within=1 的结果灌进 labels")
    args = ap.parse_args()

    A = np.load(os.path.join(REVIEW, "char-anchors.npy"))
    ids = [l.strip() for l in open(os.path.join(REVIEW, "char-anchors.txt")) if l.strip()]
    assert len(A) == len(ids), (len(A), len(ids))
    F = np.load(os.path.join(REVIEW, "full-features.npy"))
    with open(os.path.join(REVIEW, "full-manifest.csv")) as f:
        man = [r for r in csv.DictReader(f)]
    assert len(man) == len(F)
    # Images added by the Photos sync keep their CCIP features in pi.db (web/infer.py).
    db = sqlite3.connect("file:%s?mode=ro" % os.path.join(REVIEW, "pi.db"), uri=True)
    have = {r["uuid"].strip().upper() for r in man}
    try:
        extra = [(u, v) for u, v in db.execute("SELECT uuid,vec FROM embeddings WHERE model='ccip'") if u.upper() not in have]
    except sqlite3.OperationalError:
        extra = []
    db.close()
    if extra:
        F = np.concatenate([F, np.stack([np.frombuffer(v, dtype=np.float32) for _, v in extra])])
        man = man + [{"uuid": u} for u, _ in extra]
        log("加上同步进来的 %d 张（pi.db embeddings）" % len(extra))
    log("锚点 %d × %d，图库 %d 张" % (A.shape[0], A.shape[1], len(F)))

    meta, th, th50, th75, ent = anchor_index(A, ids, os.path.join(REVIEW, "pi.db"))
    limit = claim_thresholds(ids, th, th75)

    st = {}
    p = os.path.join(REVIEW, "match-status.csv")
    if os.path.exists(p):
        st = {r["uuid"]: r["match_status"] for r in csv.DictReader(open(p))}

    out = open(args.output, "w", newline="")
    w = csv.writer(out)
    w.writerow(["uuid", "char_id", "entity", "dist", "thresh", "tier", "rank", "within"])

    t0 = time.time()
    claimed = collections.Counter()
    claimed_un = collections.Counter()
    by_status = collections.Counter()
    n_within = 0
    tiers = collections.Counter()
    first = True
    for i in range(0, len(F), args.chunk):
        D = cross(A, F[i:i + args.chunk], check=first)   # (n_anchor, chunk)
        first = False
        # 每张图的 top-k 最近角色
        k = min(args.top_k * 3, len(A))   # retain the existing nearest-9 search budget
        idx = np.argpartition(D, k - 1, axis=0)[:k]                     # (k, chunk)
        for j in range(D.shape[1]):
            col = idx[:, j]
            order = col[np.argsort(D[col, j])]
            uuid = man[i + j]["uuid"].strip()
            hit = False
            seen_ent = set()
            r = 0
            for a in order:
                e = ent[a]
                if e in seen_ent:        # 同一个人的另一个标签，不再占一个名额
                    continue
                seen_ent.add(e)
                d = float(D[a, j]); t = float(limit[a])
                ok = int(t > 0 and d <= t)
                # 三档：紧(p50)/中(p75)/松(p90)。实测 p50 档质量明显更高，
                # p90 档已经开始混入别的角色 —— 见 review/new-sakurai_miyo.jpg。
                tier = ("p50" if d <= th50[a] else "p75" if d <= th75[a] else "p90") if ok else "-"
                w.writerow([uuid, ids[a], e, "%.4f" % d, "%.4f" % t, tier, r, ok])
                r += 1
                if ok:
                    tiers[tier] += 1
                if ok and not hit:
                    hit = True
                    claimed[e] += 1
                    s = st.get(uuid, "?")
                    by_status[s] += 1
                    if s == "unmatched":
                        claimed_un[e] += 1
            n_within += hit
        if (i // args.chunk) % 3 == 0:
            log("  %d/%d  %.0fs" % (i + D.shape[1], len(F), time.time() - t0))
    out.close()
    log("扫描完成 %.0fs" % (time.time() - t0))

    print()
    print("落在自适应阈值内（至少一个角色）: %d / %d = %.1f%%" % (
        n_within, len(F), 100 * n_within / len(F)))
    print("按 WD match_status 分:", dict(by_status))
    print("按置信档分（含 top-3 里的全部命中）:", dict(tiers))
    un = by_status.get("unmatched", 0)
    if st:
        tot_un = sum(1 for v in st.values() if v == "unmatched")
        print("★ unmatched 池被认领: %d / %d = %.1f%%" % (un, tot_un, 100 * un / tot_un))
    print("\n在 unmatched 池里认领最多的 25 个角色:")
    for c, n in claimed_un.most_common(25):
        m = meta.get(c) or (None,) * 9
        flag = "WD有" if m[2] else "**WD没有**"
        warn = "  ⚠低gender把握" if (m[6] or 1) < 0.9 else ""
        print("   %-44s %5d   %s  %s%s" % (c, n, m[1] or "?", flag, warn))

    if args.load:
        log("灌库 dim='char_ext' ...")
        db = sqlite3.connect(os.path.join(REVIEW, "pi.db"))
        db.execute("DELETE FROM labels WHERE dim='char_ext'")
        rows = []
        for r in csv.DictReader(open(args.output)):
            if r["within"] == "1":
                rows.append((r["uuid"], "char_ext", r["entity"],
                             1.0 - float(r["dist"]), "char_index",
                             args.index, args.run))
        db.executemany(
            "INSERT OR REPLACE INTO labels(uuid,dim,value,conf,source,model_ver,run_id)"
            " VALUES (?,?,?,?,?,?,?)", rows)
        db.commit()
        log("  灌入 %d 行" % len(rows))
        db.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
