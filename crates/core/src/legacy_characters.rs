//! 角色、皮肤、作品的旧兼容表在核心第 8 版里的去留（#201）。
//!
//! - 身份、链接、作品复核、身份基线：旧表 `entities`、`links`、`work_review`、`identity_baseline` 在升级时逐行核对后
//!   删除（`retire_*`），之后只在 `terms`、`term_tree`、`term_links`、`term_identity_baseline` 里。
//! - 还没切的兼容表 `assertions`、`candidates`、`identity_relations`、`identity_evidence`、`identity_decisions`、
//!   `identity_decision_evidence` 由核心照 `term_*` 维护（`COMPAT`、`refresh_candidates`、`project_relation`）；升级时
//!   由核心主数据生成 TEMP 影子与旧表逐表比对（`verify_in`），不一致就拒绝升级。
//! - 写保护名单 `GUARDED`、第 4 版删掉的冻结副本 `FROZEN_COPIES`。
//!
//! 原来的收编同步、切换与核对命令（`sync-characters`、`cutover-characters`、`verify-characters`）随第 4 步删除：第 8 版
//! 拒绝没切换完的图库，它们再也遇不到真实数据；新建图库见 `new_library`。

use crate::schema::guard;
use crate::strict_json::sha256_hex;
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const ORIGIN: &str = "legacy:characters";
/// 角色在核心里的标记（新建图库由 `new_library` 写）。
pub const CHARACTERS_IN_CORE: &str = "terms_characters";
pub const SHADOW: &str = "shadow_";
/// 报告里每类问题最多带几条样例。
const SAMPLES: usize = 5;

/// 一张兼容表：旧表名、列、由核心生成这些列的 SELECT。
struct Compat {
    table: &'static str,
    columns: &'static [&'static str],
    select: &'static str,
    /// 只有身份层装过（有 identity_relations）时才有。
    identity: bool,
}

/// 核心第 8 版迁移（#201）在它自己的事务里做的同一个核对：旧兼容表与由核心主数据生成的样子逐表一致才升级。
/// 只建连接内的 TEMP 影子，结束即清掉。
pub(crate) fn verify_in(tx: &Connection) -> Result<Report> {
    let identity = table_exists(tx, "identity_relations")?;
    let mut report = Report::default();
    shadow(tx, identity)?;
    compare(tx, identity, &mut report)?;
    clear_shadow(tx)?;
    Ok(report)
}

/// Shared read mapping for S3 commands/full rebuild. The whitelist is the S2 contract.
/// 策略未设置时，候选的身份沿破坏式合并解析到代表（`_candidate` 与 `merge` 都这样写旧表）；hybrid 下是原始身份。
const CANDIDATES_RESOLVED: &str = "WITH RECURSIVE chain(term,key,depth) AS (
   SELECT r.term_id,t.key,0 FROM term_tree r JOIN terms t ON t.id=r.term_id
   UNION ALL SELECT c.term,r2.merged_into,c.depth+1 FROM chain c JOIN terms t2 ON t2.key=c.key
    JOIN term_tree r2 ON r2.term_id=t2.id WHERE r2.merged_into IS NOT NULL AND r2.merged_into<>'' AND c.depth<16),
  resolved(term,key) AS (SELECT term,key FROM chain c WHERE depth=(SELECT max(depth) FROM chain x WHERE x.term=c.term))
 SELECT a.key,k.key,CASE e.source WHEN 'wd' THEN 'WD' ELSE 'CCIP' END,max(e.score)
 FROM term_active_evidence active
 JOIN term_evidence_set_rows current_row ON current_row.set_id=active.set_id
 JOIN term_evidence e ON e.id=current_row.evidence_id AND e.asset_id=active.asset_id
   AND e.source=active.source AND active.method IN (e.method,'*')
 JOIN term_assets a ON a.id=e.asset_id AND a.namespace='library'
 JOIN resolved k ON k.term=e.term_id
 WHERE e.source||'.'||e.method IN ('wd.wd_character','ccip_index.char_ext')
 GROUP BY a.key,k.key,e.source";

fn select_for(c: &Connection, table: &str, select: &'static str) -> Result<&'static str> {
    Ok(if table == "candidates" && !crate::character_projection::policy(c)? { CANDIDATES_RESOLVED } else { select })
}

const CANDIDATE_SCOPE: (&str, &str) = (
    "FROM term_active_evidence active",
    "FROM candidate_scope scoped CROSS JOIN term_assets scoped_asset
               ON scoped_asset.namespace='library' AND scoped_asset.key=scoped.uuid
             CROSS JOIN term_active_evidence active ON active.asset_id=scoped_asset.id",
);

/// 核心维护兼容表时，按图重算兼容 `candidates`（= 由当前有效证据全量算出的那几行）。
pub(crate) fn refresh_candidates(c: &Connection, uuids: &[String]) -> Result<()> {
    c.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS candidate_scope(uuid TEXT PRIMARY KEY); DELETE FROM candidate_scope;",
    )?;
    let mut put = c.prepare_cached("INSERT OR IGNORE INTO candidate_scope VALUES (?)")?;
    for u in uuids {
        put.execute([u])?;
    }
    c.execute("DELETE FROM candidates WHERE uuid IN (SELECT uuid FROM candidate_scope)", [])?;
    let select = select_for(c, "candidates", compat_select("candidates")?)?;
    // 小范围由图片驱动；原计划会先扫描全库证据并分组，最后才过滤图片。
    // 全量 refresh 保留原计划，避免为大集合增加逐图索引探测。
    let select = if uuids.len() <= 4096 {
        crate::character_projection::scoped(select, CANDIDATE_SCOPE.0, CANDIDATE_SCOPE.1)?
    } else {
        select.to_owned()
    };
    c.execute(
        &format!(
            "WITH rebuilt(uuid,entity,source,score) AS ({select})
             INSERT INTO candidates(uuid,entity,source,score) SELECT * FROM rebuilt WHERE uuid IN (SELECT uuid FROM candidate_scope)"
        ),
        [],
    )?;
    Ok(())
}

/// 核心第 8 版（#201）：`term_links` 成为角色、皮肤、作品链接的唯一权威（`character_links`）。旧表 `links` 还在时
/// （升级前的库），每条改链接的命令都同步镜像过（`mirror_state`），所以按原来的兼容表映射逐行核对，与角色的核对
/// 一样不一致就拒绝升级；一致就删掉旧表。旧表不在时什么都不做。
pub(crate) fn retire_links(c: &Connection) -> Result<()> {
    if !table_exists(c, "links")? {
        return Ok(());
    }
    let columns = "system, key, entity_id, current, source, added_at";
    let (old, new) =
        (format!("SELECT {columns} FROM links"), format!("SELECT {columns} FROM {}", crate::character_links::LINKS));
    let differs = differ(c, &old, &new)?;
    if differs > 0 {
        return Err(Error::Schema(format!("旧链接表与词条核心不一致，没有升级：{differs} 行")));
    }
    c.execute_batch("DROP TABLE links")?;
    Ok(())
}

/// 核心第 8 版：作品复核只在 `term_tree` 的复核三列里（`character_review`）。旧表 `work_review` 还在时，每条改它的
/// 命令都同步镜像过，所以逐行核对，不一致就拒绝升级；一致就删掉旧表。
pub(crate) fn retire_work_review(c: &Connection) -> Result<()> {
    if !table_exists(c, "work_review")? {
        return Ok(());
    }
    let old = "SELECT entity_id, status, note, parent_locked FROM work_review".to_owned();
    let new = format!("SELECT entity_id, status, note, parent_locked FROM {}", crate::character_review::REVIEWS);
    let differs = differ(c, &old, &new)?;
    if differs > 0 {
        return Err(Error::Schema(format!("旧作品复核表与词条核心不一致，没有升级：{differs} 行")));
    }
    c.execute_batch("DROP TABLE work_review")?;
    Ok(())
}

/// 核心第 8 版：身份基线只在 `term_identity_baseline` 里（`seq` 是原来的 rowid）。旧表 `identity_baseline` 是给旧代码
/// 追加的副本，逐行核对（连 rowid 一起），不一致就拒绝升级；一致就删掉旧表。
pub(crate) fn retire_identity_baseline(c: &Connection) -> Result<()> {
    if !table_exists(c, "identity_baseline")? {
        return Ok(());
    }
    let old = "SELECT rowid, id, parent_id, variant, kind, merged_into FROM identity_baseline";
    let new = "SELECT seq, id, parent_id, variant, kind, merged_into FROM term_identity_baseline";
    let differs = differ(c, old, new)?;
    if differs > 0 {
        return Err(Error::Schema(format!("旧身份基线表与词条核心不一致，没有升级：{differs} 行")));
    }
    c.execute_batch("DROP TABLE identity_baseline")?;
    Ok(())
}

/// `term_tree` 在核心第 8 版加的列：身份的属性（原来在 `terms.attrs`，默认值与旧表 `entities` 相同）和先后 `seq`。
const TREE_COLUMNS: [(&str, &str); 7] = [
    ("variant", "TEXT NOT NULL DEFAULT ''"),
    ("artist", "TEXT NOT NULL DEFAULT ''"),
    ("label_only", "INTEGER NOT NULL DEFAULT 0"),
    ("mascot", "INTEGER NOT NULL DEFAULT 0"),
    ("mascot_origin", "TEXT"),
    ("original", "INTEGER NOT NULL DEFAULT 0"),
    ("seq", "INTEGER"),
];

/// 旧表 `entities` 与 `character_entities::ENTITIES` 共有的列，最后一列是先后（旧表的 rowid、核心的 `seq`）。核心从不
/// 承载的 `name`、`external_key`、`tag` 不在内：没有读者，随表删除。
const ENTITY_COLUMNS: &str =
    "id, kind, work, variant, parent_id, merged_into, artist, label_only, mascot, mascot_origin, work_id, original";

/// 核心第 8 版（#201）：身份只在 `terms`（key、kind）和 `term_tree` 里（`character_entities`）。
/// - `term_tree` 加属性列和先后 `seq`（`TREE_COLUMNS`），属性从 `terms.attrs` 填入，这些词条的 `attrs` 清空
///   （用户 2026-10-20 决定）；
/// - 旧表 `entities` 还在时：核心里还没有身份（新建的库）就照旧表建一次；`seq` 取旧表的 rowid；然后逐行核对，不一致就
///   拒绝升级（第 7 版里每条改身份的命令都同步镜像过）。引用它的视图改按核心表重写（列逐一对应），
///   `identity_relations` 的外键改为 `REFERENCES terms(key)` 重建，最后删表。
///
/// 放在别的旧表退役之后：外键开着，引用 `entities` 的旧表（`links`、`names`、`work_review`）都删掉了才能删它。
pub(crate) fn retire_entities(c: &Connection) -> Result<()> {
    // `term_tree` 和 `identity_relations` 有写保护；迁移结束时 `schema::migrate_in` 收回钥匙。
    c.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'1')", [crate::schema::WRITER_KEY])?;
    let have = columns(c, "term_tree")?;
    for (column, definition) in TREE_COLUMNS {
        if !have.iter().any(|h| h == column) {
            c.execute_batch(&format!("ALTER TABLE term_tree ADD COLUMN {column} {definition}"))?;
        }
    }
    c.execute_batch(
        "UPDATE term_tree SET
           variant = coalesce(json_extract(t.attrs, '$.variant'), ''),
           artist = coalesce(json_extract(t.attrs, '$.artist'), ''),
           label_only = coalesce(json_extract(t.attrs, '$.label_only'), 0),
           mascot = coalesce(json_extract(t.attrs, '$.mascot'), 0),
           mascot_origin = json_extract(t.attrs, '$.mascot_origin'),
           original = coalesce(json_extract(t.attrs, '$.original'), 0)
         FROM terms t
         WHERE t.id = term_tree.term_id AND t.attrs <> '{}';
         UPDATE terms SET attrs = '{}' WHERE attrs <> '{}' AND id IN (SELECT term_id FROM term_tree);",
    )?;
    if table_exists(c, "entities")? {
        let empty = |table: &str| -> Result<bool> {
            Ok(c.query_row(&format!("SELECT NOT EXISTS(SELECT 1 FROM {table})"), [], |r| r.get(0))?)
        };
        // 空的旧表可能只建了一部分列（应用层建到一半的库）：核心里也没有身份就算一致。
        let (old_empty, core_empty) = (empty("entities")?, empty("term_tree")?);
        if old_empty && !core_empty {
            let differs: i64 = c.query_row("SELECT count(*) FROM term_tree", [], |r| r.get(0))?;
            return Err(Error::Schema(format!("旧身份表与词条核心不一致，没有升级：{differs} 行")));
        }
        if !old_empty && core_empty {
            c.execute(
                "INSERT INTO terms(key, kind, origin) SELECT id, kind, ?1 FROM entities ORDER BY rowid",
                [ORIGIN],
            )?;
            c.execute_batch(
                "INSERT INTO term_tree(term_id, parent_id, merged_into, work_id, work_text, variant, artist, label_only,
                   mascot, mascot_origin, original)
                 SELECT t.id, e.parent_id, e.merged_into, e.work_id, e.work, e.variant, e.artist, e.label_only, e.mascot,
                   e.mascot_origin, e.original
                 FROM entities e
                 JOIN terms t ON t.key = e.id
                 ORDER BY e.rowid",
            )?;
        }
        if !old_empty {
            c.execute_batch(
                "UPDATE term_tree SET seq = e.rowid
                 FROM entities e
                 JOIN terms t ON t.key = e.id
                 WHERE t.id = term_tree.term_id",
            )?;
            let old = format!("SELECT {ENTITY_COLUMNS}, rowid FROM entities");
            let new = format!("SELECT {ENTITY_COLUMNS}, seq FROM {}", crate::character_entities::ENTITIES);
            let differs = differ(c, &old, &new)?;
            if differs > 0 {
                return Err(Error::Schema(format!("旧身份表与词条核心不一致，没有升级：{differs} 行")));
            }
        }
        rewrite_views(c)?;
        if table_exists(c, "identity_relations")? {
            rebuild_relations(c)?;
        }
        c.execute_batch("DROP TABLE entities; DROP TABLE IF EXISTS shadow_entities;")?;
    }
    // 没有旧表的库（只有核心的库）：按词条 id 的先后。
    let last: i64 = c.query_row("SELECT coalesce(max(seq), 0) FROM term_tree", [], |r| r.get(0))?;
    c.execute("UPDATE term_tree SET seq = ?1 + term_id WHERE seq IS NULL", [last])?;
    c.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS term_tree_seq ON term_tree(seq)")?;
    Ok(())
}

/// 引用 `entities` 的视图（`family_confirmed`、`membership_rules`、`variant_memberships`、`ambiguous_claims`、
/// `identity_nodes`）改按核心表重写：旧表名换成同形的 `character_entities::ENTITIES`，其余照旧。
fn rewrite_views(c: &Connection) -> Result<()> {
    let word = regex::Regex::new(r"\bentities\b").expect("pattern");
    let views: Vec<(String, String)> = c
        .prepare(
            "SELECT name, sql FROM main.sqlite_master WHERE type = 'view' AND sql LIKE '%entities%' ORDER BY rowid",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (name, sql) in views {
        if !word.is_match(&sql) {
            continue;
        }
        let sql = word.replace_all(&sql, regex::NoExpand(crate::character_entities::ENTITIES));
        c.execute_batch(&format!("DROP VIEW \"{name}\"; {sql};"))?;
    }
    Ok(())
}

/// `identity_relations` 的两端改为 `REFERENCES terms(key)`（`terms.key` 唯一）：行先存进临时表，照原来的建表语句（只换
/// 外键）重建，搬回原样的行，索引和触发器照原样重建。外键开着，删表时引用它的关系证据、决议暂时悬空，所以这段推迟外键
/// 检查，行搬回来就恢复一致。
fn rebuild_relations(c: &Connection) -> Result<()> {
    let sql: String = c.query_row(
        "SELECT sql FROM main.sqlite_master WHERE type = 'table' AND name = 'identity_relations'",
        [],
        |r| r.get(0),
    )?;
    let open = sql.find('(').ok_or_else(|| Error::Schema("看不懂 identity_relations 的建表语句".into()))?;
    let body = sql[open..].replace("REFERENCES entities(id)", "REFERENCES terms(key)");
    let depend: Vec<String> = c
        .prepare(
            "SELECT sql FROM main.sqlite_master
             WHERE tbl_name = 'identity_relations' AND type IN ('index', 'trigger') AND sql IS NOT NULL
             ORDER BY rowid",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let deferred: bool = c.pragma_query_value(None, "defer_foreign_keys", |r| r.get(0))?;
    c.pragma_update(None, "defer_foreign_keys", true)?;
    let result = (|| -> Result<()> {
        c.execute_batch(&format!(
            "CREATE TEMP TABLE relations_v7 AS SELECT * FROM main.identity_relations;
             DROP TABLE main.identity_relations;
             CREATE TABLE main.identity_relations {body};
             INSERT INTO main.identity_relations SELECT * FROM temp.relations_v7;
             DROP TABLE temp.relations_v7;"
        ))?;
        for definition in depend {
            c.execute_batch(&definition)?;
        }
        Ok(())
    })();
    c.pragma_update(None, "defer_foreign_keys", deferred)?;
    result
}

/// 两个查询的结果双向相差的行数。
fn differ(c: &Connection, old: &str, new: &str) -> Result<i64> {
    let count =
        |sql: String| -> Result<i64> { Ok(c.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |r| r.get(0))?) };
    Ok(count(format!("{old} EXCEPT {new}"))? + count(format!("{new} EXCEPT {old}"))?)
}

pub(crate) fn compat_select(table: &str) -> Result<&'static str> {
    COMPAT
        .iter()
        .find(|item| item.table == table)
        .map(|item| item.select)
        .ok_or_else(|| Error::Invalid(format!("未知角色兼容表：{table}")))
}

/// Rebuild only the changed relation's compatibility rows. Rebuilding all four
/// tables per accepted edge would introduce quadratic writes on top of the old
/// algorithm's required per-edge graph validation.
pub(crate) fn project_relation(c: &Connection, id: i64) -> Result<()> {
    let selected = [
        ("identity_relations", "id=?1"),
        ("identity_evidence", "relation_id=?1"),
        ("identity_decisions", "relation_id=?1"),
        ("identity_decision_evidence", "decision_id IN (SELECT id FROM identity_decisions WHERE relation_id=?1)"),
    ];
    for (table, filter) in selected.iter().rev() {
        c.execute(&format!("DELETE FROM {table} WHERE {filter}"), [id])?;
    }
    for (table, filter) in selected {
        let definition = COMPAT.iter().find(|d| d.table == table).expect("compatibility table");
        let columns = definition.columns.join(",");
        c.execute(&format!("WITH rebuilt({columns}) AS ({}) INSERT INTO {table}({columns}) SELECT {columns} FROM rebuilt WHERE {filter}", definition.select), [id])?;
    }
    Ok(())
}

/// 生成顺序即插入顺序（被引用的在前），删除时倒过来。
const COMPAT: &[Compat] = &[
    Compat {
        table: "assertions",
        columns: &["uuid", "entity", "status", "origin"],
        select: "SELECT a.key,t.key,CASE v.verdict WHEN 'yes' THEN 'confirmed' ELSE 'rejected' END,v.via
         FROM term_effective_verdicts v JOIN term_assets a ON a.id=v.asset_id AND a.namespace='library'
         JOIN term_tree r ON r.term_id=v.term_id JOIN terms t ON t.id=v.term_id",
        identity: false,
    },
    Compat {
        table: "candidates",
        columns: &["uuid", "entity", "source", "score"],
        select: "SELECT a.key,t.key,CASE e.source WHEN 'wd' THEN 'WD' ELSE 'CCIP' END,max(e.score)
         FROM term_active_evidence active
 JOIN term_evidence_set_rows current_row ON current_row.set_id=active.set_id
 JOIN term_evidence e ON e.id=current_row.evidence_id AND e.asset_id=active.asset_id
   AND e.source=active.source AND active.method IN (e.method,'*')
 JOIN term_assets a ON a.id=e.asset_id AND a.namespace='library'
         JOIN term_tree r ON r.term_id=e.term_id JOIN terms t ON t.id=e.term_id
         WHERE e.source||'.'||e.method IN ('wd.wd_character','ccip_index.char_ext')
         GROUP BY a.key,t.key,e.source",
        identity: false,
    },
    Compat {
        table: "identity_relations",
        columns: &["id", "a", "b", "kind", "status"],
        select: "SELECT m.legacy_id,
          CASE r.kind WHEN 'contains' THEN tb.key WHEN 'different_from' THEN min(ta.key,tb.key) ELSE ta.key END,
          CASE r.kind WHEN 'contains' THEN ta.key WHEN 'different_from' THEN max(ta.key,tb.key) ELSE tb.key END,
          CASE r.kind WHEN 'same_as' THEN 'duplicate_of' WHEN 'contains' THEN 'form_of' ELSE 'different_from' END,
          CASE l.verdict WHEN 'yes' THEN 'accepted' WHEN 'no' THEN 'rejected' ELSE 'proposed' END
         FROM term_legacy_ids m JOIN term_relations r ON r.id=m.core_id
         JOIN terms ta ON ta.id=r.a JOIN terms tb ON tb.id=r.b
         LEFT JOIN term_relation_latest l ON l.relation_id=r.id WHERE m.kind='relation'",
        identity: true,
    },
    Compat {
        table: "identity_evidence",
        columns: &["relation_id", "fingerprint", "source", "payload", "created_at"],
        select: "SELECT m.legacy_id,json_extract(e.data,'$.legacy_fingerprint'),e.source,json_extract(e.data,'$.payload'),
          json_extract(e.data,'$.created_at')
         FROM term_evidence e JOIN term_legacy_ids m ON m.kind='relation' AND m.core_id=e.relation_id
         WHERE e.method='identity_evidence'",
        identity: true,
    },
    Compat {
        table: "identity_decisions",
        columns: &["id", "relation_id", "previous_status", "status", "reason", "created_at"],
        select: "SELECT d.legacy_id,m.legacy_id,
          coalesce(lag(s.status) OVER (PARTITION BY v.relation_id ORDER BY v.id),'proposed'),s.status,v.reason,v.created_at
         FROM term_legacy_ids d JOIN term_verdicts v ON v.id=d.core_id
         JOIN term_legacy_ids m ON m.kind='relation' AND m.core_id=v.relation_id
         JOIN (SELECT 'yes' AS verdict,'accepted' AS status UNION ALL SELECT 'no','rejected' UNION ALL SELECT 'clear','proposed') s
          ON s.verdict=v.verdict
         WHERE d.kind='decision'",
        identity: true,
    },
    Compat {
        table: "identity_decision_evidence",
        columns: &["decision_id", "fingerprint"],
        select: "SELECT d.legacy_id,json_extract(e.data,'$.legacy_fingerprint')
         FROM term_legacy_ids d JOIN term_verdicts v ON v.id=d.core_id, json_each(v.reviewed) j
         JOIN term_evidence e ON e.id=j.value WHERE d.kind='decision'",
        identity: true,
    },
];

/// 升级时的核对：每张兼容表的旧表行数、影子行数、旧表有而影子没有、影子有而旧表没有（含样例）。
#[derive(Debug, Default)]
pub struct Report {
    pub tables: BTreeMap<String, Value>,
}

impl Report {
    pub fn clean(&self) -> bool {
        self.tables.values().all(|t| t["missing"] == 0 && t["extra"] == 0)
    }

    pub fn to_json(&self) -> Value {
        json!({ "tables": self.tables })
    }
}

/// 切换后由核心写、Python 只读的表（写保护，见 schema::guard）：兼容表（含身份基线）、归属派生表、角色判定器的私有状态、
/// 学习产物，以及相册导入的 `imported`。`deferred`、`asked`、`reviews` 由核心命令一起写，但 Python 撤销自己的步骤
/// （旧的零成员 sweep、curate、identity_skip）时照旧要删它们的行；`duplicates`、`excluded`、`auto_rules` 仍有
/// Python 的写入口（curate、相似分组）。这些都不在里面。
pub const GUARDED: [&str; 22] = [
    "assertions",
    "candidates",
    "identity_relations",
    "identity_evidence",
    "identity_decisions",
    "identity_decision_evidence",
    "memberships",
    "tag_memberships",
    "family_evidence",
    "single_assets",
    "lead_assets",
    "identity_canonical",
    "identity_scopes",
    "identity_suspended",
    "identity_uncertain",
    "identity_anchor_decisions",
    "identity_anchor_history",
    "prototypes",
    "proto_claims",
    "weak_claims",
    "learn_state",
    "imported",
];

/// 第 4 版之前切换时留下的冻结副本（schema::v4 删掉它们；之后的切换不再生成）。
pub const FROZEN_COPIES: [&str; 6] = [
    "legacy_assertions",
    "legacy_identity_relations",
    "legacy_identity_evidence",
    "legacy_identity_decisions",
    "legacy_identity_decision_evidence",
    "legacy_identity_baseline",
];

pub fn characters_in_core(conn: &Connection) -> Result<bool> {
    flag(conn, CHARACTERS_IN_CORE)
}

fn flag(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |x| x.get::<_, String>(0))
        .optional()?
        .is_some_and(|v| v == "core"))
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type='table' AND name=?1)",
        [name],
        |x| x.get(0),
    )?)
}

fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    Ok(conn
        .prepare(&format!("SELECT name FROM pragma_table_info('{table}') ORDER BY cid"))?
        .query_map([], |x| x.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// 查询的前几行，给报告当样例。
fn samples(conn: &Connection, sql: &str) -> Result<Vec<Value>> {
    let mut stmt = conn.prepare(&format!("SELECT * FROM ({sql}) LIMIT {SAMPLES}"))?;
    let n = stmt.column_count();
    let rows = stmt
        .query_map([], |x| {
            (0..n)
                .map(|i| {
                    Ok(match x.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(v) => json!(v),
                        rusqlite::types::ValueRef::Real(v) => json!(v),
                        rusqlite::types::ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        rusqlite::types::ValueRef::Blob(b) => json!(format!("<{n} 字节>", n = b.len())),
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
                .map(Value::Array)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

pub(crate) fn fingerprint16(legacy: &str) -> Vec<u8> {
    let hex = if legacy.len() == 64 && legacy.bytes().all(|b| b.is_ascii_hexdigit()) {
        legacy.to_ascii_lowercase()
    } else {
        sha256_hex(legacy.as_bytes())
    };
    (0..16).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("十六进制")).collect()
}

// ---- 影子兼容表与比对 ----

/// 旧表的建表语句改成影子表：换表名，去掉外键（旧数据的外键 Python 不强制，悬空引用在比对里报出来）。
pub(crate) fn shadow_ddl(sql: &str, table: &str) -> Result<String> {
    let open = sql.find('(').ok_or_else(|| Error::Schema(format!("看不懂 {table} 的建表语句")))?;
    let mut body = sql[open..].to_owned();
    while let Some(at) = body.find("REFERENCES") {
        // REFERENCES 名字(列)
        let rest = &body[at..];
        let close = rest.find(')').ok_or_else(|| Error::Schema(format!("看不懂 {table} 的外键")))?;
        body.replace_range(at..at + close + 1, "");
    }
    Ok(format!("CREATE TABLE {SHADOW}{table}{body}"))
}

/// 已完成比对的影子没有运行时读者，不把校验副本永久留在图库里。
pub(crate) fn clear_shadow(c: &Connection) -> Result<()> {
    for definition in COMPAT.iter().rev() {
        c.execute_batch(&format!("DROP TABLE IF EXISTS {SHADOW}{}", definition.table))?;
    }
    Ok(())
}

/// 由核心主数据生成连接内的 TEMP 影子兼容表（建表语句照旧表，去掉外键），只读不写主文件。
fn shadow(tx: &Connection, identity: bool) -> Result<()> {
    for c in COMPAT.iter().rev() {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {SHADOW}{}", c.table))?;
    }
    for c in COMPAT.iter().filter(|c| !c.identity || identity) {
        let sql: String =
            tx.query_row("SELECT sql FROM main.sqlite_master WHERE type='table' AND name=?1", [c.table], |x| x.get(0))?;
        let ddl = shadow_ddl(&sql, c.table)?.replacen("CREATE TABLE", "CREATE TEMP TABLE", 1);
        tx.execute_batch(&ddl)?;
        let select = select_for(tx, c.table, c.select)?;
        tx.execute(&format!("INSERT INTO {SHADOW}{}({}) {}", c.table, c.columns.join(","), select), [])?;
        guard(tx, &format!("{SHADOW}{}", c.table))?;
    }
    Ok(())
}

fn compare(tx: &Connection, identity: bool, r: &mut Report) -> Result<()> {
    let mut pairs: Vec<(String, String, String)> = COMPAT
        .iter()
        .filter(|c| !c.identity || identity)
        .map(|c| {
            let cols = c.columns.join(",");
            (
                c.table.to_owned(),
                format!("SELECT {cols} FROM main.{}", c.table),
                format!("SELECT {cols} FROM {SHADOW}{}", c.table),
            )
        })
        .collect();
    if table_exists(tx, "identity_baseline")? {
        pairs.push((
            "identity_baseline".into(),
            "SELECT rowid,id,parent_id,variant,kind,merged_into FROM identity_baseline".into(),
            "SELECT seq,id,parent_id,variant,kind,merged_into FROM term_identity_baseline".into(),
        ));
    }
    for (table, old, new) in pairs {
        let count = |sql: &str| -> Result<i64> {
            Ok(tx.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |x| x.get(0))?)
        };
        let (missing_sql, extra_sql) = (format!("{old} EXCEPT {new}"), format!("{new} EXCEPT {old}"));
        let (missing, extra) = (count(&missing_sql)?, count(&extra_sql)?);
        let mut entry = json!({"old": count(&old)?, "shadow": count(&new)?, "missing": missing, "extra": extra});
        if missing > 0 {
            entry["missing_samples"] = json!(samples(tx, &missing_sql)?);
        }
        if extra > 0 {
            entry["extra_samples"] = json!(samples(tx, &extra_sql)?);
        }
        r.tables.insert(table, entry);
    }
    Ok(())
}

#[cfg(test)]
#[path = "legacy_characters/tests.rs"]
mod tests;
