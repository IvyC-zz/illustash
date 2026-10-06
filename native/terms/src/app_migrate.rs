//! Existing web.library.upgrade_source / Store._migrate / initialize semantics.
use crate::{app_library::Library, Error, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path};
fn meta(c: &Connection, key: &str) -> Result<Option<String>> {
    if !c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='meta')", [], |r| {
        r.get::<_, bool>(0)
    })? {
        return Ok(None);
    }
    Ok(c.query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0)).optional()?)
}
fn columns(c: &Connection, table: &str) -> Result<BTreeSet<String>> {
    Ok(c.prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?)
}
fn add(c: &Connection, name: &str, ddl: &str) -> Result<()> {
    if !columns(c, "entities")?.contains(name) {
        c.execute_batch(&format!("ALTER TABLE entities ADD COLUMN {name} {ddl}"))?;
    }
    Ok(())
}
fn label_only(c: &Connection) -> Result<()> {
    for (n, t) in [
        ("label_only", "INTEGER NOT NULL DEFAULT 0"),
        ("mascot", "INTEGER NOT NULL DEFAULT 0"),
        ("mascot_origin", "TEXT"),
    ] {
        add(c, n, t)?;
    }
    Ok(())
}
pub(crate) fn source(path: &Path, config: &Value) -> Result<()> {
    let mut c = crate::open(path)?;
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let cols = columns(&tx, "assets")?;
    for col in ["source_id", "preset"] {
        if !cols.contains(col) {
            tx.execute_batch(&format!("ALTER TABLE assets ADD COLUMN {col} TEXT"))?;
        }
    }
    tx.execute_batch(include_str!("app_source_schema.sql"))?;
    if let Some(photos) = config["sources"].as_array().and_then(|s| s.iter().find(|s| s["kind"] == "photos")) {
        let missing: Vec<String> = tx
            .prepare("SELECT uuid FROM assets WHERE source_id IS NULL OR preset IS NULL")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        if !missing.is_empty() {
            let mut apps = std::collections::HashMap::<String, String>::new();
            // Same read-only optional Photos lookup as Python, no scan or file/image access.
            if let Some(path) = photos["path"].as_str() {
                let path = if let Some(p) = path.strip_prefix("~/") {
                    crate::app_paths::user_home()?.join(p)
                } else {
                    path.into()
                };
                if let Ok(db) = crate::app_library::read_connection(&path.join("database/Photos.sqlite")) {
                    if let Ok(mut q)=db.prepare("SELECT a.ZUUID,coalesce(d.ZIMPORTEDBYBUNDLEIDENTIFIER,'') FROM ZASSET a LEFT JOIN ZADDITIONALASSETATTRIBUTES d ON a.ZADDITIONALATTRIBUTES=d.Z_PK WHERE a.ZKIND=0 AND a.ZTRASHEDSTATE=0") {
                        if let Ok(rows)=q.query_map([],|r|Ok((r.get::<_,String>(0)?.to_uppercase(),r.get::<_,String>(1)?))) {
                            if let Ok(rows)=rows.collect::<rusqlite::Result<Vec<_>>>() { apps.extend(rows); }
                        }
                    }
                }
            }
            let id = photos["id"].as_str().ok_or_else(|| Error::Invalid("Photos 来源缺少 id".into()))?;
            // 显式旧包升级保持历史口径：缺少 apps 时按其他回填，不能套用扫描默认映射。
            let mapping = &photos["apps"];
            for u in missing {
                let preset = apps.get(&u).map(|a| mapping[a].as_str().unwrap_or(crate::origin::get("").id()));
                tx.execute(
                    "UPDATE assets SET source_id=?, preset=coalesce(?,preset) WHERE uuid=?",
                    params![id, preset, u],
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO asset_links(asset,system,key,source_id) VALUES (?,'photos',?,?)",
                    params![u, u, id],
                )?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}
fn derived_statement(prefix: &str) -> Result<&'static str> {
    include_str!("app_migration_sql/derived.sql")
        .split(';')
        .find(|s| s.trim_start().starts_with(prefix))
        .ok_or_else(|| Error::Schema("缺少迁移 SQL".into()))
}
fn lead(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS lead_assets(uuid TEXT PRIMARY KEY)")?;
    c.execute_batch(derived_statement("CREATE TABLE IF NOT EXISTS tag_memberships")?)?;
    c.execute_batch(derived_statement("CREATE INDEX IF NOT EXISTS tag_memberships_uuid")?)?;
    if meta(c, "identity_policy")?.as_deref() != Some("hybrid") {
        c.execute_batch("DROP VIEW IF EXISTS membership_rules")?;
        c.execute_batch(derived_statement("CREATE VIEW IF NOT EXISTS membership_rules")?)?;
    }
    c.execute_batch("DROP VIEW IF EXISTS ambiguous_claims")?;
    c.execute_batch(include_str!("app_migration_sql/ambiguous.sql"))?;
    crate::character_projection::legacy_evidence(c)?;
    crate::character_projection::single(c, None)?;
    crate::character_projection::legacy_memberships(c)
}
fn names_from_tables(c: &Connection) -> Result<()> {
    let rows: Vec<(String, String, String, String)> = c
        .prepare("SELECT key,lang,source,value FROM display_names WHERE kind='character'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (key, lang, source, value) in rows {
        for id in tag_entities(c, &key)? {
            crate::character_names::put_name(c, &id, &lang, &source, &value, "display", 0, 0)?;
        }
    }
    c.execute("DELETE FROM display_names WHERE kind='character'", [])?;
    let rows: Vec<(String, String, i64)> = c
        .prepare("SELECT key,value,position FROM name_aliases WHERE kind='character'")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (key, value, pos) in rows {
        for id in tag_entities(c, &key)? {
            crate::character_names::put_name(
                c,
                &id,
                if pos >= 10000 { "zh" } else { "" },
                if pos >= 10000 { "bangumi" } else { "danbooru" },
                &value,
                "alias",
                pos,
                0,
            )?;
        }
    }
    c.execute("DELETE FROM name_aliases WHERE kind='character'", [])?;
    if c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='aliases' AND type='table')", [], |r| {
        r.get::<_, bool>(0)
    })? {
        c.execute("INSERT OR IGNORE INTO names(entity_id,value,source,role) SELECT entity,alias,'legacy','alias' FROM aliases WHERE trim(alias)<>''",[])?;
        retire_aliases(c)?;
    }
    let rows: Vec<(String, String, String, bool)> = c
        .prepare(
            "SELECT id,name,kind,EXISTS(SELECT 1 FROM links WHERE entity_id=e.id) FROM entities e WHERE trim(name)<>''",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, name, kind, linked) in rows {
        let alias = linked && kind != "oc";
        crate::character_names::put_name(
            c,
            &id,
            if alias { "" } else { crate::character_labels::lang_of(&name) },
            if alias { "legacy" } else { "user" },
            &name,
            if alias { "alias" } else { "display" },
            0,
            if alias { 0 } else { 1 },
        )?;
    }
    c.execute("UPDATE entities SET name='' WHERE name<>''", [])?;
    Ok(())
}
fn retire_aliases(c: &Connection) -> Result<()> {
    c.execute_batch("DROP TABLE aliases")?;
    Ok(())
}
fn tag_entities(c: &Connection, key: &str) -> Result<Vec<String>> {
    for system in ["danbooru", "wd", "photos_album", "ccip"] {
        let ids = c
            .prepare("SELECT entity_id FROM links WHERE system=? AND key=? AND current=1")?
            .query_map([system, key], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Ok(c.prepare("SELECT entity_id FROM links WHERE system='ccip_base' AND json_extract(key,'$[0]')=?")?
        .query_map([key], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}
fn works(c: &Connection) -> Result<()> {
    add(c, "work_id", "TEXT REFERENCES entities(id)")?;
    add(c, "original", "INTEGER NOT NULL DEFAULT 0")?;
    c.execute_batch("CREATE INDEX IF NOT EXISTS entities_work_id ON entities(work_id); CREATE TABLE IF NOT EXISTS work_review(entity_id TEXT PRIMARY KEY REFERENCES entities(id),\n        status TEXT NOT NULL DEFAULT 'pending',note TEXT NOT NULL DEFAULT '',parent_locked INTEGER NOT NULL DEFAULT 0);")?;
    c.execute("UPDATE entities SET original=1 WHERE kind='oc'", [])?;
    c.execute(
        "UPDATE entities SET kind=CASE WHEN parent_id IS NULL THEN 'character' ELSE 'skin' END WHERE kind<>'work'",
        [],
    )?;
    let keys: Vec<String> = c
        .prepare("SELECT DISTINCT work FROM entities WHERE work<>'' AND kind<>'work'")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for key in keys {
        let id: Option<String> = c
            .query_row("SELECT entity_id FROM links WHERE system='qualifier' AND key=?", [&key], |r| r.get(0))
            .optional()?;
        let id = match id {
            Some(id) => id,
            None => {
                let id = crate::store::new_key()?;
                c.execute("INSERT INTO entities(id,name,kind) VALUES (?,'','work')", [&id])?;
                c.execute(
                    "INSERT INTO links(system,key,entity_id,source) VALUES ('qualifier',?,?,'import')",
                    params![key, id],
                )?;
                crate::character_names::put_name(
                    c,
                    &id,
                    "en",
                    "index",
                    &crate::names_view::readable(&key, false),
                    "display",
                    0,
                    0,
                )?;
                id
            }
        };
        c.execute("INSERT OR IGNORE INTO work_review(entity_id) VALUES (?)", [&id])?;
        let rows: Vec<(String, String, String)> = c
            .prepare("SELECT lang,source,value FROM display_names WHERE kind='work' AND key=?")?
            .query_map([&key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (lang, source, value) in rows {
            crate::character_names::put_name(c, &id, &lang, &source, &value, "display", 0, 0)?;
        }
        let rows: Vec<(String, i64)> = c
            .prepare("SELECT value,position FROM name_aliases WHERE kind='work' AND key=?")?
            .query_map([&key], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (value, pos) in rows {
            crate::character_names::put_name(c, &id, "", "danbooru", &value, "alias", pos, 0)?;
        }
        c.execute("UPDATE entities SET work_id=? WHERE work=? AND kind='character' AND original=0", params![id, key])?;
    }
    c.execute_batch("UPDATE entities SET work='',work_id=NULL WHERE original=1; UPDATE entities SET work_id=NULL WHERE kind='skin';")?;
    Ok(())
}
fn links(c: &Connection) -> Result<()> {
    for (prefix, system) in [("index:", "ccip"), ("wd:", "wd"), ("album:", "photos_album"), ("parent:", "ccip_base")] {
        c.execute("INSERT OR IGNORE INTO links(system,key,entity_id,source) SELECT ?,substr(external_key,?),id,'import' FROM entities WHERE external_key LIKE ? ESCAPE '\\'",params![system,prefix.len()+1,format!("{prefix}%")])?;
    }
    c.execute_batch("INSERT OR IGNORE INTO links(system,key,entity_id,source) SELECT 'danbooru',tag,id,'import' FROM entities WHERE coalesce(tag,'')<>'' AND (external_key LIKE 'index:%' OR external_key LIKE 'wd:%'); UPDATE entities SET external_key=NULL,tag=NULL WHERE external_key IS NOT NULL OR tag IS NOT NULL;")?;
    Ok(())
}
fn migrate(c: &Connection) -> Result<()> {
    let version = meta(c, "schema_version")?.and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    if version == 36 {
        return Ok(());
    }
    if version >= 25 {
        label_only(c)?;
        c.execute_batch(include_str!("app_migration_sql/identity.sql"))?;
        c.execute_batch(include_str!("app_migration_sql/ambiguous.sql"))?;
        if meta(c, "identity_policy")?.as_deref() == Some("hybrid") {
            c.execute_batch(include_str!("app_migration_sql/hybrid.sql"))?;
        }
        c.execute_batch(include_str!("app_migration_sql/asked.sql"))?;
        c.execute("UPDATE entities SET mascot_origin='user' WHERE mascot=1 AND mascot_origin IS NULL", [])?;
        crate::character_import::default_tags(c, &mut BTreeSet::new())?;
        lead(c)?;
    } else {
        // Python skips indexes whose target is already a labels-core view.
        for sql in include_str!("app_migration_sql/base.sql")
            .lines()
            .filter(|s| !s.starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n")
            .split(';')
            .filter(|s| !s.trim().is_empty() && !s.trim_start().starts_with("--"))
        {
            let tokens = sql.split_whitespace().collect::<Vec<_>>();
            if let Some(i) = tokens.iter().position(|t| t.eq_ignore_ascii_case("ON")) {
                if sql.trim_start().to_uppercase().starts_with("CREATE INDEX")
                    && c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='view' AND name=?)",
                        [tokens[i + 1].split('(').next().expect("app_migrate internal invariant")],
                        |r| r.get::<_, bool>(0),
                    )?
                {
                    continue;
                }
            }
            c.execute_batch(sql)?;
        }
        c.execute_batch("DELETE FROM display_names WHERE source='danbooru'; UPDATE OR IGNORE display_names SET source='model' WHERE source='deepseek'; DELETE FROM display_names WHERE source='deepseek';")?;
        for (key, title) in crate::names_view::WORK_NAMES {
            let ids: Vec<String> = c
                .prepare("SELECT entity_id FROM links WHERE system='qualifier' AND key=?")?
                .query_map([key], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in ids {
                crate::character_names::put_name(c, &id, "zh", "builtin", title, "display", 0, 0)?;
            }
            c.execute("INSERT OR IGNORE INTO display_names VALUES ('work',?,'zh','builtin',?)", params![key, title])?;
        }
        if version < 18 {
            for (n, t) in [
                ("variant", "TEXT NOT NULL DEFAULT ''"),
                ("parent_id", "TEXT REFERENCES entities(id)"),
                ("tag", "TEXT"),
                ("merged_into", "TEXT REFERENCES entities(id)"),
                ("kind", "TEXT NOT NULL DEFAULT ''"),
                ("artist", "TEXT NOT NULL DEFAULT ''"),
            ] {
                add(c, n, t)?;
            }
        }
        if version < 18 {
            label_only(c)?;
        }
        links(c)?;
        names_from_tables(c)?;
        works(c)?;
        if version >= 18 {
            c.execute_batch(include_str!("app_migration_sql/identity.sql"))?;
            c.execute_batch(include_str!("app_migration_sql/ambiguous.sql"))?;
            label_only(c)?;
            c.execute_batch(include_str!("app_migration_sql/asked.sql"))?;
            lead(c)?;
        } else {
            early_derived(c)?;
        }
    }
    c.execute("INSERT OR REPLACE INTO meta VALUES ('schema_version','36')", [])?;
    Ok(())
}
fn early_derived(c: &Connection) -> Result<()> {
    for name in [
        "variant_memberships",
        "memberships",
        "membership_rules",
        "family_confirmed",
        "family_evidence",
        "single_assets",
        "lead_assets",
        "tag_memberships",
        "duplicates",
        "auto_rules",
        "excluded",
        "clusters",
        "cluster_info",
        "prototypes",
        "proto_claims",
        "learn_state",
        "weak_claims",
        "similar_groups",
        "ambiguous_claims",
    ] {
        let kind: Option<String> =
            c.query_row("SELECT type FROM sqlite_master WHERE name=?", [name], |r| r.get(0)).optional()?;
        if let Some(kind) = kind {
            c.execute_batch(&format!("DROP {kind} {name}"))?;
        }
    }
    c.execute_batch(include_str!("app_migration_sql/derived.sql"))?;
    let src = crate::app_library::read_connection(
        Path::new(&c.path().expect("app_migrate internal invariant"))
            .parent()
            .expect("internal file path must have a parent")
            .join("source.sqlite")
            .as_path(),
    )?;
    legacy_hierarchy(c, &src)?;
    crate::app_migrate_data::early_groups(c, &src)?;
    crate::character_projection::legacy_evidence(c)?;
    crate::character_projection::single(c, None)?;
    crate::character_projection::legacy_memberships(c)?;
    let realistic = crate::app_migrate_data::rows(
        &src,
        "SELECT uuid FROM labels WHERE dim='general' AND value='realistic' AND conf>=0.15",
    )?
    .into_iter()
    .map(|r| r[0].clone())
    .collect::<Vec<_>>();
    crate::character_import::rules(c, &json!(realistic))?;
    crate::character_import::excluded(c)?;
    c.execute_batch(include_str!("app_migration_sql/identity.sql"))?;
    Ok(())
}

pub(crate) fn application(stage: &Path, original: &Library) -> Result<()> {
    let path = stage.join("library.sqlite");
    let source = stage.join("source.sqlite");
    let mut c = crate::open(&path)?;
    let born =
        !c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table')", [], |r| r.get::<_, bool>(0))?;
    if meta(&c, "terms_schema")?.is_some() {
        crate::migrate(&mut c)?;
    }
    // The private stage retains its own backup proof for the shared destructive identity step.
    // Prepare before taking the stage writer; original two-file publication/backup remains unchanged.
    let identity_backup = if meta(&c, "identity_policy")?.as_deref() == Some("v2") {
        Some(crate::schema_rules::prepare_upgrade_backup(
            &c,
            crate::schema::version(&c)?,
            &crate::platform_backup::capability(),
        )?)
    } else {
        None
    };
    let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
    migrate(&tx).map_err(|e| Error::Schema(format!("应用 schema 迁移失败：{e}")))?;
    if meta(&tx, "identity_policy")?.as_deref() == Some("v2") {
        tx.execute("UPDATE meta SET value='hybrid' WHERE key='identity_policy'", [])?;
        tx.execute_batch(include_str!("app_migration_sql/hybrid.sql"))?;
        crate::schema_rules::retire_identity_calibration_in(
            &tx,
            identity_backup.as_ref().ok_or_else(|| Error::Invalid("升级前备份没有生成".into()))?,
        )?;
        crate::character_projection::legacy_evidence(&tx)?;
        crate::character_projection::legacy_memberships(&tx)?;
    }
    if !born {
        crate::app_create::names_triggers(&tx)?;
    }
    if !born {
        tx.execute_batch(include_str!("app_migration_sql/effects.sql"))?;
    }
    if !born && !crate::store::artists_in_core(&tx)? {
        tx.execute_batch(include_str!("app_migration_sql/artists.sql"))?;
    }
    tx.commit()?;
    crate::migrate(&mut c)?;
    let provenance = original.path().join("provenance.sqlite");
    crate::legacy_artists::cutover(&mut c, provenance.is_file().then_some(provenance.as_path()))
        .map_err(|e| Error::Schema(format!("画师切换失败：{e}")))?;
    deliver_wd(&mut c, &source, original.id(), None, false)?;
    crate::legacy_labels::cutover(&mut c).map_err(|e| Error::Schema(format!("标签切换失败：{e}")))?;
    crate::legacy_characters::cutover(&mut c).map_err(|e| Error::Schema(format!("角色切换失败：{e}")))?;
    crate::legacy_names::cutover(&mut c).map_err(|e| Error::Schema(format!("名称切换失败：{e}")))?;
    if born {
        crate::app_create::names_triggers(&c)?;
        c.execute_batch(include_str!("app_migration_sql/effects.sql"))?;
    }
    initialize(&mut c, &source, &path)?;
    album_aliases(&mut c, &source)?;
    Ok(())
}
pub(crate) fn deliver_wd(
    c: &mut Connection,
    source: &Path,
    id: &str,
    assets: Option<&[String]>,
    new_assets: bool,
) -> Result<()> {
    let all = if let Some(assets) = assets {
        assets.to_vec()
    } else {
        c.prepare("SELECT uuid FROM assets ORDER BY uuid")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let all = if crate::store::labels_in_core(c)? {
        crate::wd_pending::pending(c, source, &all, &json!(id))?["pending"]
            .as_array()
            .expect("validated internal JSON array")
            .iter()
            .map(|v| v.as_str().expect("validated internal text field").to_owned())
            .collect()
    } else {
        all
    };
    let src = crate::app_library::read_connection(source)?;
    let have = columns(&src, "labels")?;
    let method = if have.contains("source") { "source" } else { "'wd'" };
    let version = if have.contains("model_ver") { "model_ver" } else { "NULL" };
    let ts = if have.contains("ts") { "ts" } else { "NULL" };
    let now: String = src.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ','now')", [], |r| r.get(0))?;
    for chunk in all.chunks(1000) {
        let mut q=src.prepare(&format!("SELECT uuid,dim,value,conf,{method} AS source,{version},{ts} FROM labels WHERE uuid IN (SELECT value FROM json_each(?)) AND (+dim='rating' OR (+dim='general' AND +conf>=0.2)) ORDER BY uuid,dim,value,source"))?;
        let mut rows = q.query([serde_json::to_string(chunk).map_err(|e| Error::Invalid(e.to_string()))?])?;
        let mut evidence = String::new();
        let mut assets = Vec::new();
        let mut last = String::new();
        while let Some(r) = rows.next()? {
            let Some(conf) = r.get::<_, Option<f64>>(3)? else { continue };
            let (uuid, dim, value, method): (String, String, String, String) =
                (r.get(0)?, r.get(1)?, r.get(2)?, r.get(4)?);
            let version = r.get::<_, Option<String>>(5)?.filter(|v| !v.is_empty()).unwrap_or_else(|| "unknown".into());
            let ts = r.get::<_, Option<String>>(6)?;
            let observed = ts
                .and_then(|t| {
                    src.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ',?)", [t], |r| r.get::<_, Option<String>>(0))
                        .ok()
                        .flatten()
                })
                .unwrap_or_else(|| now.clone());
            let (kind, system) = if dim == "general" { ("topic", "wd") } else { ("rating", "wd_rating") };
            let j = |v: &str| serde_json::to_string(v).expect("JSON text");
            let score = crate::character_json::dump(&Value::from(conf), false);
            evidence.push_str(&format!("{{\"id\":{},\"asset\":{{\"namespace\":\"library\",\"id\":{}}},\"term\":{{\"kind\":{},\"link\":{{\"system\":{},\"key\":{}}}}},\"source\":\"wd\",\"method\":{},\"version\":{},\"score\":{{\"value\":{},\"metric\":\"wd_probability\"}},\"observed_at\":{},\"data\":{{}}}}\n",j(&format!("{uuid}:{dim}:{value}:{method}")),j(&uuid),j(kind),j(system),j(&value),j(&method),j(&version),score,j(&observed)));
            if last != uuid {
                assets.push(json!({"asset":{"namespace":"library","id":uuid},"status":"completed","error":null}));
                last = uuid;
            }
        }
        if assets.is_empty() {
            continue;
        }
        let manifest = json!({"format":"illustash-evidence/1","library_id":id,"producer":{"name":"wd","version":"labels"},"created_at":now,"assets":assets,"files":{"evidence.jsonl":{"bytes":evidence.len(),"sha256":crate::strict_json::sha256_hex(evidence.as_bytes())}}});
        let pkg = crate::package::Package::from_parts(&manifest.to_string(), &evidence)?;
        crate::store::import_package_with(c, &pkg, id, new_assets)?;
    }
    Ok(())
}
fn initialize(c: &mut Connection, source: &Path, path: &Path) -> Result<()> {
    let indexed = meta(c, "indexed")?.is_some();
    let general = meta(c, "general_indexed")?.is_some();
    let ratings = meta(c, "rating_indexed")?.is_some();
    if indexed && general && ratings {
        return Ok(());
    }
    let src = crate::app_library::read_connection(source)?;
    let now: String = src.query_row("SELECT strftime('%Y-%m-%d %H:%M:%S','now','localtime')", [], |r| r.get(0))?;
    let realistic = crate::app_migrate_data::rows(
        &src,
        "SELECT uuid FROM labels WHERE dim='general' AND value='realistic' AND conf>=0.15",
    )?
    .into_iter()
    .map(|r| r[0].clone())
    .collect::<Vec<_>>();
    let args = if !indexed {
        let assets=crate::app_migrate_data::rows(&src,"SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets WHERE in_scope=1 AND COALESCE(trashed,0)=0")?;
        let album = source.parent().expect("internal file path must have a parent").join("album-char-map.json");
        let album = if album.is_file() {
            serde_json::from_str::<Value>(&std::fs::read_to_string(album)?)
                .map_err(|e| Error::Invalid(e.to_string()))?["map"]
                .clone()
        } else {
            json!({})
        };
        let id = serde_json::from_str::<Value>(&std::fs::read_to_string(
            path.parent().expect("internal file path must have a parent").join("library.json"),
        )?)
        .map_err(|e| Error::Invalid(e.to_string()))?["id"]
            .as_str()
            .expect("internal JSON value is serializable")
            .to_owned();
        let uuids =
            assets.iter().map(|r| r[0].as_str().expect("validated internal text field").to_owned()).collect::<Vec<_>>();
        deliver_wd(c, source, &id, Some(&uuids), true)?;
        json!({"op":"refresh","finalize_labels":true,"realistic":realistic,"general_indexed":now,"rating_indexed":now,"index":crate::app_migrate_data::index(&src)?,"duplicates":crate::app_migrate_data::duplicates(c,&src,&assets)?,"assets":assets,"labels":crate::app_migrate_data::rows(&src,"SELECT uuid,value,conf,dim FROM labels WHERE ((dim='character' AND conf>=0.35) OR dim='char_ext')")?,"decisions":crate::app_migrate_data::rows(&src,"SELECT uuid,value FROM decisions WHERE dim='character'")?,"album_map":album,"indexed":now})
    } else {
        json!({"op":"rederive","realistic":realistic,"general_indexed":now,"rating_indexed":now})
    };
    crate::app_service::character_request(path, &args)?;

    Ok(())
}
fn legacy_hierarchy(c: &Connection, src: &Connection) -> Result<()> {
    crate::character_import::legacy_hierarchy(c, &crate::app_migrate_data::index(src)?)
}

fn album_aliases(c: &mut Connection, source: &Path) -> Result<()> {
    let src = crate::app_library::read_connection(source)?;
    if !columns(&src, "decisions")?.contains("note") {
        return Ok(());
    }
    let path = source.parent().expect("internal file path must have a parent").join("album-char-map.json");
    let mapping = if path.is_file() {
        serde_json::from_str::<Value>(&std::fs::read_to_string(path)?).map_err(|e| Error::Invalid(e.to_string()))?
            ["map"]
            .clone()
    } else {
        json!({})
    };
    let rows = crate::app_migrate_data::rows(
        &src,
        "SELECT value,min(note) FROM decisions WHERE dim='character' GROUP BY value",
    )?;
    let mut ops = Vec::new();
    for row in rows {
        let value = row[0].as_str().ok_or_else(|| Error::Schema("无效相册 key".into()))?;
        let note = row[1].as_str().unwrap_or("");
        let Some(note) = note.strip_prefix("album:") else { continue };
        let Some((title, _)) = note.rsplit_once(" pk=") else { continue };
        let title = crate::character_names::strip(title);
        if title.is_empty() {
            continue;
        }
        let mapped = mapping[value]["char_id"].as_str().filter(|v| !v.is_empty());
        let (system, key) = mapped.map(|id| ("ccip", id)).unwrap_or(("photos_album", value));
        let id:Option<String>=c.query_row("SELECT coalesce(e.parent_id,e.id) FROM links l JOIN entities e ON e.id=l.entity_id WHERE l.system=? AND l.key=?",[system,key],|r|r.get(0)).optional()?;
        if let Some(id) = id {
            ops.push(json!({"op":"alias","entity":id,"value":title,"source":"album"}));
        }
    }
    if !ops.is_empty() {
        crate::names_commands::run(c, &json!({"op":"batch","ops":ops}))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../tests/support/origin_migration.rs"]
mod origin_tests;
