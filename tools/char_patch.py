#!/usr/bin/env python3
"""补外部角色索引：deepghs/character_index 停在 2025-09-25，缺的角色从 Danbooru 在本机补齐。

做法与上游一致：每个角色取一批 Danbooru 单人图，本机算 CCIP 特征，写成与
review/char-index/ 相同的 <short_tag>.npy + .json，然后照常跑 tools/build_characters.py
（算锚点与 p50/p75/p90 阈值）和 tools/char_scan.py（全库重扫）。

只下载公开图片与标签（720px webp，约 55 KB/张），不上传任何本地图片或特征。

    .venv/bin/python -u tools/char_patch.py plan --works blue_archive,wuthering_waves --min-posts 100
    .venv/bin/python -u tools/char_patch.py fetch            # 可续跑：已有 .npy 的角色跳过
    .venv/bin/python -u tools/char_patch.py clean            # 清洗参考图，再跑 build_characters.py
    .venv/bin/python -u tools/char_patch.py drop mashu_tan --reason "..."   # 看图后人工放弃某个补充角色
    .venv/bin/python -u tools/char_patch.py apply --only a_tag,b_tag   # 设置页用：只补这几个，然后重建锚点、重扫
    .venv/bin/python -u tools/char_patch.py drop x --reason "..." --rebuild

设置页（tests/reference/web/index_update.py）的「检查新角色」只发文本查询（related_tag），结果给用户挑；
选中后调用 apply：备份 → fetch → clean → build_characters.py → char_scan.py --load，都只处理选中的角色。
以 @ 开头的输出行是给设置页看的进度（JSON）。

clean 为什么必须做：Danbooru 的「单人」图并不干净。上游索引的参考图是清洗过的（原有角色 p90 阈值中位 0.094），
未清洗的补丁角色 p90 中位 0.126、156/426 个超过 0.15，会大量误认领。例如 abby_(wuthering_waves)
的 20 张参考图是好几个不同的角色（她多作为配角/贴纸出现）。清洗：取参考图里最密集的一团为核心，
反复用核心重算锚点、只留离锚点 ≤ KEEP_DIST 的图；核心不足 5 张或不足四成的角色放弃（移到 rejected/）。
原始特征保存在 review/char-patch/raw/。
清洗后若锚点离任何已有角色 < SAME_DIST（0.03），也放弃：这类几乎都是已有角色的伴生物或皮肤，它们的「单人」
参考图画的其实是主人（jue→jinhsi 0.011、bun_bun→miyako 0.017、qingming_bird→yixuan 0.007）。

plan 的筛选（2026-09-23 按用户选定）：
    related_tag 共现角色中 overlap ≥ 0.5（大部分图属于该作品，排除客串）、Danbooru ≥ min-posts 张、
    本地 characters 表（danbooru_tag / wd_tag）里没有、且是本体（tag 去掉作品后缀后不再带括号段）。
"""
import argparse
import collections
import json
import os
import re
import sqlite3
import sys
import time
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

import numpy as np
from PIL import Image

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
IDX = os.path.join(REVIEW, "char-index")
PATCH = os.path.join(REVIEW, "char-patch")
PLAN = os.path.join(PATCH, "plan.json")
sys.path.insert(0, os.path.join(ROOT, "tools"))

UA = {"User-Agent": "photo-intelligence/0.1 (personal, local library tagging)"}
SOURCE = "danbooru-patch"


def log(m):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), m), flush=True)


def get(url, **q):
    """Danbooru API (paced ~1 req/s) or CDN (not paced), with retries: the API returns 500 under load now and then."""
    if q:
        url += "?" + urllib.parse.urlencode(q)
    api = "danbooru.donmai.us/" in url
    err = None
    for attempt in range(4):
        if api or attempt:
            time.sleep(1.0 * (attempt + 1))
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=90) as r:
                return r.read()
        except Exception as e:
            err = e
    raise err


def short_tag(tag):
    """Same id convention as character_index: mika_(swimsuit)_(blue_archive) -> mika_swimsuit_blue_archive."""
    return re.sub(r"[^a-z0-9]+", "_", tag.lower()).strip("_")


def _safe(fn, *args):
    try:
        fn(*args)
    except Exception as e:
        return e


def rejected_ids():
    folder = os.path.join(PATCH, "rejected")
    return {n[:-5] for n in os.listdir(folder) if n.endswith(".json")} if os.path.isdir(folder) else set()


def candidates(works, min_posts, per_work=None):
    """Characters of `works` that Danbooru has (≥ min_posts, mostly in that work, not a skin) and we lack.

    Only text queries go out. Characters given up before (rejected/) are not offered again.
    `per_work(work, count or None)` is told how many each work lacks; None: Danbooru knows no such work tag.
    """
    db = sqlite3.connect("file:%s?mode=ro" % os.path.join(REVIEW, "pi.db"), uri=True)
    known = {r[0] for r in db.execute("SELECT danbooru_tag FROM characters WHERE danbooru_tag IS NOT NULL")}
    known |= {r[0] for r in db.execute("SELECT wd_tag FROM characters WHERE wd_tag IS NOT NULL")}
    known_ids = {r[0] for r in db.execute("SELECT char_id FROM characters")} | rejected_ids()
    db.close()
    rows, seen = [], set()
    for cp in works:
        rel = json.loads(get("https://danbooru.donmai.us/related_tag.json", query=cp, category="character", limit=1000))
        n = 0
        for r in rel.get("related_tags", []):
            tag, posts, overlap = r["tag"]["name"], r["tag"]["post_count"], r["overlap_coefficient"]
            inner = re.sub(r"_\([^()]*\)$", "", tag)
            if (overlap < 0.5 or posts < min_posts or tag in known or tag in seen or short_tag(tag) in known_ids
                    or os.path.exists(os.path.join(IDX, short_tag(tag) + ".json")) or re.search(r"_\([^()]*\)$", inner)):
                continue
            seen.add(tag)
            rows.append({"tag": tag, "short_tag": short_tag(tag), "copyright": cp, "post_count": posts, "overlap": round(overlap, 3)})
            n += 1
        log("%-24s 缺 %d 个" % (cp, n))
        if per_work:
            per_work(cp, n if rel.get("related_tags") else None)
    return rows


def plan(works, min_posts):
    rows = candidates(works, min_posts)
    os.makedirs(PATCH, exist_ok=True)
    with open(PLAN, "w", encoding="utf-8") as f:
        json.dump(rows, f, ensure_ascii=False, indent=1)
    log("计划 %d 个角色 -> %s" % (len(rows), PLAN))


def progress(step, **fields):
    print("@" + json.dumps({"step": step, **fields}, ensure_ascii=False), flush=True)


def reject(meta, reason):
    folder = os.path.join(PATCH, "rejected")
    os.makedirs(folder, exist_ok=True)
    meta["rejected"] = reason
    with open(os.path.join(folder, meta["short_tag"] + ".json"), "w", encoding="utf-8") as f:
        json.dump(meta, f, ensure_ascii=False, indent=1)


def fetch(per, only=None):
    import ccip
    rows = [r for r in json.load(open(PLAN, encoding="utf-8")) if only is None or r["short_tag"] in only]
    done = skipped = images = 0
    started = time.monotonic()
    for i, row in enumerate(rows, 1):
        cid = row["short_tag"]
        progress("fetch", done=i - 1, total=len(rows), tag=row["tag"])
        if any(os.path.exists(p) for p in (os.path.join(IDX, cid + ".npy"), os.path.join(PATCH, "raw", cid + ".npy"),
                                           os.path.join(PATCH, "rejected", cid + ".json"))):
            skipped += 1
            continue
        folder = os.path.join(PATCH, "img", cid)
        os.makedirs(folder, exist_ok=True)
        posts = json.loads(get("https://danbooru.donmai.us/posts.json", tags=row["tag"] + " solo rating:g,s",
                               limit=per, only="id,tag_string_general,tag_string_character,md5,parent_id,source,is_deleted,is_banned,media_asset"))
        feats, general, kept = [], collections.Counter(), []
        wanted = []
        for p in posts:
            variant = next((v for v in (p.get("media_asset") or {}).get("variants", []) if v["type"] == "720x720"), None)
            if variant:
                wanted.append((p, variant["url"], os.path.join(folder, "%d.%s" % (p["id"], variant["file_ext"]))))

        def download(item):
            if not os.path.exists(item[2]):
                data = get(item[1])
                with open(item[2], "wb") as f:
                    f.write(data)

        with ThreadPoolExecutor(6) as pool:
            failures = list(pool.map(lambda item: _safe(download, item), wanted))
        for (p, _, path), failure in zip(wanted, failures):
            try:
                if failure:
                    raise failure
                with Image.open(path) as im:
                    feats.append(ccip.features([im.convert("RGB")])[0])
            except Exception as e:
                log("  跳过 %s #%s：%s" % (cid, p["id"], type(e).__name__))
                continue
            kept.append(p["id"])
            general.update(set(p["tag_string_general"].split()))
        n = len(kept)
        if n < 3:
            log("(%d/%d) %s 只有 %d 张可用单人图，跳过" % (i, len(rows), row["tag"], n))
            reject(dict(row), "Danbooru 上可用的全年龄单人图只有 %d 张" % n)
            continue
        # Tags on most reference images describe the character itself (hair, eyes, halo...).
        core = [t for t, c in general.most_common() if c / n >= 0.6 and t not in ("solo", "1girl", "1boy", "looking_at_viewer")]
        meta = {"tag": row["tag"], "short_tag": cid, "copyright": row["copyright"], "copyrights": [row["copyright"]],
                "post_count": row["post_count"], "core_tags": core[:24],
                "gender": {"girl": general["1girl"] / n, "boy": general["1boy"] / n},
                "source": SOURCE, "fetched_at": time.strftime("%Y-%m-%d"), "posts": kept}
        np.save(os.path.join(IDX, cid + ".npy"), np.stack(feats).astype(np.float32))
        with open(os.path.join(IDX, cid + ".json"), "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=1)
        done += 1
        images += n
        el = time.monotonic() - started
        log("(%d/%d) %s  %d 张  | 已补 %d，平均 %.0f 秒/角色" % (i, len(rows), row["tag"], n, done, el / done))
    progress("fetch", done=len(rows), total=len(rows))
    log("完成：补 %d 个角色 / %d 张参考图，已存在跳过 %d" % (done, images, skipped))


KEEP_DIST = 0.10     # ≈ the 90th-percentile threshold of the curated upstream anchors (median 0.094)
MIN_CORE, MIN_SHARE = 5, 0.4
SAME_DIST = 0.03
# Patch anchors come from uncurated references, so only their tight and middle tiers claim images (see char_scan).


def existing_anchors(skip=None):
    """Anchors of the upstream (non-patch) characters, keyed by char_id.

    With `skip` (the characters being cleaned now): every other character, earlier patches included.
    """
    from build_characters import merge
    ids, rows = [], []
    for name in sorted(os.listdir(IDX)):
        if not name.endswith(".json"):
            continue
        try:
            meta = json.load(open(os.path.join(IDX, name), encoding="utf-8"))
        except ValueError:           # a few upstream metadata files are empty; build_characters skips them too
            continue
        feat = os.path.join(IDX, name[:-5] + ".npy")
        cid = meta.get("short_tag") or name[:-5]
        if (cid in skip if skip is not None else meta.get("source") == SOURCE) or not os.path.exists(feat):
            continue
        try:
            A = np.load(feat)
        except ValueError:           # and a few upstream feature files are truncated
            continue
        if A.ndim == 2 and len(A) >= 3:
            ids.append(cid)
            rows.append(merge(A))
    return ids, np.stack(rows) if rows else np.empty((0, 768), dtype=np.float32)


def core_of(A):
    """Indices of the consistent majority of a character's reference features, or None."""
    from ccip import cross
    from build_characters import merge
    D = cross(A, A, check=False)
    seed = int(np.argmax((D <= KEEP_DIST).sum(1)))
    keep = np.nonzero(D[seed] <= KEEP_DIST + 0.02)[0]
    for _ in range(5):
        if len(keep) < 2:
            return None
        anchor = merge(A[keep])
        d = cross(anchor[None], A, check=False)[0]
        new = np.nonzero(d <= KEEP_DIST)[0]
        if set(new) == set(keep):
            break
        keep = new
    return keep if len(keep) >= MIN_CORE and len(keep) >= MIN_SHARE * len(A) else None


def clean(only=None):
    """Clean every planned patch character, or with `only` just those (checked against all others, earlier patches too)."""
    raw, rejected = os.path.join(PATCH, "raw"), os.path.join(PATCH, "rejected")
    os.makedirs(raw, exist_ok=True)
    os.makedirs(rejected, exist_ok=True)
    from ccip import cross
    from build_characters import merge
    known_ids, known = existing_anchors(only)
    kept = dropped = 0
    accepted = []                                   # (short_tag, anchor) of patch characters kept so far
    # Most-posted first, so of two tags for one person (civilian / transformed, base / skin) the main one stays.
    for row in sorted(json.load(open(PLAN, encoding="utf-8")), key=lambda r: -r["post_count"]):
        cid = row["short_tag"]
        if only is not None and cid not in only:
            continue
        meta_path, feat_path, raw_path = (os.path.join(IDX, cid + ".json"), os.path.join(IDX, cid + ".npy"),
                                          os.path.join(raw, cid + ".npy"))
        if not os.path.exists(meta_path):
            continue
        meta = json.load(open(meta_path, encoding="utf-8"))
        if meta.get("source") != SOURCE:
            continue
        if not os.path.exists(raw_path):
            os.replace(feat_path, raw_path)          # keep the uncleaned features once
        A = np.load(raw_path)
        keep = core_of(A)
        posts = meta.get("posts_all") or meta["posts"]
        meta["posts_all"] = posts
        reason = None if keep is not None else "参考图没有一致的主体"
        if keep is not None:
            d = cross(merge(A[keep])[None], known, check=False)[0]
            if d.size and d.min() < SAME_DIST:
                reason = "与已有角色 %s 几乎相同（距离 %.3f）：多为其伴生物或皮肤" % (known_ids[int(d.argmin())], d.min())
            elif accepted:
                dp = cross(merge(A[keep])[None], np.stack([a for _, a in accepted]), check=False)[0]
                if dp.min() < SAME_DIST:
                    twin = accepted[int(dp.argmin())][0]
                    reason = "与补充角色 %s 是同一人（距离 %.3f）：变身前后、本体与皮肤等，并入其别名" % (twin, dp.min())
                    meta["same_as"] = twin
        if reason:
            meta["rejected"] = reason
            for path in (feat_path,):
                if os.path.exists(path):
                    os.remove(path)
            with open(os.path.join(rejected, cid + ".json"), "w", encoding="utf-8") as f:
                json.dump(meta, f, ensure_ascii=False, indent=1)
            os.remove(meta_path)
            dropped += 1
            continue
        accepted.append((cid, merge(A[keep])))
        meta["posts"] = [posts[i] for i in keep]
        meta["refs_total"], meta["refs_kept"] = len(A), int(len(keep))
        np.save(feat_path, A[keep].astype(np.float32))
        with open(meta_path, "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=1)
        kept += 1
    log("清洗完成：保留 %d 个角色，放弃 %d 个（review/char-patch/rejected/）" % (kept, dropped))


def drop(ids, reason):
    """Give up patched characters after looking at their claims; they stay out on later fetch / clean runs."""
    rejected = os.path.join(PATCH, "rejected")
    os.makedirs(rejected, exist_ok=True)
    for cid in ids:
        meta_path, feat_path = os.path.join(IDX, cid + ".json"), os.path.join(IDX, cid + ".npy")
        meta = json.load(open(meta_path, encoding="utf-8"))
        if meta.get("source") != SOURCE:
            raise SystemExit("%s 不是补充角色，不能在这里放弃" % cid)
        meta["rejected"] = "人工看图后放弃：" + reason
        with open(os.path.join(rejected, cid + ".json"), "w", encoding="utf-8") as f:
            json.dump(meta, f, ensure_ascii=False, indent=1)
        os.remove(meta_path)
        os.remove(feat_path)
        log("已放弃 %s" % cid)


def backup():
    """Copy what apply/drop rewrites: anchors, the character tables and the external-index claims."""
    import shutil
    folder = os.path.join(PATCH, "backup", time.strftime("%Y%m%d-%H%M%S"))
    os.makedirs(folder)
    for name in ("char-anchors.npy", "char-anchors.txt"):
        if os.path.exists(os.path.join(REVIEW, name)):
            shutil.copy2(os.path.join(REVIEW, name), folder)
    src = sqlite3.connect("file:%s?mode=ro" % os.path.join(REVIEW, "pi.db"), uri=True)
    out = sqlite3.connect(os.path.join(folder, "characters.sqlite"))
    for table in ("characters", "character_aliases"):
        sql = src.execute("SELECT sql FROM sqlite_master WHERE name=?", (table,)).fetchone()
        if sql:
            out.execute(sql[0])
            rows = src.execute("SELECT * FROM %s" % table).fetchall()
            if rows:
                out.executemany("INSERT INTO %s VALUES (%s)" % (table, ",".join("?" * len(rows[0]))), rows)
    out.execute("CREATE TABLE char_ext(uuid,value,conf,source,model_ver,run_id)")
    out.executemany("INSERT INTO char_ext VALUES (?,?,?,?,?,?)",
                    src.execute("SELECT uuid,value,conf,source,model_ver,run_id FROM labels WHERE dim='char_ext'"))
    out.commit()
    out.close()
    src.close()
    kept = sorted(n for n in os.listdir(os.path.join(PATCH, "backup")) if re.fullmatch(r"\d{8}-\d{6}", n))
    for old in kept[:-5]:                 # the first hand-made backup (files at the top level) always stays
        shutil.rmtree(os.path.join(PATCH, "backup", old))
    log("已备份到 %s" % folder)


def rebuild():
    """Anchors and thresholds for the current index, then a rescan of the whole library into labels (a new run id)."""
    import subprocess
    progress("build")
    subprocess.run([sys.executable, "-u", os.path.join(ROOT, "tools", "build_characters.py")], check=True)
    db = sqlite3.connect("file:%s?mode=ro" % os.path.join(REVIEW, "pi.db"), uri=True)
    runs = [int(m.group(1)) for (r,) in db.execute("SELECT DISTINCT run_id FROM labels WHERE dim='char_ext'")
            for m in [re.fullmatch(r"char-scan-(\d+)", r or "")] if m]
    db.close()
    progress("scan")
    subprocess.run([sys.executable, "-u", os.path.join(ROOT, "tools", "char_scan.py"), "--load",
                    "--run", "char-scan-%d" % (max(runs, default=1) + 1),
                    "--index", "character_index@2025-09-25+danbooru@" + time.strftime("%Y-%m-%d")], check=True)


def apply(only, per):
    backup()
    fetch(per, only)
    progress("clean")
    clean(only)
    if any(os.path.exists(os.path.join(IDX, cid + ".json")) for cid in only):
        rebuild()
    else:
        log("一个都没补进来，不用重建和重扫")
    progress("done")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("plan")
    p.add_argument("--works", required=True, help="Danbooru 作品 tag，逗号分隔")
    p.add_argument("--min-posts", type=int, default=100)
    f = sub.add_parser("fetch")
    f.add_argument("--per", type=int, default=20, help="每个角色取几张单人图")
    sub.add_parser("clean")
    d = sub.add_parser("drop")
    d.add_argument("ids", nargs="+")
    d.add_argument("--reason", required=True)
    d.add_argument("--rebuild", action="store_true", help="放弃后重建锚点并重扫（设置页用）")
    sub.add_parser("rebuild", help="只重建锚点并重扫全库")
    a = sub.add_parser("apply")
    a.add_argument("--only", required=True, help="plan.json 里的 short_tag，逗号分隔")
    a.add_argument("--per", type=int, default=20)
    args = ap.parse_args()
    if args.cmd == "plan":
        plan(args.works.split(","), args.min_posts)
    elif args.cmd == "fetch":
        fetch(args.per)
    elif args.cmd == "rebuild":
        backup()
        rebuild()
        progress("done")
    elif args.cmd == "apply":
        apply(set(args.only.split(",")), args.per)
    elif args.cmd == "drop":
        if args.rebuild:
            backup()
        drop(args.ids, args.reason)
        if args.rebuild:
            rebuild()
            progress("done")
    else:
        clean()


if __name__ == "__main__":
    main()
