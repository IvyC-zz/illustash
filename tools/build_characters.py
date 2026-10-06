#!/usr/bin/env python3
"""C 层：角色实体表 —— 把「人工相册名 / WD 标签 / danbooru tag / 各语言角色名 / 外部锚点」对齐成一张表。

为什么需要这层
--------------
在这之前，同一个角色在库里有五个互不相认的名字：
    主人的相册名      聖園ミカ        （decisions.value = 'mika'）
    WD 标签名         mika_(blue_archive)
    character_index   mika_blue_archive
    danbooru          mika_(blue_archive)
    游戏官方          Misono Mika / 聖園ミカ / 美园美咲
靠人肉对齐已经错过一次（`miyo` → `murosaki_miyo`，见 review/album-char-map.json）。

产出
----
1. pi.db 的 `characters` 表          每个角色一行（canonical id = character_index 的 short_tag）
2. pi.db 的 `character_aliases` 表   一个角色多个名字，带 kind（en/ja/zh/danbooru/wd/album/base）
3. review/char-anchors.npy          (N, 768) 每角色一个 CCIP 锚点（官方 ccip_merge）
4. review/char-anchors.txt          锚点行序对应的 char_id

用法
----
    .venv/bin/python -u tools/build_characters.py            # 建表
    .venv/bin/python -u tools/build_characters.py --no-anchor  # 只建表不算锚点（快）
"""
import argparse
import csv
import glob
import json
import os
import re
import sqlite3
import sys
import time

import numpy as np
from numpy.linalg import norm

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REVIEW = os.path.join(ROOT, "review")
DB = os.path.join(REVIEW, "pi.db")
IDX = os.path.join(REVIEW, "char-index")
GAMES = os.path.join(REVIEW, "game-chars")
TAGS = os.path.join(ROOT, "tags", "tags_v3.csv")


def log(m):
    print("[%s] %s" % (time.strftime("%H:%M:%S"), m), flush=True)


def merge(embs):
    """官方 ccip_merge：归一化后取均值，再按平均模长还原尺度。"""
    L = norm(embs, axis=-1)
    u = embs / L.reshape(-1, 1)
    c = u.mean(axis=0)
    return c / norm(c) * L.mean()


def split_danbooru_tag(tag):
    """mika_(swimsuit)_(blue_archive) -> ('mika', 'swimsuit', 'blue_archive')

    danbooru 的角色 tag 形如 name_(sub1)_(sub2)_(copyright)。最后一段通常是作品，
    中间的括号段是皮肤/形态。没有括号的（murosaki_miyo）就只有 name。
    """
    parts = re.findall(r"\(([^()]+)\)", tag)
    base = re.sub(r"_?\([^()]+\)", "", tag).strip("_")
    if not parts:
        return base, None, None
    copyright_ = parts[-1]
    subs = parts[:-1]
    return base, ("_".join(subs) if subs else None), copyright_


def fold_plain_skins(rows):
    """[(char_id, tag, base, skin, copyright)] -> {char_id: (skin, copyright)} for single-qualifier skins.

    split_danbooru_tag() reads mori_calliope_(sleepwear) as character + copyright 'sleepwear'. When the
    unqualified tag (mori_calliope) is itself a character, the qualifier is a skin/form of it, and the
    copyright is the plain character's. Checked 2026-09-25 on all 405 such pairs: every one is the same
    character (1st_costume, magical_girl, timeskip, herrscher forms, collab outfits). The copyright list of
    the index can't decide this instead: Danbooru qualifiers abbreviate (kancolle vs kantai_collection).
    """
    rows = list(rows)
    plain = {tag: cr for _, tag, base, _, cr in rows if tag == base}
    return {cid: (cr, plain[base]) for cid, tag, base, skin, cr in rows
            if not skin and cr and tag != base and base in plain}


def load_index_meta():
    """review/char-index/*.json -> {char_id: data}"""
    out = {}
    for p in glob.glob(os.path.join(IDX, "*.json")):
        try:
            with open(p, encoding="utf-8") as f:
                d = json.load(f)
        except Exception:
            continue
        cid = d.get("short_tag") or os.path.basename(p)[:-5]
        out[cid] = d
    return out


def load_game_names():
    """deepghs/game_characters -> {(game, en_lower): {'en','ja','zh', ...}}

    index.json 是游戏官方角色表（含中/英/日名），danbooru_tags.json 给出
    danbooru tag 与 title/subtitles 的拆分，两者靠英文名接起来。
    """
    by_game = {}
    for p in glob.glob(os.path.join(GAMES, "*_index.json")):
        game = os.path.basename(p).rsplit("_index.json", 1)[0]
        try:
            with open(p, encoding="utf-8") as f:
                d = json.load(f)
        except Exception:
            continue
        rows = d.get("data", d if isinstance(d, list) else [])
        m = {}
        for r in rows:
            en = r.get("ennames") or []
            ja = r.get("jpnames") or []
            zh = r.get("cnnames") or []
            rec = {
                "en": en[0] if en else None,
                "ja": ja[0] if ja else None,
                "zh": zh[0] if zh else None,
                "en_all": en, "ja_all": ja, "zh_all": zh,
                "meta": r.get("data") or {},
                "n_skins": len(r.get("skins") or []),
            }
            for n in en:
                m.setdefault(n.strip().lower(), rec)
                # "Misono Mika" 也按姓/名分别登记，danbooru 的 title 往往只有名
                for part in n.split():
                    m.setdefault(part.strip().lower(), rec)
        by_game[game] = m
    return by_game


# danbooru 的 copyright 名 vs game_characters 的目录名
GAME_DIR = {
    "blue_archive": "bluearchive", "arknights": "arknights", "azur_lane": "azurlane",
    "fate_grand_order": "fgo", "fate/grand_order": "fgo", "genshin_impact": "genshin",
    "girls_frontline": "girlsfrontline", "goddess_of_victory:_nikke": "nikke",
    "nikke": "nikke", "path_to_nowhere": "pathtonowhere", "honkai:_star_rail": "starrail",
    "neural_cloud": "neuralcloud",
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--no-anchor", action="store_true", help="不算锚点（跳过读 .npy）")
    ap.add_argument("--db", default=DB)
    ap.add_argument("--fix-skins", action="store_true", help="只就地修正已有 characters 表里的单括号皮肤（fold_plain_skins），不重建")
    args = ap.parse_args()

    if args.fix_skins:
        db = sqlite3.connect(args.db)
        with db:
            fix = fold_plain_skins(db.execute("SELECT char_id,danbooru_tag,base_tag,skin,copyright FROM characters"))
            db.executemany("UPDATE characters SET skin=?,copyright=? WHERE char_id=?", [(s, c, cid) for cid, (s, c) in fix.items()])
        log("单括号皮肤归到本体：%d 行" % len(fix))
        db.close()
        return 0

    log("读 character_index 元数据 ...")
    meta = load_index_meta()
    log("  %d 个角色" % len(meta))
    if not meta:
        print("review/char-index/ 是空的 —— 先跑 tools/fetch_char_index.sh", file=sys.stderr)
        return 1

    log("读 WD 标签表 ...")
    wd = set()
    with open(TAGS, encoding="utf-8") as f:
        for r in csv.DictReader(f):
            if r.get("category") == "4":
                wd.add(r["name"])
    log("  WD character 标签 %d 个" % len(wd))

    log("读游戏官方角色表 ...")
    games = load_game_names()
    log("  %d 个游戏：%s" % (len(games), ", ".join(sorted(games))))

    with open(os.path.join(REVIEW, "album-char-map.json"), encoding="utf-8") as f:
        amap = json.load(f)["map"]

    rows, aliases, anchor_rows, anchor_ids = [], [], [], []
    n_wd = n_named = n_skin = 0
    tags = {cid: d.get("tag") or cid for cid, d in meta.items()}
    split = {cid: split_danbooru_tag(tag) for cid, tag in tags.items()}
    folded = fold_plain_skins((cid, tags[cid], base, skin, cr or meta[cid].get("copyright"))
                              for cid, (base, skin, cr) in split.items())
    for cid, d in sorted(meta.items()):
        tag = tags[cid]
        base, skin, cr = split[cid]
        skin, cr = folded.get(cid, (skin, cr))
        cr = cr or d.get("copyright")
        g = games.get(GAME_DIR.get(cr or "", ""), {})
        rec = g.get((base or "").lower())
        name_en = name_ja = name_zh = None
        if rec:
            name_en, name_ja, name_zh = rec["en"], rec["ja"], rec["zh"]
            n_named += 1
        has_wd = tag in wd
        n_wd += has_wd
        n_skin += bool(skin)

        p50 = p75 = p90 = n_ref = arow = None
        if not args.no_anchor:
            fp = os.path.join(IDX, cid + ".npy")
            if os.path.exists(fp):
                try:
                    A = np.load(fp)
                except Exception:
                    A = None
                if A is not None and A.ndim == 2 and len(A) >= 3:
                    a = merge(A)
                    # 组内 p90：到自家锚点的距离分布，后面当这个角色的自适应阈值
                    u = A / norm(A, axis=1, keepdims=True)
                    an = a / norm(a)
                    # 与 model_metrics 同档：(1 - cos)/2，见 tools/ccip.cross()
                    dd = (1 - u @ an) / 2
                    p50, p75 = (float(np.percentile(dd, q)) for q in (50, 75))
                    p90 = float(np.percentile(dd, 90))
                    n_ref = int(len(A))
                    arow = len(anchor_rows)
                    anchor_rows.append(a)
                    anchor_ids.append(cid)

        # gender 的把握度顺便当质量分用：character_index 里混着一些「非人物条目」
        # （例如 crystalfly_(genshin_impact) 是一种萤火虫），它们收的是「画面里有这东西的
        # 角色图」，gender 会摇摆在 0.6~0.7；真角色基本都 ≥0.97。
        gender = gender_conf = None
        gd = d.get("gender") or {}
        if gd and max(gd.values()) > 0:
            gender = max(gd, key=gd.get)
            gender_conf = float(gd[gender])

        rows.append((cid, tag, tag if has_wd else None, cr, base, skin,
                     d.get("post_count"), gender, gender_conf,
                     json.dumps(d.get("core_tags") or [], ensure_ascii=False),
                     n_ref, p50, p75, p90, arow, name_en, name_ja, name_zh))

        aliases.append((cid, cid, "short"))
        aliases.append((cid, tag, "danbooru"))
        if has_wd:
            aliases.append((cid, tag, "wd"))
        if base:
            aliases.append((cid, base, "base"))
        if rec:
            for k, kind in (("en_all", "en"), ("ja_all", "ja"), ("zh_all", "zh")):
                for n in rec.get(k) or []:
                    aliases.append((cid, n, kind))

    for album, info in amap.items():
        for cid in [info["char_id"]] + list(info.get("also") or []):
            aliases.append((cid, album, "album"))

    log("写库 ...")
    db = sqlite3.connect(args.db)
    db.executescript("""
    DROP TABLE IF EXISTS characters;
    DROP TABLE IF EXISTS character_aliases;
    CREATE TABLE characters (
      char_id      TEXT PRIMARY KEY,
      danbooru_tag TEXT,
      wd_tag       TEXT,          -- NULL = WD 的 2751 个角色标签里没有它
      copyright    TEXT,
      base_tag     TEXT,          -- 皮肤变体归并用：mika_(swimsuit)_(blue_archive) -> mika
      skin         TEXT,          -- swimsuit / idol / ... ，NULL = 本体
      post_count   INTEGER,       -- danbooru 全球热度
      gender       TEXT,
      gender_conf  REAL,          -- <0.9 多半是「非人物条目」，判定时要当心
      core_tags    TEXT,          -- JSON 数组，deepghs 标注的固有特征
      n_ref        INTEGER,       -- character_index 里的参考图数
      anchor_p50   REAL,          -- 三档自适应阈值：紧 / 中 / 松
      anchor_p75   REAL,
      anchor_p90   REAL,
      anchor_row   INTEGER,       -- review/char-anchors.npy 的行号
      name_en      TEXT, name_ja TEXT, name_zh TEXT
    );
    CREATE TABLE character_aliases (
      char_id TEXT NOT NULL, alias TEXT NOT NULL, kind TEXT NOT NULL,
      PRIMARY KEY (char_id, alias, kind)
    );
    """)
    db.executemany("INSERT INTO characters VALUES (%s)" % ",".join("?" * 18), rows)
    db.executemany("INSERT OR IGNORE INTO character_aliases VALUES (?,?,?)", aliases)
    db.executescript("""
    CREATE INDEX idx_char_wd ON characters(wd_tag);
    CREATE INDEX idx_char_cr ON characters(copyright);
    CREATE INDEX idx_char_base ON characters(base_tag);
    CREATE INDEX idx_alias ON character_aliases(alias);
    """)
    db.commit()

    if anchor_rows:
        np.save(os.path.join(REVIEW, "char-anchors.npy"), np.stack(anchor_rows).astype(np.float32))
        with open(os.path.join(REVIEW, "char-anchors.txt"), "w") as f:
            f.write("\n".join(anchor_ids))
        log("锚点 %d × 768 -> review/char-anchors.npy" % len(anchor_rows))

    log("完成：characters %d 行 / aliases %d 行" % (len(rows), db.execute(
        "SELECT COUNT(*) FROM character_aliases").fetchone()[0]))
    log("  其中 WD 认得的 %d（%.1f%%），WD 认不出的 %d" % (n_wd, 100 * n_wd / len(rows), len(rows) - n_wd))
    log("  皮肤/形态变体 %d，有官方中英日名的 %d" % (n_skin, n_named))
    low = db.execute("SELECT COUNT(*) FROM characters WHERE gender_conf < 0.9").fetchone()[0]
    log("  gender 把握度 <0.9 的 %d 个（疑似非人物条目，判定时要当心）" % low)
    db.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
