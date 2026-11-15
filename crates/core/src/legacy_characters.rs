//! 角色、皮肤、作品的旧兼容表在核心第 8 版里的去留（#201）。
//!
//! - 身份、链接、作品复核、身份基线、身份关系：旧表 `entities`、`links`、`work_review`、`identity_baseline` 和
//!   `identity_relations` 等四张关系表在升级时逐行核对后删除（`retire_*`），之后只在 `terms`、`term_tree`、
//!   `term_links`、`term_identity_baseline`、`term_relations` 等核心表里。
//! - `assertions`、`candidates` 是核心维护的派生表（与 `memberships` 一样，#201 第 4 步用户决定），不是兼容层：
//!   人工结论的权威是 `term_verdicts`，候选的权威是证据批次（`character_projection::refresh_candidates`）。
//! - 写保护名单 `GUARDED`、第 4 版删掉的冻结副本 `FROZEN_COPIES`。原来升级时由核心主数据生成影子表、与兼容表逐表
//!   比对的机制随第 4 步删除。
//!
//! 原来的收编同步、切换与核对命令（`sync-characters`、`cutover-characters`、`verify-characters`）随第 4 步删除：第 8 版
//! 拒绝没切换完的图库，它们再也遇不到真实数据；新建图库见 `new_library`。

use crate::strict_json::sha256_hex;
use crate::{Error, Result};
use rusqlite::{Connection, OptionalExtension};

pub const ORIGIN: &str = "legacy:characters";
/// 角色在核心里的标记（新建图库由 `new_library` 写）。
pub const CHARACTERS_IN_CORE: &str = "terms_characters";
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

/// 核心第 8 版：身份关系、关系证据和决议只在核心里（`character_relations`）。旧表还在时，每条改关系的命令都按核心
/// 重算过它们，所以逐表核对，不一致就拒绝升级；一致就删掉（引用别人的在前）。只有身份层装过的库才有这些表。
///
/// 放在 `retire_entities` 之前：`identity_relations` 引用 `entities`，外键开着时要先删它。
pub(crate) fn retire_relations(c: &Connection) -> Result<()> {
    let mut differs = 0;
    for (table, columns, fragment) in crate::character_relations::TABLES {
        if table_exists(c, table)? {
            differs +=
                differ(c, &format!("SELECT {columns} FROM {table}"), &format!("SELECT {columns} FROM {fragment}"))?;
        }
    }
    if differs > 0 {
        return Err(Error::Schema(format!("旧身份关系表与词条核心不一致，没有升级：{differs} 行")));
    }
    for (table, _, _) in crate::character_relations::TABLES.iter().rev() {
        c.execute_batch(&format!("DROP TABLE IF EXISTS {table}"))?;
    }
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
///   拒绝升级（第 7 版里每条改身份的命令都同步镜像过）。引用它的视图改按核心表重写（列逐一对应），最后删表。
///
/// 放在别的旧表退役之后：外键开着，引用 `entities` 的旧表（`links`、`names`、`work_review`、`identity_relations`）都
/// 删掉了才能删它。
pub(crate) fn retire_entities(c: &Connection) -> Result<()> {
    // `term_tree` 有写保护；迁移结束时 `schema::migrate_in` 收回钥匙。
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

/// 两个查询的结果双向相差的行数。
fn differ(c: &Connection, old: &str, new: &str) -> Result<i64> {
    let count =
        |sql: String| -> Result<i64> { Ok(c.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |r| r.get(0))?) };
    Ok(count(format!("{old} EXCEPT {new}"))? + count(format!("{new} EXCEPT {old}"))?)
}

/// 切换后只由核心写、宿主其余代码只读的表（写保护，见 schema::guard）：人工结论与候选（`assertions`、`candidates`）和归属的派生表、角色判定器的私有状态、
/// 学习产物，以及相册导入的 `imported`。`deferred`、`asked`、`reviews` 由核心命令一起写，但宿主撤销自己的步骤
/// （旧的零成员 sweep、curate、identity_skip）时照旧要删它们的行；`duplicates`、`excluded`、`auto_rules` 仍有
/// 宿主的写入口（curate、相似分组）。这些都不在里面。
pub const GUARDED: [&str; 18] = [
    "assertions",
    "candidates",
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

pub(crate) fn fingerprint16(legacy: &str) -> Vec<u8> {
    let hex = if legacy.len() == 64 && legacy.bytes().all(|b| b.is_ascii_hexdigit()) {
        legacy.to_ascii_lowercase()
    } else {
        sha256_hex(legacy.as_bytes())
    };
    (0..16).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("十六进制")).collect()
}

#[cfg(test)]
#[path = "legacy_characters/tests.rs"]
mod tests;
