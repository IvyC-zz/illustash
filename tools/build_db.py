#!/usr/bin/env python3
"""第 1 步：建标签库，把已经算好的东西迁进去。

纯 CPU，不碰 GPU，几分钟。产出 review/pi.db。

核心约定：
    labels      模型产出，可重算、可多版本
    decisions   人工结论，永不被模型覆盖

用法：
    .venv/bin/python tools/build_db.py
"""
import csv
import os
import sqlite3
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
DB = os.path.join(REVIEW, "pi.db")
LIB = os.environ.get("PI_PHOTOS_LIBRARY", os.path.expanduser("~/Pictures/Photos Library.photoslibrary"))
PHOTOS_DB = os.path.join(LIB, "database", "Photos.sqlite")

SCHEMA = """
CREATE TABLE IF NOT EXISTS assets (
    uuid        TEXT PRIMARY KEY,
    filename    TEXT,
    z_pk        INTEGER,
    width       INTEGER,
    height      INTEGER,
    added_at    TEXT,
    hidden      INTEGER,
    trashed     INTEGER,
    stable_hash TEXT,
    orig_size   INTEGER,
    src_kind    TEXT,
    src_path    TEXT,
    in_scope    INTEGER DEFAULT 1
);
CREATE INDEX IF NOT EXISTS idx_assets_zpk ON assets(z_pk);

CREATE TABLE IF NOT EXISTS labels (
    uuid      TEXT NOT NULL,
    dim       TEXT NOT NULL,
    value     TEXT NOT NULL,
    conf      REAL,
    source    TEXT NOT NULL,
    model_ver TEXT,
    run_id    TEXT,
    ts        TEXT DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (uuid, dim, value, source)
);
CREATE INDEX IF NOT EXISTS idx_labels_dim_val ON labels(dim, value);
CREATE INDEX IF NOT EXISTS idx_labels_uuid ON labels(uuid);

CREATE TABLE IF NOT EXISTS decisions (
    uuid  TEXT NOT NULL,
    dim   TEXT NOT NULL,
    value TEXT NOT NULL,
    by    TEXT,
    ts    TEXT DEFAULT CURRENT_TIMESTAMP,
    note  TEXT,
    -- 主键含 value：一张图在一个维度上可以有多个人工结论
    -- （实测需要：4 张图同时被放进两个角色相册，都是多角色同框，两个标注都对）
    PRIMARY KEY (uuid, dim, value)
);

CREATE TABLE IF NOT EXISTS runs (
    run_id    TEXT PRIMARY KEY,
    dim       TEXT,
    model     TEXT,
    model_ver TEXT,
    started   TEXT,
    finished  TEXT,
    n_assets  INTEGER,
    params    TEXT
);

CREATE TABLE IF NOT EXISTS shards (
    shard    TEXT PRIMARY KEY,
    n_files  INTEGER,
    machine  TEXT,
    started  TEXT,
    finished TEXT,
    bytes    INTEGER
);
"""


def tag_categories():
    """tag -> category（0=general 4=character 9=rating）"""
    p = os.path.join(ROOT, "tags", "tags_v3.csv")
    with open(p, encoding="utf-8") as fh:
        return {r["name"]: int(r["category"]) for r in csv.DictReader(fh)}


def main():
    t0 = time.time()
    if os.path.exists(DB):
        os.remove(DB)
    db = sqlite3.connect(DB)
    db.executescript(SCHEMA)
    print(f"schema created -> {DB}")

    # ---------- assets ----------
    man = list(csv.DictReader(open(os.path.join(REVIEW, "full-manifest.csv"))))
    print(f"manifest {len(man)}")
    prow = {}
    pdb = sqlite3.connect(f"file:{PHOTOS_DB}?mode=ro", uri=True)
    pdb.execute("PRAGMA query_only=ON")
    for r in pdb.execute("""
        SELECT a.ZUUID, a.Z_PK, a.ZWIDTH, a.ZHEIGHT, a.ZHIDDEN, a.ZTRASHEDSTATE,
               a.ZADDEDDATE, d.ZORIGINALSTABLEHASH, d.ZORIGINALFILESIZE
        FROM ZASSET a
        LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES = d.Z_PK
    """):
        prow[r[0].upper()] = r
    pdb.close()
    print(f"photos rows {len(prow)}")

    rows = []
    for r in man:
        u = r["uuid"].upper()
        p = prow.get(u)
        added = ""
        if p and p[6] is not None:
            # Core Data 时间戳 = Unix - 978307200
            import datetime
            added = datetime.datetime.fromtimestamp(p[6] + 978307200).isoformat(timespec="seconds")
        rows.append((
            u, r["file"],
            p[1] if p else None, p[2] if p else None, p[3] if p else None,
            added, p[4] if p else None, p[5] if p else None,
            p[7] if p else None, p[8] if p else None,
            r["source_kind"], r["src"], 1,
        ))
    db.executemany("INSERT OR REPLACE INTO assets VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)", rows)
    db.commit()
    print(f"assets inserted {len(rows)}")

    # ---------- labels: WD 角色 / rating ----------
    cats = tag_categories()
    chars = {t for t, c in cats.items() if c == 4}
    ratings = {t for t, c in cats.items() if c == 9}
    f2u = {r["file"]: r["uuid"].upper() for r in man}

    lab = []
    skip = 0
    for name in ("wd-char-0.csv", "wd-char-1.csv"):
        p = os.path.join(REVIEW, "wd", name)
        if not os.path.exists(p):
            continue
        with open(p, encoding="utf-8") as fh:
            for r in csv.DictReader(fh):
                u = f2u.get(r["file"])
                if not u:
                    skip += 1
                    continue
                t = r["tag"]
                if t in chars:
                    dim = "character"
                elif t in ratings:
                    dim = "rating"
                else:
                    dim = "content"      # 兜底
                lab.append((u, dim, t, float(r["prob"]), "wd_v3", "swinv2_448", "wd-full-1"))
    db.executemany(
        "INSERT OR REPLACE INTO labels (uuid,dim,value,conf,source,model_ver,run_id) "
        "VALUES (?,?,?,?,?,?,?)", lab)
    db.commit()
    print(f"labels (wd) inserted {len(lab)}  (skipped {skip})")

    # ---------- labels: WD general（top20，暂时） ----------
    gen = []
    for name in ("wd-top-0.csv", "wd-top-1.csv"):
        p = os.path.join(REVIEW, "wd", name)
        if not os.path.exists(p):
            continue
        with open(p, encoding="utf-8") as fh:
            for r in csv.DictReader(fh):
                u = f2u.get(r["file"])
                if u:
                    gen.append((u, "content", r["tag"], float(r["prob"]),
                                "wd_v3", "swinv2_448", "wd-full-1-top20"))
    db.executemany(
        "INSERT OR REPLACE INTO labels (uuid,dim,value,conf,source,model_ver,run_id) "
        "VALUES (?,?,?,?,?,?,?)", gen)
    db.commit()
    print(f"labels (general top20) inserted {len(gen)}")

    # ---------- labels: CCIP 距离 ----------
    tiers = []
    p = os.path.join(REVIEW, "mika-strata.csv")
    if os.path.exists(p):
        with open(p, encoding="utf-8") as fh:
            for r in csv.DictReader(fh):
                tiers.append((r["uuid"].upper(), "ccip", "mika_dist",
                              float(r["d_anchor"]), "ccip", "caformer24", "ccip-1"))
    db.executemany(
        "INSERT OR REPLACE INTO labels (uuid,dim,value,conf,source,model_ver,run_id) "
        "VALUES (?,?,?,?,?,?,?)", tiers)
    db.commit()
    print(f"labels (ccip dist) inserted {len(tiers)}")

    # ---------- decisions: 相册成员 = 人工整理的结论 ----------
    dec = []
    for pk, name in ((1341, "mika"), (1344, "miyo"), (1342, "seia")):
        for r in db.execute("SELECT uuid FROM assets"):
            pass
        cur = sqlite3.connect(f"file:{PHOTOS_DB}?mode=ro", uri=True)
        cur.execute("PRAGMA query_only=ON")
        us = [x[0].upper() for x in cur.execute(
            "SELECT ZUUID FROM ZASSET WHERE Z_PK IN "
            "(SELECT Z_3ASSETS FROM Z_33ASSETS WHERE Z_33ALBUMS=?) AND ZKIND=0", (pk,))]
        cur.close()
        for u in us:
            dec.append((u, "character", name, "human:album", None))
    db.executemany(
        "INSERT OR REPLACE INTO decisions (uuid,dim,value,by,note) VALUES (?,?,?,?,?)", dec)
    db.commit()
    print(f"decisions (album membership) inserted {len(dec)}")

    # ---------- runs ----------
    db.execute("INSERT OR REPLACE INTO runs VALUES (?,?,?,?,?,?,?,?)",
               ("wd-full-1", "character,rating,content", "wd_swinv2_v3_emb", "swinv2_448",
                "2026-09-21T20:31", "2026-09-21T21:48", 46980, "prob>=0.30, top20 general"))
    db.commit()

    # ---------- 汇总 ----------
    print("\n=== 库统计 ===")
    for t in ("assets", "labels", "decisions", "runs"):
        n = db.execute(f"SELECT COUNT(*) FROM {t}").fetchone()[0]
        print(f"  {t:<10} {n:>8}")
    print("  labels 按维度:")
    for d, n in db.execute("SELECT dim, COUNT(*) FROM labels GROUP BY dim ORDER BY 2 DESC"):
        print(f"    {d:<12} {n:>8}")
    print(f"\n  大小 {os.path.getsize(DB)/1e6:.1f} MB   耗时 {time.time()-t0:.1f}s")
    db.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
