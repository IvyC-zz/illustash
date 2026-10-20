//! 新建图库的四个领域一开始就在词条核心里（#201 第 4 步）。
//!
//! 原来新建图库在空表上依次跑画师、标签、角色、名字的收编切换，只为写下切换标记和随之而来的结构。收编与切换的机制
//! 删除后（第 8 版拒绝没切换完的图库，它们再也遇不到真实数据），由这里在一个核心写事务里一次做完，结果与原来逐行相同：
//! - 三个空的全库证据批次：溯源一个，候选的 CCIP、WD 各一个（producer 是空候选表的摘要），与原来切换时写下的相同；
//! - 画师：删掉 `artist_verdicts`；
//! - 标签：`asset_tags`、`general_hits`、`asset_rating` 换成核心之上的同名视图（`legacy_labels::VIEWS`）；
//! - 角色：删掉 `assertions` 上的身份触发器，兼容表与核心独占的表加写保护，派生表压缩成 WITHOUT ROWID；
//! - 名字：`names_version` 从 0 起，建显示投影；
//! - 四个切换标记 `terms_*=core`。
use crate::schema::{begin_writing, commit_writing, guard};
use crate::strict_json::{canonical, sha256_hex};
use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::json;

const FLAGS: [&str; 4] = [
    crate::store::ARTISTS_IN_CORE,
    crate::store::LABELS_IN_CORE,
    crate::legacy_characters::CHARACTERS_IN_CORE,
    crate::legacy_names::NAMES_IN_CORE,
];

/// 刚迁移到当前版本、还没有任何数据的新图库：写下四个领域都在核心里的结构和标记。已经写过（四个标记都在）就什么都
/// 不做；只有一部分标记的库不是新库，拒绝。
pub fn switch(conn: &mut Connection) -> Result<()> {
    crate::schema::require_current(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let present: i64 =
        tx.query_row("SELECT count(*) FROM meta WHERE key IN (?1, ?2, ?3, ?4) AND value = 'core'", FLAGS, |r| {
            r.get(0)
        })?;
    if present == FLAGS.len() as i64 {
        return Ok(());
    }
    if present > 0 {
        return Err(Error::Conflict("这个图库只有一部分领域在词条核心里，不是新建的图库".into()));
    }
    begin_writing(&tx)?;
    // 证据批次：溯源全库一个；候选按来源各一个（原来按 CCIP、WD 的顺序写），producer 带空候选表的摘要。
    tx.execute(
        "INSERT INTO term_evidence_sets(producer, source, method, scope) VALUES ('legacy:provenance', 'provenance', '*', 'all')",
        [],
    )?;
    let producer = format!("migration:{}", &sha256_hex(canonical(&json!([])).as_bytes())[..16]);
    for (source, method) in [("ccip_index", "char_ext"), ("wd", "wd_character")] {
        tx.execute(
            "INSERT INTO term_evidence_sets(producer, source, method, scope) VALUES (?1, ?2, ?3, 'all')",
            [producer.as_str(), source, method],
        )?;
    }
    crate::store::rebuild_active_evidence(&tx)?;
    tx.execute_batch("DROP TABLE IF EXISTS artist_verdicts")?;
    tx.execute_batch("DROP TABLE asset_tags; DROP TABLE general_hits; DROP TABLE asset_rating;")?;
    tx.execute_batch(crate::legacy_labels::VIEWS)?;
    let triggers: Vec<String> = tx
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'trigger' AND tbl_name = 'assertions' AND name LIKE 'identity_assertion_%'",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for trigger in triggers {
        tx.execute_batch(&format!("DROP TRIGGER {trigger}"))?;
    }
    for table in crate::legacy_characters::GUARDED {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type = 'table' AND name = ?1)",
            [table],
            |r| r.get(0),
        )?;
        if exists {
            guard(&tx, table)?;
        }
    }
    crate::character_projection::mirror_all(&tx)?;
    crate::schema::compact_characters(&tx)?;
    for flag in FLAGS {
        tx.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, 'core')", [flag])?;
    }
    tx.execute("INSERT OR IGNORE INTO meta(key, value) VALUES ('names_version', '0')", [])?;
    commit_writing(tx)?;
    crate::schema::compact_pending(conn);
    Ok(())
}
