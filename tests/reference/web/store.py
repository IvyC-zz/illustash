"""仅供合成测试及对照工具；不属于生产应用宿主。"""
"""Transactional curated knowledge and a rebuildable browse index."""
import csv
import threading
from web import effects
from collections import Counter
import json
import os
import random
import re
import shutil
import sqlite3
import time
import uuid
from contextlib import contextmanager
from pathlib import Path

from web.relate import GROUPING as SIMILAR_KINDS
from web import names
from web import terms_core
from web import terms_route
from web.library import backups_dir
from web.identity import LEAD_RULE, NOT_MASCOT, WEAK_CCIP

# Look-alike relations folded like byte-identical copies unless the settings say otherwise.
FOLD_DEFAULT = ('duplicate', 'identical')

SCHEMA = """
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS entities(id TEXT PRIMARY KEY,name TEXT NOT NULL,work TEXT NOT NULL DEFAULT '',external_key TEXT UNIQUE);
CREATE TABLE IF NOT EXISTS assets(uuid TEXT PRIMARY KEY,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT);
CREATE INDEX IF NOT EXISTS assets_date ON assets(added_at DESC,uuid);
CREATE TABLE IF NOT EXISTS candidates(uuid TEXT NOT NULL,entity TEXT NOT NULL,source TEXT NOT NULL,score REAL NOT NULL,PRIMARY KEY(uuid,entity,source));
CREATE INDEX IF NOT EXISTS candidates_entity ON candidates(entity,uuid);
CREATE TABLE IF NOT EXISTS general_hits(uuid TEXT NOT NULL,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS assertions(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL CHECK(status IN ('confirmed','rejected')),origin TEXT NOT NULL,PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS assertions_entity ON assertions(entity,status,uuid);
CREATE TABLE IF NOT EXISTS deferred(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS imported(uuid TEXT NOT NULL,value TEXT NOT NULL,PRIMARY KEY(uuid,value));
CREATE TABLE IF NOT EXISTS asset_tags(uuid TEXT NOT NULL,tag TEXT NOT NULL,origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,tag));
CREATE INDEX IF NOT EXISTS asset_tags_tag ON asset_tags(tag,uuid);
CREATE TABLE IF NOT EXISTS asset_rating(uuid TEXT PRIMARY KEY,rating TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS actions(id INTEGER PRIMARY KEY AUTOINCREMENT,label TEXT NOT NULL,before_json TEXT NOT NULL,ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,undone INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS image_sigs(uuid TEXT PRIMARY KEY,ratio REAL,grid BLOB,ok INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS relations(a TEXT NOT NULL,b TEXT NOT NULL,kind TEXT NOT NULL,changed REAL,note TEXT NOT NULL DEFAULT '',
 checked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(a,b));
CREATE TABLE IF NOT EXISTS links(system TEXT NOT NULL,key TEXT NOT NULL,entity_id TEXT NOT NULL REFERENCES entities(id),
 current INTEGER NOT NULL DEFAULT 1,source TEXT NOT NULL,added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(system,key));
CREATE INDEX IF NOT EXISTS links_entity ON links(entity_id,system);
CREATE TABLE IF NOT EXISTS names(entity_id TEXT NOT NULL REFERENCES entities(id),lang TEXT NOT NULL DEFAULT '' CHECK(lang IN ('','zh','ja','en')),
 value TEXT NOT NULL,source TEXT NOT NULL,role TEXT NOT NULL CHECK(role IN ('display','alias')),preferred INTEGER NOT NULL DEFAULT 0,
 position INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(entity_id,role,lang,source,value));
CREATE INDEX IF NOT EXISTS names_value ON names(value COLLATE NOCASE);
"""
# Questions asked while browsing (主动提问) and how they were answered: keeps an image from being asked twice,
# and later gives calibration an unbiased sample of machine claims. action_id ties an answer to its undo step.
# Human verdicts on the artists provenance/ found (docs/PROVENANCE.md section 8): 'yes' confirms, 'no' hides that artist for
# that picture. Machine results stay in provenance.sqlite untouched; the core applies these when it reads them (tests/reference/web/artists.py).
ARTIST_VERDICTS = """CREATE TABLE IF NOT EXISTS artist_verdicts(uuid TEXT NOT NULL,artist TEXT NOT NULL,
 verdict TEXT NOT NULL CHECK(verdict IN ('yes','no')),created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,PRIMARY KEY(uuid,artist));
CREATE INDEX IF NOT EXISTS artist_verdicts_artist ON artist_verdicts(artist,verdict)"""
ASKED = """CREATE TABLE IF NOT EXISTS asked(uuid TEXT NOT NULL,entity TEXT NOT NULL,kind TEXT NOT NULL,
 answer TEXT NOT NULL CHECK(answer IN ('yes','no','later')),action_id INTEGER,asked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 PRIMARY KEY(uuid,entity));
CREATE INDEX IF NOT EXISTS asked_entity ON asked(entity,answer,asked_at);
CREATE TABLE IF NOT EXISTS audit_sample(uuid TEXT PRIMARY KEY,entity TEXT NOT NULL,stratum TEXT NOT NULL,
 solo INTEGER NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP)"""
# 随机抽查: machine claims drawn uniformly over images nobody had judged, plus a stratum of
# learning-only (PROTO) claims. Answers are the unbiased ground truth the evaluation bench scores against.
AUDIT_UNIFORM, AUDIT_PROTO = 250, 50
# Open = the sampled claim itself is still unanswered: no Y/N, no verdict on that character (or a skin of it), and on a
# single-person image nobody else confirmed. Fixing the image's other tags leaves the question open.
AUDIT_OPEN = """EXISTS(SELECT 1 FROM audit_sample u WHERE u.uuid={0}.uuid)
 AND NOT EXISTS(SELECT 1 FROM asked k JOIN audit_sample u ON u.uuid=k.uuid AND u.entity=k.entity WHERE k.uuid={0}.uuid AND k.answer<>'later')
 AND NOT EXISTS(SELECT 1 FROM audit_sample u JOIN assertions s ON s.uuid=u.uuid JOIN entities e ON e.id=s.entity
   WHERE u.uuid={0}.uuid AND (coalesce(e.parent_id,e.id)=u.entity OR (s.status='confirmed' AND u.solo=1)))"""
SCHEMA += ASKED + ';'
# Up to this many touched images, memberships are refreshed image by image (Store._refresh_memberships).
FEW_TOUCHED = 10   # ~25 ms an image one by one vs ~150 ms fixed + ~1 ms an image as a set
# A character with fewer human-confirmed images than this has no learned prototype yet (web/learn.py MIN_SEEDS).
ASK_SEEDS = 5
# External identifiers are links on an identity (docs/DATA_MODEL.md); an entity is found by its links, never by a name.
# ccip_base: the index's (base tag, work) pair for a skin whose base character the index lacks.
LINK_SYSTEMS = ('danbooru', 'ccip', 'ccip_base', 'wd', 'bangumi', 'photos_album', 'qualifier')
# These SQL functions only render/search; identity is the id and its links.
NAME = DISPLAY = "entity_name(e.id)"
SCHEMA_VERSION = '36'
# Identities that start as「不算角色，只当标签」in a new library (entities.mascot_origin='default'): player avatars
# without a fixed look and franchise mascots. Keys are Danbooru tags checked against the index links (2026-09-27);
# designed protagonists (旅行者, 开拓者, 藤丸立香, the anime Sensei…) stay characters. The user can turn any back.
DEFAULT_TAG_KEYS = ('sensei_(blue_archive)', 'doodle_sensei_(blue_archive)', 'peroro_(blue_archive)',
                    'doctor_(arknights)', 'male_doctor_(arknights)', 'female_doctor_(arknights)', 'trainer_(umamusume)',
                    'producer_(idolmaster)', 'p-head_producer', 'commander_(azur_lane)', "commander_(girls'_frontline)",
                    'admiral_(kancolle)', 'master_(vocaloid)')
# WD rating labels counted as NSFW for the hide/only filter; `sensitive` stays visible.
NSFW = ('questionable', 'explicit')


def day_bounds(day):
    """[start, end) of one added_at calendar day; added_at is ISO text, so plain string comparison works."""
    try:
        start = time.strptime(day, '%Y-%m-%d')
    except (TypeError, ValueError):
        raise Invalid('日期不正确')
    nxt = time.localtime(time.mktime((start.tm_year, start.tm_mon, start.tm_mday + 1, 12, 0, 0, 0, 0, -1)))
    return day, time.strftime('%Y-%m-%d', nxt)


def rating(alias, nsfw):
    """Extra predicate for the NSFW view mode ('' shows all), so every count matches what the grid shows."""
    if nsfw not in ('', 'hide', 'only'):
        raise Invalid('未知内容筛选')
    return f" AND {'NOT ' if nsfw == 'hide' else ''}coalesce({alias}.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN {NSFW!r}),0)" if nsfw else ''

def _indexes_a_view(statement, views):
    """An index on a table that is now one of the terms core's views (general_hits, asset_rating, asset_tags once
    labels are the core's, docs/TERMS_LABELS.md): SQLite refuses it even with IF NOT EXISTS, so upgrades skip it."""
    m = re.match(r'\s*CREATE\s+(?:UNIQUE\s+)?INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?\S+\s+ON\s+(\w+)', statement, re.I)
    return bool(m) and m.group(1) in views


# A copy whose characters differ from another copy in its group (status aside: confirmed vs machine is not a conflict).
MIXED = """EXISTS(SELECT 1 FROM duplicates y WHERE y.grp=d.grp AND y.uuid<>d.uuid AND (
 EXISTS(SELECT entity FROM memberships WHERE uuid=d.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=y.uuid) OR
 EXISTS(SELECT entity FROM memberships WHERE uuid=y.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=d.uuid)))"""
# Folded copies: a consistent group shows only its representative everywhere except the duplicates page.
UNFOLDED = 'NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid={0}.uuid AND h.hidden=1)'
# What the library counts and lists: unfolded and not excluded from the collection.
VISIBLE = UNFOLDED + ' AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid={0}.uuid)'
# Collection rules, checked against sample sheets on 2026-09-23 (docs/WEB_V2.md). WD is trained on illustrations:
# `cosplay` means a drawn character in another's outfit and `cellphone_photo` a drawn phone, so neither is used.
REALISTIC = 0.15
SHOT_TAGS = ('chat_log', 'fake_screenshot', 'fake_phone_screenshot', 'user_interface')
TEXT_TAGS = ('text_focus', 'chinese_text', 'korean_text', 'english_text')
PEOPLE_TAGS = ('1girl', '1boy', 'solo', 'multiple_girls', 'multiple_boys', '1other', 'male_focus')
REASONS = ('photo', 'screenshot', 'text', 'manual')
# Strong single evidence, calibrated on human verdicts (2026-09-23): WD >= 0.9 was wrong 0.0-0.7% of the time
# (0.8-0.9: 12.5%, below 0.8: ~30-80%); external index score >= 0.95 (CCIP distance <= 0.05) 0.1% (0.90-0.95: 11.8%).
STRONG_WD, STRONG_CCIP = 0.9, 0.95
# Machine evidence: model candidates plus claims of prototypes learned from the user's own verdicts (web/learn.py).
# PROTO is CCIP distance to those prototypes, so it counts on the external-index side of the ranking.
EVIDENCE = "(SELECT uuid,entity,source,score FROM candidates UNION ALL SELECT uuid,entity,'PROTO',score FROM proto_claims)"
# Leads for「也可能是」: strong evidence plus weak claims (middle-tier matches that never decide ownership).
HINTS = "(SELECT uuid,entity,score,0 AS weak FROM family_evidence UNION ALL SELECT uuid,entity,score,1 FROM weak_claims)"
# Machine leads a person has not rejected; weak claims sort after all strong evidence (scores stay below 1).
LEADS = f"(SELECT v.* FROM {HINTS} v WHERE NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=v.entity AND s.status='rejected'))"
HINT_KEY = f'SELECT min(v.weak)*10-max(v.score) FROM {LEADS} v WHERE v.uuid=a.uuid'
# Source labels that become machine character candidates.
CANDIDATE_LABELS = "SELECT uuid,value,conf,dim FROM labels WHERE ((dim='character' AND conf>=0.35) OR dim='char_ext')"
# Several raw anchor ids may resolve to an explicitly merged identity. Keep the best
# evidence rather than whichever anchor happened to be imported last.
INSERT_CANDIDATE = """INSERT INTO candidates VALUES (?,?,?,?) ON CONFLICT(uuid,entity,source)
    DO UPDATE SET score=max(candidates.score,excluded.score)"""
# WD general tags that mean more than one person; `solo` without these marks a single-person image.
MULTI_TAGS = ('multiple_girls', 'multiple_boys', 'multiple_others', '2girls', '3girls', '4girls', '5girls', '6+girls',
              '2boys', '3boys', '4boys', '5boys', '6+boys')
# Rebuildable objects; migrations drop and recreate them so their definitions can change.
DERIVED = ('variant_memberships', 'memberships', 'membership_rules', 'family_confirmed', 'family_evidence', 'single_assets', 'lead_assets', 'tag_memberships', 'duplicates',
           'auto_rules', 'excluded', 'clusters', 'cluster_info', 'prototypes', 'proto_claims', 'learn_state', 'weak_claims', 'similar_groups',
           'ambiguous_claims')
# membership_rules is the ownership rule set; the `memberships` table stores its result, refreshed per image on every write.
V2_SCHEMA = """
CREATE INDEX IF NOT EXISTS entities_parent ON entities(parent_id,id);
CREATE TABLE IF NOT EXISTS reviews(entity TEXT NOT NULL,reviewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,action_id INTEGER PRIMARY KEY);
CREATE TABLE IF NOT EXISTS prefs(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS family_evidence(uuid TEXT NOT NULL,entity TEXT NOT NULL,sources TEXT NOT NULL,score REAL NOT NULL,
 source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,wd REAL,ccip REAL,rank INTEGER NOT NULL,PRIMARY KEY(entity,uuid));
CREATE INDEX IF NOT EXISTS family_evidence_uuid ON family_evidence(uuid,entity);
CREATE TABLE IF NOT EXISTS single_assets(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS lead_assets(uuid TEXT PRIMARY KEY);
CREATE TABLE IF NOT EXISTS duplicates(uuid TEXT PRIMARY KEY,grp TEXT NOT NULL,size INTEGER NOT NULL,pos INTEGER NOT NULL,grp_added TEXT NOT NULL,
 rep INTEGER NOT NULL,hidden INTEGER NOT NULL DEFAULT 0);
CREATE INDEX IF NOT EXISTS duplicates_grp ON duplicates(grp,pos);
CREATE TABLE IF NOT EXISTS curation(uuid TEXT PRIMARY KEY,status TEXT NOT NULL CHECK(status IN ('included','excluded')),
 origin TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
CREATE TABLE IF NOT EXISTS sync_runs(id INTEGER PRIMARY KEY AUTOINCREMENT,started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
 finished_at TEXT,status TEXT NOT NULL,found INTEGER NOT NULL DEFAULT 0,removed INTEGER NOT NULL DEFAULT 0,
 imported INTEGER NOT NULL DEFAULT 0,failed INTEGER NOT NULL DEFAULT 0,sources TEXT,message TEXT);
CREATE TABLE IF NOT EXISTS sync_added(uuid TEXT PRIMARY KEY,run_id INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS sync_added_run ON sync_added(run_id,uuid);
CREATE TABLE IF NOT EXISTS similar_groups(uuid TEXT PRIMARY KEY,gid INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS similar_groups_gid ON similar_groups(gid,uuid);
CREATE TABLE IF NOT EXISTS clusters(uuid TEXT PRIMARY KEY,cid INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS clusters_cid ON clusters(cid,uuid);
CREATE TABLE IF NOT EXISTS cluster_info(cid INTEGER PRIMARY KEY,size INTEGER NOT NULL,near INTEGER,near_dist REAL,built_at TEXT NOT NULL,
 near_kind TEXT,traits TEXT);
CREATE TABLE IF NOT EXISTS prototypes(entity TEXT NOT NULL,k INTEGER NOT NULL,vec BLOB NOT NULL,threshold REAL NOT NULL,
 seeds INTEGER NOT NULL,hint REAL NOT NULL,PRIMARY KEY(entity,k));
CREATE TABLE IF NOT EXISTS weak_claims(uuid TEXT NOT NULL,entity TEXT NOT NULL,source TEXT NOT NULL,score REAL NOT NULL,
 PRIMARY KEY(entity,uuid,source));
CREATE INDEX IF NOT EXISTS weak_claims_uuid ON weak_claims(uuid,entity);
CREATE TABLE IF NOT EXISTS proto_claims(uuid TEXT NOT NULL,entity TEXT NOT NULL,score REAL NOT NULL,PRIMARY KEY(entity,uuid));
CREATE INDEX IF NOT EXISTS proto_claims_uuid ON proto_claims(uuid,entity);
CREATE TABLE IF NOT EXISTS learn_state(entity TEXT PRIMARY KEY,signature TEXT NOT NULL,learned_at TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS auto_rules(uuid TEXT PRIMARY KEY,kind TEXT NOT NULL,reason TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS excluded(uuid TEXT PRIMARY KEY,reason TEXT NOT NULL,origin TEXT NOT NULL);
CREATE VIEW IF NOT EXISTS family_confirmed AS
 SELECT s.uuid,coalesce(e.parent_id,e.id) AS entity
 FROM assertions s JOIN entities e ON e.id=s.entity JOIN assets a ON a.uuid=s.uuid
 WHERE s.status='confirmed' AND NOT EXISTS
 (SELECT 1 FROM assertions r WHERE r.uuid=s.uuid AND r.entity=coalesce(e.parent_id,e.id) AND r.status='rejected')
 GROUP BY s.uuid,coalesce(e.parent_id,e.id);
CREATE VIEW IF NOT EXISTS membership_rules AS
 SELECT f.uuid,f.entity,'confirmed' AS status,
 (SELECT s.origin FROM assertions s JOIN entities x ON x.id=s.entity
  WHERE s.uuid=f.uuid AND coalesce(x.parent_id,x.id)=f.entity AND s.status='confirmed'
  ORDER BY (x.parent_id IS NULL) DESC,s.origin,s.entity LIMIT 1) AS origin,
 coalesce(v.sources,'') AS sources,v.score,coalesce(v.source_count,0) AS source_count,coalesce(v.ccip_only,0) AS ccip_only
 FROM family_confirmed f LEFT JOIN family_evidence v ON v.uuid=f.uuid AND v.entity=f.entity
 UNION ALL
 SELECT v.uuid,v.entity,'machine','machine',v.sources,v.score,v.source_count,v.ccip_only
 FROM family_evidence v
 WHERE NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=v.entity AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity=v.entity)
 AND NOT (v.ccip_only=1 AND EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity))
 AND NOT (EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=v.uuid) AND {not_mascot} AND
  (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))
 {lead};
CREATE TABLE IF NOT EXISTS memberships(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL,origin TEXT,
 sources TEXT NOT NULL,score REAL,source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,skey TEXT NOT NULL,
 PRIMARY KEY(entity,uuid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS memberships_uuid ON memberships(uuid,skey);
CREATE TABLE IF NOT EXISTS tag_memberships(uuid TEXT NOT NULL,entity TEXT NOT NULL,status TEXT NOT NULL,origin TEXT,
 sources TEXT NOT NULL,score REAL,source_count INTEGER NOT NULL,ccip_only INTEGER NOT NULL,skey TEXT NOT NULL,
 PRIMARY KEY(entity,uuid)) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS tag_memberships_uuid ON tag_memberships(uuid);
CREATE VIEW IF NOT EXISTS variant_memberships AS
 SELECT m.uuid,e.id AS entity,e.parent_id,e.variant,
 CASE WHEN s.status='confirmed' AND s.origin<>'album' THEN 'confirmed' ELSE 'machine' END AS status
 FROM memberships m JOIN entities e ON e.parent_id=m.entity
 LEFT JOIN assertions s ON s.uuid=m.uuid AND s.entity=e.id
 WHERE (s.status='confirmed' AND s.origin<>'album') OR (coalesce(s.status,'')<>'rejected' AND EXISTS
 (SELECT 1 FROM candidates v WHERE v.uuid=m.uuid AND v.entity=e.id));
"""
# Machine owners of single-person images whose runner-up is in the same evidence tier and within
# AMBIGUOUS_MARGIN of CCIP score (or either lacks CCIP). Measured 2026-09-25 on 3,749 reviewed solo images:
# margin < 0.01 → 94% of such owners were wrong, 0.01–0.03 → 60%, no rival or a lower-tier rival → 6–8%.
# Mostly neither candidate is right (a character outside the index between two look-alikes), so the
# owner is flagged「可能认错」, never swapped for the runner-up and never withdrawn.
AMBIGUOUS_MARGIN = 0.03
TIER = "(({0}.wd IS NOT NULL AND {0}.ccip IS NOT NULL)*4 + (coalesce({0}.wd,0)>={1} OR coalesce({0}.ccip,0)>={2})*2 + ({0}.wd IS NOT NULL))"
AMBIGUOUS_VIEW = f"""
CREATE VIEW IF NOT EXISTS ambiguous_claims AS
 SELECT m.uuid,m.entity,r.entity AS rival,t.ccip-r.ccip AS margin
 FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
 JOIN family_evidence t ON t.uuid=m.uuid AND t.entity=m.entity AND t.rank=1
 JOIN family_evidence r ON r.uuid=m.uuid AND r.rank=2
 WHERE m.status='machine' AND {TIER.format('t', STRONG_WD, STRONG_CCIP)}={TIER.format('r', STRONG_WD, STRONG_CCIP)}
 AND (t.ccip IS NULL OR r.ccip IS NULL OR t.ccip-r.ccip<{AMBIGUOUS_MARGIN})
 AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=m.uuid AND s.entity=r.entity AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM entities x WHERE x.id=r.entity AND x.mascot=1)
"""
V2_SCHEMA = V2_SCHEMA.replace('{lead}', LEAD_RULE + ' ' + WEAK_CCIP).replace('{not_mascot}', NOT_MASCOT)
V2_SCHEMA += ';' + AMBIGUOUS_VIEW


class Invalid(ValueError):
    pass


_WRITES = {sqlite3.SQLITE_INSERT, sqlite3.SQLITE_UPDATE, sqlite3.SQLITE_DELETE, sqlite3.SQLITE_CREATE_TABLE,
           sqlite3.SQLITE_CREATE_INDEX, sqlite3.SQLITE_CREATE_VIEW, sqlite3.SQLITE_CREATE_TRIGGER, sqlite3.SQLITE_DROP_TABLE,
           sqlite3.SQLITE_DROP_INDEX, sqlite3.SQLITE_DROP_VIEW, sqlite3.SQLITE_DROP_TRIGGER, sqlite3.SQLITE_ALTER_TABLE}


def _read_only(action, arg1, arg2, database, source):
    """SQLite authorizer of a read-only library: no change to the library file; reads' temporary tables are fine."""
    return sqlite3.SQLITE_DENY if action in _WRITES and database == 'main' else sqlite3.SQLITE_OK


class Store:
    def __init__(self, source, db, build_general=True, *, initialize=True, legacy=False, identity=None):
        """新库默认直接建成完整核心形态；legacy=True 仅用于旧库输入与启动迁移验收。
        identity='hybrid' 在新库出生切换前激活身份策略，已有核心库不能借此改策略。
        """
        self.source, self.path = Path(source).resolve(), Path(db).resolve()
        self.legacy = legacy
        if identity not in (None, 'hybrid'):
            raise Invalid('身份策略不正确')
        self._birth_identity = identity
        self.read_only = None   # why the library opened read-only (a failed switch to the terms core), else None
        terms_core.allow_writes(self.path)   # a new start tries the switch again
        self.artist_assets = None   # artist id -> asset ids, from provenance (tests/reference/web/artists.py); set by the server
        if self.source == self.path:
            raise Invalid('应用数据库不能使用源库路径')
        self.on_effect = None
        self._effects_lock = threading.Lock()
        self._effects_requested = threading.Event()
        # Syncing new pictures, rebuilding WD hits and handing labels to the terms core never interleave (docs/TERMS_LABELS.md).
        # Once characters are the core's it is the process write lock of every core command (tests/reference/web/terms_route.py).
        self._labels_lock = threading.RLock()
        self.album_titles = {}
        with (Path(__file__).resolve().parents[3] / 'tags' / 'tags_v3.csv').open(encoding='utf-8') as f:
            self.tag_names = [r['name'] for r in csv.DictReader(f) if r['category'] == '0']
        self.tag_set = set(self.tag_names)
        if initialize:
            self.initialize(build_general=build_general)

    @classmethod
    def open(cls, source, db):
        """Open existing state for ordinary operations: no mkdir, migration, backup or source/Photos scan."""
        if not Path(db).is_file():
            raise Invalid('应用数据库尚未建立，请先初始化')
        return cls(source, db, initialize=False)

    def initialize(self, build_general=True):
        """Explicit startup/migration operation. Constructor retains backwards compatibility."""
        self.path.parent.mkdir(parents=True, exist_ok=True)
        born = not self.path.exists() or not self._tables()
        self._migrate()
        if born and self._birth_identity:
            self.activate_identity(self._birth_identity)
        if born and not self.legacy:
            self._born_in_core()
        self._retire_v2()
        self._names_version()
        with self.connect() as c:
            c.execute(effects.SCHEMA)
        with self.connect() as c:
            if not terms_core.artists_in_core(c):   # the core's since the switch, and dropped (terms schema 4)
                for statement in ARTIST_VERDICTS.split(';'):
                    c.execute(statement)
        self.album_titles = self._album_titles(add_aliases=build_general and self.write_refusal() is None)
        if build_general and self.write_refusal() is None and self.ready() and not self.general_ready():
            self.refresh_general_hits()
        elif build_general and self.write_refusal() is None and self.ready() and not self.ratings_ready():
            self.refresh_ratings()
        return self

    @staticmethod
    def _label_only(c):
        # A mark rather than a person (the shared 原创角色（未细分）): never learned, matched or compared.
        columns = {r[1] for r in c.execute('PRAGMA table_info(entities)')}
        if 'label_only' not in columns:
            c.execute('ALTER TABLE entities ADD COLUMN label_only INTEGER NOT NULL DEFAULT 0')
        # The user's choice that an identity is a mascot, not a character (web/identity.py NOT_MASCOT).
        if 'mascot' not in columns:
            c.execute('ALTER TABLE entities ADD COLUMN mascot INTEGER NOT NULL DEFAULT 0')
        # Who set it: NULL = nobody yet, 'default' = DEFAULT_TAG_KEYS, 'user' = the switch. Defaults never override the user.
        if 'mascot_origin' not in columns:
            c.execute('ALTER TABLE entities ADD COLUMN mascot_origin TEXT')

    @staticmethod
    def _lead(c):
        """Schema 32: the lead rule; the plain (non-identity) ownership view is recreated and ownership recomputed."""
        c.execute('CREATE TABLE IF NOT EXISTS lead_assets(uuid TEXT PRIMARY KEY)')
        tag_table = V2_SCHEMA[V2_SCHEMA.index('CREATE TABLE IF NOT EXISTS tag_memberships'):]
        for statement in tag_table[:tag_table.index('uuid);') + 5].split(';'):
            c.execute(statement)
        from . import identity
        if not identity.enabled(c):
            c.execute('DROP VIEW IF EXISTS membership_rules')
            view = V2_SCHEMA[V2_SCHEMA.index('CREATE VIEW IF NOT EXISTS membership_rules'):]
            c.execute(view[:view.index(';')])
        c.execute('DROP VIEW IF EXISTS ambiguous_claims')
        c.execute(AMBIGUOUS_VIEW)
        Store._refresh_evidence(c)
        Store._refresh_single(c)
        Store._refresh_memberships(c)

    @staticmethod
    def _default_tags(c):
        """Set DEFAULT_TAG_KEYS identities to「只当标签」unless someone already decided; returns how many changed.
        Callers recompute ownership afterwards (refresh and migrations do)."""
        keys = ','.join('?' for _ in DEFAULT_TAG_KEYS)
        return c.execute(f"""UPDATE entities SET mascot=1,mascot_origin='default' WHERE mascot_origin IS NULL AND mascot=0
            AND parent_id IS NULL AND merged_into IS NULL AND kind<>'work'
            AND id IN (SELECT entity_id FROM links WHERE key IN ({keys}))""", DEFAULT_TAG_KEYS).rowcount

    @staticmethod
    def _asked(c):
        for statement in ASKED.split(';'):
            c.execute(statement)

    def _names_version(self):
        """Triggers that bump meta names_version whenever a table name snapshots are built from changes, so the
        connections of one process can share a snapshot (tests/reference/web/names.py View). Idempotent; no schema bump needed."""
        with self.connect() as c:
            tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            if 'meta' not in tables:
                return
            c.execute("INSERT OR IGNORE INTO meta VALUES ('names_version','0')")
            for table in ('entities', 'links', 'names', 'display_names'):
                if table not in tables:
                    continue
                for event in ('INSERT', 'UPDATE', 'DELETE'):
                    c.execute(f"""CREATE TRIGGER IF NOT EXISTS names_version_{table}_{event.lower()} AFTER {event} ON {table}
                        BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='names_version'; END""")

    def refuse_writes(self, reason):
        """Open read-only from now on (user's decision 2026-09-30): the library could not be switched to the terms core,
        and without the switch its old tables are not written any more. Every Python connection refuses changes to the
        library (temporary tables of reads stay usable) and every core write command refuses it; `reason` is shown."""
        self.read_only = reason
        terms_core.refuse_writes(self.path, reason)

    def write_refusal(self):
        """切换失败保留原只读原因；四个模块全部切换前，公开写入口临时只读。"""
        if self.read_only:
            return self.read_only
        with self.connect() as c:
            if all(check(c) for check in (terms_core.artists_in_core, terms_core.labels_in_core,
                                        terms_core.characters_in_core, terms_core.names_in_core)):
                return None
        return '图库正在切换到词条核心，完成前只读'

    def require_writable(self):
        reason = self.write_refusal()
        if reason:
            raise Invalid(reason)

    def _tables(self):
        with self.connect() as c:
            return {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}

    def _born_in_core(self):
        """A new library starts in the terms core's shape (user's decision 2026-09-30):
        artists, labels, characters and names are switched while it is still empty, so every write from the first
        refresh on is a core command. Without the core program a library cannot be created."""
        if terms_core.binary() is None:
            raise Invalid('找不到词条核心程序，无法新建图库')
        terms_core.run('migrate', '--db', self.path)
        terms_core.cutover_artists(self.path)
        terms_core.cutover_labels(self.path)
        terms_core.cutover_characters(self.path)
        terms_core.cutover_names(self.path)

    def _retire_v2(self):
        """The strict identity policy v2 was abandoned (2026-09-30): a library still marked v2 is converted to
        hybrid once, after a backup. The core never switched such a library (it refuses v2), so its tables are still
        Python's: the relation layer is reused, hybrid's views replace v2's, machine results are regrouped under hybrid's
        rule, and v2's own tables go (the terms core's schema 7 drops them from switched libraries)."""
        with self.connect() as c:
            if not c.execute("SELECT 1 FROM sqlite_master WHERE name='meta'").fetchone():
                return
            row = c.execute("SELECT value FROM meta WHERE key='identity_policy'").fetchone()
        if not row or row[0] != 'v2':
            return
        print(f'身份策略 v2 已停用，转成 hybrid 前已备份：{self.backup()}', flush=True)
        from . import identity
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            c.execute("UPDATE meta SET value='hybrid' WHERE key='identity_policy'")
            identity.install_views(c)
            c.execute('DROP TABLE IF EXISTS identity_claims')
            c.execute('DROP TABLE IF EXISTS identity_calibration')
            c.execute("DELETE FROM meta WHERE key IN ('identity_calibration_active','identity_bank_error')")
            self._refresh_evidence(c)
            self._refresh_memberships(c)
        print('身份策略已从 v2 转成 hybrid', flush=True)

    def _migrate(self):
        with self.connect() as c:
            tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
            version = c.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone() if 'meta' in tables else None
        if version and version[0] == SCHEMA_VERSION:
            return
        with self.connect() as c:
            characters = 'meta' in tables and terms_core.characters_in_core(c)
        if characters:
            # The core owns the character tables now; an upgrade touching them has to be a core migration.
            raise Invalid('角色已切到词条核心：这次数据库升级要先改成词条核心的迁移')
        if version and version[0] not in ('1', '2', '3', '4', '5', '6', '7', '8', '9', '10', '11', '12', '13', '14', '15', '16', '17', '18', '19', '20', '21', '22', '23', '24', '25', '26', '27', '28', '29', '30', '31', '32', '33', '34', '35'):
            raise Invalid('应用库版本较新，已停止迁移')
        if tables:
            self.backup()
        with self.connect() as c:
            c.execute('PRAGMA journal_mode=WAL')
            c.execute('BEGIN IMMEDIATE')
            from . import identity
            if version and version[0] in ('25', '26', '27', '28', '29', '30', '31', '32', '33', '34', '35'):
                self._label_only(c)         # 33: mascot column, needed by the views below
                identity.install(c)
                c.execute(AMBIGUOUS_VIEW)   # 27
                if identity.enabled(c):     # 28: index-friendly hybrid variant view; 32: lead rule
                    identity.install_views(c)
                self._label_only(c)         # 29
                self._asked(c)              # 30, 31: audit_sample
                # 36: tags chosen so far were the user's; then the default list (recomputed by _lead below)
                c.execute("UPDATE entities SET mascot_origin='user' WHERE mascot=1 AND mascot_origin IS NULL")
                self._default_tags(c)
                self._lead(c)               # 32, 33: mascots, 34: tag_memberships, 35: weak CCIP claims are leads
                c.execute("INSERT OR REPLACE INTO meta VALUES ('schema_version',?)", (SCHEMA_VERSION,))
                return
            views = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='view'")}
            for statement in (SCHEMA + names.SCHEMA).split(';'):
                if statement.strip() and not _indexes_a_view(statement, views):
                    c.execute(statement)
            c.execute("DELETE FROM display_names WHERE source='danbooru'")
            c.execute("UPDATE OR IGNORE display_names SET source='model' WHERE source='deepseek'")
            c.execute("DELETE FROM display_names WHERE source='deepseek'")
            for work, title in names.WORK_NAMES.items():
                names.put(c, 'work', work, 'zh', 'builtin', title)
            if version and version[0] in ('18', '19', '20', '21', '22', '23', '24'):
                self._links_from_columns(c)
                self._names_from_tables(c)
                from . import works
                works.migrate(c)
                identity.install(c)
                c.execute(AMBIGUOUS_VIEW)
                self._label_only(c)
                self._asked(c)
                self._lead(c)
                c.execute("INSERT OR REPLACE INTO meta VALUES ('schema_version',?)", (SCHEMA_VERSION,))
                return
            columns = {r[1] for r in c.execute('PRAGMA table_info(entities)')}
            if 'variant' not in columns:
                c.execute("ALTER TABLE entities ADD COLUMN variant TEXT NOT NULL DEFAULT ''")
            if 'parent_id' not in columns:
                c.execute('ALTER TABLE entities ADD COLUMN parent_id TEXT REFERENCES entities(id)')
            if 'tag' not in columns:
                c.execute('ALTER TABLE entities ADD COLUMN tag TEXT')
            if 'merged_into' not in columns:
                # A merged character keeps its row so every later source of it (sync, album import, refresh) lands on the target.
                c.execute('ALTER TABLE entities ADD COLUMN merged_into TEXT REFERENCES entities(id)')
            if 'kind' not in columns:
                # 'oc': an artist's original character. `artist` is kept structured so it can later name the artist of every image.
                c.execute("ALTER TABLE entities ADD COLUMN kind TEXT NOT NULL DEFAULT ''")
                c.execute("ALTER TABLE entities ADD COLUMN artist TEXT NOT NULL DEFAULT ''")
            self._label_only(c)
            self._links_from_columns(c)
            self._names_from_tables(c)
            from . import works
            works.migrate(c)
            for name in DERIVED:
                kind = c.execute('SELECT type FROM sqlite_master WHERE name=?', (name,)).fetchone()
                if kind:
                    c.execute(f'DROP {kind[0].upper()} {name}')
            for statement in V2_SCHEMA.split(';'):
                if statement.strip():
                    c.execute(statement)
            with self.original() as src:
                self._hierarchy(c, src)
                self._tags(c, src)
                self._refresh_duplicates(c, src)
            self._refresh_evidence(c)
            self._refresh_single(c)
            self._refresh_memberships(c)
            with self.original() as src:
                self._refresh_rules(c, self._realistic(src, 'main'))
            self._refresh_excluded(c)
            self._refresh_similar(c)
            identity.install(c)
            c.execute("INSERT OR REPLACE INTO meta VALUES ('schema_version',?)", (SCHEMA_VERSION,))

    @staticmethod
    def _links_from_columns(c):
        """Schema 23: `external_key` and `tag` become links; the columns are emptied so nothing reads them again."""
        for prefix, system in (('index:', 'ccip'), ('wd:', 'wd'), ('album:', 'photos_album'), ('parent:', 'ccip_base')):
            c.execute("""INSERT OR IGNORE INTO links(system,key,entity_id,source) SELECT ?,substr(external_key,?),id,'import'
                FROM entities WHERE external_key LIKE ? ESCAPE '\\'""", (system, len(prefix) + 1, prefix + '%'))
        # Index tags are Danbooru tags, and so are WD tags; album titles are not.
        c.execute("""INSERT OR IGNORE INTO links(system,key,entity_id,source) SELECT 'danbooru',tag,id,'import' FROM entities
            WHERE coalesce(tag,'')<>'' AND (external_key LIKE 'index:%' OR external_key LIKE 'wd:%')""")
        c.execute('UPDATE entities SET external_key=NULL,tag=NULL WHERE external_key IS NOT NULL OR tag IS NOT NULL')

    @staticmethod
    def _names_from_tables(c):
        """Schema 24: every character name lives in `names`, on its identity (docs/DATA_MODEL.md).

        Character display names and wiki aliases were keyed by tag; `aliases` and `entities.name` were per identity.
        Work names and skin words stay in display_names / name_aliases: they are shared vocabularies, not identities.
        """
        tables = {r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")}
        if 'display_names' in tables:
            for key, lang, source, value in c.execute("SELECT key,lang,source,value FROM display_names WHERE kind='character'").fetchall():
                for entity in names.tag_entities(c, key):
                    names.put_name(c, entity, lang, source, value)
            c.execute("DELETE FROM display_names WHERE kind='character'")
        if 'name_aliases' in tables:
            for key, value, position in c.execute("SELECT key,value,position FROM name_aliases WHERE kind='character'").fetchall():
                for entity in names.tag_entities(c, key):
                    # position 10000 marked a Bangumi infobox Chinese name kept for search only.
                    names.put_name(c, entity, 'zh' if position >= 10000 else '', 'bangumi' if position >= 10000 else 'danbooru',
                                   value, role='alias', position=position)
            c.execute("DELETE FROM name_aliases WHERE kind='character'")
        if 'aliases' in tables:
            c.execute("INSERT OR IGNORE INTO names(entity_id,value,source,role) SELECT entity,alias,'legacy','alias' FROM aliases WHERE trim(alias)<>''")
            c.execute('DROP TABLE aliases')
        linked = {r[0] for r in c.execute('SELECT DISTINCT entity_id FROM links')}
        for ident, name, kind in c.execute("SELECT id,name,kind FROM entities WHERE trim(name)<>''").fetchall():
            if ident in linked and kind != 'oc':   # the index's own name (or a tag): searchable, never shown
                names.put_name(c, ident, '', 'legacy', name, role='alias')
            else:                 # a name you gave
                names.put_name(c, ident, names.lang_of(name), 'user', name, preferred=1)
        c.execute("UPDATE entities SET name='' WHERE name<>''")

    @staticmethod
    def alias(c, entity, value, source, lang=''):
        """A search-only name of an identity; a value it already has as an alias is not added again."""
        if isinstance(value, str) and not c.execute("SELECT 1 FROM names WHERE entity_id=? AND role='alias' AND value=?",
                                                    (entity, value.strip())).fetchone():
            names.put_name(c, entity, lang, source, value, role='alias')

    @staticmethod
    def _linked(c, system, key):
        """The identity a link points to (before following merges), or None."""
        row = c.execute('SELECT entity_id FROM links WHERE system=? AND key=?', (system, key)).fetchone() if key else None
        return row[0] if row else None

    @staticmethod
    def link(c, entity, system, key, source='import'):
        """Attach a link unless another identity already owns it; returns the owner."""
        if system not in LINK_SYSTEMS:
            raise Invalid('未知的外部来源')
        c.execute('INSERT OR IGNORE INTO links(system,key,entity_id,source) VALUES (?,?,?,?)', (system, key, entity, source))
        return Store._linked(c, system, key)

    def _hierarchy(self, c, src):
        from . import identity
        known = None
        if identity.enabled(c):
            # Hybrid: existing identities are governed by relations; only characters new to the index
            # get the name rule, then enter the baseline like the rest.
            known = {r[0] for r in c.execute('SELECT id FROM identity_baseline')}
        rows = [dict(r) for r in src.execute('SELECT * FROM characters')]
        parents = {}
        by_id = {r['char_id']: r for r in rows}
        # The unqualified character (dokibird): parent of last resort once dokibird_(bounty_hunter) is itself a skin
        # (tools/build_characters.fold_plain_skins), which leaves dokibird_(vtuber)_(bounty_hunter) without its old parent.
        plain = {r['base_tag']: r['char_id'] for r in rows if r.get('base_tag') and not r.get('skin') and r.get('danbooru_tag') == r['base_tag']}
        for r in rows:
            if not r.get('skin'):
                parents.setdefault((r.get('base_tag'), r.get('copyright') or ''), []).append(r['char_id'])
        for r in rows:
            if not r.get('skin'):
                continue
            child = self._linked(c, 'ccip', r['char_id'])
            if not child or (known is not None and child in known):
                continue
            key = (r.get('base_tag'), r.get('copyright') or '')
            matches = parents.get(key, []) if key[0] else []
            if len(matches) != 1 and key[0] in plain:
                matches = [plain[key[0]]]
            parent = None
            if len(matches) == 1:
                p = by_id[matches[0]]
                parent = self._entity(c, 'ccip', p['char_id'], p['name_zh'] or p['name_ja'] or p['name_en'] or p['char_id'], key[1])
            else:
                base = json.dumps(key if key[0] else (r['char_id'], key[1]), ensure_ascii=False)
                parent = self._entity(c, 'ccip_base', base, key[0] or r['char_id'], key[1])
            c.execute("UPDATE entities SET parent_id=?,variant=?,kind='skin',work_id=NULL WHERE id=?", (self._resolve(c, parent), r['skin'], child))
        if known is not None:
            before = c.total_changes
            identity.baseline(c)
            if c.total_changes != before:
                identity.rebuild(c)

    @staticmethod
    def _tags(c, src):
        """Link each index character to its Danbooru tag; local names stay untouched."""
        columns = {r[1] for r in src.execute('PRAGMA table_info(characters)')}
        tag = 'coalesce(nullif(danbooru_tag,\'\'),char_id)' if 'danbooru_tag' in columns else 'char_id'
        for char_id, value in src.execute(f'SELECT char_id,{tag} FROM characters'):
            ident = Store._linked(c, 'ccip', char_id)
            if ident:
                Store.link(c, ident, 'danbooru', value)
                Store.alias(c, ident, value, 'index')

    @staticmethod
    def _touch(c, uuids):
        """Fill the per-transaction `touched` table used by partial refreshes."""
        c.execute('CREATE TEMP TABLE IF NOT EXISTS touched(uuid TEXT PRIMARY KEY)')
        c.execute('DELETE FROM touched')
        c.executemany('INSERT OR IGNORE INTO touched VALUES (?)', [(i,) for i in uuids])

    @staticmethod
    def _refresh_evidence(c, uuids=None):
        from . import identity
        safe = identity.enabled(c)
        family = 'e.character_id' if safe else 'coalesce(e.parent_id,e.id)'
        nodes = 'identity_nodes' if safe else 'entities'
        # rank orders machine families per image: both sources > one strong source > weak evidence, then WD, then scores.
        # A strong external-index match (distance <= 0.05) now beats a weak WD guess (< 0.9) — see STRONG_WD.
        if uuids is None:
            c.execute('DELETE FROM family_evidence')
        else:
            Store._touch(c, uuids)
            c.execute('DELETE FROM family_evidence WHERE uuid IN (SELECT uuid FROM touched)')
        c.execute(f"""INSERT INTO family_evidence
            SELECT uuid,entity,sources,score,source_count,ccip_only,wd,ccip,
                   row_number() OVER (PARTITION BY uuid ORDER BY coalesce((SELECT x.mascot FROM entities x WHERE x.id=entity),0),
                   wd IS NOT NULL AND ccip IS NOT NULL DESC,
                   coalesce(wd,0)>={STRONG_WD} OR coalesce(ccip,0)>={STRONG_CCIP} DESC,
                   wd IS NOT NULL DESC,coalesce(wd,0) DESC,coalesce(ccip,0) DESC,entity)
            FROM (SELECT v.uuid,{family} AS entity,group_concat(DISTINCT v.source ORDER BY v.source) AS sources,
                   max(v.score) AS score,count(DISTINCT v.source) AS source_count,min(v.source<>'WD') AS ccip_only,
                   max(CASE WHEN v.source='WD' THEN v.score END) AS wd,max(CASE WHEN v.source<>'WD' THEN v.score END) AS ccip
                  FROM {EVIDENCE} v JOIN {nodes} e ON e.id=v.entity JOIN assets a ON a.uuid=v.uuid
                  {'' if uuids is None else 'WHERE v.uuid IN (SELECT uuid FROM touched)'}
                  GROUP BY {family},v.uuid)""")

    @staticmethod
    def _refresh_single(c, uuids=None):
        """Single-person images keep only their top machine identity."""
        marks = ','.join('?' for _ in MULTI_TAGS)
        scope = ''
        if uuids is None:
            c.execute('DELETE FROM single_assets')
        else:
            Store._touch(c, uuids)
            c.execute('DELETE FROM single_assets WHERE uuid IN (SELECT uuid FROM touched)')
            scope = ' AND g.uuid IN (SELECT uuid FROM touched)'
        c.execute(f"""INSERT INTO single_assets SELECT DISTINCT uuid FROM general_hits g WHERE value='solo'{scope}
            AND NOT EXISTS(SELECT 1 FROM general_hits m WHERE m.uuid=g.uuid AND m.value IN ({marks}))""", MULTI_TAGS)
        # One named character with a nameless man (see web/identity.py LEAD_RULE).
        c.execute('DELETE FROM lead_assets' + (' WHERE uuid IN (SELECT uuid FROM touched)' if uuids is not None else ''))
        c.execute(f"""INSERT INTO lead_assets SELECT DISTINCT uuid FROM general_hits g WHERE value='1girl'{scope}
            AND EXISTS(SELECT 1 FROM general_hits b WHERE b.uuid=g.uuid AND b.value='1boy')
            AND NOT EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=g.uuid)
            AND NOT EXISTS(SELECT 1 FROM general_hits m WHERE m.uuid=g.uuid AND m.value IN ({marks}))""", MULTI_TAGS)

    @staticmethod
    def _refresh_duplicates(c, src):
        """Copies of one picture, folded to their best copy: byte-identical files (Photos' stable hash), joined with
        look-alikes whose relation (web/similar.py) is one of the folded kinds (prefs 'fold_kinds')."""
        c.execute('DELETE FROM duplicates')
        c.executemany('INSERT INTO duplicates(uuid,grp,size,pos,grp_added,rep) VALUES (?,?,?,?,?,?)', Store._duplicate_rows(c, src))

    @staticmethod
    def _duplicate_rows(c, src, assets=None):
        """The duplicates rows for the library's assets, or for `assets` (uuid, added_at, width, height) given instead
        (the terms core writes them: tests/reference/web/terms_characters.py)."""
        added, pixels = {}, {}
        if assets is None:
            assets = c.execute("SELECT uuid,coalesce(added_at,''),coalesce(width,0),coalesce(height,0) FROM assets").fetchall()
        for ident, when, width, height in assets:
            added[ident], pixels[ident] = when, width * height
        parent, key_of = {}, {}
        def find(u):
            while parent.setdefault(u, u) != u:
                parent[u] = parent[parent[u]]
                u = parent[u]
            return u
        if 'stable_hash' in {r[1] for r in src.execute('PRAGMA table_info(assets)')}:
            first = {}
            for ident, key in src.execute("SELECT uuid,stable_hash FROM assets WHERE coalesce(stable_hash,'')<>''"):
                if ident in added:
                    key_of[ident] = key
                    parent[find(ident)] = find(first.setdefault(key, ident))
        kinds = Store.fold_kinds_of(c)
        if kinds:
            for a, b in c.execute(f"SELECT a,b FROM relations WHERE kind IN ({','.join('?' for _ in kinds)})", kinds):
                if a in added and b in added:
                    parent[find(a)] = find(b)
        groups = {}
        for ident in parent:
            groups.setdefault(find(ident), []).append(ident)
        # A group keeps its hash as key while it has one, so byte-identical groups keep their ids.
        groups = {min((key_of[m] for m in g if m in key_of), default='sim:' + min(g)): g for g in groups.values()}
        rows = []
        for key, members in groups.items():
            if len(members) < 2:
                continue
            members.sort(key=lambda i: (added[i], i))
            # Representative: highest resolution, then the earliest copy.
            best = min(members, key=lambda i: (-pixels[i], added[i], i))
            rows += [(ident, key, len(members), pos, added[members[-1]], ident == best) for pos, ident in enumerate(members, 1)]
        return rows

    @staticmethod
    def _refresh_memberships(c, uuids=None):
        """Store membership_rules for all images, or only for `uuids` after a write in the same transaction."""
        # skey is the suspicious-first key, ascending: confirmed identities always sort last.
        # An identity the user set to「当作标签」(entities.mascot) goes to tag_memberships instead: it stays on the image
        # and can be filtered on, but no character view, count, review queue or single/multi rule sees it.
        select = """SELECT m.uuid,m.entity,m.status,m.origin,m.sources,m.score,m.source_count,m.ccip_only,
            printf('%d%d%d%020.12f%d',m.status='confirmed',m.source_count<>1,1-m.ccip_only,coalesce(m.score,1),
            NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=m.uuid AND f.entity<>m.entity))
            FROM membership_rules m WHERE {} EXISTS(SELECT 1 FROM entities t WHERE t.id=m.entity AND t.mascot=1)"""
        scope = '' if uuids is None else ' AND m.uuid IN (SELECT uuid FROM touched)'
        if uuids is not None:
            Store._touch(c, uuids)
            ids = [(u,) for u in dict.fromkeys(uuids)]
        for table, negate in (('memberships', 'NOT'), ('tag_memberships', '')):
            if uuids is not None and len(ids) <= FEW_TOUCHED:
                # One image at a time: `m.uuid=?` reaches the view's indexes (~25 ms an image), while `IN (touched)`
                # rescans assertions inside the view's correlated subqueries (~150 ms even for one image).
                c.executemany(f'DELETE FROM {table} WHERE uuid=?', ids)
                c.executemany(f'INSERT INTO {table} ' + select.format(negate) + ' AND m.uuid=?', ids)
                continue
            c.execute(f'DELETE FROM {table}' + ('' if uuids is None else ' WHERE uuid IN (SELECT uuid FROM touched)'))
            c.execute(f'INSERT INTO {table} ' + select.format(negate) + scope)
        Store._refresh_fold(c, touched=uuids is not None)

    @staticmethod
    def _refresh_fold(c, touched=False):
        """Hide non-representative copies of groups whose copies agree (all groups, or those in `touched`)."""
        fold = f'UPDATE duplicates AS d SET hidden=(rep=0 AND NOT {MIXED})'
        c.execute(fold + (' WHERE d.grp IN (SELECT grp FROM duplicates WHERE uuid IN (SELECT uuid FROM touched))' if touched else ''))

    @staticmethod
    def _realistic(c, schema):
        """Images WD sees as realistic: photos of people, pets, food, news frames."""
        return [r[0] for r in c.execute(f"SELECT uuid FROM {schema}.labels WHERE dim='general' AND value='realistic' AND conf>=?", (REALISTIC,))]

    @staticmethod
    def _refresh_rules(c, realistic):
        """Machine collection rules: clear non-illustrations are excluded, screenshots with people are left pending."""
        c.execute('DELETE FROM auto_rules')
        marks = lambda tags: ','.join('?' for _ in tags)
        has = lambda tags: f'EXISTS(SELECT 1 FROM general_hits g WHERE g.uuid=a.uuid AND g.value IN ({marks(tags)}))'
        nochar = 'NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)'
        c.executemany("INSERT OR IGNORE INTO auto_rules SELECT uuid,'exclude','photo' FROM assets WHERE uuid=?", [(u,) for u in realistic])
        c.execute(f"INSERT OR IGNORE INTO auto_rules SELECT a.uuid,'exclude','screenshot' FROM assets a WHERE {has(SHOT_TAGS)} AND NOT {has(PEOPLE_TAGS)} AND {nochar}",
                  SHOT_TAGS + PEOPLE_TAGS)
        c.execute(f"""INSERT OR IGNORE INTO auto_rules SELECT a.uuid,'exclude','text' FROM assets a
            WHERE {has(TEXT_TAGS)} AND NOT {has(SHOT_TAGS)} AND NOT {has(PEOPLE_TAGS)} AND {nochar}""", TEXT_TAGS + SHOT_TAGS + PEOPLE_TAGS)
        c.execute(f"INSERT OR IGNORE INTO auto_rules SELECT a.uuid,'pending','screenshot' FROM assets a WHERE {has(SHOT_TAGS)} AND {has(PEOPLE_TAGS)} AND {nochar}",
                  SHOT_TAGS + PEOPLE_TAGS)

    @staticmethod
    def _refresh_excluded(c, uuids=None):
        """Effective exclusion: a human decision wins over the rules, in either direction."""
        insert = """INSERT INTO excluded SELECT a.uuid,coalesce(r.reason,'manual'),CASE WHEN k.status='excluded' THEN 'review' ELSE 'rule' END
            FROM assets a LEFT JOIN curation k ON k.uuid=a.uuid LEFT JOIN auto_rules r ON r.uuid=a.uuid AND r.kind='exclude'
            WHERE (k.status='excluded' OR (r.uuid IS NOT NULL AND k.uuid IS NULL))"""
        if uuids is None:
            c.execute('DELETE FROM excluded')
            c.execute(insert)
            return
        Store._touch(c, uuids)
        c.execute('DELETE FROM excluded WHERE uuid IN (SELECT uuid FROM touched)')
        c.execute(insert + ' AND a.uuid IN (SELECT uuid FROM touched)')

    def _album_titles(self, add_aliases=False):
        if add_aliases:
            self.require_writable()
        """Photos album titles are search aliases; display always uses the original tag."""
        mapping_path = self.source.parent / 'album-char-map.json'
        mapping = json.loads(mapping_path.read_text()).get('map', {}) if mapping_path.exists() else {}
        with self.original() as src:
            columns = {r[1] for r in src.execute('PRAGMA table_info(decisions)')}
            if 'note' not in columns:
                return {}
            albums = list(src.execute("SELECT value, min(note) AS note FROM decisions WHERE dim='character' GROUP BY value"))
        titles, core = {}, []
        with self.connect() as c:
            in_core = terms_core.names_in_core(c)
            for row in albums:
                note = row['note'] or ''
                if not note.startswith('album:') or ' pk=' not in note:
                    continue
                title = note[6:].rsplit(' pk=', 1)[0].strip()
                if not title:
                    continue
                mapped = mapping.get(row['value'], {}).get('char_id')
                linked = self._linked(c, 'ccip', mapped) if mapped else self._linked(c, 'photos_album', row['value'])
                entity = c.execute('SELECT coalesce(parent_id,id) FROM entities WHERE id=?', (linked,)).fetchone() if linked else None
                if entity:
                    titles[entity[0]] = title
                    if add_aliases and in_core:
                        core.append({'op': 'alias', 'entity': entity[0], 'value': title, 'source': 'album'})
        if core:   # names are the core's (docs/TERMS_NAMES_PLAN.md): one core transaction, under the write lock
            with self._labels_lock:
                terms_core.names_command(self.path, {'op': 'batch', 'ops': core})
        return titles

    @staticmethod
    def _display_reader(c, entity_ids=None):
        """Share one name/identity snapshot within a read transaction, never across requests."""
        from . import identity as identities
        view = names.register(c)
        scope = ' WHERE id IN (SELECT value FROM json_each(?))' if entity_ids is not None else ''
        args = (json.dumps(entity_ids),) if entity_ids is not None else ()
        nodes = {r['id']: dict(r) for r in c.execute(
            'SELECT id,node_type,character_id,variant_id,canonical_id FROM identity_nodes' + scope, args
        )} if identities.enabled(c) else {}
        return lambda row, skin=False: Store._display(row, c, skin, (view, nodes))

    @staticmethod
    def _display(row, c, skin=False, snapshot=None):
        result = dict(row)
        if snapshot is None:
            result.update(json.loads(c.execute('SELECT entity_info(?,?)', (result['id'], skin)).fetchone()[0]))
            identity = c.execute("SELECT e.kind,e.original,coalesce(p.work_id,e.work_id) FROM entities e LEFT JOIN entities p ON p.id=e.parent_id AND e.kind='skin' WHERE e.id=?", (result['id'],)).fetchone()
        else:
            view, nodes = snapshot
            result.update(view.entity(result['id'], skin))
            entity = view.entities.get(result['id'])
            parent = view.entities.get(entity['parent_id']) if entity and entity['kind'] == 'skin' else None
            work_id = parent['work_id'] if parent and parent['work_id'] is not None else entity['work_id'] if entity else None
            identity = (entity['kind'], entity['original'], work_id) if entity else None
        if identity:
            if 'kind' in result and identity[0] != 'work':
                result['kind'] = 'oc' if identity[1] else ''
            result['work_id'] = identity[2]
        if 'work' in result:
            result['work_name'] = (view.entity(identity[2]).get('name', '') if snapshot is not None else
                                   c.execute('SELECT entity_name(?)', (identity[2],)).fetchone()[0]) if identity and identity[2] else ''
        from . import identity as identities
        if snapshot is not None:
            result.update({k: v for k, v in nodes.get(result['id'], {}).items() if k != 'id'})
        elif identities.enabled(c):
            node = c.execute('SELECT node_type,character_id,variant_id,canonical_id FROM identity_nodes WHERE id=?', (result['id'],)).fetchone()
            if node:
                result.update(dict(node))
        return result

    @contextmanager
    def connect(self):
        c = sqlite3.connect(self.path, timeout=20, uri=True)
        if self.read_only:
            c.set_authorizer(_read_only)
        c.row_factory = sqlite3.Row
        names.register(c)
        try:
            yield c
            event = effects.CURRENT.get()
            if event and event[0] is self and c.total_changes:
                c.execute('INSERT INTO backend_effects(kind) VALUES (?)', (event[1],))
            c.commit()
        except BaseException:
            c.rollback()
            raise
        finally:
            c.close()

    def characters_in_core(self):
        with self.connect() as c:
            return terms_core.characters_in_core(c)

    def _core(self, command):
        """One character command in the terms core, under the write lock; refusals read like the old messages.

        A new work's id is drawn when the core reaches the point where the old method drew it: the core stops (writing
        nothing) and names the slot, which gets a fresh uuid, and the command runs again. So uuids are drawn as often
        and in the same order as by the old method."""
        self.require_writable()
        with self._labels_lock:
            while True:
                try:
                    return terms_core.character_command(self.path, command)
                except terms_core.NeedId as need:
                    terms_core.fill_id(command, need.pointer, str(uuid.uuid4()))
                except terms_core.Failed as exc:
                    raise Invalid(str(exc)) from exc

    @contextmanager
    def original(self):
        c = sqlite3.connect(self.source.as_uri() + '?mode=ro', uri=True, timeout=20)
        c.row_factory = sqlite3.Row
        c.execute('PRAGMA query_only=ON')
        c.execute('BEGIN')
        try:
            yield c
        finally:
            c.close()

    def backup(self):
        folder = backups_dir(self.path)
        folder.mkdir(parents=True, exist_ok=True)
        dest = folder / ('library-' + time.strftime('%Y%m%d-%H%M%S') + '-' + uuid.uuid4().hex[:6] + '.sqlite')
        with self.connect() as src, sqlite3.connect(dest) as target:
            src.backup(target)
        with sqlite3.connect(dest) as check:
            if check.execute('PRAGMA quick_check').fetchone()[0] != 'ok':
                raise Invalid('备份完整性检查失败，已停止迁移')
        return dest

    def ready(self):
        with self.connect() as c:
            return bool(c.execute("SELECT 1 FROM meta WHERE key='indexed'").fetchone())

    def general_ready(self):
        with self.connect() as c:
            return bool(c.execute("SELECT 1 FROM meta WHERE key='general_indexed'").fetchone())

    def ratings_ready(self):
        with self.connect() as c:
            return bool(c.execute("SELECT 1 FROM meta WHERE key='rating_indexed'").fetchone())

    def refresh_ratings(self, c=None):
        """分级由核心证据生成；这里只记录索引时间，不再维护旧分级表。"""
        self.require_writable()
        if c is None:
            with self.connect() as connection:
                return self.refresh_ratings(connection)
        c.execute("INSERT OR REPLACE INTO meta VALUES ('rating_indexed',?)", (time.strftime('%Y-%m-%d %H:%M:%S'),))

    def refresh_general_hits(self):
        """WD 证据交给核心，再重算派生结果；旧表写入已移除。"""
        self.require_writable()
        with self._labels_lock:
            self._deliver_wd(self.library_uuids())
            self._refresh_general_hits(True)

    def _refresh_general_hits(self, in_core):
        self.require_writable()
        from . import terms_characters
        with self._labels_lock:
            with self.original() as src:
                realistic = self._realistic(src, 'main')
            self._core(terms_characters.rederive(self, realistic))
        return

    def library_id(self):
        """The id in library.json beside this database; evidence packages are bound to it. 'local' outside a package."""
        try:
            return json.loads((self.path.parent / 'library.json').read_text(encoding='utf-8'))['id']
        except (OSError, ValueError, KeyError):
            return 'local'

    def _deliver_wd(self, uuids, new_assets=False):
        """Hand these pictures' WD ratings and general tags to the terms core as evidence packages (tests/reference/web/wd_export.py).
        None of our connections may be open: the core writes on its own."""
        from web import wd_export
        uuids = list(uuids)
        if not uuids:
            return 0
        with self.connect() as c:
            moved = terms_core.labels_in_core(c)
        if moved:
            # 核心用原内容指纹在两个只读快照中检查；真正变化仍交完整证据包，冲突原样拒绝。
            try:
                uuids = terms_core.run('wd-pending', '--db', self.path, '--source', self.source,
                                       stdin=json.dumps({'uuids': uuids, 'library_id': self.library_id()}), timeout=600)['pending']
            except terms_core.Failed as exc:
                raise Invalid(f'WD 结果交给词条核心失败：{exc}') from exc
            if not uuids:
                return 0
        out = self.path.parent / '.evidence' / f'wd-{time.strftime("%Y%m%d-%H%M%S")}-{os.getpid()}-{threading.get_ident()}'
        added = 0
        try:
            for package in wd_export.packages(self.source, self.library_id(), uuids, out):
                added += terms_core.import_package(self.path, self.library_id(), package, new_assets=new_assets)['added']
                shutil.rmtree(package, ignore_errors=True)
        except terms_core.Failed as exc:
            raise Invalid(f'WD 结果交给词条核心失败：{exc}') from exc
        finally:
            shutil.rmtree(out, ignore_errors=True)
        return added

    def wd_undelivered(self):
        """Rated pictures the terms core has no WD evidence for yet (before labels are handed over)."""
        with self.connect() as c:
            if not c.execute("SELECT 1 FROM sqlite_master WHERE name='term_evidence'").fetchone():
                return [r[0] for r in c.execute('SELECT uuid FROM asset_rating')]
            return [r[0] for r in c.execute("""SELECT r.uuid FROM asset_rating r WHERE NOT EXISTS(
                SELECT 1 FROM term_assets a JOIN term_evidence e ON e.asset_id=a.id JOIN terms t ON t.id=e.term_id AND t.kind='rating'
                WHERE a.namespace='library' AND a.key=r.uuid)""")]

    def refresh(self):
        """Only invoked at startup/explicit CLI; never replaces curated tables."""
        started = time.monotonic()
        self.require_writable()
        from . import terms_characters
        with self._labels_lock:
            with self.original() as src, self.connect() as c:
                command = terms_characters.refresh(self, src, c)
                realistic = self._realistic(src, 'main')
            # 先交新范围的 WD，再一次性生成最终派生结果，避免全库归属算两遍。
            self._deliver_wd([r[0] for r in command['assets']], new_assets=True)
            derived = terms_characters.rederive(self, realistic)
            command.update({k: v for k, v in derived.items() if k != 'op'})
            command['finalize_labels'] = True
            self._core(command)
        self.album_titles = self._album_titles(add_aliases=True)
        return round(time.monotonic() - started, 2)


    def reindex_characters(self, uuids):
        """After the external character index changed (characters added or dropped, claims rescanned): bring in the new
        characters and re-import the machine candidates of `uuids`, the images whose claims changed. Equivalent to refresh()."""
        self.require_writable()
        from . import terms_characters
        with self._labels_lock:
            with self.original() as src, self.connect() as c:
                command = terms_characters.reindex(self, src, c, uuids)
            return self._core(command)['changed']

    def works(self, query=''):
        from . import works
        with self.connect() as c:
            return works.rows(c, VISIBLE.format('m'), query)

    def edit_work(self, action, ident=None, name=None, parent=None, tag=None):
        self.require_writable()
        with self._labels_lock:
            return terms_route.edit_work(self, action, ident, name, parent, tag)

    def apply_work_evidence(self, evidence):
        """Former tools/reconcile_works.py --apply (removed in #186): Danbooru copyright tags and implications grow the work tree (works.py)."""
        self.require_writable()
        readable = {t['name']: names.readable(t['name']) for t in evidence.get('tags', []) if isinstance(t.get('name'), str)}
        # New works' ids are asked for by the core, in the old order (Store._core).
        return self._core({'op': 'work_evidence', 'evidence': evidence, 'readable': readable, 'ids': {}})

    @terms_route.via_core(terms_route.set_work, terms_route.returned)
    def set_work(self, entity, work):
        """写入统一由词条核心路由执行。"""

    def index_entities(self, char_ids):
        """Library characters of external-index ids (char_id -> id, name, visible count), for the settings page."""
        found = {}
        with self.connect() as c:
            for char_id in char_ids:
                row = c.execute(f"""SELECT e.id,{NAME} AS name,(SELECT count(*) FROM memberships m WHERE m.entity=e.id AND {VISIBLE.format('m')}) AS count
                    FROM entities e WHERE e.id=(SELECT coalesce(parent_id,id) FROM entities WHERE id=?)""", (self._linked(c, 'ccip', char_id),)).fetchone()
                if row:
                    found[char_id] = self._display(row, c) | {'id': self._resolve(c, row['id'])}
        return found

    @staticmethod
    def _entity(c, system, key, name, work=''):
        """The identity linked to (system, key); a new one carrying that link when there is none."""
        found = Store._linked(c, system, key)
        if found:
            return found
        ident = str(uuid.uuid4())
        c.execute("INSERT INTO entities(id,name,work,kind,work_id) VALUES (?,'',?,'character',(SELECT entity_id FROM links WHERE system='qualifier' AND key=?))", (ident, work or '', work or ''))
        Store.link(c, ident, system, key)
        if name and name != key:
            Store.alias(c, ident, name, 'index')
        return ident

    @staticmethod
    def _resolve(c, ident):
        """Follow merges to the character that now stands for `ident`."""
        for _ in range(16):
            row = c.execute('SELECT merged_into FROM entities WHERE id=?', (ident,)).fetchone()
            if not row or not row[0]:
                return ident
            ident = row[0]
        return ident



    def _album_map(self):
        path = self.source.parent / 'album-char-map.json'
        return json.loads(path.read_text()).get('map', {}) if path.exists() else {}

    @effects.operation('imported')
    def import_changes(self, added=(), removed=(), run_id=None):
        """Bring newly observed images in and drop removed ones without a full refresh.

        Uses the same per-row logic as refresh(); derived tables are refreshed for the touched images,
        except duplicates, folding, collection rules and exclusions, which are cheap to rebuild whole.
        Once labels are the terms core's, the new pictures' WD results go to it first, as evidence packages
        (their assets rows are not written yet, hence new_assets); general_hits and asset_rating are its views.
        """
        self.require_writable()
        added, removed = list(dict.fromkeys(added)), list(dict.fromkeys(removed))
        with self._labels_lock:
            with self.connect() as c:
                in_core = terms_core.labels_in_core(c)
            if in_core and added:
                self._deliver_wd(added, new_assets=True)
            return self._import_changes(added, removed, run_id, in_core)

    def _import_changes(self, added, removed, run_id, in_core):
        self.require_writable()
        from . import terms_characters
        with self.original() as src, self.connect() as c:
            command = terms_characters.import_changes(self, src, c, added, removed, run_id)
        out = self._core(command | {'effect': 'imported'})
        return {'added': out['added'], 'removed': out['removed']}

    def library_uuids(self):
        with self.connect() as c:
            return {r[0] for r in c.execute('SELECT uuid FROM assets')}

    def refresh_paths(self):
        """Follow files the source moved or renamed (folder sources): copy changed names and paths into the library.
        Only where the file is; nothing derived depends on the path. Returns how many changed."""
        self.require_writable()
        with self.original() as src:
            where = {u: (name, path) for u, name, path in
                     src.execute('SELECT uuid,filename,src_path FROM assets WHERE in_scope=1 AND COALESCE(trashed,0)=0')}
        with self.connect() as c:
            stale = [(name, path, u) for u, old_name, old_path in c.execute('SELECT uuid,filename,src_path FROM assets')
                     for name, path in [where.get(u, (old_name, old_path))] if (name, path) != (old_name, old_path)]
            if stale:
                c.execute('BEGIN IMMEDIATE')
                c.executemany('UPDATE assets SET filename=?,src_path=? WHERE uuid=?', stale)
                c.execute('COMMIT')
        return len(stale)

    def sync_begin(self):
        """Open a sync run; a run still marked running was cut off by a stop."""
        self.require_writable()
        with self.connect() as c:
            c.execute("UPDATE sync_runs SET status='interrupted',finished_at=datetime('now','localtime') WHERE status='running'")
            return c.execute("INSERT INTO sync_runs(status,started_at) VALUES ('running',datetime('now','localtime'))").lastrowid

    def sync_update(self, run_id, finished=False, **fields):
        self.require_writable()
        allowed = ('status', 'found', 'removed', 'imported', 'failed', 'sources', 'message')
        fields = {k: v for k, v in fields.items() if k in allowed}
        sets = [f'{k}=?' for k in fields] + (["finished_at=datetime('now','localtime')"] if finished else [])
        if sets:
            with self.connect() as c:
                c.execute(f"UPDATE sync_runs SET {','.join(sets)} WHERE id=?", [*fields.values(), run_id])

    def sync_history(self, limit=8):
        with self.connect() as c:
            runs = [dict(r) for r in c.execute('SELECT * FROM sync_runs ORDER BY id DESC LIMIT ?', (limit,))]
        for r in runs:
            r['sources'] = json.loads(r['sources']) if r['sources'] else None
        return runs

    def entities(self, query='', limit=30):
        query = query.strip()[:160]
        with self.connect() as c:
            if query:
                self._search(c, query)
            matched = 'JOIN name_matches sm ON sm.entity=e.id' if query else ''
            ranking = 'sm.rank DESC,' if query else ''
            rows = c.execute(f"""SELECT e.id,{DISPLAY} AS name,e.work,e.kind,e.artist,e.label_only,e.mascot,e.parent_id,
                (SELECT count(*) FROM (SELECT uuid FROM memberships WHERE entity=e.id UNION ALL SELECT uuid FROM tag_memberships WHERE entity=e.id) m
                  WHERE {VISIBLE.format('m')}) AS count,
                (SELECT group_concat(value,' · ') FROM (SELECT DISTINCT n.value FROM names n WHERE n.entity_id=e.id
                  AND n.value LIKE ? ESCAPE '\\' AND n.value<>{NAME} LIMIT 2)) AS matched
                FROM entities e {matched} WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL
                ORDER BY {ranking}count DESC,{NAME} LIMIT ?""", (self.like(query), limit))
            return [self._display(r, c) for r in rows]

    @staticmethod
    def like(q):
        return '%' + q.replace('\\', '\\\\').replace('%', '\\%').replace('_', '\\_') + '%'

    @staticmethod
    def _search(c, query):
        c.execute('CREATE TEMP TABLE IF NOT EXISTS name_matches(entity TEXT PRIMARY KEY,rank INTEGER NOT NULL)')
        c.execute('DELETE FROM name_matches')
        hits = {}
        for entity, rank in names.search_hits(c, query).items():
            entity = Store._resolve(c, entity)
            hits[entity] = max(rank, hits.get(entity, -1))
        c.executemany('INSERT INTO name_matches VALUES (?,?)', hits.items())

    def overview(self, nsfw=''):
        with self.connect() as c:
            from . import identity
            confirmed = "SELECT uuid FROM family_confirmed" if identity.enabled(c) else "SELECT uuid FROM assertions WHERE status='confirmed'"
            return {'total': c.execute(f"SELECT count(*) FROM assets a WHERE {VISIBLE.format('a')}{rating('a', nsfw)}").fetchone()[0],
                    'confirmed': c.execute(f"SELECT count(DISTINCT uuid) FROM ({confirmed}) a JOIN assets USING(uuid) WHERE {VISIBLE.format('a')}").fetchone()[0],
                    'indexed': c.execute("SELECT value FROM meta WHERE key='indexed'").fetchone()[0],
                    'undo': self.last_action(c)}

    @staticmethod
    def last_action(c):
        r = c.execute('SELECT id,label,ts,before_json FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 1').fetchone()
        return {'id': r['id'], 'label': names.action_label(c, r), 'ts': r['ts']} if r else None

    def characters(self, query='', minimum=0, ident='', nsfw='', work='', *, summary=False):
        if not isinstance(minimum, int) or minimum < 0:
            raise Invalid('最小成员数不正确')
        query = query.strip()[:160]
        with self.connect() as c:
            c.execute('BEGIN')
            if query:
                self._search(c, query)
            matched = 'JOIN name_matches sm ON sm.entity=e.id' if query else ''
            ranking = 'sm.rank DESC,' if query else ''
            work_ids = []
            if work:
                from . import works
                work_ids = works.descendants(c, work)
            work_filter = ' AND e.work_id IN (' + ','.join('?' for _ in work_ids) + ')' if work_ids else ''
            entity_ids = [r[0] for r in c.execute('SELECT id FROM entities WHERE id=? OR parent_id=?', (ident, ident))] if ident else None
            display = self._display_reader(c, [] if summary else entity_ids)
            id_filter = ' AND e.id=?' if ident else ''
            rows = c.execute(f"""SELECT e.id,{NAME} AS name,e.work,e.kind,e.artist,e.label_only,e.mascot,e.mascot_origin,count(m.uuid) AS count,
                coalesce(sum(m.status='machine'),0) AS unchecked,
                min(CASE WHEN ?<>'hide' OR NOT coalesce(m.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN {NSFW!r}),0) THEN m.uuid END) AS cover,
                (SELECT max(reviewed_at) FROM reviews r WHERE r.entity=e.id) AS reviewed_at
                FROM entities e {matched} LEFT JOIN {'(SELECT * FROM memberships WHERE entity=? UNION ALL SELECT * FROM tag_memberships WHERE entity=?)' if ident else 'memberships'} m
                  ON m.entity=e.id AND {VISIBLE.format('m')}{rating('m', nsfw)}
                WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL{'' if ident else ' AND e.mascot=0'}
                {id_filter}{work_filter}
                GROUP BY e.id HAVING count(m.uuid)>=? ORDER BY {ranking}count DESC,name,e.id""",
                (nsfw, *((ident, ident, ident) if ident else ()), *work_ids, minimum))
            result = [display(r) for r in rows]
            if not result:
                return result
            variants = {}
            # A LEFT JOIN materializes the entire variant_memberships view even for one root.
            # Correlate by skin ID so SQLite can restrict membership/candidate reads first.
            # Sidebar/pickers need variant identities, not a full-library recount of each outfit.
            # Counts remain available through the full directory and per-character reads.
            variant_count = 'NULL' if summary else f"(SELECT count(*) FROM variant_memberships v WHERE v.entity=e.id AND {VISIBLE.format('v')}{rating('v', nsfw)})"
            for r in c.execute(f"""SELECT e.id,e.parent_id,e.variant AS name,
                {variant_count} AS count
                FROM entities e
                WHERE e.parent_id IN (SELECT value FROM json_each(?)) AND e.kind<>'work' AND e.merged_into IS NULL ORDER BY e.variant,e.id""",
                (json.dumps([r['id'] for r in result]),)):
                variants.setdefault(r['parent_id'], []).append(display(r, skin=True))
            for r in result:
                r['variants'] = variants.get(r['id'], [])
                if summary:
                    r['summary'] = True
            # Cover: a picture of this character alone first, then one a person confirmed, then portrait-shaped, then newest;
            # a picture already chosen for a bigger character is skipped when there is another choice.
            # (Any-image covers picked group shots and comic pages that several characters then shared.)
            ranked = c.execute(f"""SELECT entity,uuid FROM (
                SELECT m.entity,m.uuid,row_number() OVER (PARTITION BY m.entity ORDER BY
                    m.status='machine', coalesce(a.width>a.height,0), a.added_at DESC, m.uuid) AS rk
                FROM memberships m JOIN assets a ON a.uuid=m.uuid
                WHERE m.entity IN (SELECT value FROM json_each(?)){rating('m', nsfw)}) WHERE rk<=12 ORDER BY entity,rk""",
                (json.dumps([r['id'] for r in result]),)).fetchall()
            pool = json.dumps(list({u for _, u in ranked}))
            # Visibility (folded copies, exclusions) is checked on these few candidates only; it is the costly part.
            visible = {u for (u,) in c.execute(f'SELECT a.uuid FROM assets a WHERE a.uuid IN (SELECT value FROM json_each(?)) AND {VISIBLE.format("a")}', (pool,))}
            shared = {u for (u,) in c.execute('SELECT uuid FROM memberships WHERE uuid IN (SELECT value FROM json_each(?)) GROUP BY uuid HAVING count(*)>1', (pool,))}
            ranked = [(e, u) for e, u in ranked if u in visible]
            options = {}
            for entity, uuid_ in ranked:
                options.setdefault(entity, []).append(uuid_)
            for entity, uuids in options.items():   # solo pictures first, keeping the order above within each kind
                uuids.sort(key=lambda u: u in shared)
            covers, used = {}, set()
            for r in sorted(result, key=lambda r: -r['count']):
                choice = next((u for u in options.get(r['id'], []) if u not in used), None) or (options.get(r['id']) or [r['cover']])[0]
                covers[r['id']] = choice; used.add(choice)
            for r in result:
                r['cover'] = covers.get(r['id'], r['cover'])
            if summary:
                fields = ('id', 'name', 'tag', 'name_source', 'work', 'work_id', 'work_name', 'kind',
                          'count', 'unchecked', 'cover', 'summary')
                return [{**{k: r[k] for k in fields if k in r},
                         'variants': [{'id': v['id'], 'name': v['name']} for v in r['variants']]} for r in result]
            return result

    def review_pools(self, nsfw=''):
        r = lambda alias: rating(alias, nsfw)
        with self.connect() as c:
            row = c.execute(f"""SELECT count(*),min(a.uuid) FROM assets a WHERE {VISIBLE.format('a')}{r('a')} AND
                NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)
                AND NOT EXISTS(SELECT 1 FROM tag_memberships t WHERE t.uuid=a.uuid)
                AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)""").fetchone()
            skipped = c.execute(f"SELECT count(*) FROM deferred a JOIN assets USING(uuid) WHERE {VISIBLE.format('a')}{r('a')}").fetchone()[0]
            # Same set the duplicates page lists: excluded copies are not counted.
            listed = 'NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=d.uuid)' + r('d')
            dups = c.execute(f'SELECT count(*),count(DISTINCT grp) FROM duplicates d WHERE {listed}').fetchone()
            mixed = c.execute(f'SELECT count(DISTINCT d.grp) FROM duplicates d WHERE {MIXED} AND {listed}').fetchone()[0]
            reasons = dict(c.execute(f"SELECT reason,count(*) FROM excluded a WHERE {UNFOLDED.format('a')}{r('a')} GROUP BY reason"))
            recent = c.execute(f"""SELECT count(*) FROM sync_added a WHERE run_id=(SELECT max(run_id) FROM sync_added)
                AND {VISIBLE.format('a')}{r('a')} AND EXISTS(SELECT 1 FROM assets x WHERE x.uuid=a.uuid)""").fetchone()[0]
            pending = c.execute(f"""SELECT count(*) FROM auto_rules a WHERE kind='pending' AND {VISIBLE.format('a')}{r('a')}
                AND NOT EXISTS(SELECT 1 FROM curation k WHERE k.uuid=a.uuid)""").fetchone()[0]
            audit = c.execute(f"SELECT count(*) FROM assets a WHERE {AUDIT_OPEN.format('a')} AND {VISIBLE.format('a')}{r('a')}").fetchone()[0]
        return [{'kind': 'unmatched', 'name': '未识别', 'count': row[0], 'cover': row[1]},
                {'kind': 'skipped', 'name': '已跳过', 'count': skipped, 'cover': None},
                {'kind': 'duplicates', 'name': '重复', 'count': dups[0], 'groups': dups[1], 'mixed': mixed, 'cover': None},
                {'kind': 'excluded', 'name': '已排除', 'count': sum(reasons.values()), 'reasons': reasons, 'cover': None},
                {'kind': 'pending', 'name': '待确认', 'count': pending, 'cover': None},
                {'kind': 'recent', 'name': '新加入', 'count': recent, 'cover': None},
                {'kind': 'audit', 'name': '随机抽查', 'count': audit, 'cover': None}]

    def make_audit(self, uniform=AUDIT_UNIFORM, proto=AUDIT_PROTO, seed=None):
        """Draw the 随机抽查 sample once (no-op when one exists): `uniform` images picked uniformly among visible images
        with a machine claim and no human verdict, one machine claim each; plus `proto` claims made by learning alone."""
        self.require_writable()
        rng = random.Random(seed)
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            if c.execute('SELECT 1 FROM audit_sample LIMIT 1').fetchone():
                return 0
            rows = c.execute(f"""SELECT m.uuid,m.entity,m.sources,EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=m.uuid) AS solo
                FROM memberships m WHERE m.status='machine' AND {VISIBLE.format('m')} AND {self._LEARNABLE.format('m')}
                AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=m.uuid) ORDER BY m.uuid,m.entity""").fetchall()
            claims = {}
            for r in rows:
                claims.setdefault(r['uuid'], []).append(r)
            images = sorted(claims)
            picked = {u: (rng.choice(claims[u]), 'uniform') for u in rng.sample(images, min(uniform, len(images)))}
            learned = [r for r in rows if r['sources'] == 'PROTO' and r['uuid'] not in picked]
            for r in rng.sample(learned, min(proto, len(learned))):
                picked[r['uuid']] = (r, 'proto')
            c.executemany('INSERT INTO audit_sample(uuid,entity,stratum,solo) VALUES (?,?,?,?)',
                          [(u, r['entity'], stratum, r['solo']) for u, (r, stratum) in picked.items()])
            return len(picked)

    def clusters(self, nsfw=''):
        """Groups of look-alike unmatched images, with how many are still unmatched and up to four covers.
        Sizes and covers follow the R18 view: a group with fewer than two images left in view is dropped."""
        with self.connect() as c:
            rows = [dict(r) for r in c.execute(f"""SELECT i.cid,i.near,i.near_dist,i.near_kind,i.traits,i.built_at,count(k.uuid) AS size,
                group_concat(k.uuid) AS members FROM cluster_info i JOIN clusters k ON k.cid=i.cid JOIN assets a ON a.uuid=k.uuid
                WHERE {VISIBLE.format('a')}{rating('a', nsfw)} AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=k.uuid)
                AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=k.uuid)
                GROUP BY i.cid HAVING count(k.uuid)>=2 ORDER BY size DESC,i.cid""")]
            # A group's lead: the character most of its members' leads (「也可能是」) point to. CCIP groups images of one
            # person reliably (99%) but names them poorly (45%), so the group is asked「是 TA 吗？」as a whole.
            members = {u: r['cid'] for r in rows for u in r['members'].split(',')}
            leads = {}
            for u, e in c.execute(f'''SELECT v.uuid,v.entity FROM {LEADS} v WHERE v.uuid IN (SELECT value FROM json_each(?))
                    ORDER BY v.weak,v.score DESC''', (json.dumps(list(members)),)):
                leads.setdefault(u, e)
            votes = {}
            for u, e in leads.items():
                votes.setdefault(members[u], Counter())[e] += 1
            ids = list({e for v in votes.values() for e in v})
            display = self._display_reader(c, ids)
            names_ = {e: display({'id': e})['name'] for e in ids}
        for r in rows:
            r['members'] = r['members'].split(',')
            r['covers'] = r['members'][:4]
            r['traits'] = json.loads(r['traits']) if r['traits'] else []
            top = votes.get(r['cid'], Counter()).most_common(1)
            r['lead'] = ({'id': top[0][0], 'name': names_[top[0][0]], 'count': top[0][1]}
                         if top and top[0][1] >= max(2, (r['size'] + 1) // 2) else None)
        return rows

    @staticmethod
    def _refresh_similar(c):
        """Look-alike groups: images joined by any relation short of 'different' (web/similar.py fills `relations`)."""
        c.execute('DELETE FROM similar_groups')
        present = {r[0] for r in c.execute('SELECT uuid FROM assets')}
        parent = {}
        def find(u):
            while parent.setdefault(u, u) != u:
                parent[u] = parent[parent[u]]
                u = parent[u]
            return u
        for a, b in c.execute(f"SELECT a,b FROM relations WHERE kind IN {SIMILAR_KINDS!r}"):
            if a in present and b in present:
                parent[find(a)] = find(b)
        groups = {}
        for u in parent:
            groups.setdefault(find(u), []).append(u)
        ordered = sorted((sorted(g) for g in groups.values() if len(g) > 1), key=lambda g: (-len(g), g[0]))
        c.executemany('INSERT INTO similar_groups VALUES (?,?)', [(u, gid) for gid, g in enumerate(ordered, 1) for u in g])

    def refresh_similar(self):
        """New relations: rebuild the look-alike groups and the folded copies (some relations fold)."""
        self.require_writable()
        with self.original() as src, self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            self._refresh_similar(c)
            self._refresh_duplicates(c, src)
            self._refresh_fold(c)

    def similar_groups(self, kind='', nsfw=''):
        """Look-alike groups with their visible members, covers and the kinds of relation inside them (R18 view applied)."""
        if kind and kind not in SIMILAR_KINDS:
            raise Invalid('未知关系')
        with self.connect() as c:
            c.execute('BEGIN')
            members = {}
            for gid, uuid_ in c.execute(f'SELECT g.gid,g.uuid FROM similar_groups g JOIN assets a ON a.uuid=g.uuid WHERE {VISIBLE.format("a")}{rating("a", nsfw)} ORDER BY g.gid,a.added_at DESC'):
                members.setdefault(gid, []).append(uuid_)
            kinds = {}
            for gid, k, n in c.execute(f'''SELECT g.gid,r.kind,count(*) FROM relations r JOIN similar_groups g ON g.uuid=r.a
                    JOIN similar_groups h ON h.uuid=r.b AND h.gid=g.gid JOIN assets a ON a.uuid=r.a JOIN assets b ON b.uuid=r.b
                    WHERE {VISIBLE.format('a')} AND {VISIBLE.format('b')}{rating('a', nsfw)}{rating('b', nsfw)} GROUP BY g.gid,r.kind'''):
                if k in SIMILAR_KINDS:
                    kinds.setdefault(gid, {})[k] = n
        rows = [{'gid': gid, 'size': len(m), 'covers': m[:4], 'kinds': kinds.get(gid, {})} for gid, m in members.items() if len(m) > 1]
        return [r for r in rows if not kind or kind in r['kinds']]

    def general_tags(self, query):
        query = query.strip().casefold()[:80]
        return [tag for tag in self.tag_names if query in tag.casefold()][:30]

    def _selection(self, c, filters):
        allowed = {'entity', 'variant', 'status', 'q', 'tag', 'utag', 'pool', 'order', 'nsfw', 'dup', 'reason', 'cluster', 'work', 'day', 'artist'}
        if not isinstance(filters, dict) or set(filters) - allowed or any(not isinstance(v, str) for v in filters.values()):
            raise Invalid('筛选参数不正确')
        entity, variant = filters.get('entity', ''), filters.get('variant', '')
        clauses, args = ['1'], []
        if filters.get('day'):
            clauses.append('a.added_at>=? AND a.added_at<?')
            args += list(day_bounds(filters['day']))
        if filters.get('artist'):
            # The artist's pictures come from provenance (tests/reference/web/artists.py, set by the server); attribution is untouched.
            if not self.artist_assets:
                raise Invalid('这个图库还没有画师数据')
            clauses.append('a.uuid IN (SELECT value FROM json_each(?))')
            args.append(json.dumps(self.artist_assets(filters['artist'])))
        if filters.get('work'):
            from . import works
            work_ids = works.descendants(c, filters['work'])
            clauses.append('EXISTS(SELECT 1 FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=a.uuid AND e.work_id IN (' + ','.join('?' for _ in work_ids) + '))')
            args.extend(work_ids)
        if entity:
            entity = self._resolve(c, entity)
            r = c.execute('SELECT parent_id FROM entities WHERE id=?', (entity,)).fetchone()
            if not r:
                raise Invalid('角色不存在')
            if r[0]:
                variant, entity = entity, r[0]
            if filters.get('status') == 'maybe':
                # Machine evidence points at this character, but the image counts for someone else (or no one).
                clauses.append(f'''EXISTS(SELECT 1 FROM {HINTS} v WHERE v.uuid=a.uuid AND v.entity=?)
                    AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid AND m.entity=?)
                    AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=a.uuid AND s.entity=? AND s.status='rejected')''')
                args += [entity] * 3
            else:
                tagged = c.execute('SELECT mascot FROM entities WHERE id=?', (entity,)).fetchone()
                table = 'tag_memberships' if tagged and tagged[0] else 'memberships'
                clauses.append(f'EXISTS(SELECT 1 FROM {table} m WHERE m.uuid=a.uuid AND m.entity=?)')
                args.append(entity)
        if variant:
            r = c.execute('SELECT parent_id FROM entities WHERE id=?', (variant,)).fetchone()
            if not r or not r[0] or (entity and r[0] != entity):
                raise Invalid('子标签不属于该角色')
            clauses.append('EXISTS(SELECT 1 FROM variant_memberships v WHERE v.uuid=a.uuid AND v.entity=?)')
            args.append(variant)
        pool = filters.get('pool', '')
        if pool == 'unmatched':
            clauses += ['NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)',
                        'NOT EXISTS(SELECT 1 FROM tag_memberships t WHERE t.uuid=a.uuid)',
                        'NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)']
        elif pool == 'skipped':
            clauses.append('EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)')
        elif pool == 'duplicates':
            clauses.append('EXISTS(SELECT 1 FROM duplicates d WHERE d.uuid=a.uuid)')
        elif pool == 'excluded':
            clauses.append('EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)')
        elif pool == 'cluster':
            if not filters.get('cluster', '').isdigit():
                raise Invalid('请指定分组')
            clauses += ['EXISTS(SELECT 1 FROM clusters k WHERE k.uuid=a.uuid AND k.cid=?)',
                        'NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid)',
                        'NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)']
            args.append(int(filters['cluster']))
        elif pool == 'similar':
            if not filters.get('cluster', '').isdigit():
                raise Invalid('请指定相似组')
            clauses.append('EXISTS(SELECT 1 FROM similar_groups g WHERE g.uuid=a.uuid AND g.gid=?)')
            args.append(int(filters['cluster']))
        elif pool == 'recent':
            clauses.append('EXISTS(SELECT 1 FROM sync_added s WHERE s.uuid=a.uuid AND s.run_id=(SELECT max(run_id) FROM sync_added))')
        elif pool == 'audit':
            # Answered images stay in place (their badge shows the answer); the sidebar counts the open ones.
            clauses.append('EXISTS(SELECT 1 FROM audit_sample u WHERE u.uuid=a.uuid)')
        elif pool == 'pending':
            clauses.append("EXISTS(SELECT 1 FROM auto_rules r WHERE r.uuid=a.uuid AND r.kind='pending') AND NOT EXISTS(SELECT 1 FROM curation k WHERE k.uuid=a.uuid)")
        elif pool:
            raise Invalid('未知图片池')
        if pool == 'duplicates':
            clauses.append('NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid)')
        elif pool == 'excluded':
            clauses.append(UNFOLDED.format('a'))
        else:
            clauses.append(VISIBLE.format('a'))
        reason = filters.get('reason', '')
        if reason:
            if pool != 'excluded' or reason not in REASONS:
                raise Invalid('未知排除原因')
            clauses.append('EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=a.uuid AND x.reason=?)')
            args.append(reason)
        dup = filters.get('dup', '')
        if dup == 'mixed':
            clauses.append(f'EXISTS(SELECT 1 FROM duplicates d WHERE d.uuid=a.uuid AND {MIXED})')
        elif dup:
            raise Invalid('未知重复筛选')
        status = filters.get('status', 'all')
        if status == 'maybe':
            if not entity:
                raise Invalid('「可能也是」需要指定角色')
        elif status in ('confirmed', 'machine'):
            clauses.append('EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=a.uuid AND m.status=?' + (' AND m.entity=?' if entity else '') + ')')
            args += [status] + ([entity] if entity else [])
        elif status == 'ambiguous':
            clauses.append('EXISTS(SELECT 1 FROM ambiguous_claims x WHERE x.uuid=a.uuid' + (' AND x.entity=?' if entity else '') + ')')
            args += [entity] if entity else []
        elif status == 'unknown':
            clauses.append('EXISTS(SELECT 1 FROM deferred d WHERE d.uuid=a.uuid)')
        elif status != 'all':
            raise Invalid('未知筛选状态')
        query = filters.get('q', '').strip()[:160]
        if query:
            matches = list(names.search_hits(c, query))
            search = "a.filename LIKE ? ESCAPE '\\'"
            args.append(self.like(query))
            if matches:
                search += ' OR a.uuid IN (SELECT uuid FROM memberships WHERE entity IN (' + ','.join('?' for _ in matches) + '))'
                args += matches
            clauses.append('(' + search + ')')
        tag = filters.get('tag', '')
        nsfw = filters.get('nsfw', '')
        combined = tag and nsfw == 'only' and terms_core.labels_in_core(c)
        if tag:
            if tag not in self.tag_set:
                raise Invalid('请选择已有的通用标签')
            if combined:
                # 由 NSFW 投影驱动，再按图片主键查标签；不用展开整库标签或在两个视图里重复连接 assets。
                clauses.append(f'''a.uuid IN (
                    SELECT ta.key FROM term_projection rated JOIN term_assets ta ON ta.id=rated.asset_id AND ta.namespace='library'
                    WHERE rated.state IN ('machine','confirmed') AND rated.term_id IN (
                        SELECT t.id FROM terms t JOIN term_links l ON l.term_id=t.id AND l.system='wd_rating'
                        WHERE t.kind='rating' AND l.key IN {NSFW!r})
                    AND EXISTS(SELECT 1 FROM term_projection tagged WHERE tagged.asset_id=rated.asset_id
                        AND tagged.state IN ('machine','confirmed') AND tagged.term_id IN (
                            SELECT t.id FROM terms t JOIN term_links l ON l.term_id=t.id AND l.system='wd'
                            WHERE t.kind='topic' AND l.key=?)))''')
            else:
                clauses.append('a.uuid IN (SELECT uuid FROM general_hits WHERE value=?)')
            args.append(tag)
        if nsfw in ('hide', 'only') and not combined:
            hit = f"a.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN {NSFW!r})"
            # 正向筛选允许 SQL 的 NULL 直接被 WHERE 排除，保留 IN 驱动 uuid 索引的计划。
            clauses.append(f'NOT coalesce({hit},0)' if nsfw == 'hide' else hit)
        elif nsfw and nsfw not in ('hide', 'only'):
            raise Invalid('未知内容筛选')
        utag = filters.get('utag', '')
        if utag:
            clauses.append('a.uuid IN (SELECT uuid FROM asset_tags WHERE tag=?)')
            args.append(utag)
        order = filters.get('order', 'newest')
        if order not in ('newest', 'oldest', 'suspicious', 'hinted'):
            raise Invalid('未知排序')
        # Ascending lexicographic key; confirmed identities are always last.
        ordering = 'a.added_at ' + ('ASC' if order == 'oldest' else 'DESC') + ',a.uuid'
        if (pool == 'duplicates' or dup) and order != 'suspicious':
            # Copies stay adjacent: groups by their latest copy, then each group's copies oldest first.
            ordering = (f"(SELECT d.grp_added FROM duplicates d WHERE d.uuid=a.uuid) {'ASC' if order == 'oldest' else 'DESC'},"
                        '(SELECT d.grp FROM duplicates d WHERE d.uuid=a.uuid),(SELECT d.pos FROM duplicates d WHERE d.uuid=a.uuid),a.uuid')
        if filters.get('status') == 'maybe' and order == 'suspicious':
            # Strongest evidence first: these are the likeliest misassignments.
            ordering = (f"(SELECT min(v.weak)*10-max(v.score) FROM {HINTS} v WHERE v.uuid=a.uuid AND v.entity='"
                        + entity.replace("'", "''") + "'),a.uuid")
        elif order == 'hinted':
            # Images with machine leads first (strong evidence before weak claims, higher score first), then newest.
            ordering = f'coalesce(({HINT_KEY}),99),a.added_at DESC,a.uuid'
        elif order == 'suspicious':
            target = ' AND m.entity=' + "'" + entity.replace("'", "''") + "'" if entity else ''
            ordering = f'(SELECT min(m.skey) FROM memberships m WHERE m.uuid=a.uuid{target}),a.uuid'
        return ' AND '.join(clauses), args, ordering

    def _memberships(self, c, ident):
        results = []
        for r in c.execute(f'SELECT m.*,e.id,{NAME} AS name,e.work FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=? ORDER BY m.status=\'machine\',name,e.id', (ident,)):
            item = self._display(r, c)
            item['evidence'] = {'sources': sorted(filter(None, r['sources'].split(','))), 'max_score': r['score']}
            item['variants'] = [self._display(v, c, skin=True) for v in c.execute('SELECT entity AS id,variant AS name,status FROM variant_memberships WHERE uuid=? AND parent_id=? ORDER BY variant', (ident, r['id']))]
            results.append(item)
        return results

    def asset_count(self, filters):
        with self.connect() as c:
            c.execute('BEGIN')
            where, args, _ = self._selection(c, filters)
            return {'total': c.execute(f'SELECT count(*) FROM assets a WHERE {where}', args).fetchone()[0]}

    def assets(self, entity='', status='all', query='', offset=0, limit=100, order='newest', pool='', tag='', variant='', utag='', nsfw='', dup='', reason='', cluster='', work='', include_total=True, day='', artist=''):
        with self.connect() as c:
            c.execute('BEGIN')
            where, args, ordering = self._selection(c, dict(entity=entity, status=status, q=query, order=order, pool=pool, tag=tag, variant=variant, utag=utag, nsfw=nsfw, dup=dup, reason=reason, cluster=cluster, work=work, day=day, artist=artist))
            # 同一读事务内复用筛选结果；只为需要总数的 NSFW 请求物化，普通分页仍可提前停止。
            if include_total and nsfw:
                c.execute(f'CREATE TEMP TABLE selected_assets AS SELECT a.uuid,a.width,a.height,a.added_at FROM assets a WHERE {where}', args)
                total = c.execute('SELECT count(*) FROM selected_assets').fetchone()[0]
                rows = [dict(r) for r in c.execute(f'SELECT a.uuid,a.width,a.height FROM selected_assets a ORDER BY {ordering} LIMIT ? OFFSET ?', [limit, offset])]
            else:
                total = c.execute(f'SELECT count(*) FROM assets a WHERE {where}', args).fetchone()[0] if include_total else None
                rows = [dict(r) for r in c.execute(f'SELECT a.uuid,a.width,a.height FROM assets a WHERE {where} ORDER BY {ordering} LIMIT ? OFFSET ?', [*args, limit, offset])]
            return {'items': self._decorate(c, rows), 'total': total, 'offset': offset}

    def _decorate(self, c, rows, extra_ids=()):
        """Memberships, skins, tags, copies and leads for a page of grid rows (each needs 'uuid').
        `extra_ids` are more entities to resolve in the same name lookup; the lookup is returned as `rows.display`."""
        display = None
        if rows:
            page = (json.dumps([row['uuid'] for row in rows]),)
            selected = 'IN (SELECT value FROM json_each(?))'
            # Best unrejected lead for images nobody owns yet: shown as「可能是」on unmatched cards.
            leads = {}
            for u, e in c.execute(f'''SELECT v.uuid,v.entity FROM {LEADS} v WHERE v.uuid {selected}
                    AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=v.uuid) ORDER BY v.weak,v.score DESC''', page):
                leads.setdefault(u, e)
            entity_ids = [r[0] for r in c.execute(f'''SELECT entity FROM memberships WHERE uuid {selected}
                UNION SELECT e.id FROM entities e JOIN memberships m ON m.entity=e.parent_id WHERE m.uuid {selected}''', page + page)]
            # The claim a 随机抽查 image asks about, shown on its card so the grid can be skimmed.
            audit = dict(c.execute(f'SELECT uuid,entity FROM audit_sample WHERE uuid {selected}', page))
            # Its answer: the Y/N given, else a verdict on that character, else another person confirmed on a solo image.
            answered = dict(c.execute(f"""SELECT u.uuid,coalesce(
                (SELECT k.answer FROM asked k WHERE k.uuid=u.uuid AND k.entity=u.entity AND k.answer<>'later'),
                (SELECT CASE s.status WHEN 'confirmed' THEN 'yes' ELSE 'no' END FROM assertions s JOIN entities e ON e.id=s.entity
                   WHERE s.uuid=u.uuid AND coalesce(e.parent_id,e.id)=u.entity ORDER BY s.status='confirmed' LIMIT 1),
                (SELECT 'no' FROM assertions s WHERE s.uuid=u.uuid AND s.status='confirmed' AND u.solo=1 LIMIT 1))
                FROM audit_sample u WHERE u.uuid {selected}""", page))
            entity_ids += list(set(leads.values())) + list(set(audit.values())) + list(extra_ids)
            display = self._display_reader(c, entity_ids)
            variants, memberships, tags = {}, {}, {}
            for r in c.execute(f'SELECT uuid,parent_id,entity AS id,variant AS name,status FROM variant_memberships WHERE uuid {selected} ORDER BY variant', page):
                item = display({k: r[k] for k in ('id', 'name', 'status')}, skin=True)
                variants.setdefault((r['uuid'], r['parent_id']), []).append(item)
            for r in c.execute(f"SELECT m.*,e.id,{NAME} AS name,e.work FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid {selected} ORDER BY m.status='machine',name,e.id", page):
                item = display(r)
                item['evidence'] = {'sources': sorted(filter(None, r['sources'].split(','))), 'max_score': r['score']}
                item['variants'] = variants.get((r['uuid'], r['id']), [])
                memberships.setdefault(r['uuid'], []).append(item)
            for r in c.execute(f'SELECT uuid,tag FROM asset_tags WHERE uuid {selected} ORDER BY tag', page):
                tags.setdefault(r['uuid'], []).append(r['tag'])
            deferred = {r[0] for r in c.execute(f'SELECT uuid FROM deferred WHERE uuid {selected}', page)}
            duplicates = {r['uuid']: {k: r[k] for k in ('grp', 'size', 'pos')}
                          for r in c.execute(f'SELECT uuid,grp,size,pos FROM duplicates WHERE uuid {selected}', page)}
            excluded = dict(c.execute(f'SELECT uuid,reason FROM excluded WHERE uuid {selected}', page))
            for row in rows:
                ident = row['uuid']
                row['memberships'] = memberships.get(ident, [])
                row['confirmed'] = [m for m in row['memberships'] if m['status'] == 'confirmed']
                row['unknown'] = ident in deferred
                row['user_tags'] = tags.get(ident, [])
                row['dup'] = duplicates.get(ident)
                row['excluded'] = excluded.get(ident)
                row['hint'] = {k: display({'id': leads[ident]}).get(k) for k in ('id', 'name')} if ident in leads else None
                row['audit'] = ({k: display({'id': audit[ident]}).get(k) for k in ('id', 'name')} | {'answer': answered.get(ident)}
                                if ident in audit else None)
        self._last_display = display
        return rows

    def ids(self, filters, offset=0, limit=100):
        if not 0 <= offset <= 10000000 or not 1 <= limit <= 5000:
            raise Invalid('分页参数不正确')
        with self.connect() as c:
            where, args, order = self._selection(c, filters)
            return [r[0] for r in c.execute(f'SELECT a.uuid FROM assets a WHERE {where} ORDER BY {order} LIMIT ? OFFSET ?', [*args, limit, offset])]

    def detail(self, ident):
        with self.connect() as c:
            r = c.execute('SELECT uuid,filename,width,height,added_at,src_kind FROM assets WHERE uuid=?', (ident,)).fetchone()
            if not r:
                raise Invalid('图片不存在')
            result = dict(r)
            result['memberships'] = self._memberships(c, ident)
            result['tag_identities'] = [dict(r) for r in c.execute(f'SELECT e.id,{NAME} AS name,t.status FROM tag_memberships t JOIN entities e ON e.id=t.entity WHERE t.uuid=? ORDER BY name', (ident,))]
            result['ownership'] = list(result['memberships'])
            for removed in c.execute(f'''SELECT e.id,{NAME} AS name,s.status,s.origin,v.sources,v.score
                    FROM assertions s JOIN entities e ON e.id=s.entity
                    LEFT JOIN family_evidence v ON v.entity=e.id AND v.uuid=s.uuid
                    WHERE s.uuid=? AND s.status='rejected' AND e.parent_id IS NULL''', (ident,)):
                item = self._display(removed, c)
                item['evidence'] = {'sources': sorted(filter(None, (removed['sources'] or '').split(','))), 'max_score': removed['score']}
                result['ownership'].append(item)
            # Machine identities not counted for this image (single-person rule or collision), best first.
            # Strong evidence that lost (single-person rule, collision) first, then weak middle-tier matches.
            result['alternatives'] = [
                self._display({'id': r['id'], 'name': r['name'], 'work': r['work']}, c) | {'weak': bool(r['weak']),
                 'evidence': {'sources': sorted(filter(None, r['sources'].split(','))), 'max_score': r['score'], 'wd': r['wd'], 'ccip': r['ccip']}}
                for r in c.execute(f'''SELECT * FROM (
                    SELECT e.id,{NAME} AS name,e.work,v.sources,v.score,v.wd,v.ccip,v.rank,0 AS weak FROM family_evidence v
                    JOIN entities e ON e.id=v.entity WHERE v.uuid=:u
                    UNION ALL
                    SELECT e.id,{NAME},e.work,group_concat(w.source),max(w.score),NULL,max(w.score),1000,1 FROM weak_claims w
                    JOIN entities e ON e.id=w.entity WHERE w.uuid=:u
                    AND NOT EXISTS(SELECT 1 FROM family_evidence f WHERE f.uuid=w.uuid AND f.entity=w.entity) GROUP BY e.id) x
                    WHERE NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=:u AND m.entity=x.id)
                    AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=:u AND s.entity=x.id AND s.status='rejected')
                    ORDER BY weak,rank,score DESC LIMIT 12''', {'u': ident})]
            result['single'] = bool(c.execute('SELECT 1 FROM single_assets WHERE uuid=?', (ident,)).fetchone())
            result['assertions'] = [self._display(r, c) for r in c.execute(f'SELECT e.id,{DISPLAY} AS name,s.status,s.origin FROM assertions s JOIN entities e ON e.id=s.entity WHERE s.uuid=? ORDER BY name,e.id', (ident,))]
            result['candidates'] = [self._display(r, c) for r in c.execute(f'SELECT e.id,{DISPLAY} AS name,c.source,c.score FROM candidates c JOIN entities e ON e.id=c.entity WHERE c.uuid=? ORDER BY name,e.id', (ident,))]
            result['unknown'] = bool(c.execute('SELECT 1 FROM deferred WHERE uuid=?', (ident,)).fetchone())
            result['user_tags'] = [r[0] for r in c.execute('SELECT tag FROM asset_tags WHERE uuid=? ORDER BY tag', (ident,))]
            x = c.execute('SELECT reason,origin FROM excluded WHERE uuid=?', (ident,)).fetchone()
            result['excluded'] = dict(x) if x else None
            k = c.execute('SELECT status FROM curation WHERE uuid=?', (ident,)).fetchone()
            result['curation'] = k[0] if k else None
            result['pending'] = not k and bool(c.execute("SELECT 1 FROM auto_rules WHERE uuid=? AND kind='pending'", (ident,)).fetchone())
            result['duplicates'] = []
            for r in c.execute('''SELECT y.uuid,y.pos,y.size,a.width,a.height,a.added_at FROM duplicates d
                    JOIN duplicates y ON y.grp=d.grp AND y.uuid<>d.uuid JOIN assets a ON a.uuid=y.uuid
                    WHERE d.uuid=? ORDER BY y.pos''', (ident,)):
                copy = dict(r)
                copy['names'] = [m[0] for m in c.execute(f"SELECT {NAME} FROM memberships m JOIN entities e ON e.id=m.entity WHERE m.uuid=? ORDER BY m.status='machine',1", (r['uuid'],))]
                result['duplicates'].append(copy)
        with self.original() as c:
            result['tags'] = [dict(r) for r in c.execute("SELECT value,conf FROM labels WHERE uuid=? AND dim='general' AND conf>=0.35 ORDER BY conf DESC LIMIT 24", (ident,))]
        result['identity'] = self.identity_assignments(ident)
        with self.connect() as c:
            result['question'] = self._question(c, ident)
        return result

    def character_activity(self, nsfw='', days=30):
        """Per character: images added in the last `days` days and the latest added date (R18 view applied)."""
        since = time.strftime('%Y-%m-%d', time.localtime(time.time() - days * 86400))
        seen = f"{VISIBLE.format('m')}{rating('m', nsfw)}"
        with self.connect() as c:
            rows = c.execute(f'''SELECT m.entity,sum(a.added_at>=?),max(substr(a.added_at,1,10)) FROM memberships m
                JOIN assets a ON a.uuid=m.uuid WHERE {seen} GROUP BY m.entity''', (since,)).fetchall()
        return {e: {'recent': r or 0, 'last': last or ''} for e, r, last in rows}

    def home(self, nsfw='', before='', days=5, per_day=18):
        """The 首页 feed: newest visible images grouped by the day they were added, with who appears that day."""
        if before:
            day_bounds(before)
        if not 1 <= days <= 20 or not 1 <= per_day <= 60:
            raise Invalid('分页参数不正确')
        seen = f"{VISIBLE.format('a')}{rating('a', nsfw)}"
        with self.connect() as c:
            c.execute('BEGIN')
            groups = c.execute(f'''SELECT substr(a.added_at,1,10) AS day,count(*) AS n FROM assets a
                WHERE {seen} AND a.added_at IS NOT NULL{' AND a.added_at<?' if before else ''}
                GROUP BY 1 ORDER BY 1 DESC LIMIT ?''', ([before] if before else []) + [days]).fetchall()
            result, rows, wanted = [], [], set()
            for day, n in groups:
                lo, hi = day_bounds(day)
                people = c.execute(f'''SELECT m.entity,count(*) AS n FROM memberships m JOIN assets a ON a.uuid=m.uuid
                    WHERE a.added_at>=? AND a.added_at<? AND {seen} GROUP BY m.entity ORDER BY n DESC,m.entity''', (lo, hi)).fetchall()
                items = [dict(r) for r in c.execute(f'SELECT a.uuid,a.width,a.height FROM assets a WHERE a.added_at>=? AND a.added_at<? AND {seen} ORDER BY a.added_at DESC,a.uuid LIMIT ?', (lo, hi, per_day))]
                wanted.update(r[0] for r in people[:8]); rows += items
                result.append({'day': day, 'count': n, 'people': len(people), 'top': people[:8], 'items': items})
            # One name lookup for every row and every listed character (it is the expensive part).
            self._decorate(c, rows, sorted(wanted))
            display = self._last_display or self._display_reader(c, sorted(wanted))
            for g in result:
                g['characters'] = [{k: x.get(k) for k in ('id', 'name', 'tag')} | {'count': n} for x, n in ((display({'id': e}), n) for e, n in g.pop('top'))]
        for g in result:
            top = g['characters'][0]['id'] if g['characters'] else None
            g['hero'] = next((x['uuid'] for x in g['items'] if top and any(m['id'] == top for m in x['memberships'])), g['items'][0]['uuid'] if g['items'] else None)
        return {'days': result, 'next': result[-1]['day'] if len(result) == days else ''}

    def character_stats(self, ident, nsfw='', exclude='', months=12):
        """Collection rhythm for the detail card: visible images per month (last `months`), first month, latest others."""
        if not isinstance(ident, str) or not ident:
            raise Invalid('缺少角色')
        today = time.localtime()
        keys = []
        y, m = today.tm_year, today.tm_mon
        for _ in range(months):
            keys.append(f'{y:04d}-{m:02d}')
            y, m = (y - 1, 12) if m == 1 else (y, m - 1)
        keys.reverse()
        where = f"m.entity=? AND {VISIBLE.format('m')}{rating('m', nsfw)}"
        with self.connect() as c:
            per = dict(c.execute(f'SELECT substr(a.added_at,1,7),count(*) FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {where} GROUP BY 1', (ident,)).fetchall())
            recent = [r[0] for r in c.execute(f'SELECT m.uuid FROM memberships m JOIN assets a ON a.uuid=m.uuid WHERE {where} AND m.uuid<>? ORDER BY a.added_at DESC,m.uuid LIMIT 4', (ident, exclude or ''))]
        dated = sorted(k for k in per if k)
        return {'count': sum(per.values()), 'first': dated[0] if dated else '', 'months': [{'month': k, 'count': per.get(k, 0)} for k in keys], 'recent': recent}

    def image_path(self, ident):
        with self.connect() as c:
            r = c.execute('SELECT src_path FROM assets WHERE uuid=?', (ident,)).fetchone()
            return Path(r[0]) if r and r[0] else None

    def _resolve_ids(self, c, ids, filters, exclude, confirm, copies=True):
        """Selected images, plus (by default) every copy in their duplicate groups: a write applies to the whole artwork."""
        if not isinstance(exclude, list) or not all(isinstance(x, str) for x in exclude):
            raise Invalid('排除列表不正确')
        if filters is not None:
            if ids is not None:
                raise Invalid('图片列表与筛选条件只能使用一种')
            where, args, _ = self._selection(c, filters)
            excluded = set(exclude)
            ids = [r[0] for r in c.execute(f'SELECT a.uuid FROM assets a WHERE {where} ORDER BY a.uuid', args) if r[0] not in excluded]
        elif exclude:
            raise Invalid('排除列表仅支持按筛选条件选择')
        if not isinstance(ids, list) or not ids or not all(isinstance(x, str) for x in ids):
            raise Invalid('请选择图片')
        if len(ids) > 500 and confirm is not True:
            raise Invalid('超过 500 张，请确认数量并携带 confirm:true；整批可撤销')
        ids = list(dict.fromkeys(ids))
        for ident in ids:
            if not c.execute('SELECT 1 FROM assets WHERE uuid=?', (ident,)).fetchone():
                raise Invalid('所选图片不存在，请刷新')
        if copies:
            for ident in list(ids):
                ids += [r[0] for r in c.execute('SELECT y.uuid FROM duplicates d JOIN duplicates y ON y.grp=d.grp AND y.uuid<>d.uuid WHERE d.uuid=? ORDER BY y.pos', (ident,))]
            ids = list(dict.fromkeys(ids))
        return ids

    @staticmethod
    def _works(c, ids):
        """Artwork count for labels: copies of one image count once."""
        grps = dict(c.execute(f"SELECT uuid,grp FROM duplicates WHERE uuid IN ({','.join('?' for _ in ids)})", ids)) if ids else {}
        return len({grps.get(i, i) for i in ids})


    @staticmethod
    def _change_args(action, entity, entities):
        """change's argument checks, before any transaction (shared with the core route, tests/reference/web/terms_route.py)."""
        if action not in ('confirmed', 'rejected', 'unknown', 'resume', 'add_variant', 'remove_variant'):
            raise Invalid('未知操作')
        if entities is not None:
            if action != 'confirmed' or entity is not None or not isinstance(entities, list) or not 1 <= len(entities) <= 20:
                raise Invalid('请选择角色')

    @staticmethod
    def _answer_args(yes, no, kind, switch, later):
        lists = [yes, no, later]
        if kind not in ('ambiguous', 'seed', 'sample', 'audit', 'hint') or not all(isinstance(x, (list, tuple)) and all(isinstance(i, str) for i in x) for x in lists):
            raise Invalid('回答格式不正确')
        if not any(lists) or sum(map(len, lists)) > 60 or len({i for x in lists for i in x}) != sum(map(len, lists)):
            raise Invalid('回答格式不正确')
        if switch is not None and (not no or yes):
            raise Invalid('回答格式不正确')

    @staticmethod
    def _audit_args(yes, no):
        lists = [yes, no]
        if not all(isinstance(x, (list, tuple)) and all(isinstance(i, str) for i in x) for x in lists):
            raise Invalid('回答格式不正确')
        ids = [i for x in lists for i in x]
        if not ids or len(ids) > 500 or len(set(ids)) != len(ids):
            raise Invalid('回答格式不正确')
        return ids

    @staticmethod
    def _wanted(new_entity):
        wanted = new_entity if isinstance(new_entity, list) else [new_entity]
        if not wanted or len(wanted) > 20:
            raise Invalid('请选择要换成的角色')
        return wanted

    @staticmethod
    def _require_entity(c, entity):
        if not isinstance(entity, str):
            raise Invalid('请选择角色')
        row = c.execute("SELECT id,parent_id FROM entities WHERE kind<>'work' AND id=?", (Store._resolve(c, entity),)).fetchone()
        if not row:
            raise Invalid('角色不存在')
        return row

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.change)
    def change(self, ids, action, entity=None, *, filters=None, exclude=None, confirm=False, entities=None):
        """写入统一由词条核心路由执行。"""

    # ---------- 主动提问: occasionally asking "是 TA 吗？" while browsing ----------
    # Two kinds of machine claims are worth a question: a「可能认错」claim (its runner-up is as likely), and a claim on a
    # character too few confirmations to learn from, so a "yes" moves it toward a prototype (web/learn.py).
    # When every character has enough, the card keeps asking at random: an unbiased sample for calibration.
    # Asked before both: learning's outer ring (weak PROTO claims, 1–1.5× a prototype's
    # threshold). Corrected images sit a median 1.5× out, so widening thresholds would guess; a "yes" here grows the
    # prototype instead, one confirmed image at a time.
    _UNASKED = "NOT EXISTS(SELECT 1 FROM asked k WHERE k.uuid={0}.uuid AND k.entity={0}.entity)"
    _LEARNABLE = "NOT EXISTS(SELECT 1 FROM entities l WHERE l.id={0}.entity AND l.label_only=1)"

    def _hints(self, c, where='', args=()):
        """Weak PROTO claims worth a question: single-person images nobody has named, not owned by that character."""
        return f"""SELECT w.uuid,w.entity,w.score FROM weak_claims w JOIN single_assets o ON o.uuid=w.uuid
            WHERE w.source='PROTO' AND {self._UNASKED.format('w')} AND {self._LEARNABLE.format('w')}
            AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=w.uuid AND m.entity=w.entity)
            AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=w.uuid AND (s.status='confirmed' OR s.entity=w.entity))
            AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=w.uuid) {where}"""

    def _confirmed_count(self, c, entity):
        return c.execute("SELECT count(*) FROM memberships WHERE entity=? AND status='confirmed'", (entity,)).fetchone()[0]

    def _question(self, c, ident):
        """The question worth asking about one image, or None."""
        if c.execute('SELECT 1 FROM excluded WHERE uuid=?', (ident,)).fetchone():
            return None
        audit = c.execute(f"SELECT u.entity FROM audit_sample u JOIN assets a ON a.uuid=u.uuid WHERE u.uuid=? AND {AUDIT_OPEN.format('a')}", (ident,)).fetchone()
        r = None if audit else c.execute(f"""SELECT m.entity,m.rival FROM ambiguous_claims m WHERE m.uuid=? AND {self._UNASKED.format('m')}
            AND {self._LEARNABLE.format('m')} ORDER BY m.margin LIMIT 1""", (ident,)).fetchone()
        hint = None if audit else c.execute(self._hints(c, 'AND w.uuid=?') + ' ORDER BY w.score DESC LIMIT 1', (ident,)).fetchone()
        if audit:
            kind, entity, rival = 'audit', audit[0], None
        elif hint:
            kind, entity, rival = 'hint', hint['entity'], None
        elif r:
            kind, entity, rival = 'ambiguous', r[0], r[1]
        else:
            rows = c.execute(f"""SELECT m.entity FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
                WHERE m.uuid=? AND m.status='machine' AND {self._UNASKED.format('m')} AND {self._LEARNABLE.format('m')}""", (ident,)).fetchall()
            entity = next((e for (e,) in rows if self._confirmed_count(c, e) < ASK_SEEDS), None)
            if not entity:
                return None
            kind, rival = 'seed', None
        name = lambda e: c.execute(f'SELECT {NAME} FROM entities e WHERE id=?', (e,)).fetchone()[0]
        current = c.execute('SELECT entity FROM memberships WHERE uuid=? AND entity<>? LIMIT 1', (ident, entity)).fetchone() if kind == 'hint' else None
        return {'kind': kind, 'entity': entity, 'name': name(entity), 'confirmed': self._confirmed_count(c, entity),
                **({'rival': rival, 'rival_name': name(rival)} if rival else {}),
                **({'current': current[0], 'current_name': name(current[0])} if current else {})}

    def ask_batch(self, nsfw='', size=6, skip=()):
        """One character and up to `size` of its unasked machine images (single-person) for the home card, or None.

        Learning's outer ring comes first (a character with the most weak PROTO claims, lightly shuffled), then characters
        still short of seeds (most machine images first, lightly shuffled); a character put off
        with「以后再说」rests for a week. `skip` lists characters already shown this visit."""
        if not 1 <= size <= 12 or not isinstance(skip, (list, tuple)) or len(skip) > 50:
            raise Invalid('参数不正确')
        seen = f"{VISIBLE.format('m')}{rating('m', nsfw)}"
        rest = "SELECT DISTINCT entity FROM asked WHERE answer='later' AND asked_at>=datetime('now','-7 days')"
        with self.connect() as c:
            c.execute('BEGIN')
            hints = [r for r in c.execute(f"""SELECT h.entity,count(*) AS n FROM ({self._hints(c)}) h JOIN assets m ON m.uuid=h.uuid
                WHERE {seen} AND h.entity NOT IN ({rest}) GROUP BY h.entity HAVING n>=2 ORDER BY n DESC""") if r['entity'] not in skip]
            if hints:
                pick = random.choice(hints[:12])['entity']
                rows = c.execute(f"""SELECT h.uuid,m.width,m.height FROM ({self._hints(c)}) h JOIN assets m ON m.uuid=h.uuid
                    WHERE h.entity=? AND {seen} ORDER BY h.score DESC LIMIT ?""", (pick, size)).fetchall()
                name = c.execute(f'SELECT {NAME} FROM entities e WHERE id=?', (pick,)).fetchone()[0]
                return {'kind': 'hint', 'entity': pick, 'name': name, 'confirmed': self._confirmed_count(c, pick),
                        'items': [dict(r) for r in rows]}
            pool = c.execute(f"""SELECT m.entity,count(*) AS n,
                (SELECT count(*) FROM memberships x WHERE x.entity=m.entity AND x.status='confirmed') AS conf
                FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
                WHERE m.status='machine' AND {seen} AND {self._UNASKED.format('m')} AND {self._LEARNABLE.format('m')}
                AND m.entity NOT IN ({rest}) GROUP BY m.entity HAVING n>=2""").fetchall()
            pool = [r for r in pool if r['entity'] not in skip]
            if not pool:
                return None
            short = sorted((r for r in pool if r['conf'] < ASK_SEEDS), key=lambda r: -r['n'])
            kind = 'seed' if short else 'sample'
            choices = short[:12] if short else pool
            pick = random.choice(choices)['entity']
            rows = c.execute(f"""SELECT m.uuid,a.width,a.height FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
                JOIN assets a ON a.uuid=m.uuid WHERE m.entity=? AND m.status='machine' AND {seen}
                AND {self._UNASKED.format('m')} ORDER BY random() LIMIT ?""", (pick, size)).fetchall()
            name = c.execute(f'SELECT {NAME} FROM entities e WHERE id=?', (pick,)).fetchone()[0]
            return {'kind': kind, 'entity': pick, 'name': name, 'confirmed': self._confirmed_count(c, pick),
                    'items': [dict(r) for r in rows]}

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.answer, terms_route.answered)
    def answer(self, entity, yes=(), no=(), kind='seed', switch=None, later=()):
        """写入统一由词条核心路由执行。"""

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.answer_audit)
    def answer_audit(self, yes=(), no=()):
        """写入统一由词条核心路由执行。"""

    @effects.operation('verdict')
    def correct(self, ident, old_entity, new_entity):
        return self.reassign([ident], old_entity, new_entity)

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.reassign)
    def reassign(self, ids, old_entity, new_entity, *, filters=None, exclude=None, confirm=False):
        """写入统一由词条核心路由执行。"""

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.sweep)
    def sweep(self, entity):
        """写入统一由词条核心路由执行。"""

    def activate_identity(self, mode='hybrid'):
        if self.characters_in_core():
            raise Invalid('角色已切到词条核心，身份策略不能再改（改策略是一次迁移）')
        from . import identity
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            identity.activate(c, mode)
            self._refresh_evidence(c)
            self._refresh_memberships(c)
        return self.identity_status()

    def identity_status(self):
        from . import identity
        with self.connect() as c:
            return {'enabled': identity.enabled(c), 'policy': identity.policy(c), 'graph_version': identity.graph_version(c),
                    'ambiguous': c.execute('SELECT count(*) FROM ambiguous_claims').fetchone()[0],
                    'legacy_hierarchy_pending_people': len(identity.pending_hierarchy_people(c)),
                    'relations': [dict(r) for r in c.execute('SELECT kind,status,count(*) AS count FROM identity_relations GROUP BY kind,status')],
                    'suspended': c.execute('SELECT count(*) FROM identity_suspended').fetchone()[0]}

    @staticmethod
    def _uncertain_args(options, reason):
        if not isinstance(options, list) or len(options) < 2 or len(options) > 20 or not all(isinstance(x,str) for x in options) or len(set(options)) != len(options):
            raise Invalid('未定状态需要 2–20 个不同身份候选')
        if not isinstance(reason, str) or not reason.strip():
            raise Invalid('请保存未定理由')

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.decide)
    def decide_identity(self, *, a=None, b=None, kind=None, relation_id=None, status='proposed', reason='', evidence=None):
        """写入统一由词条核心路由执行。"""

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.uncertain)
    def uncertain_identity(self, ids, options, reason='人工暂不判断'):
        """写入统一由词条核心路由执行。"""

    def identity_relations(self, status='', limit=100, offset=0):
        if status not in ('', 'proposed', 'accepted', 'rejected') or not 1 <= limit <= 200 or offset < 0:
            raise Invalid('关系筛选或分页不正确')
        with self.connect() as c:
            members = {}
            for u, e in c.execute('SELECT uuid,entity FROM memberships UNION SELECT uuid,entity FROM family_evidence'):
                members.setdefault(e, set()).add(u)
            nodes = {r[0]: r[1] for r in c.execute('SELECT id,character_id FROM identity_nodes')}
            out = []
            for row in c.execute("SELECT * FROM identity_relations WHERE ?='' OR status=?", (status,status)):
                r = dict(row)
                affected = members.get(nodes.get(r['a']), set()) | members.get(nodes.get(r['b']), set())
                r.update(affected_images=len(affected), samples=sorted(affected)[:6])
                out.append(r)
            out.sort(key=lambda r: (-r['affected_images'], r['id']))
            result = out[offset:offset+limit]
            for r in result:
                r['evidence'] = [dict(e) | {'payload': json.loads(e['payload'])} for e in c.execute('SELECT * FROM identity_evidence WHERE relation_id=?', (r['id'],))]
                r['decisions'] = [dict(e) for e in c.execute('SELECT * FROM identity_decisions WHERE relation_id=? ORDER BY id', (r['id'],))]
                reviewed = {e[0] for e in c.execute('SELECT fingerprint FROM identity_decision_evidence WHERE decision_id=(SELECT max(id) FROM identity_decisions WHERE relation_id=?)', (r['id'],))}
                r['new_evidence_requires_review'] = r['status'] != 'proposed' and any(e['fingerprint'] not in reviewed for e in r['evidence'])
                r['conflicting_decisions'] = [dict(e) for e in c.execute("SELECT id,kind,status FROM identity_relations WHERE status='accepted' AND id<>? AND ((a=? AND b=?) OR (a=? AND b=?)) AND (kind='different_from' OR ?='different_from')", (r['id'],r['a'],r['b'],r['b'],r['a'],r['kind']))]
            return {'items': result, 'total': len(out)}

    def identity_queue(self, limit=20, offset=0, nsfw='', count_only=False):
        """Character pairs still waiting for a person, most images first: look-alike anchors (identity_pairs) and
        proposed relations. Pairs already one person, already decided, set aside, or without images are left out."""
        if not 1 <= limit <= 100 or offset < 0:
            raise Invalid('分页参数不正确')
        shown = rating('m', nsfw)
        with self.connect() as c:
            c.execute('BEGIN')
            person = dict(c.execute('SELECT id,character_id FROM identity_nodes'))
            fam = lambda e: person.get(e, e)
            # Same count as the sidebar: folded copies and hidden ratings do not count.
            counts = dict(c.execute(f"SELECT entity,count(*) FROM memberships m WHERE {VISIBLE.format('m')}{shown} GROUP BY entity"))
            decided = {frozenset((fam(a), fam(b))) for a, b in c.execute("SELECT a,b FROM identity_relations WHERE status<>'proposed'")}
            skipped = set(json.loads((c.execute("SELECT value FROM prefs WHERE key='identity_skipped'").fetchone() or ['[]'])[0]))
            marks = {fam(r[0]) for r in c.execute('SELECT id FROM entities WHERE label_only=1')}
            found = {}
            def add(a, b, distance, hint, relation_id=None):
                a, b = fam(a), fam(b)
                key = frozenset((a, b))
                if a == b or a in marks or b in marks or key in decided or '|'.join(sorted(key)) in skipped or not (counts.get(a) and counts.get(b)):
                    return
                item = found.setdefault(key, {'a': a, 'b': b, 'distance': None, 'hint': None, 'relation_id': None})
                if distance is not None and (item['distance'] is None or distance < item['distance']):
                    item['distance'] = distance
                if hint and not item['hint']:
                    item.update(hint=hint, relation_id=relation_id)
            for a, b, original, distance, evidence in c.execute('SELECT a,b,original_distance,distance,evidence_json FROM identity_pairs'):
                if count_only:
                    add(a, b, None, None)
                    continue
                e = json.loads(evidence)
                hint = None
                if e.get('relation') and e.get('subject') and e.get('object'):
                    hint = {'kind': e['relation'], 'subject': e['subject'], 'object': e['object'], 'source': 'Danbooru'}
                add(a, b, distance if distance is not None else original, hint)
            for ident, a, b, kind, source in c.execute("""SELECT r.id,r.a,r.b,r.kind,(SELECT min(source) FROM identity_evidence e WHERE e.relation_id=r.id)
                    FROM identity_relations r WHERE r.status='proposed'"""):
                if count_only:
                    add(a, b, None, None)
                    continue
                tags = dict(c.execute("SELECT entity_id,key FROM links WHERE system='danbooru' AND entity_id IN (?,?)", (a, b)))
                add(a, b, None, {'kind': kind, 'subject': tags.get(a, ''), 'object': tags.get(b, ''), 'source': source or ''}, ident)
            if count_only:
                return {'total': len(found)}
            items = sorted(found.values(), key=lambda x: (-(counts[x['a']] + counts[x['b']]), x['a'], x['b']))
            page = items[offset:offset + limit]
            for x in page:
                for side in ('a', 'b'):
                    e = x[side]
                    info = json.loads(c.execute('SELECT entity_info(?,0)', (e,)).fetchone()[0])
                    x[side] = {'id': e, 'name': info.get('name', ''), 'tag': info.get('tag', ''), 'count': counts[e],
                               'samples': [r[0] for r in c.execute(f"""SELECT m.uuid FROM memberships m WHERE m.entity=? AND {VISIBLE.format('m')}{shown}
                                   ORDER BY m.status='confirmed' DESC,m.score DESC,m.uuid LIMIT 4""", (e,))]}
            return {'items': page, 'total': len(items)}

    def skip_identity_pair(self, a, b):
        """「暂不判断」: set a pair aside so the queue moves on; it returns if this is undone."""
        self.require_writable()
        if not all(isinstance(x, str) and x for x in (a, b)) or a == b:
            raise Invalid('请选择两个角色')
        key = '|'.join(sorted((a, b)))
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            row = c.execute("SELECT value FROM prefs WHERE key='identity_skipped'").fetchone()
            before = row[0] if row else None
            skipped = json.loads(before or '[]')
            if key not in skipped:
                skipped.append(key)
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('identity_skipped',?)", (json.dumps(skipped),))
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)', ('已暂不判断一对角色', json.dumps([{'identity_skip': before}])))
            return self.last_action(c)

    def identity_pairs(self, limit=100, offset=0, include_empty=False):
        from . import identity
        if not 1 <= limit <= 200 or offset < 0:
            raise Invalid('分页参数不正确')
        with self.connect() as c:
            nodes = {r[0]: r[1] for r in c.execute('SELECT id,character_id FROM identity_nodes')}
            canonical = dict(c.execute('SELECT id,canonical_id FROM identity_nodes'))
            affected = {}
            for u,e in c.execute('SELECT uuid,entity FROM memberships UNION SELECT uuid,entity FROM family_evidence'):
                affected.setdefault(e,set()).add(u)
            anchors = {}
            try:
                bank = identity.load_bank(c)
            except (OSError,ValueError,KeyError):
                bank = None
            if bank:
                for p in bank[0]['prototypes']:
                    anchors.setdefault(canonical.get(p['entity'],p['entity']),[]).append(p)
            result=[]
            for row in c.execute('SELECT * FROM identity_pairs'):
                r=dict(row); a,b=nodes.get(r['a'],r['a']),nodes.get(r['b'],r['b'])
                images=affected.get(a,set())|affected.get(b,set())
                if not images and not include_empty:
                    continue
                r.update(affected_images=len(images),samples=sorted(images)[:6])
                result.append(r)
            result.sort(key=lambda r:(-r['affected_images'],r['a'],r['b']))
            page=result[offset:offset+limit]
            for r in page:
                r['evidence']=json.loads(r.pop('evidence_json'))
                r['anchors_a']=anchors.get(canonical.get(r['a'],r['a']),[])
                r['anchors_b']=anchors.get(canonical.get(r['b'],r['b']),[])
            return {'items':page,'total':len(result),'impact_policy':'current memberships or stored identity evidence; distinct UUID union'}

    def identity_assignments(self, ident):
        from . import identity
        with self.connect() as c:
            if not identity.enabled(c):
                return {'assignments': [], 'undetermined': []}
            # The identity_assignments view, filtered by uuid: joining the view made SQLite materialize
            # variant_memberships for the whole library (~140 ms per detail); filtering it first uses the indexes.
            assignments = [dict(r) for r in c.execute("""SELECT m.uuid,m.entity AS character_id,v.entity AS variant_id,m.status,m.origin
                FROM memberships m LEFT JOIN (SELECT entity,parent_id FROM variant_memberships WHERE uuid=?) v ON v.parent_id=m.entity
                WHERE m.uuid=?""", (ident, ident))]
            pending = [dict(r) for r in c.execute('SELECT options_json,reason FROM identity_uncertain WHERE uuid=? UNION ALL SELECT options_json,reason FROM identity_suspended WHERE uuid=?', (ident,ident))]
            for r in pending:
                r['options'] = json.loads(r.pop('options_json'))
            return {'assignments': assignments, 'undetermined': pending}

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.merge)
    def merge(self, source, target):
        """写入统一由词条核心路由执行。"""


    @effects.operation('verdict')
    def undo(self, action_id):
        # A step the Rust terms core wrote is undone by the core, after this connection is closed (tests/reference/web/terms_core.py).
        with self.connect() as c:
            row = c.execute('SELECT before_json FROM actions WHERE id=? AND undone=0', (action_id,)).fetchone()
        if row and terms_core.character_owns(json.loads(row[0])):
            # A character step (new, or an old shape the core took over): the core checks it is still the latest.
            self._core({'op': 'undo', 'action': action_id, 'effect': 'verdict'})
            with self.connect() as c:
                return self.last_action(c)
        if row and terms_core.owns(json.loads(row[0]), True, True):
            return self._undo_terms(action_id)
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            last = self.last_action(c)
            if not last or last['id'] != action_id:
                raise Invalid('操作历史已变化，请刷新后撤销最近一步')
            row = c.execute('SELECT before_json FROM actions WHERE id=?', (action_id,)).fetchone()
            before = json.loads(row[0])
            # An undone answer to 主动提问 / 随机抽查 was never given: the image can be asked again.
            c.execute('DELETE FROM asked WHERE action_id=?', (action_id,))
            for b in before:
                if 'identity_skip' in b:
                    if b['identity_skip'] is None:
                        c.execute("DELETE FROM prefs WHERE key='identity_skipped'")
                    else:
                        c.execute("INSERT OR REPLACE INTO prefs VALUES ('identity_skipped',?)", (b['identity_skip'],))
                    continue
                if 'curation' in b:
                    c.execute('DELETE FROM curation WHERE uuid=?', (b['uuid'],))
                    if b['curation']:
                        c.execute('INSERT INTO curation VALUES (?,?,?,?)', (b['uuid'], b['curation']['status'], b['curation']['origin'], b['curation']['created_at']))
                    continue
                # 未知历史仍按旧入口拒绝，事务回滚；不能因删除旧分支而把它标成已撤销。
                raise KeyError('entity')
            self._refresh_excluded(c, [b['uuid'] for b in before if 'curation' in b])
            c.execute('DELETE FROM reviews WHERE action_id=?', (action_id,))
            c.execute('UPDATE actions SET undone=1 WHERE id=?', (action_id,))
            return self.last_action(c)

    def _undo_terms(self, action_id):
        """The core checks, in its own transaction, that this is still the latest step. Nothing Python derives reads
        the terms tables yet, so there is no post-commit work to record."""
        try:
            terms_core.undo(self.path, action_id)
        except terms_core.Failed as exc:
            raise Invalid(str(exc)) from exc
        with self.connect() as c:
            return self.last_action(c)

    @staticmethod
    def _tag_name(tag):
        if not isinstance(tag, str) or not tag.strip() or len(tag.strip()) > 60:
            raise Invalid('标签应为 1–60 个字符')
        return ' '.join(tag.split())

    def judge_artist(self, ids, artist, verdict, name=''):
        """是 / 不是 TA for provenance's artist on these pictures; None clears the verdict. Undoable like every human write."""
        self.require_writable()
        if verdict not in ('yes', 'no', None):
            raise Invalid('画师结论只能是「是」或「不是」')
        if not isinstance(artist, str) or not 1 <= len(artist) <= 80:
            raise Invalid('画师不正确')
        if not isinstance(ids, list) or not 1 <= len(ids) <= 5000 or not all(isinstance(i, str) for i in ids):
            raise Invalid('图片不正确')
        name = name if isinstance(name, str) and name.strip() else '该画师'
        label = lambda n: {'yes': f'已确认 {n} 张是 {name} 画的', 'no': f'已标记 {n} 张不是 {name} 画的', None: f'已清除 {n} 张对 {name} 的判断'}[verdict]
        with self.connect() as c:
            ids = [r[0] for r in c.execute(f"SELECT uuid FROM assets WHERE uuid IN ({','.join('?' * len(ids))})", ids)]
        if not ids:
            raise Invalid('图片不存在')
        try:
            terms_core.judge(self.path, artist, verdict, label(len(ids)), ids)
        except terms_core.Failed as exc:
            raise Invalid(str(exc)) from exc
        with self.connect() as c:
            return self.last_action(c)

    def uuids(self):
        """Every picture in the library (excluded and folded ones too)."""
        with self.connect() as c:
            return {r[0] for r in c.execute('SELECT uuid FROM assets')}

    def artist_verdicts(self, uuid=None):
        """{artist: verdict} for one picture, or {artist: {uuid: verdict}} for the whole library."""
        with self.connect() as c:
            if terms_core.artists_in_core(c):
                # The core's effective verdicts (the latest step not undone); artist_verdicts is frozen since cutover.
                rows = c.execute("""SELECT t.key,a.key,v.verdict FROM term_effective_verdicts v
                    JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library' JOIN terms t ON t.id=v.term_id AND t.kind='artist'
                    WHERE ? IS NULL OR a.key=?""", (uuid, uuid)).fetchall()
                if uuid is not None:
                    return {artist: v for artist, _, v in rows}
                out = {}
                for artist, u, v in rows:
                    out.setdefault(artist, {})[u] = v
                return out
            if uuid is not None:
                return dict(c.execute('SELECT artist,verdict FROM artist_verdicts WHERE uuid=?', (uuid,)))
            out = {}
            for a, u, v in c.execute('SELECT artist,uuid,verdict FROM artist_verdicts'):
                out.setdefault(a, {})[u] = v
            return out

    def user_tags(self, query=''):
        with self.connect() as c:
            return [dict(r) for r in c.execute(f"""SELECT tag,count(*) AS count FROM asset_tags a JOIN assets USING(uuid)
                WHERE {VISIBLE.format('a')} AND tag LIKE ? ESCAPE '\\' GROUP BY tag ORDER BY count DESC,tag LIMIT 60""", (self.like(query.strip()[:60]),))]

    def tag(self, ids, tag, add=True, *, filters=None, exclude=None, confirm=False):
        """Human general tags; kept apart from WD machine observations. Undoable per batch."""
        self.require_writable()
        tag = self._tag_name(tag)
        with self.connect() as c:
            c.execute('BEGIN')
            ids = self._resolve_ids(c, ids, filters, [] if exclude is None else exclude, confirm)
            n = self._works(c, ids)
        if not ids:
            raise Invalid('图片不存在')
        label = f'已给 {n} 张加上标签 {tag}' if add else f'已从 {n} 张去掉标签 {tag}'
        try:
            terms_core.tag(self.path, tag, add, label, ids)
        except terms_core.Failed as exc:
            raise Invalid(str(exc)) from exc
        with self.connect() as c:
            return self.last_action(c)

    def curate(self, ids, action, *, filters=None, exclude=None, confirm=False):
        """Human collection decision per artwork: exclude, include (overrides a rule) or reset to the rules."""
        if not self.read_only:
            self.require_writable()
        if action not in ('exclude', 'include', 'reset'):
            raise Invalid('未知操作')
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            ids = self._resolve_ids(c, ids, filters, [] if exclude is None else exclude, confirm)
            before = []
            for ident in ids:
                had = c.execute('SELECT status,origin,created_at FROM curation WHERE uuid=?', (ident,)).fetchone()
                before.append({'uuid': ident, 'curation': dict(had) if had else None})
                if action == 'reset':
                    c.execute('DELETE FROM curation WHERE uuid=?', (ident,))
                else:
                    c.execute("INSERT OR REPLACE INTO curation(uuid,status,origin) VALUES (?,?,'review')", (ident, action + 'd'))
            self._refresh_excluded(c, ids)
            n = self._works(c, ids)
            label = {'exclude': f'已排除 {n} 张', 'include': f'已收录 {n} 张', 'reset': f'已将 {n} 张交回自动规则'}[action]
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)', (label, json.dumps(before)))
            return self.last_action(c)

    @staticmethod
    def fold_kinds_of(c):
        row = c.execute("SELECT value FROM prefs WHERE key='fold_kinds'").fetchone()
        return json.loads(row[0]) if row else list(FOLD_DEFAULT)

    def fold_kinds(self):
        with self.connect() as c:
            return self.fold_kinds_of(c)

    def set_fold_kinds(self, kinds):
        """Which look-alike relations fold into one picture; byte-identical copies always do."""
        self.require_writable()
        if not isinstance(kinds, list) or not all(k in SIMILAR_KINDS for k in kinds):
            raise Invalid('未知关系')
        with self.original() as src, self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('fold_kinds',?)", (json.dumps(sorted(set(kinds))),))
            self._refresh_duplicates(c, src)
            self._refresh_fold(c)
        return self.fold_kinds()

    STARTUP_DEFAULT = {'sync': True, 'similar': True, 'index_check': True, 'names': True, 'provenance': True, 'local_limit': 3000}

    def startup(self):
        """Background work to start on launch (command-line --no-* flags still win) and the local recognition cap."""
        with self.connect() as c:
            row = c.execute("SELECT value FROM prefs WHERE key='startup'").fetchone()
        return {**self.STARTUP_DEFAULT, **(json.loads(row[0]) if row else {})}

    def set_startup(self, changes):
        self.require_writable()
        if not isinstance(changes, dict) or not changes or not set(changes) <= set(self.STARTUP_DEFAULT):
            raise Invalid('未知设置')
        for key, value in changes.items():
            if key == 'local_limit':
                if not isinstance(value, int) or isinstance(value, bool) or not 100 <= value <= 100000:
                    raise Invalid('本机识别上限要在 100–100,000 张之间')
            elif not isinstance(value, bool):
                raise Invalid('开关取值不正确')
        merged = {**self.startup(), **changes}
        with self.connect() as c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('startup',?)", (json.dumps(merged),))
        return merged

    def prefs(self):
        with self.connect() as c:
            row = c.execute("SELECT value FROM prefs WHERE key='pins'").fetchone()
            pins = json.loads(row[0]) if row else []
            pins = list(dict.fromkeys(self._resolve(c, p) for p in pins))
            valid = {r[0] for r in c.execute(f"SELECT id FROM entities WHERE parent_id IS NULL AND kind<>'work' AND merged_into IS NULL AND id IN ({','.join('?' for _ in pins)})", pins)} if pins else set()
            return {'pins': [p for p in pins if p in valid]}

    def set_pins(self, pins):
        self.require_writable()
        if not isinstance(pins, list) or len(pins) > 50 or not all(isinstance(p, str) for p in pins):
            raise Invalid('置顶列表不正确')
        pins = list(dict.fromkeys(pins))
        with self.connect() as c:
            c.execute('BEGIN IMMEDIATE')
            for ident in pins:
                if not c.execute("SELECT 1 FROM entities WHERE id=? AND parent_id IS NULL AND kind<>'work' AND merged_into IS NULL", (ident,)).fetchone():
                    raise Invalid('角色不存在')
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('pins',?)", (json.dumps(pins),))
        return {'pins': pins}

    def suggest(self, ids=None, filters=None, exclude=None):
        """Machine alternatives shared by the selected images, most frequent first."""
        with self.connect() as c:
            c.execute('BEGIN')
            ids = self._resolve_ids(c, ids, filters, [] if exclude is None else exclude, True, copies=False)[:2000]
            c.execute('CREATE TEMP TABLE IF NOT EXISTS picked(uuid TEXT PRIMARY KEY)')
            c.execute('DELETE FROM picked')
            c.executemany('INSERT OR IGNORE INTO picked VALUES (?)', [(i,) for i in ids])
            rows = c.execute(f"""SELECT e.id,{NAME} AS name,e.work,count(DISTINCT v.uuid) AS count FROM {HINTS} v
                JOIN picked p ON p.uuid=v.uuid JOIN entities e ON e.id=v.entity
                WHERE NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=v.uuid AND m.entity=v.entity)
                AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND s.entity=v.entity AND s.status='rejected')
                GROUP BY e.id ORDER BY count DESC,min(v.weak),max(v.score) DESC,name LIMIT 9""").fetchall()
            return [self._display(r, c) for r in rows]

    @staticmethod
    def _oc_fields(kind, artist):
        if kind not in ('', 'oc'):
            raise Invalid('未知角色类型')
        if not isinstance(artist, str) or len(artist.strip()) > 120 or (artist.strip() and kind != 'oc'):
            raise Invalid('画师名称不正确')
        return kind, artist.strip()

    def _entity_args(self, name, kind, artist):
        """create_entity's argument checks, before any transaction (shared with the core route)."""
        kind, artist = self._oc_fields(kind, artist)
        if kind == 'oc' and (name is None or name == ''):
            name = None                      # an original character needs no name: 原创角色 N
        elif not isinstance(name, str) or not name.strip() or len(name.strip()) > 120:
            raise Invalid('角色名称应为 1–120 个字符')
        return name, kind, artist

    @staticmethod
    def _entity_named(c, name):
        """The name a new character gets (原创角色 N when none) and an existing identity it would duplicate. Reads the
        names, which stay Python's in this step (docs/TERMS_CHARACTERS_PLAN.md D4)."""
        if name is None:
            taken = {r[0] for r in c.execute("SELECT value FROM names WHERE source='user' AND value LIKE '原创角色 %'")}
            name = next(f'原创角色 {n}' for n in range(1, len(taken) + 2) if f'原创角色 {n}' not in taken)
        name = name.strip()
        row = c.execute(f"""SELECT e.id,{NAME} AS name,e.kind,e.artist FROM entities e WHERE e.parent_id IS NULL AND e.kind<>'work' AND e.merged_into IS NULL
            AND (e.id IN (SELECT entity_id FROM links WHERE system IN ('danbooru','wd') AND key=? COLLATE NOCASE)
                 OR e.id IN (SELECT entity_id FROM names WHERE value=? COLLATE NOCASE AND role='alias')
                 OR (e.id IN (SELECT entity_id FROM names WHERE value=? COLLATE NOCASE AND source='user')
                     AND NOT EXISTS(SELECT 1 FROM links l WHERE l.entity_id=e.id)))""", (name, name, name)).fetchone()
        return name, row

    @terms_route.via_core(terms_route.create_entity, terms_route.shown_created)
    def create_entity(self, name, kind='', artist=''):
        """写入统一由词条核心路由执行。"""

    def entity_id(self, entity):
        with self.connect() as c:
            return self._require_entity(c, entity)['id']

    # ---------- character profiles: Danbooru links, names, aliases, work ----------
    @staticmethod
    def _tag(tag):
        if not isinstance(tag, str) or not tag.strip() or len(tag.strip()) > 200:
            raise Invalid('请选择 Danbooru 角色 tag')
        tag = tag.strip().lower().replace(' ', '_')
        if any(x.isspace() for x in tag):
            raise Invalid('请选择 Danbooru 角色 tag')
        return tag

    @staticmethod
    def _qualifier(tag):
        import re
        m = re.search(r'_\(([^()]+)\)$', tag)
        return m.group(1) if m else ''

    @staticmethod
    def _local_work(c, *keys):
        """A work already in the library for a Danbooru copyright or a tag qualifier."""
        for key in filter(None, keys):
            for system in ('danbooru', 'qualifier'):
                row = c.execute("""SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id
                    WHERE l.system=? AND l.key=? AND e.kind='work' AND e.merged_into IS NULL""", (system, key)).fetchone()
                if row:
                    return row[0]
        return None


    def _needs_copyright(self, tag):
        """Whether linking `tag` needs a network lookup of its work (no local work matches its qualifier)."""
        with self.connect() as c:
            return not self._local_work(c, self._qualifier(tag))

    @staticmethod
    def _tag_owner(c, tag, besides=None):
        row = c.execute("""SELECT l.entity_id FROM links l JOIN entities e ON e.id=l.entity_id
            WHERE l.system IN ('danbooru','wd') AND l.key=? AND l.current=1 AND e.kind<>'work' ORDER BY l.system='wd' LIMIT 1""", (tag,)).fetchone()
        owner = Store._resolve(c, row[0]) if row else None
        return owner if owner != besides else None


    def character_hints(self, rows):
        """Danbooru search results for the picker: drop tags already in the library, add the work they would join."""
        with self.connect() as c:
            out = []
            for r in rows:
                if self._tag_owner(c, r['tag']):
                    continue
                work = self._local_work(c, self._qualifier(r['tag']))
                out.append({**r, 'name': names.readable(r['tag'], character=True),
                            'work_name': c.execute('SELECT entity_name(?)', (work,)).fetchone()[0] if work else names.readable(self._qualifier(r['tag'])) if self._qualifier(r['tag']) else ''})
            return out

    @terms_route.via_core(terms_route.create_linked, terms_route.shown_linked)
    def create_linked(self, tag, typed='', copyright_of=None):
        """写入统一由词条核心路由执行。"""

    def profile(self, entity):
        """Everything the「编辑资料」dialog shows for a character."""
        with self.connect() as c:
            row = self._require_entity(c, entity)
            if row['parent_id']:
                raise Invalid('请编辑角色本身，子标签跟随角色')
            ident = row['id']
            e = c.execute('SELECT kind,original,label_only,work_id FROM entities WHERE id=?', (ident,)).fetchone()
            shown = self._display({'id': ident}, c)
            user = c.execute("SELECT value FROM names WHERE entity_id=? AND role='display' AND source='user' ORDER BY preferred DESC LIMIT 1", (ident,)).fetchone()
            tag = c.execute("SELECT key FROM links WHERE entity_id=? AND system='danbooru' AND current=1 LIMIT 1", (ident,)).fetchone()
            return {'id': ident, 'name': shown['name'], 'user_name': user[0] if user else '',
                    'aliases': [r[0] for r in c.execute("SELECT value FROM names WHERE entity_id=? AND role='alias' AND source='user' ORDER BY position,value", (ident,))],
                    'other_aliases': c.execute("SELECT count(DISTINCT value) FROM names WHERE entity_id=? AND role='alias' AND source<>'user'", (ident,)).fetchone()[0],
                    'tag': tag[0] if tag else '', 'work_id': e['work_id'],
                    'work_name': c.execute('SELECT entity_name(?)', (e['work_id'],)).fetchone()[0] if e['work_id'] else '',
                    'oc': bool(e['original']), 'label_only': bool(e['label_only'])}




    @effects.operation('verdict')
    @terms_route.via_core(terms_route.set_profile, terms_route.profiled)
    def set_profile(self, entity, *, name=None, aliases=None, work=None, tag=None, copyright_of=None):
        """写入统一由词条核心路由执行。"""


    def local_link_suggestions(self, lookup, copyright_of=None, search=None):
        """Your own characters whose name is a Danbooru tag (a link to add, or a character already here); with `search`,
        the closest Danbooru character for the rest, marked `guess` so it is offered unticked."""
        with self.connect() as c:
            rows = c.execute(f"""SELECT e.id,{NAME} AS name,(SELECT count(*) FROM memberships m WHERE m.entity=e.id) AS count
                FROM entities e WHERE e.kind='character' AND e.parent_id IS NULL AND e.merged_into IS NULL AND e.original=0 AND e.label_only=0
                AND NOT EXISTS(SELECT 1 FROM links l WHERE l.entity_id=e.id AND l.current=1) ORDER BY count DESC""").fetchall()
            local = []
            for r in rows:
                if not r['count']:
                    continue
                guess = r['name'].strip().lower().replace(' ', '_')
                owner = self._tag_owner(c, guess, besides=r['id']) if guess else None
                local.append((dict(r), guess, owner))
        out = []
        for r, guess, owner in local:
            item = {'id': r['id'], 'name': r['name'], 'count': r['count'], 'tag': guess}
            if owner:
                with self.connect() as c:
                    target = self._display({'id': owner}, c)
                out.append({**item, 'kind': 'merge', 'target': {'id': owner, 'name': target['name']}})
            elif guess and not any(x.isspace() for x in guess) and len(guess) <= 200 and (hit := lookup(guess)):
                # Show the work it would join: a same-named character of another work is easy to spot.
                with self.connect() as c:
                    work = self._local_work(c, self._qualifier(guess))
                    work_name = c.execute('SELECT entity_name(?)', (work,)).fetchone()[0] if work else ''
                if not work_name and copyright_of:
                    copyright = copyright_of(guess)
                    with self.connect() as c:
                        work = self._local_work(c, copyright)
                        work_name = c.execute('SELECT entity_name(?)', (work,)).fetchone()[0] if work else names.readable(copyright) if copyright else ''
                out.append({**item, 'kind': 'link', 'posts': hit['posts'], 'work_name': work_name})
            elif search and (hit := self._closest(r['name'], search)):
                with self.connect() as c:
                    owner = self._tag_owner(c, hit['tag'], besides=r['id'])
                    target = self._display({'id': owner}, c) if owner else None
                if owner:
                    out.append({**item, 'kind': 'merge', 'guess': True, 'tag': hit['tag'], 'target': {'id': owner, 'name': target['name']}})
                else:
                    out.append({**item, 'kind': 'link', 'guess': True, 'tag': hit['tag'], 'posts': hit['posts'], 'work_name': self.character_hints([hit])[0]['work_name']})
        return out

    def _closest(self, name, search):
        """The Danbooru character a loosely typed name most likely means (maybe one the library already has), or None."""
        import re
        qualifier = self._qualifier(name.strip().lower().replace(' ', '_'))
        base = re.sub(r'[\s_]*\([^()]*\)$', '', name.strip()).replace('_', ' ').strip()
        if not base:
            return None
        found = search(base)
        if qualifier:
            # 'Vepley_(girls'_frontline)' means vepley_(girls'_frontline_2) over a same-named character elsewhere.
            words = [w for w in re.split(r"[_\s:']+", qualifier) if len(w) > 2]
            found = [r for r in found if all(w in r['tag'] for w in words)] or found
        # The character itself over its outfits: sharkry_(girls'_frontline_2) before sharkry_(swimsuit_idol)_(…).
        found = [r for r in found if r['tag'].count('(') <= 1] or found
        return found[0] if found else None

    @effects.operation('verdict')
    @terms_route.via_core(terms_route.link_local, terms_route.linked)
    def link_local(self, items, copyright_of=None):
        """写入统一由词条核心路由执行。"""

    def _link_args(self, items):
        """link_local's argument checks: (id, normalized tag) per item, before any transaction."""
        if not isinstance(items, list) or not 1 <= len(items) <= 200 or any(not isinstance(x, dict) for x in items):
            raise Invalid('请选择要关联的角色')
        return [(x.get('id'), self._tag(x.get('tag'))) for x in items]

    SHARED_OC = '原创角色（未细分）'

    @terms_route.via_core(terms_route.shared_oc, terms_route.shown_shared)
    def shared_oc(self):
        """写入统一由词条核心路由执行。"""

    @terms_route.via_core(terms_route.set_kind, terms_route.returned)
    def set_kind(self, entity, kind, artist=''):
        """写入统一由词条核心路由执行。"""

    def tagged_identities(self, nsfw=''):
        """Identities set to「当作标签」, with how many visible images carry them (for the 角色 page footer)."""
        with self.connect() as c:
            return [dict(r) for r in c.execute(f"""SELECT e.id,{NAME} AS name,
                (SELECT count(*) FROM tag_memberships m WHERE m.entity=e.id AND {VISIBLE.format('m')}{rating('m', nsfw)}) AS count
                FROM entities e WHERE e.mascot=1 AND e.merged_into IS NULL ORDER BY count DESC,name""")]

    @staticmethod
    def _mascot_arg(mascot):
        if not isinstance(mascot, bool):
            raise Invalid('参数不正确')

    @terms_route.via_core(terms_route.set_mascot, terms_route.returned)
    def set_mascot(self, entity, mascot):
        """写入统一由词条核心路由执行。"""

    def artists(self):
        """Artists named on OCs so far, most characters first (for autocompletion)."""
        with self.connect() as c:
            return [dict(r) for r in c.execute("""SELECT artist,count(*) AS characters FROM entities
                WHERE original=1 AND artist<>'' AND merged_into IS NULL GROUP BY artist ORDER BY characters DESC,artist""")]
