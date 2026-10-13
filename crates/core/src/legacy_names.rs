//! 名字收编（docs/TERMS_NAMES_PLAN.md N2）：`names`、`display_names`、`name_aliases` 三张 Python 表收进核心主数据。
//! - `names`（按身份）→ `term_names`，只动角色 / 皮肤 / 作品（`term_tree` 里的词条）的行；画师、用户概念的名字不碰。
//! - `display_names`（按文字键：作品 qualifier、皮肤 variant）→ `term_key_names`。
//! - `name_aliases`（按文字键的别名，搜索用）→ `term_key_aliases`。
//!
//! 与角色收编相同的套路：只读旧表、把主数据补到与旧表一致、再由主数据生成结果与旧表逐行比对；一个事务，预检失败或
//! 有差异都整次回滚。名字表是状态不是历史（改名的撤销快照在 actions 里，由角色撤销处理），没有「出处」要解释。
//! 可以反复运行；旧表没变时不改任何行。
use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{json, Map, Value};

pub const TABLES: &str = "
CREATE TABLE IF NOT EXISTS term_key_names(
 kind TEXT NOT NULL, key TEXT NOT NULL, lang TEXT NOT NULL, source TEXT NOT NULL, value TEXT NOT NULL,
 PRIMARY KEY(kind,key,lang,source,value)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS term_key_aliases(
 kind TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, position INTEGER NOT NULL,
 PRIMARY KEY(kind,key,value)) WITHOUT ROWID;
";

/// 旧表 → 核心：每对（旧的样子，核心生成的样子），列一一对应。
const PAIRS: [(&str, &str, &str); 3] = [
    (
        "names",
        "SELECT entity_id,lang,value,source,role,preferred,position FROM names",
        "SELECT t.key,n.lang,n.value,n.source,n.role,n.preferred,n.position FROM term_names n
         JOIN terms t ON t.id=n.term_id JOIN term_tree r ON r.term_id=t.id",
    ),
    (
        "display_names",
        "SELECT kind,key,lang,source,value FROM display_names",
        "SELECT kind,key,lang,source,value FROM term_key_names",
    ),
    (
        "name_aliases",
        "SELECT kind,key,value,position FROM name_aliases",
        "SELECT kind,key,value,position FROM term_key_aliases",
    ),
];

#[derive(Default, Debug)]
pub struct Report {
    pub preflight: Vec<Value>,
    pub changed: Map<String, Value>,
    pub tables: Map<String, Value>,
    pub committed: bool,
}

impl Report {
    pub fn clean(&self) -> bool {
        self.preflight.is_empty()
            && self.tables.values().all(|t| t["missing"].as_i64() == Some(0) && t["extra"].as_i64() == Some(0))
    }

    pub fn to_json(&self) -> Value {
        json!({"preflight": self.preflight, "changed": self.changed, "tables": self.tables, "committed": self.committed})
    }
}

fn count(c: &Connection, sql: &str) -> Result<i64> {
    Ok(c.query_row(&format!("SELECT count(*) FROM ({sql})"), [], |r| r.get(0))?)
}

fn samples(c: &Connection, sql: &str) -> Result<Vec<Value>> {
    let mut stmt = c.prepare(&format!("SELECT * FROM ({sql}) LIMIT 5"))?;
    let n = stmt.column_count();
    let rows = stmt.query_map([], |r| {
        (0..n)
            .map(|i| {
                Ok(match r.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(v) => json!(v),
                    rusqlite::types::ValueRef::Real(v) => json!(v),
                    rusqlite::types::ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                    rusqlite::types::ValueRef::Blob(_) => json!("<blob>"),
                })
            })
            .collect::<rusqlite::Result<Vec<Value>>>()
            .map(Value::Array)
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn preflight(c: &Connection, r: &mut Report) -> Result<()> {
    for (check, sql) in [
        ("名字属于不在核心里的身份", "SELECT entity_id FROM names WHERE entity_id NOT IN (SELECT t.key FROM terms t JOIN term_tree r ON r.term_id=t.id)"),
        ("名字为空或超过 200 字", "SELECT entity_id,value FROM names WHERE length(value) NOT BETWEEN 1 AND 200"),
        ("名字的语言或角色不合约定", "SELECT entity_id,lang,role FROM names WHERE lang NOT IN ('','zh','ja','en') OR role NOT IN ('display','alias')"),
        ("首选不是 0 / 1", "SELECT entity_id,preferred FROM names WHERE preferred NOT IN (0,1)"),
    ] {
        let n = count(c, sql)?;
        if n > 0 {
            r.preflight.push(json!({"check": check, "count": n, "samples": samples(c, sql)?}));
        }
    }
    Ok(())
}

/// 主数据补到与旧表一致：多的删、少的加（整行比较，首选、顺序变了就是一删一加）。返回每张表改了多少行。
fn mirror(c: &Connection, r: &mut Report) -> Result<()> {
    let names_scope = "term_id IN (SELECT term_id FROM term_tree)";
    let removed = c.execute(
        &format!(
            "DELETE FROM term_names WHERE {names_scope} AND (term_id,lang,value,source,role,preferred,position) NOT IN (
               SELECT t.id,n.lang,n.value,n.source,n.role,n.preferred,n.position FROM names n JOIN terms t ON t.key=n.entity_id)"
        ),
        [],
    )?;
    let added = c.execute(
        "INSERT INTO term_names(term_id,lang,value,source,role,preferred,position)
         SELECT t.id,n.lang,n.value,n.source,n.role,n.preferred,n.position FROM names n
         JOIN terms t ON t.key=n.entity_id JOIN term_tree r ON r.term_id=t.id
         WHERE NOT EXISTS(SELECT 1 FROM term_names x WHERE x.term_id=t.id AND x.role=n.role AND x.lang=n.lang
                          AND x.source=n.source AND x.value=n.value AND x.preferred=n.preferred AND x.position=n.position)
         ON CONFLICT(term_id,role,lang,source,value) DO UPDATE SET preferred=excluded.preferred,position=excluded.position",
        [],
    )?;
    r.changed.insert("names".into(), json!(removed + added));
    let removed = c.execute(
        "DELETE FROM term_key_names WHERE (kind,key,lang,source,value) NOT IN (SELECT kind,key,lang,source,value FROM display_names)",
        [],
    )?;
    let added =
        c.execute("INSERT OR IGNORE INTO term_key_names SELECT kind,key,lang,source,value FROM display_names", [])?;
    r.changed.insert("display_names".into(), json!(removed + added));
    let removed = c.execute(
        "DELETE FROM term_key_aliases WHERE (kind,key,value,position) NOT IN (SELECT kind,key,value,position FROM name_aliases)",
        [],
    )?;
    let added = c.execute(
        "INSERT INTO term_key_aliases SELECT kind,key,value,position FROM name_aliases WHERE true
         ON CONFLICT(kind,key,value) DO UPDATE SET position=excluded.position WHERE position<>excluded.position",
        [],
    )?;
    r.changed.insert("name_aliases".into(), json!(removed + added));
    Ok(())
}

/// 由主数据生成的样子与旧表逐行比较。
pub fn compare(c: &Connection, r: &mut Report) -> Result<()> {
    for (table, old, new) in PAIRS {
        let (missing_sql, extra_sql) = (format!("{old} EXCEPT {new}"), format!("{new} EXCEPT {old}"));
        let (missing, extra) = (count(c, &missing_sql)?, count(c, &extra_sql)?);
        let mut entry = json!({"old": count(c, old)?, "core": count(c, new)?, "missing": missing, "extra": extra});
        if missing > 0 {
            entry["missing_samples"] = json!(samples(c, &missing_sql)?);
        }
        if extra > 0 {
            entry["extra_samples"] = json!(samples(c, &extra_sql)?);
        }
        r.tables.insert(table.into(), entry);
    }
    Ok(())
}

/// `illustash-terms sync-names`：前提是角色已切到核心（名字挂在角色的词条上）。一个事务，通过才提交。
pub fn verify(conn: &mut Connection) -> Result<Report> {
    crate::schema::require_current(conn)?;
    let mut r = Report::default();
    compare(conn, &mut r)?;
    r.committed = r.clean();
    Ok(r)
}

pub fn sync(conn: &mut Connection) -> Result<Report> {
    crate::schema::require_current(conn)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if !crate::legacy_characters::characters_in_core(&tx)? {
        return Err(Error::Conflict("先把角色切到词条核心，再收编名字".into()));
    }
    let mut r = Report::default();
    preflight(&tx, &mut r)?;
    if !r.preflight.is_empty() {
        return Ok(r);
    }
    mirror(&tx, &mut r)?;
    compare(&tx, &mut r)?;
    if r.clean() {
        tx.commit()?;
        r.committed = true;
    }
    Ok(r)
}

pub const NAMES_IN_CORE: &str = "terms_names";
/// 主数据与投影都已同步的 `names_version`（`entities`、`links`、`names`、`display_names` 触发器递增它）。
const PROJECTED: &str = "names_projected";
/// 显示投影可以先于主数据镜像完成，两者分别记版本，避免事务中途读取后再次重建。
const DISPLAY_PROJECTED: &str = "names_display_projected";
/// 切换后由核心维护、加写保护的名字兼容表。
pub const GUARDED: [&str; 3] = ["names", "display_names", "name_aliases"];

pub fn names_in_core(c: &Connection) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM meta WHERE key=?1 AND value='core')", [NAMES_IN_CORE], |r| r.get(0))?)
}

fn meta(c: &Connection, key: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(c.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional()?)
}

/// 只在当前连接的写事务内追踪名字变化；结构变更或旧投影过期时退回完整重建。
/// TEMP 表与触发器不改变持久 schema，事务回滚会一并撤销追踪状态。
pub fn track(c: &Connection) -> Result<()> {
    if !names_in_core(c)? {
        return Ok(());
    }
    c.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS names_dirty_entities(id TEXT PRIMARY KEY);
        CREATE TEMP TABLE IF NOT EXISTS names_dirty_keys(kind TEXT,key TEXT,PRIMARY KEY(kind,key));
        CREATE TEMP TABLE IF NOT EXISTS names_dirty_aliases(kind TEXT,key TEXT,PRIMARY KEY(kind,key));
        CREATE TEMP TABLE IF NOT EXISTS names_dirty_full(value INTEGER PRIMARY KEY);
        DELETE FROM names_dirty_entities; DELETE FROM names_dirty_keys; DELETE FROM names_dirty_aliases;
        DELETE FROM names_dirty_full;",
    )?;
    if meta(c, "names_version")? != meta(c, PROJECTED)? {
        c.execute("INSERT INTO names_dirty_full VALUES (2)", [])?;
    }
    // 外层 UPSERT 的冲突策略可能覆盖触发器的 OR IGNORE，用存在性条件避免重复插入。
    for table in ["entities", "links", "names", "display_names"] {
        for event in ["INSERT", "UPDATE", "DELETE"] {
            let mut body = String::new();
            if matches!(table, "entities" | "links") {
                body.push_str(
                    "INSERT INTO names_dirty_full SELECT 1 WHERE NOT EXISTS(SELECT 1 FROM names_dirty_full WHERE value=1);",
                );
                if table == "entities" {
                    for side in ["old", "new"] {
                        if (event == "INSERT" && side == "old") || (event == "DELETE" && side == "new") {
                            continue;
                        }
                        body.push_str(&format!("INSERT INTO names_dirty_entities SELECT {side}.id WHERE NOT EXISTS(SELECT 1 FROM names_dirty_entities WHERE id={side}.id);"));
                    }
                }
            } else {
                for side in ["old", "new"] {
                    if (event == "INSERT" && side == "old") || (event == "DELETE" && side == "new") {
                        continue;
                    }
                    if table == "names" {
                        body.push_str(&format!(
                            "INSERT INTO names_dirty_entities SELECT {side}.entity_id WHERE NOT EXISTS(SELECT 1 FROM names_dirty_entities WHERE id={side}.entity_id);"
                        ));
                    } else {
                        body.push_str(&format!(
                            "INSERT INTO names_dirty_keys SELECT {side}.kind,{side}.key WHERE NOT EXISTS(SELECT 1 FROM names_dirty_keys WHERE kind={side}.kind AND key={side}.key);"
                        ));
                    }
                }
            }
            c.execute_batch(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS track_names_{table}_{event}
                AFTER {event} ON main.{table} BEGIN {body} END;"
            ))?;
        }
    }
    // 别名表不在 `names_version` 的触发器里，单独记下受影响的键，提交时只同步这些键（#102 A1）。
    for event in ["INSERT", "UPDATE", "DELETE"] {
        let mut body = String::new();
        for side in ["old", "new"] {
            if (event == "INSERT" && side == "old") || (event == "DELETE" && side == "new") {
                continue;
            }
            body.push_str(&format!(
                "INSERT INTO names_dirty_aliases SELECT {side}.kind,{side}.key WHERE NOT EXISTS(SELECT 1 FROM names_dirty_aliases WHERE kind={side}.kind AND key={side}.key);"
            ));
        }
        c.execute_batch(&format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS track_names_name_aliases_{event}
            AFTER {event} ON main.name_aliases BEGIN {body} END;"
        ))?;
    }
    Ok(())
}

pub fn tracked(c: &Connection) -> Result<bool> {
    let exists: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name='names_dirty_full')", [], |r| r.get(0))?;
    Ok(exists && !c.query_row("SELECT EXISTS(SELECT 1 FROM names_dirty_full)", [], |r| r.get::<_, bool>(0))?)
}

/// 结构变更可能需要全量显示投影，但名字主数据仍只受变更身份影响。
/// 若开始时投影已经过期（值 2），来源未被本事务完整追踪，保留全量同步。
fn tracked_master(c: &Connection) -> Result<bool> {
    let exists: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name='names_dirty_full')", [], |r| r.get(0))?;
    Ok(exists
        && !c.query_row("SELECT EXISTS(SELECT 1 FROM names_dirty_full WHERE value=2)", [], |r| r.get::<_, bool>(0))?)
}

/// 按本事务记下的身份和键补齐名字主数据。由 dirty 表驱动（`CROSS JOIN` 固定连接顺序），不扫整张 `names`、`terms`、
/// `display_names`（#102 A2）。`term_names` 是普通表，固定契约连隐藏 rowid 一起比较，所以插入仍按 `names` 的 rowid
/// 顺序（原计划整表扫描 `names` 的顺序），新行分到的 rowid 与原来相同。
fn mirror_changed(c: &Connection) -> Result<()> {
    c.execute_batch(
        "DELETE FROM term_names WHERE term_id IN (
            SELECT t.id FROM names_dirty_entities d CROSS JOIN terms t ON t.key=d.id CROSS JOIN term_tree r ON r.term_id=t.id)
        AND NOT EXISTS(SELECT 1 FROM names n JOIN terms t ON t.key=n.entity_id
            WHERE t.id=term_names.term_id AND n.lang=term_names.lang AND n.value=term_names.value
            AND n.source=term_names.source AND n.role=term_names.role
            AND n.preferred=term_names.preferred AND n.position=term_names.position);
        INSERT INTO term_names(term_id,lang,value,source,role,preferred,position)
        SELECT t.id,n.lang,n.value,n.source,n.role,n.preferred,n.position FROM names_dirty_entities d
        CROSS JOIN names n ON n.entity_id=d.id CROSS JOIN terms t ON t.key=n.entity_id
        CROSS JOIN term_tree r ON r.term_id=t.id WHERE true ORDER BY n.rowid
        ON CONFLICT(term_id,role,lang,source,value) DO UPDATE SET preferred=excluded.preferred,position=excluded.position;
        DELETE FROM term_key_names WHERE (kind,key) IN (SELECT kind,key FROM names_dirty_keys);
        INSERT INTO term_key_names SELECT n.kind,n.key,n.lang,n.source,n.value FROM names_dirty_keys d
        CROSS JOIN display_names n ON n.kind=d.kind AND n.key=d.key;",
    )?;
    Ok(())
}

/// 别名表只影响搜索，不在 `names_version` 的触发器里：提交前按本事务记下的键同步（#102 A1）。没有追踪（这个连接
/// 没开过核心写事务）时退回整表同步。`term_key_aliases` 是 WITHOUT ROWID 表，写入顺序不影响比较。
fn mirror_aliases_changed(c: &Connection) -> Result<bool> {
    let tracked: bool =
        c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_temp_master WHERE name='names_dirty_aliases')", [], |r| {
            r.get(0)
        })?;
    if !tracked {
        mirror_aliases(c)?;
        return Ok(false);
    }
    c.execute_batch(
        "DELETE FROM term_key_aliases WHERE (kind,key) IN (SELECT kind,key FROM names_dirty_aliases)
         AND NOT EXISTS(SELECT 1 FROM name_aliases a WHERE a.kind=term_key_aliases.kind AND a.key=term_key_aliases.key
          AND a.value=term_key_aliases.value AND a.position=term_key_aliases.position);
        INSERT INTO term_key_aliases SELECT a.kind,a.key,a.value,a.position FROM names_dirty_aliases d
         CROSS JOIN name_aliases a ON a.kind=d.kind AND a.key=d.key WHERE true
         ON CONFLICT(kind,key,value) DO UPDATE SET position=excluded.position WHERE position<>excluded.position;",
    )?;
    Ok(true)
}

/// 整表同步别名：多的删、少的加。只用于没有追踪的事务和对照核对。
fn mirror_aliases(c: &Connection) -> Result<()> {
    c.execute(
        "DELETE FROM term_key_aliases WHERE (kind,key,value,position) NOT IN (SELECT kind,key,value,position FROM name_aliases)",
        [],
    )?;
    c.execute(
        "INSERT INTO term_key_aliases SELECT kind,key,value,position FROM name_aliases WHERE true
         ON CONFLICT(kind,key,value) DO UPDATE SET position=excluded.position WHERE position<>excluded.position",
        [],
    )?;
    Ok(())
}

/// 每个核心写事务提交前（schema::commit_writing）：名字已在核心、且这个事务改到了名字相关的数据，就在同一事务里
/// 补齐主数据、维护显示投影。名字行使用依赖闭包，结构变更 / 过期状态回退整表。别名每个事务只同步一次（#102 A1）。
pub fn after_write(c: &Connection) -> Result<()> {
    after_write_with(c, None)
}

/// 同 `after_write`；`view` 是调用方手里与这个事务此刻的库逐项相同的名字快照（名字更新器吸收了本事务写入的批快照，
/// #102 A5a），重建投影时用它，不再整库载入。
pub fn after_write_with(c: &Connection, view: Option<&crate::names_view::View>) -> Result<()> {
    if !names_in_core(c)? {
        return Ok(());
    }
    let version = meta(c, "names_version")?;
    if version.is_some() && version == meta(c, PROJECTED)? {
        // 名字版本没变：别名表不在版本触发器里，仍可能改过。
        if mirror_aliases_changed(c)? {
            verify_incremental(c, "aliases")?;
        }
        return Ok(());
    }
    if tracked_master(c)? {
        mirror_changed(c)?;
        mirror_aliases_changed(c)?;
        verify_incremental(c, "names")?;
    } else {
        mirror(c, &mut Report::default())?;
    }
    refresh_projection_with(c, &mut None, view)?;
    c.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
        rusqlite::params![PROJECTED, version.unwrap_or_default()],
    )?;
    Ok(())
}

/// 对照核对的开关（#102；#38 2026-10-12 用户决定）：只在测试特性 `verify-incremental` 下编译。设置了环境变量
/// `ILLUSTASH_TEST_VERIFY_INCREMENTAL`（一个记录文件的路径）时，每次走增量路径都在同一事务里核对「增量结果 = 整表
/// 重建」，并往记录文件追加一行核对的种类，测试据此证明核对确实执行过。产品构建里恒为关闭。
#[cfg(feature = "verify-incremental")]
pub fn verifying() -> bool {
    std::env::var_os(VERIFY_ENV).is_some_and(|v| !v.is_empty())
}
#[cfg(not(feature = "verify-incremental"))]
pub fn verifying() -> bool {
    false
}
#[cfg(feature = "verify-incremental")]
const VERIFY_ENV: &str = "ILLUSTASH_TEST_VERIFY_INCREMENTAL";

/// 记下一次已通过的核对（见 `verifying`）。
#[cfg(feature = "verify-incremental")]
pub fn record_verified(kind: &str) -> Result<()> {
    use std::io::Write;
    let Some(path) = std::env::var_os(VERIFY_ENV).filter(|v| !v.is_empty()) else { return Ok(()) };
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(format!("{kind}\n").as_bytes())?;
    Ok(())
}
#[cfg(not(feature = "verify-incremental"))]
pub fn record_verified(_kind: &str) -> Result<()> {
    Ok(())
}

/// 增量镜像之后、同一事务里：在保存点里再跑一遍整表同步，它不应改动任何一行；随后回滚保存点。
fn verify_incremental(c: &Connection, kind: &str) -> Result<()> {
    if !verifying() {
        return Ok(());
    }
    c.execute_batch("SAVEPOINT verify_incremental")?;
    let mut r = Report::default();
    let full = mirror(c, &mut r);
    c.execute_batch("ROLLBACK TO verify_incremental; RELEASE verify_incremental")?;
    full?;
    if r.changed.values().any(|v| v.as_i64() != Some(0)) {
        return Err(Error::Conflict(format!(
            "增量名字镜像与整表重建不一致（{kind}）：整表重建还要改 {}",
            Value::Object(r.changed)
        )));
    }
    record_verified(&format!("mirror_{kind}"))
}

/// 事务中途要读显示名（核心生成历史文案，character_labels）：这个事务改过名字相关的数据就先重建显示投影。只重建投影
/// （它只读兼容表），不补主数据、也不记 `names_projected`：主数据按 `term_tree` 挑身份，事务里后面的结构镜像可能还没
/// 做；显示版本单独记录，提交时仍补主数据，但只在版本再次变化时重建投影。
/// `seen`：调用方上次重建时的版本，没变就不再重建。
pub fn refresh_projection(c: &Connection, seen: &mut Option<String>) -> Result<()> {
    refresh_projection_with(c, seen, None)
}

fn refresh_projection_with(
    c: &Connection,
    seen: &mut Option<String>,
    view: Option<&crate::names_view::View>,
) -> Result<()> {
    let version = meta(c, "names_version")?;
    if version.is_some()
        && (version == *seen || version == meta(c, PROJECTED)? || version == meta(c, DISPLAY_PROJECTED)?)
    {
        return Ok(());
    }
    match (tracked(c)?, view) {
        (true, Some(view)) => crate::names_view::rebuild_changed_from(c, view)?,
        (true, None) => crate::names_view::rebuild_changed(c)?,
        (false, Some(view)) => crate::names_view::rebuild_from(c, view)?,
        (false, None) => crate::names_view::rebuild(c)?,
    };
    c.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES (?1,?2)",
        rusqlite::params![DISPLAY_PROJECTED, version.clone().unwrap_or_default()],
    )?;
    *seen = version;
    Ok(())
}

/// `illustash-terms cutover-names`：一个事务里最后一次收编（必须零差异）、整表建投影、名字兼容表加写保护、写
/// `terms_names=core`。前提是角色已在核心。已经切过就什么都不做，返回 None；失败什么都不留。
pub fn cutover_with_backup(conn: &mut Connection, backup: &crate::backup::RequiredBackup) -> Result<Option<Report>> {
    crate::schema_rules::migrate_with_backup(conn, backup)?;
    if names_in_core(conn)? {
        return Ok(None);
    }
    crate::schema_rules::backup_before_cutover(conn, "names", backup)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if names_in_core(&tx)? {
        return Ok(None);
    }
    if !crate::legacy_characters::characters_in_core(&tx)? {
        return Err(Error::Conflict("先把角色切到词条核心，再切名字".into()));
    }
    crate::schema::begin_writing(&tx)?;
    let mut r = Report::default();
    preflight(&tx, &mut r)?;
    if r.preflight.is_empty() {
        mirror(&tx, &mut r)?;
        compare(&tx, &mut r)?;
    }
    if !r.clean() {
        return Err(Error::Conflict(format!("名字收编核对没有通过，不切换：{}", r.to_json())));
    }
    for table in GUARDED {
        crate::schema::guard(&tx, table)?;
    }
    tx.execute("INSERT OR REPLACE INTO meta(key,value) VALUES (?1,'core')", [NAMES_IN_CORE])?;
    tx.execute("INSERT OR IGNORE INTO meta(key,value) VALUES ('names_version','0')", [])?;
    after_write(&tx)?;
    crate::schema::commit_writing(tx)?;
    r.committed = true;
    Ok(Some(r))
}
