//! 角色、皮肤、作品收编：影子同步（docs/TERMS_CHARACTERS_PLAN.md 第 12 节 S2）。
//!
//! 旧表仍是写入方，这里只读它们，把核心的 `term_*` 补到与旧表一致，再由 `term_*` 生成影子兼容表
//! （`shadow_entities` 等，建表语句从旧表原样复制），与旧表逐行逐列比对。一个事务：预检失败、出处无法解释或
//! 比对有差异，整次回滚，只带回报告。可以反复运行；旧表没变时再跑一次不新增行。
//!
//! - `entities` → `terms`（key = 原 id，kind 原样，`variant`、`artist`、`label_only`、`mascot`、`mascot_origin`、
//!   `original` 进 attrs）+ `term_tree`（`parent_id`、`merged_into`、`work_id`、`work` 文字、work_review 三列）。
//!   有 `term_tree` 行的词条就是兼容 `entities` 里的一行。
//! - `links` → `term_links`，角色的 `wd` 改名 `wd_character`（WD 题材占了 `wd`）。
//! - `identity_baseline` → `term_identity_baseline`（`seq` = 原 rowid）。
//! - `candidates` → 证据，整表作为一个迁移批次（全库覆盖）：WD → `wd.wd_character`，CCIP → `ccip_index.char_ext`。
//! - `assertions` → 裁决，按 `actions` 历史逐目标重建（方案第 6.2 节）：快照项挂原来那一步，破坏式合并挂合并那一步，
//!   没有 action 的值进非用户批次（相册 / 基线），排在看到它的那一步之前；解释不了的计数、报样例，不归给最近的步骤。
//! - `identity_relations` / `identity_evidence` / `identity_decisions` / `identity_decision_evidence` → 关系、关系证据、
//!   关系裁决（有 action 的挂 action，其余进迁移或撤销审计批次），旧 id 记进 `term_legacy_ids`。

use crate::legacy_artists::LIBRARY;
use crate::schema::{begin_writing, commit_writing, guard, RelationKind};
use crate::store::{add_evidence, rebuild_active_evidence, relation, AssetId, Claim, NewEvidence, TermId};
use crate::strict_json::{canonical, sha256_hex};
use crate::{Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};

pub const ORIGIN: &str = "legacy:characters";
/// 角色切到核心后 `meta` 里的标记（S4 写）；之后旧表不再同步。
pub const CHARACTERS_IN_CORE: &str = "terms_characters";
pub const SHADOW: &str = "shadow_";
/// 旧关系证据收编成的核心证据的方法名。
const RELATION_METHOD: &str = "identity_evidence";
/// 报告里每类问题最多带几条样例。
const SAMPLES: usize = 5;

/// 一张兼容表：旧表名、列（旧表必须恰好是这些列）、由核心生成这些列的 SELECT。
struct Compat {
    table: &'static str,
    columns: &'static [&'static str],
    select: &'static str,
    /// 只有身份层装过（有 identity_relations）时才有。
    identity: bool,
}

const ENTITY_COLUMNS: &[&str] = &[
    "id",
    "name",
    "work",
    "external_key",
    "variant",
    "parent_id",
    "tag",
    "merged_into",
    "kind",
    "artist",
    "label_only",
    "mascot",
    "mascot_origin",
    "work_id",
    "original",
];

/// S3: rebuild the shadow exclusively from core master data, without resynchronizing
/// any legacy input. A difference rolls back the check, just like S2 synchronization.
pub fn verify(conn: &mut Connection) -> Result<Report> {
    crate::schema::require_current(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    begin_writing(&tx)?;
    let identity = table_exists(&tx, "identity_relations")?;
    let mut report = Report::default();
    shadow(&tx, identity, true)?;
    compare_with_baseline(&tx, identity, &mut report, true)?;
    if report.clean() {
        clear_shadow(&tx)?;
        commit_writing(tx)?;
        report.committed = true;
    }
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
        select.replace(
            "FROM term_active_evidence active",
            "FROM candidate_scope scoped CROSS JOIN term_assets scoped_asset
               ON scoped_asset.namespace='library' AND scoped_asset.key=scoped.uuid
             CROSS JOIN term_active_evidence active ON active.asset_id=scoped_asset.id",
        )
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

/// 结构状态（实体的列、work_review、链接）由核心照旧 SQL 写进兼容表之后，把这些身份镜像进 `terms` / `term_tree` /
/// `term_links`。映射与 S2 收编相同；新身份的 origin 是 `origin`。链接已属于别的类型的词条时拒绝。
pub(crate) fn mirror_state(c: &Connection, ids: &[String], origin: &str) -> Result<()> {
    c.execute_batch("CREATE TEMP TABLE IF NOT EXISTS state_scope(id TEXT PRIMARY KEY); DELETE FROM state_scope;")?;
    let mut put = c.prepare_cached("INSERT OR IGNORE INTO state_scope VALUES (?)")?;
    for id in ids {
        put.execute([id])?;
    }
    c.execute(
        &format!(
            "INSERT INTO terms(key,kind,origin,attrs) SELECT e.id,e.kind,?1,{ATTRS} FROM entities e
             WHERE e.id IN (SELECT id FROM state_scope) AND NOT EXISTS(SELECT 1 FROM terms t WHERE t.key=e.id)
             ORDER BY e.rowid"
        ),
        [origin],
    )?;
    c.execute(
        &format!(
            "UPDATE terms SET kind=e.kind,attrs={ATTRS} FROM entities e
             WHERE e.id=terms.key AND e.id IN (SELECT id FROM state_scope) AND (terms.kind IS NOT e.kind OR terms.attrs IS NOT {ATTRS})"
        ),
        [],
    )?;
    c.execute(
        "INSERT INTO term_tree(term_id,parent_id,merged_into,work_id,work_text,review_status,review_note,parent_locked)
         SELECT t.id,e.parent_id,e.merged_into,e.work_id,e.work,w.status,w.note,w.parent_locked
         FROM entities e JOIN terms t ON t.key=e.id LEFT JOIN work_review w ON w.entity_id=e.id WHERE e.id IN (SELECT id FROM state_scope)
         ON CONFLICT(term_id) DO UPDATE SET parent_id=excluded.parent_id,merged_into=excluded.merged_into,
          work_id=excluded.work_id,work_text=excluded.work_text,review_status=excluded.review_status,
          review_note=excluded.review_note,parent_locked=excluded.parent_locked
         WHERE term_tree.parent_id IS NOT excluded.parent_id OR term_tree.merged_into IS NOT excluded.merged_into
          OR term_tree.work_id IS NOT excluded.work_id OR term_tree.work_text IS NOT excluded.work_text
          OR term_tree.review_status IS NOT excluded.review_status OR term_tree.review_note IS NOT excluded.review_note
          OR term_tree.parent_locked IS NOT excluded.parent_locked",
        [],
    )?;
    let foreign: Option<(String, String)> = c
        .query_row(
            "SELECT l.system,l.key FROM links l
             JOIN term_links x ON x.system=CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END AND x.key=l.key
             WHERE l.entity_id IN (SELECT id FROM state_scope) AND x.term_id NOT IN (SELECT term_id FROM term_tree) LIMIT 1",
            [],
            |x| Ok((x.get(0)?, x.get(1)?)),
        )
        .optional()?;
    if let Some((system, key)) = foreign {
        return Err(Error::Conflict(format!("链接 {system}:{key} 已属于别的类型的词条")));
    }
    // 链接从这些身份移到了别的身份：跟着旧表改归属（新主人不一定在范围里）。由范围驱动（`CROSS JOIN` 固定连接顺序），
    // 不扫整张 `links`、`term_links`（#102 A2）；`term_links` 是普通表，插入仍按 `links` 的 rowid 顺序（原计划整表扫描
    // `links` 的顺序）。每行旧链接只对应一个 (system,key)，连接不会产生重复行。
    c.execute(
        "INSERT INTO term_links(system,key,term_id,url,source,current,added_at)
         SELECT CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END,l.key,t.id,'',l.source,l.current,l.added_at
         FROM state_scope s CROSS JOIN terms st ON st.key=s.id CROSS JOIN term_links x ON x.term_id=st.id
         CROSS JOIN links l ON l.key=x.key AND l.system IN (x.system,CASE x.system WHEN 'wd_character' THEN 'wd' END)
          AND CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END=x.system
         CROSS JOIN terms t ON t.key=l.entity_id WHERE true ORDER BY l.rowid
         ON CONFLICT(system,key) DO UPDATE SET term_id=excluded.term_id,source=excluded.source,current=excluded.current,
          added_at=excluded.added_at
         WHERE term_links.term_id IS NOT excluded.term_id OR term_links.source IS NOT excluded.source
          OR term_links.current IS NOT excluded.current OR term_links.added_at IS NOT excluded.added_at",
        [],
    )?;
    c.execute(
        "DELETE FROM term_links WHERE term_id IN (SELECT t.id FROM state_scope s CROSS JOIN terms t ON t.key=s.id) AND NOT EXISTS(
          SELECT 1 FROM links l WHERE l.system=CASE term_links.system WHEN 'wd_character' THEN 'wd' ELSE term_links.system END
          AND l.key=term_links.key)",
        [],
    )?;
    c.execute(
        "INSERT INTO term_links(system,key,term_id,url,source,current,added_at)
         SELECT CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END,l.key,t.id,'',l.source,l.current,l.added_at
         FROM links l JOIN terms t ON t.key=l.entity_id WHERE l.entity_id IN (SELECT id FROM state_scope)
         ON CONFLICT(system,key) DO UPDATE SET term_id=excluded.term_id,source=excluded.source,current=excluded.current,
          added_at=excluded.added_at
         WHERE term_links.term_id IS NOT excluded.term_id OR term_links.source IS NOT excluded.source
          OR term_links.current IS NOT excluded.current OR term_links.added_at IS NOT excluded.added_at",
        [],
    )?;
    Ok(())
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
        table: "entities",
        columns: ENTITY_COLUMNS,
        select: "SELECT t.key,'',r.work_text,NULL,json_extract(t.attrs,'$.variant'),r.parent_id,NULL,r.merged_into,t.kind,
          json_extract(t.attrs,'$.artist'),json_extract(t.attrs,'$.label_only'),json_extract(t.attrs,'$.mascot'),
          json_extract(t.attrs,'$.mascot_origin'),r.work_id,json_extract(t.attrs,'$.original')
         FROM term_tree r JOIN terms t ON t.id=r.term_id",
        identity: false,
    },
    Compat {
        table: "links",
        columns: &["system", "key", "entity_id", "current", "source", "added_at"],
        select: "SELECT CASE l.system WHEN 'wd_character' THEN 'wd' ELSE l.system END,l.key,t.key,l.current,l.source,l.added_at
         FROM term_links l JOIN term_tree r ON r.term_id=l.term_id JOIN terms t ON t.id=l.term_id",
        identity: false,
    },
    Compat {
        table: "work_review",
        columns: &["entity_id", "status", "note", "parent_locked"],
        select: "SELECT t.key,r.review_status,r.review_note,r.parent_locked FROM term_tree r JOIN terms t ON t.id=r.term_id
         WHERE r.review_status IS NOT NULL",
        identity: false,
    },
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

#[derive(Debug, Default)]
pub struct Report {
    pub committed: bool,
    pub policy: Option<String>,
    pub terms_created: usize,
    pub terms_updated: usize,
    pub tree_changed: usize,
    pub links_changed: usize,
    pub baseline_changed: usize,
    pub assets_added: usize,
    pub evidence_added: usize,
    pub evidence_sets_added: usize,
    pub relations_added: usize,
    pub verdicts_added: usize,
    pub batches_added: usize,
    /// 旧表不合收编条件（例如 `entities.name` 不空、链接系统名冲突）：一条都不能有。
    pub preflight: Vec<Value>,
    /// 按历史解释不了的人工结论或关系决议：一条都不能有，否则逐条给用户看。
    pub unexplained: Vec<Value>,
    pub unexplained_count: usize,
    /// 每张兼容表的比对：旧表行数、影子行数、旧表有而影子没有、影子有而旧表没有（含样例）。
    pub tables: BTreeMap<String, Value>,
}

impl Report {
    pub fn clean(&self) -> bool {
        self.preflight.is_empty()
            && self.unexplained_count == 0
            && self.tables.values().all(|t| t["missing"] == 0 && t["extra"] == 0)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "committed": self.committed, "policy": self.policy,
            "terms_created": self.terms_created, "terms_updated": self.terms_updated, "tree_changed": self.tree_changed,
            "links_changed": self.links_changed, "baseline_changed": self.baseline_changed, "assets_added": self.assets_added,
            "evidence_added": self.evidence_added, "evidence_sets_added": self.evidence_sets_added,
            "relations_added": self.relations_added, "verdicts_added": self.verdicts_added, "batches_added": self.batches_added,
            "preflight": self.preflight, "unexplained_count": self.unexplained_count, "unexplained": self.unexplained,
            "tables": self.tables,
        })
    }

    fn unexplained(&mut self, sample: Value) {
        self.unexplained_count += 1;
        if self.unexplained.len() < SAMPLES {
            self.unexplained.push(sample);
        }
    }
}

/// 影子同步。报告里 `committed` 为假时什么都没留下（预检、出处或比对没通过）。
pub fn sync(conn: &mut Connection) -> Result<Report> {
    crate::schema::require_current(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if flag(&tx, CHARACTERS_IN_CORE)? {
        return Err(Error::Conflict("角色已切到词条核心，旧表不再同步".into()));
    }
    begin_writing(&tx)?;
    let mut r = Report::default();
    sync_in(&tx, &mut r)?;
    if r.clean() {
        commit_writing(tx)?;
        r.committed = true;
    }
    Ok(r)
}

/// 同步的主体（在调用方的事务里）：预检、补 term_*、生成影子兼容表并比对，结果记进报告。
fn sync_in(tx: &Connection, r: &mut Report) -> Result<()> {
    let identity = table_exists(tx, "identity_relations")?;
    preflight(tx, identity, r)?;
    if !r.preflight.is_empty() {
        return Ok(());
    }
    let mut assets = Assets::load(tx)?;
    sync_entities(tx, r)?;
    sync_links(tx, r)?;
    if table_exists(tx, "identity_baseline")? {
        sync_baseline(tx, r)?;
    }
    let terms = terms(tx)?;
    sync_candidates(tx, &terms, &mut assets, r)?;
    sync_assertions(tx, &terms, &mut assets, r)?;
    if identity {
        sync_relations(tx, &terms, r)?;
    }
    r.assets_added = assets.added;
    if assets.added > 0 || r.evidence_sets_added > 0 {
        rebuild_active_evidence(tx)?;
    }
    shadow(tx, identity, false)?;
    compare(tx, identity, r)?;
    Ok(())
}

/// 切换后由核心写、Python 只读的表（写保护，见 schema::guard）：兼容表（含身份基线）、归属派生表、角色判定器的私有状态、
/// 学习产物，以及相册导入的 `imported`。`deferred`、`asked`、`reviews` 由核心命令一起写，但 Python 撤销自己的步骤
/// （旧的零成员 sweep、curate、identity_skip）时照旧要删它们的行；`duplicates`、`excluded`、`auto_rules` 仍有
/// Python 的写入口（curate、相似分组）。这些都不在里面。
pub const GUARDED: [&str; 26] = [
    "identity_baseline",
    "entities",
    "links",
    "work_review",
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

/// 角色切到核心（方案第 9 节第 7 条）：一个事务里最后一次同步（必须零差异）、
/// 删掉 `assertions` 上的身份触发器（语义已在核心写结论的同一段代码里）、兼容表与核心独占的表加写保护，
/// 写下 `terms_characters=core`。前提：标签已在核心。已经切过就什么都不做，返回 None；失败什么都不留。
pub fn cutover_with_backup(conn: &mut Connection, backup: &crate::backup::RequiredBackup) -> Result<Option<Report>> {
    crate::schema_rules::migrate_with_backup(conn, backup)?;
    if characters_in_core(conn)? {
        return Ok(None);
    }
    crate::schema_rules::backup_before_cutover(conn, "characters", backup)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if flag(&tx, CHARACTERS_IN_CORE)? {
        return Ok(None);
    }
    if !crate::store::labels_in_core(&tx)? {
        return Err(Error::Conflict("先把用户标签、分级和题材切到词条核心，再切角色".into()));
    }
    begin_writing(&tx)?;
    let mut r = Report::default();
    sync_in(&tx, &mut r)?;
    if !r.clean() {
        return Err(Error::Conflict(format!("角色收编核对没有通过，不切换：{report}", report = r.to_json())));
    }
    // 第 4 版起不再冻结旧人工表（回退靠切换前的自动备份）；兼容表 `identity_baseline` 照旧由核心维护。
    let triggers: Vec<String> = tx
        .prepare("SELECT name FROM sqlite_master WHERE type='trigger' AND tbl_name='assertions' AND name LIKE 'identity_assertion_%'")?
        .query_map([], |x| x.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for t in triggers {
        tx.execute_batch(&format!("DROP TRIGGER {t}"))?;
    }
    for c in COMPAT.iter().rev() {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {SHADOW}{}", c.table))?;
    }
    for table in GUARDED {
        if table_exists(&tx, table)? {
            guard(&tx, table)?;
        }
    }
    crate::character_projection::mirror_all(&tx)?;
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'core')", [CHARACTERS_IN_CORE])?;
    crate::schema::compact_characters(&tx)?;
    commit_writing(tx)?;
    crate::schema::compact_pending(conn);
    r.committed = true;
    Ok(Some(r))
}

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

fn check(conn: &Connection, r: &mut Report, what: &str, sql: &str) -> Result<()> {
    let n: i64 = conn.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |x| x.get(0))?;
    if n > 0 {
        r.preflight.push(json!({"check": what, "count": n, "samples": samples(conn, sql)?}));
    }
    Ok(())
}

// ---- 预检 ----

fn preflight(tx: &Connection, identity: bool, r: &mut Report) -> Result<()> {
    let policy: Option<String> =
        tx.query_row("SELECT value FROM meta WHERE key='identity_policy'", [], |x| x.get(0)).optional()?;
    r.policy = policy.clone();
    if let Some(p) = policy.as_deref().filter(|p| *p != "hybrid") {
        r.preflight.push(json!({"check": "只支持身份策略未设置或 hybrid", "policy": p}));
        return Ok(());
    }
    for c in COMPAT.iter().filter(|c| !c.identity || identity) {
        if !table_exists(tx, c.table)? {
            r.preflight.push(json!({"check": "缺少旧表", "table": c.table}));
            continue;
        }
        // 按列名比较、不计顺序：真实库由多次 ALTER TABLE 加列，列序随升级历史不同；影子表复制旧表自己的建表语句，
        // 收编的读写与比对都按列名，列序不影响结果。
        let have = columns(tx, c.table)?;
        let (mut sorted, mut wanted) = (have.clone(), c.columns.to_vec());
        sorted.sort_unstable();
        wanted.sort_unstable();
        if sorted != wanted {
            r.preflight.push(json!({"check": "旧表的列与收编约定不同", "table": c.table, "columns": have}));
        }
    }
    if !r.preflight.is_empty() {
        return Ok(());
    }
    check(tx, r, "entities.name 不为空（旧迁移应已清空）", "SELECT id,name FROM entities WHERE name<>''")?;
    check(
        tx,
        r,
        "entities.external_key 或 tag 不为空",
        "SELECT id FROM entities WHERE external_key IS NOT NULL OR tag IS NOT NULL",
    )?;
    check(
        tx,
        r,
        "entities.kind 不是 character / skin / work",
        "SELECT id,kind FROM entities WHERE kind NOT IN ('character','skin','work')",
    )?;
    check(tx, r, "entities.id 超过 128 字", "SELECT id FROM entities WHERE length(id) NOT BETWEEN 1 AND 128")?;
    check(
        tx,
        r,
        "词条 key 已被别的词条占用",
        &format!(
            "SELECT e.id,t.kind,t.origin FROM entities e JOIN terms t ON t.key=e.id
             WHERE NOT EXISTS(SELECT 1 FROM term_tree x WHERE x.term_id=t.id) AND t.origin<>'{ORIGIN}'"
        ),
    )?;
    check(
        tx,
        r,
        "links 里已有 wd_character（与角色 WD 链接改名冲突）",
        "SELECT system,key FROM links WHERE system='wd_character'",
    )?;
    check(
        tx,
        r,
        "links 的系统名或键超长",
        "SELECT system,key FROM links WHERE length(system) NOT BETWEEN 1 AND 32 OR length(key) NOT BETWEEN 1 AND 256",
    )?;
    check(tx, r, "links.current 不是 0 / 1", "SELECT system,key,current FROM links WHERE current NOT IN (0,1)")?;
    check(
        tx,
        r,
        "链接挂在不存在的身份上",
        "SELECT system,key,entity_id FROM links WHERE entity_id NOT IN (SELECT id FROM entities)",
    )?;
    check(
        tx,
        r,
        "链接已属于核心里别的类型的词条",
        "SELECT l.system,l.key,t.kind FROM links l
         JOIN term_links x ON x.system=CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END AND x.key=l.key
         JOIN terms t ON t.id=x.term_id WHERE NOT EXISTS(SELECT 1 FROM term_tree r WHERE r.term_id=t.id)",
    )?;
    check(
        tx,
        r,
        "work_review 挂在不存在的身份上",
        "SELECT entity_id FROM work_review WHERE entity_id NOT IN (SELECT id FROM entities)",
    )?;
    check(
        tx,
        r,
        "人工结论指向不存在的身份",
        "SELECT uuid,entity FROM assertions WHERE entity NOT IN (SELECT id FROM entities)",
    )?;
    check(
        tx,
        r,
        "候选指向不存在的身份",
        "SELECT uuid,entity FROM candidates WHERE entity NOT IN (SELECT id FROM entities)",
    )?;
    check(
        tx,
        r,
        "候选来源不是 WD / CCIP",
        "SELECT uuid,entity,source FROM candidates WHERE source NOT IN ('WD','CCIP')",
    )?;
    check(
        tx,
        r,
        "图的 uuid 超长",
        "SELECT uuid FROM assertions WHERE length(uuid) NOT BETWEEN 1 AND 256 UNION SELECT uuid FROM candidates WHERE length(uuid) NOT BETWEEN 1 AND 256",
    )?;
    if identity {
        check(
            tx,
            r,
            "身份关系的端点不存在",
            "SELECT id,a,b FROM identity_relations WHERE a NOT IN (SELECT id FROM entities) OR b NOT IN (SELECT id FROM entities)",
        )?;
        check(
            tx,
            r,
            "同一对身份有两个方向的 duplicate_of",
            "SELECT x.id,y.id FROM identity_relations x JOIN identity_relations y
             ON x.kind='duplicate_of' AND y.kind='duplicate_of' AND x.a=y.b AND x.b=y.a AND x.id<y.id",
        )?;
        check(
            tx,
            r,
            "different_from 没有按文字顺序存",
            "SELECT id,a,b FROM identity_relations WHERE kind='different_from' AND NOT a<b",
        )?;
        check(
            tx,
            r,
            "关系证据的来源是保留字",
            "SELECT relation_id,source FROM identity_evidence WHERE source IN ('user','human','album','legacy','verdict')",
        )?;
        check(
            tx,
            r,
            "决议指向不存在的关系",
            "SELECT id,relation_id FROM identity_decisions WHERE relation_id NOT IN (SELECT id FROM identity_relations)",
        )?;
        check(
            tx,
            r,
            "决议状态不认识",
            "SELECT id,status FROM identity_decisions WHERE status NOT IN ('proposed','accepted','rejected')",
        )?;
        check(
            tx,
            r,
            "决议记下的证据不在该关系上",
            "SELECT x.decision_id,x.fingerprint FROM identity_decision_evidence x
             LEFT JOIN identity_decisions d ON d.id=x.decision_id
             WHERE d.id IS NULL OR NOT EXISTS(SELECT 1 FROM identity_evidence e WHERE e.relation_id=d.relation_id AND e.fingerprint=x.fingerprint)",
        )?;
    }
    Ok(())
}

// ---- 身份、结构、链接、基线 ----

const ATTRS: &str = "json_object('variant',e.variant,'artist',e.artist,'label_only',e.label_only,'mascot',e.mascot,
 'mascot_origin',e.mascot_origin,'original',e.original)";

fn sync_entities(tx: &Connection, r: &mut Report) -> Result<()> {
    r.terms_created = tx.execute(
        &format!(
            "INSERT INTO terms(key,kind,origin,attrs) SELECT e.id,e.kind,'{ORIGIN}',{ATTRS} FROM entities e
             WHERE NOT EXISTS(SELECT 1 FROM terms t WHERE t.key=e.id) ORDER BY e.rowid"
        ),
        [],
    )?;
    r.terms_updated = tx.execute(
        &format!(
            "UPDATE terms SET kind=e.kind,attrs={ATTRS} FROM entities e
             WHERE e.id=terms.key AND (terms.kind IS NOT e.kind OR terms.attrs IS NOT {ATTRS})"
        ),
        [],
    )?;
    r.tree_changed = tx.execute(
        "INSERT INTO term_tree(term_id,parent_id,merged_into,work_id,work_text,review_status,review_note,parent_locked)
         SELECT t.id,e.parent_id,e.merged_into,e.work_id,e.work,w.status,w.note,w.parent_locked
         FROM entities e JOIN terms t ON t.key=e.id LEFT JOIN work_review w ON w.entity_id=e.id WHERE 1
         ON CONFLICT(term_id) DO UPDATE SET parent_id=excluded.parent_id,merged_into=excluded.merged_into,
          work_id=excluded.work_id,work_text=excluded.work_text,review_status=excluded.review_status,
          review_note=excluded.review_note,parent_locked=excluded.parent_locked
         WHERE (parent_id,merged_into,work_id,work_text,review_status,review_note,parent_locked)
          IS NOT (excluded.parent_id,excluded.merged_into,excluded.work_id,excluded.work_text,excluded.review_status,
                  excluded.review_note,excluded.parent_locked)",
        [],
    )?;
    Ok(())
}

/// 旧链接 → 核心链接（系统名映射后的 (system, key)），属于身份词条的链接跟随旧表增删改。
fn sync_links(tx: &Connection, r: &mut Report) -> Result<()> {
    const WANTED: &str =
        "SELECT CASE l.system WHEN 'wd' THEN 'wd_character' ELSE l.system END AS system,l.key,t.id AS term_id,
      l.current,l.source,l.added_at FROM links l JOIN terms t ON t.key=l.entity_id";
    // 按旧表主键 (system, key) 查，不要对派生表做相关子查询（那是每行扫一遍 links）。
    r.links_changed += tx.execute(
        "DELETE FROM term_links WHERE term_id IN (SELECT term_id FROM term_tree) AND NOT EXISTS(SELECT 1 FROM links l
          WHERE l.system=CASE term_links.system WHEN 'wd_character' THEN 'wd' ELSE term_links.system END AND l.key=term_links.key)",
        [],
    )?;
    r.links_changed += tx.execute(
        &format!(
            "INSERT INTO term_links(system,key,term_id,url,source,current,added_at)
             SELECT system,key,term_id,'',source,current,added_at FROM ({WANTED}) WHERE 1
             ON CONFLICT(system,key) DO UPDATE SET term_id=excluded.term_id,source=excluded.source,current=excluded.current,
              added_at=excluded.added_at
             WHERE (term_id,source,current,added_at) IS NOT (excluded.term_id,excluded.source,excluded.current,excluded.added_at)"
        ),
        [],
    )?;
    Ok(())
}

/// 活的身份基线：与旧表不同就整表按 rowid 顺序重写（它是判定器私有状态，不是历史）。
fn sync_baseline(tx: &Connection, r: &mut Report) -> Result<()> {
    const OLD: &str = "SELECT rowid,id,parent_id,variant,kind,merged_into FROM identity_baseline";
    const NEW: &str = "SELECT seq,id,parent_id,variant,kind,merged_into FROM term_identity_baseline";
    let differs: bool =
        tx.query_row(&format!("SELECT EXISTS({OLD} EXCEPT {NEW}) OR EXISTS({NEW} EXCEPT {OLD})"), [], |x| x.get(0))?;
    if differs {
        tx.execute("DELETE FROM term_identity_baseline", [])?;
        r.baseline_changed = tx.execute(
            &format!("INSERT INTO term_identity_baseline(seq,id,parent_id,variant,kind,merged_into) {OLD}"),
            [],
        )?;
    }
    Ok(())
}

fn terms(tx: &Connection) -> Result<HashMap<String, TermId>> {
    let mut out = HashMap::new();
    for row in tx
        .prepare("SELECT t.key,t.id FROM term_tree r JOIN terms t ON t.id=r.term_id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
    {
        let (k, v) = row?;
        out.insert(k, v);
    }
    Ok(out)
}

/// 本图库的资产：已登记的用原来的（不改它的移除状态，那归画师、标签的同步）；没登记的现在登记，
/// 不在 `assets` 里的记为已移除（人工结论可以留在已移除的图上）。
struct Assets {
    ids: HashMap<String, AssetId>,
    added: usize,
}

impl Assets {
    fn load(tx: &Connection) -> Result<Assets> {
        let mut ids = HashMap::new();
        for row in tx
            .prepare("SELECT key,id FROM term_assets WHERE namespace=?1")?
            .query_map([LIBRARY], |x| Ok((x.get(0)?, x.get(1)?)))?
        {
            let (k, v) = row?;
            ids.insert(k, v);
        }
        Ok(Assets { ids, added: 0 })
    }

    fn get(&mut self, tx: &Connection, uuid: &str) -> Result<AssetId> {
        if let Some(&id) = self.ids.get(uuid) {
            return Ok(id);
        }
        tx.execute(
            "INSERT INTO term_assets(namespace,key,removed_at)
             VALUES (?1,?2,CASE WHEN EXISTS(SELECT 1 FROM assets WHERE uuid=?2) THEN NULL ELSE CURRENT_TIMESTAMP END)",
            [LIBRARY, uuid],
        )?;
        let id = tx.last_insert_rowid();
        self.ids.insert(uuid.to_owned(), id);
        self.added += 1;
        Ok(id)
    }
}

// ---- 机器候选：一个迁移批次 ----

fn sync_candidates(
    tx: &Connection,
    terms: &HashMap<String, TermId>,
    assets: &mut Assets,
    r: &mut Report,
) -> Result<()> {
    type Row = (String, String, String, f64);
    let rows: Vec<Row> = tx
        .prepare("SELECT uuid,entity,source,score FROM candidates ORDER BY uuid,entity,source")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    // 版本 = 整表内容摘要：候选变了（重扫过）就另起一版，旧的留着。
    let digest = sha256_hex(
        canonical(&json!(rows.iter().map(|(u, e, s, v)| json!([u, e, s, v])).collect::<Vec<_>>())).as_bytes(),
    );
    let version = format!("migration/{}", &digest[..16]);
    let producer = format!("migration:{}", &digest[..16]);
    let mut by_source: BTreeMap<&str, Vec<i64>> = BTreeMap::from([("CCIP", vec![]), ("WD", vec![])]);
    for (uuid, entity, source, score) in &rows {
        let (src, method, metric) = if source == "WD" {
            ("wd", "wd_character", "wd_probability")
        } else {
            ("ccip_index", "char_ext", "ccip_score")
        };
        let asset = assets.get(tx, uuid)?;
        let term = terms[entity];
        let claim = Claim::Image { asset, term, target: json!({"asset": [LIBRARY, uuid], "term": entity}) };
        let data = Map::new();
        let e = NewEvidence {
            claim: &claim,
            source: src,
            method,
            version: &version,
            score: Some((*score, metric)),
            data: &data,
            observed_at: None,
            package: None,
        };
        if add_evidence(tx, &e)? {
            r.evidence_added += 1;
        }
        let id: i64 = tx.query_row(
            "SELECT id FROM term_evidence WHERE asset_id=?1 AND term_id=?2 AND source=?3 AND method=?4 AND version=?5",
            params![asset, term, src, method, version],
            |x| x.get(0),
        )?;
        by_source.get_mut(source.as_str()).expect("预检保证只有 WD / CCIP").push(id);
    }
    for (source, rows) in by_source {
        let (src, method) = if source == "WD" { ("wd", "wd_character") } else { ("ccip_index", "char_ext") };
        let latest: Option<String> = tx
            .query_row(
                "SELECT producer FROM term_evidence_sets WHERE source=?1 AND method=?2 ORDER BY id DESC LIMIT 1",
                [src, method],
                |x| x.get(0),
            )
            .optional()?;
        if latest.as_deref() == Some(producer.as_str()) {
            continue;
        }
        tx.execute(
            "INSERT INTO term_evidence_sets(producer,source,method,scope) VALUES (?1,?2,?3,'all')",
            [producer.as_str(), src, method],
        )?;
        let set = tx.last_insert_rowid();
        let mut add = tx.prepare_cached("INSERT INTO term_evidence_set_rows(set_id,evidence_id) VALUES (?1,?2)")?;
        for id in rows {
            add.execute([set, id])?;
        }
        r.evidence_sets_added += 1;
    }
    Ok(())
}

// ---- 人工结论：按历史重建 ----

type Target = (String, String); // (图 uuid, 身份 id)
type Row = (String, String); // (status, origin)

enum Event {
    /// 快照项：这一步写了一行 review / sweep，before 是之前的值。
    Snapshot { before: Option<Row> },
    /// 破坏式合并删掉了源身份上的这条结论（before 是当时的值）。
    MergeClear { before: Row, fixed: bool },
    /// 破坏式合并把源身份的结论搬到了目标上（目标原来没有结论）。
    MergeSet { set: Row, fixed: bool },
}

/// 合并搬动结论挂在哪里。hybrid 库里撤销旧合并只把关系退回 proposed，搬走的结论不回来（S0 夹具
/// `test_undoing_a_merge_made_before_activation`）：这种搬动不能挂在合并那一步上（撤销它就会搬回去），
/// 放进锚点为那一步的迁移批次。策略未设置的库照旧挂在那一步上，撤销时一起恢复。
fn merge_at(action: i64, fixed: bool) -> At {
    if fixed {
        At::Batch { kind: "migration", anchor: action }
    } else {
        At::Action(action)
    }
}

enum At {
    Action(i64),
    Batch { kind: &'static str, anchor: i64 },
}

struct Want {
    at: At,
    target: Target,
    verdict: &'static str,
    via: String,
}

fn verdict_of(status: &str) -> &'static str {
    if status == "confirmed" {
        "yes"
    } else {
        "no"
    }
}

fn row_of(v: &Value) -> Option<Row> {
    Some((v.get("status")?.as_str()?.to_owned(), v.get("origin")?.as_str()?.to_owned()))
}

fn manual(row: &Row) -> bool {
    row.1 == "review" || row.1 == "sweep"
}

/// 没有 action 的值：相册导入进 album 批次，其余进基线批次；via 保存原 origin。
fn baseline(target: &Target, anchor: i64, row: &Row) -> Want {
    let kind = if row.1 == "album" { "album" } else { "baseline" };
    Want { at: At::Batch { kind, anchor }, target: target.clone(), verdict: verdict_of(&row.0), via: row.1.clone() }
}

/// hybrid 下撤销过这次旧合并：关系上有 `Store.undo` 那条固定理由的决议。
const MERGE_UNDONE_IN_HYBRID: &str = "撤销历史合并；原归属无法区分的认领待确认";

/// 未撤销的步骤按 id 排成每个目标的事件序列；已撤销的步骤的效果已被还原，不算——除了在 hybrid 下撤销的旧合并。
fn history(tx: &Connection, hybrid: bool) -> Result<BTreeMap<Target, Vec<(i64, Event)>>> {
    let mut events: BTreeMap<Target, Vec<(i64, Event)>> = BTreeMap::new();
    let actions: Vec<(i64, String, bool)> = tx
        .prepare(
            "SELECT id,before_json,undone FROM actions WHERE undone=0 OR before_json LIKE '%\"merge\"%' ORDER BY id",
        )?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get::<_, i64>(2)? != 0)))?
        .collect::<rusqlite::Result<_>>()?;
    let undone_in_hybrid = if hybrid { undone_in_hybrid(tx, &actions)? } else { Default::default() };
    for (action, before, undone) in actions {
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&before) else {
            return Err(Error::Conflict(format!("历史第 {action} 步的记录不是数组")));
        };
        let mut seen: Vec<Target> = Vec::new();
        let mut push = |target: Target, event: Event, events: &mut BTreeMap<Target, Vec<(i64, Event)>>| {
            // 同一步里同一个目标出现多次（复制品展开重叠）：快照都在写之前取，第一条就是之前的值。
            if !seen.contains(&target) {
                seen.push(target.clone());
                events.entry(target).or_default().push((action, event));
            }
        };
        for item in &items {
            if let Some(m) = item.get("merge") {
                let (Some(source), Some(target)) = (m["source"].as_str(), m["target"].as_str()) else {
                    return Err(Error::Conflict(format!("历史第 {action} 步的合并记录不完整")));
                };
                if undone && !undone_in_hybrid.contains(&action) {
                    continue;
                }
                let fixed = hybrid;
                for (list, moves) in [("moved", true), ("kept", false)] {
                    for row in m[list].as_array().into_iter().flatten() {
                        let (Some(u), Some(st), Some(o)) = (row[0].as_str(), row[1].as_str(), row[2].as_str()) else {
                            return Err(Error::Conflict(format!("历史第 {action} 步的合并记录不完整")));
                        };
                        let value = (st.to_owned(), o.to_owned());
                        push(
                            (u.to_owned(), source.to_owned()),
                            Event::MergeClear { before: value.clone(), fixed },
                            &mut events,
                        );
                        if moves {
                            push((u.to_owned(), target.to_owned()), Event::MergeSet { set: value, fixed }, &mut events);
                        }
                    }
                }
            } else if undone {
                continue;
            } else if let (Some(uuid), Some(entity)) =
                (item.get("uuid").and_then(Value::as_str), item.get("entity").and_then(Value::as_str))
            {
                if item.get("assertion").is_some() {
                    let before = item["assertion"].as_object().map(|_| row_of(&item["assertion"]));
                    let before = match before {
                        None => None,
                        Some(Some(row)) => Some(row),
                        Some(None) => return Err(Error::Conflict(format!("历史第 {action} 步的快照不完整"))),
                    };
                    push((uuid.to_owned(), entity.to_owned()), Event::Snapshot { before }, &mut events);
                }
            }
        }
    }
    Ok(events)
}

/// 哪些已撤销的旧合并是在 hybrid 下撤销的。同一对身份的合并只能交替发生（合并、撤销、再合并），撤销又只能撤最近一步，
/// 所以激活后撤销的一定是这一对最后被撤销的几次；次数 = 关系上那条固定理由的决议条数。
fn undone_in_hybrid(tx: &Connection, actions: &[(i64, String, bool)]) -> Result<std::collections::HashSet<i64>> {
    let mut pairs: BTreeMap<(String, String), Vec<i64>> = BTreeMap::new();
    for (action, before, _) in actions.iter().filter(|a| a.2) {
        let Ok(Value::Array(items)) = serde_json::from_str::<Value>(before) else { continue };
        for m in items.iter().filter_map(|i| i.get("merge")) {
            if let (Some(a), Some(b)) = (m["source"].as_str(), m["target"].as_str()) {
                let pair = if a < b { (a.to_owned(), b.to_owned()) } else { (b.to_owned(), a.to_owned()) };
                pairs.entry(pair).or_default().push(*action);
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    for ((a, b), mut merges) in pairs {
        let k: usize = tx.query_row(
            "SELECT count(*) FROM identity_decisions d JOIN identity_relations r ON r.id=d.relation_id
              WHERE r.kind='duplicate_of' AND ((r.a=?1 AND r.b=?2) OR (r.a=?2 AND r.b=?1)) AND d.reason=?3",
            params![a, b, MERGE_UNDONE_IN_HYBRID],
            |x| x.get(0),
        )?;
        merges.sort_unstable();
        out.extend(merges.iter().rev().take(k));
    }
    Ok(out)
}

/// 方案第 6.2 节：每个目标的事件序列 → 要有的裁决。解释不了的记进报告。
fn reconstruct(
    events: &BTreeMap<Target, Vec<(i64, Event)>>,
    current: &BTreeMap<Target, Row>,
    r: &mut Report,
) -> Vec<Want> {
    let mut wants = Vec::new();
    for (target, list) in events {
        let mut previous: Option<Option<Row>> = None; // 前一事件设成的值
        for (i, (action, event)) in list.iter().enumerate() {
            let before = match event {
                Event::Snapshot { before } => before.clone(),
                Event::MergeClear { before, .. } => Some(before.clone()),
                Event::MergeSet { .. } => None,
            };
            match (&previous, &before) {
                (None, Some(row)) => wants.push(baseline(target, action - 1, row)),
                (None, None) => {}
                (Some(set), _) if *set == before => {}
                // 前一步清掉了（合并），之后相册导入又写了一行。
                (Some(None), Some(row)) if row.1 == "album" => wants.push(baseline(target, action - 1, row)),
                (Some(set), _) => r.unexplained(json!({
                    "uuid": target.0, "entity": target.1, "action": action,
                    "problem": "这一步之前的值与上一步设成的值不同，也不是相册导入能写出的",
                    "expected": set, "found": before,
                })),
            }
            let set = match event {
                Event::Snapshot { .. } => {
                    let set = match list.get(i + 1) {
                        Some((_, Event::Snapshot { before })) => before.clone(),
                        Some((_, Event::MergeClear { before, .. })) => Some(before.clone()),
                        Some((_, Event::MergeSet { .. })) => None,
                        None => current.get(target).cloned(),
                    };
                    match &set {
                        Some(row) if manual(row) => wants.push(Want {
                            at: At::Action(*action),
                            target: target.clone(),
                            verdict: verdict_of(&row.0),
                            via: row.1.clone(),
                        }),
                        _ => r.unexplained(json!({
                            "uuid": target.0, "entity": target.1, "action": action,
                            "problem": "这一步写的应是 review / sweep 结论，之后的值却不是", "found": set,
                        })),
                    }
                    set
                }
                Event::MergeClear { fixed, .. } => {
                    wants.push(Want {
                        at: merge_at(*action, *fixed),
                        target: target.clone(),
                        verdict: "clear",
                        via: "merge".into(),
                    });
                    None
                }
                Event::MergeSet { set, fixed } => {
                    wants.push(Want {
                        at: merge_at(*action, *fixed),
                        target: target.clone(),
                        verdict: verdict_of(&set.0),
                        via: set.1.clone(),
                    });
                    Some(set.clone())
                }
            };
            previous = Some(set);
        }
        // 最后一个事件之后：快照设成的就是当前值；合并之后只可能有相册导入补上的一行。
        let last = list.last().map(|(a, _)| *a).unwrap_or(0);
        let now = current.get(target).cloned();
        if let Some(set) = previous {
            if set != now {
                match (&set, &now) {
                    (None, Some(row)) if row.1 == "album" => wants.push(baseline(target, last, row)),
                    _ => r.unexplained(json!({
                        "uuid": target.0, "entity": target.1, "action": last,
                        "problem": "当前值与历史最后设成的值不同", "expected": set, "found": now,
                    })),
                }
            }
        }
    }
    // 没有任何事件的当前行：排在时间线最前。
    for (target, row) in current {
        if !events.contains_key(target) {
            wants.push(baseline(target, 0, row));
        }
    }
    wants
}

fn sync_assertions(
    tx: &Connection,
    terms: &HashMap<String, TermId>,
    assets: &mut Assets,
    r: &mut Report,
) -> Result<()> {
    let mut current: BTreeMap<Target, Row> = BTreeMap::new();
    for row in tx.prepare("SELECT uuid,entity,status,origin FROM assertions")?.query_map([], |x| {
        Ok(((x.get::<_, String>(0)?, x.get::<_, String>(1)?), (x.get::<_, String>(2)?, x.get::<_, String>(3)?)))
    })? {
        let (k, v) = row?;
        current.insert(k, v);
    }
    let events = history(tx, r.policy.as_deref() == Some("hybrid"))?;
    let wants = reconstruct(&events, &current, r);
    let mut batches = Batches::default();
    for w in wants {
        let Some(&term) = terms.get(&w.target.1) else {
            r.unexplained(json!({"uuid": w.target.0, "entity": w.target.1, "problem": "历史里的身份已不存在"}));
            continue;
        };
        let asset = assets.get(tx, &w.target.0)?;
        let (action, batch, anchor) = match w.at {
            At::Action(a) => (Some(a), None, a),
            At::Batch { kind, anchor } => {
                (None, Some(batches.get(tx, kind, anchor, "人工结论（assertions）", r)?), anchor)
            }
        };
        // 同一步（同一批次）对同一目标只有一条裁决：已有而不同，说明旧表在上次同步后被绕过历史改过。
        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT verdict,via FROM term_verdicts WHERE action_id IS ?1 AND batch_id IS ?2 AND asset_id=?3 AND term_id=?4",
                params![action, batch, asset, term],
                |x| Ok((x.get(0)?, x.get(1)?)),
            )
            .optional()?;
        if let Some((verdict, via)) = existing {
            if verdict != w.verdict || via != w.via {
                r.unexplained(json!({
                    "uuid": w.target.0, "entity": w.target.1, "action": action, "batch": batch,
                    "problem": "核心里这一步已有不同的裁决（旧表在上次同步后被绕过历史改过？）",
                    "core": [verdict, via], "legacy": [w.verdict, w.via],
                }));
            }
        } else {
            let origin = if w.via == "album" && batch.is_some() { "album" } else { "legacy" };
            tx.execute(
                "INSERT INTO term_verdicts(action_id,batch_id,anchor,asset_id,term_id,verdict,origin,reason,via)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'assertions',?8)",
                params![action, batch, anchor, asset, term, w.verdict, origin, w.via],
            )?;
            r.verdicts_added += 1;
        }
    }
    Ok(())
}

/// 非用户批次：同一种、同一锚点、同一说明的共用一个。
#[derive(Default)]
struct Batches(HashMap<(&'static str, i64, &'static str), i64>);

impl Batches {
    fn get(
        &mut self,
        tx: &Connection,
        kind: &'static str,
        anchor: i64,
        label: &'static str,
        r: &mut Report,
    ) -> Result<i64> {
        if let Some(&id) = self.0.get(&(kind, anchor, label)) {
            return Ok(id);
        }
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM term_batches WHERE kind=?1 AND anchor=?2 AND label=?3 ORDER BY id LIMIT 1",
                params![kind, anchor, label],
                |x| x.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO term_batches(kind,anchor,label,data) VALUES (?1,?2,?3,?4)",
                    params![kind, anchor, label, canonical(&json!({"origin": ORIGIN}))],
                )?;
                r.batches_added += 1;
                tx.last_insert_rowid()
            }
        };
        self.0.insert((kind, anchor, label), id);
        Ok(id)
    }
}

// ---- 身份关系、关系证据、决议 ----

fn legacy_id(tx: &Connection, kind: &str, legacy: i64) -> Result<Option<i64>> {
    Ok(tx
        .query_row("SELECT core_id FROM term_legacy_ids WHERE kind=?1 AND legacy_id=?2", params![kind, legacy], |x| {
            x.get(0)
        })
        .optional()?)
}

/// 旧指纹（sha256 十六进制）的前 16 字节；不是 64 位十六进制时取它的摘要。
pub(crate) fn fingerprint16(legacy: &str) -> Vec<u8> {
    let hex = if legacy.len() == 64 && legacy.bytes().all(|b| b.is_ascii_hexdigit()) {
        legacy.to_ascii_lowercase()
    } else {
        sha256_hex(legacy.as_bytes())
    };
    (0..16).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("十六进制")).collect()
}

fn sync_relations(tx: &Connection, terms: &HashMap<String, TermId>, r: &mut Report) -> Result<()> {
    // 关系：duplicate_of(a,b) → same_as(a,b)，b 是代表；form_of(皮肤,人物) → contains(人物→皮肤)；different_from 原样。
    let relations: Vec<(i64, String, String, String)> = tx
        .prepare("SELECT id,a,b,kind FROM identity_relations ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut core: HashMap<i64, i64> = HashMap::new();
    for (id, a, b, kind) in relations {
        let rel = match legacy_id(tx, "relation", id)? {
            Some(rel) => rel,
            None => {
                let (ta, tb) = (terms[&a], terms[&b]);
                let rel = match kind.as_str() {
                    "duplicate_of" => relation(tx, ta, tb, RelationKind::SameAs)?,
                    "form_of" => relation(tx, tb, ta, RelationKind::Contains)?,
                    _ => relation(tx, ta, tb, RelationKind::DifferentFrom)?,
                };
                tx.execute("INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('relation',?1,?2)", [id, rel])?;
                r.relations_added += 1;
                rel
            }
        };
        core.insert(id, rel);
    }
    // 关系证据：来源原样；版本是旧指纹（同一关系同一来源可以有多条），核心指纹取它的前 16 字节。
    let evidence: Vec<(i64, String, String, String, String)> = tx
        .prepare("SELECT relation_id,fingerprint,source,payload,created_at FROM identity_evidence ORDER BY rowid")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut by_fingerprint: HashMap<(i64, String), i64> = HashMap::new();
    for (relation_id, fp, source, payload, created_at) in evidence {
        let rel = core[&relation_id];
        let data = json!({"legacy_fingerprint": fp, "payload": payload, "created_at": created_at});
        let found: Option<i64> = tx
            .query_row(
                "SELECT id FROM term_evidence WHERE relation_id=?1 AND source=?2 AND method=?3 AND version=?4",
                params![rel, source, RELATION_METHOD, fp],
                |x| x.get(0),
            )
            .optional()?;
        let id = match found {
            Some(id) => id,
            None => {
                tx.execute(
                    "INSERT INTO term_evidence(relation_id,source,method,version,data,fingerprint) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![rel, source, RELATION_METHOD, fp, canonical(&data), fingerprint16(&fp)],
                )?;
                r.evidence_added += 1;
                tx.last_insert_rowid()
            }
        };
        by_fingerprint.insert((relation_id, fp), id);
    }
    // 决议：有 action 的（before_json 里的 decision_id，含已撤销的步骤）挂在那一步上；其余进批次，锚点取同一关系上
    // 前一条决议的锚点，这样同一关系上的先后不变。
    let mut attached: HashMap<i64, i64> = HashMap::new();
    let actions: Vec<(i64, String)> = tx
        .prepare("SELECT id,before_json FROM actions WHERE before_json LIKE '%identity_relation%' ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (action, before) in actions {
        if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&before) {
            for item in items {
                if let Some(d) =
                    item.get("identity_relation").and_then(|x| x.get("decision_id")).and_then(Value::as_i64)
                {
                    attached.insert(d, action);
                }
            }
        }
    }
    type Decision = (i64, i64, String, String, String);
    let decisions: Vec<Decision> = tx
        .prepare("SELECT id,relation_id,status,reason,created_at FROM identity_decisions ORDER BY id")?
        .query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut reviewed: HashMap<i64, Vec<i64>> = HashMap::new();
    for row in tx
        .prepare("SELECT x.decision_id,d.relation_id,x.fingerprint FROM identity_decision_evidence x JOIN identity_decisions d ON d.id=x.decision_id")?
        .query_map([], |x| Ok((x.get::<_, i64>(0)?, x.get::<_, i64>(1)?, x.get::<_, String>(2)?)))?
    {
        let (d, rel, fp) = row?;
        reviewed.entry(d).or_default().push(by_fingerprint[&(rel, fp)]);
    }
    let mut anchors: HashMap<i64, i64> = HashMap::new();
    let mut batches = Batches::default();
    for (id, relation_id, status, reason, created_at) in decisions {
        let (action, kind) = match attached.get(&id) {
            Some(&a) => (Some(a), None),
            None if reason.starts_with("撤销") => (None, Some("undo_audit")),
            None => (None, Some("migration")),
        };
        let anchor = action.unwrap_or_else(|| anchors.get(&relation_id).copied().unwrap_or(0));
        anchors.insert(relation_id, anchor);
        if legacy_id(tx, "decision", id)?.is_some() {
            continue;
        }
        let batch = match kind {
            Some(kind) => Some(batches.get(tx, kind, anchor, "身份关系决议（identity_decisions）", r)?),
            None => None,
        };
        let verdict = match status.as_str() {
            "accepted" => "yes",
            "rejected" => "no",
            _ => "clear",
        };
        let via = match kind {
            None => "review",
            Some("undo_audit") => "undo",
            Some(_) => "migration",
        };
        let mut ids = reviewed.remove(&id).unwrap_or_default();
        ids.sort_unstable();
        tx.execute(
            "INSERT INTO term_verdicts(action_id,batch_id,anchor,relation_id,verdict,origin,reason,reviewed,created_at,via)
             VALUES (?1,?2,?3,?4,?5,'legacy',?6,?7,?8,?9)",
            params![action, batch, anchor, core[&relation_id], verdict, reason, canonical(&json!(ids)), created_at, via],
        )?;
        let verdict_id = tx.last_insert_rowid();
        tx.execute("INSERT INTO term_legacy_ids(kind,legacy_id,core_id) VALUES ('decision',?1,?2)", [id, verdict_id])?;
        r.verdicts_added += 1;
    }
    Ok(())
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

fn shadow(tx: &Connection, identity: bool, temporary: bool) -> Result<()> {
    for c in COMPAT.iter().rev() {
        tx.execute_batch(&format!("DROP TABLE IF EXISTS {SHADOW}{}", c.table))?;
    }
    for c in COMPAT.iter().filter(|c| !c.identity || identity) {
        let sql: String =
            tx.query_row("SELECT sql FROM main.sqlite_master WHERE type='table' AND name=?1", [c.table], |x| x.get(0))?;
        let ddl = shadow_ddl(&sql, c.table)?;
        // 在线核验只需连接内的影子，避免先扩主文件再留下空页。
        let ddl = if temporary { ddl.replacen("CREATE TABLE", "CREATE TEMP TABLE", 1) } else { ddl };
        tx.execute_batch(&ddl)?;
        let select = select_for(tx, c.table, c.select)?;
        tx.execute(&format!("INSERT INTO {SHADOW}{}({}) {}", c.table, c.columns.join(","), select), [])?;
        guard(tx, &format!("{SHADOW}{}", c.table))?;
    }
    Ok(())
}

fn compare(tx: &Connection, identity: bool, r: &mut Report) -> Result<()> {
    compare_with_baseline(tx, identity, r, true)
}

fn compare_with_baseline(tx: &Connection, identity: bool, r: &mut Report, baseline: bool) -> Result<()> {
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
    if baseline && table_exists(tx, "identity_baseline")? {
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
