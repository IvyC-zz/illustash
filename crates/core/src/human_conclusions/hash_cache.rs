//! 宿主核验完整字节后发布的 SHA-256 缓存；核心只定义存储和来源变化时的失效规则。
use crate::{schema, Result};
use rusqlite::Connection;
use std::collections::BTreeSet;

pub(crate) fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch(
        "CREATE TABLE human_asset_hashes(
        asset_key TEXT PRIMARY KEY,
        algorithm TEXT NOT NULL CHECK(algorithm='sha256'),
        digest TEXT NOT NULL CHECK(length(digest)=64),
        byte_length INTEGER NOT NULL CHECK(byte_length>=0),
        representation TEXT NOT NULL CHECK(representation IN ('original','derived')),
        source_signature TEXT NOT NULL,
        file_stamp TEXT NOT NULL CHECK(json_valid(file_stamp)),
        observed_at TEXT NOT NULL);",
    )?;
    // 来源表的受控更新可使缓存失效，不要求它额外获取核心写钥匙；外部删除缓存只会要求重新核验。
    for event in ["INSERT", "UPDATE"] {
        c.execute_batch(&format!(
            "CREATE TRIGGER human_hash_writer_{event} BEFORE {event} ON human_asset_hashes
            WHEN NOT EXISTS(SELECT 1 FROM meta WHERE key='{}')
            BEGIN SELECT RAISE(ABORT,'human hashes are published by the application'); END;",
            schema::WRITER_KEY
        ))?;
    }
    for event in ["INSERT", "UPDATE", "DELETE"] {
        c.execute_batch(&format!(
            "CREATE TRIGGER human_hash_revision_{event} AFTER {event} ON human_asset_hashes
            BEGIN UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='human_revision'; END;"
        ))?;
    }
    let columns: BTreeSet<String> =
        c.prepare("PRAGMA table_info(assets)")?.query_map([], |r| r.get(1))?.collect::<rusqlite::Result<_>>()?;
    if ["uuid", "src_path", "src_kind", "added_at"].iter().all(|k| columns.contains(*k)) {
        c.execute_batch("CREATE TRIGGER human_hash_source_insert AFTER INSERT ON assets BEGIN
            DELETE FROM human_asset_hashes WHERE asset_key=NEW.uuid; END;
            CREATE TRIGGER human_hash_source_update AFTER UPDATE OF src_path,src_kind,added_at ON assets
            WHEN NEW.src_path IS NOT OLD.src_path OR NEW.src_kind IS NOT OLD.src_kind OR NEW.added_at IS NOT OLD.added_at
            BEGIN DELETE FROM human_asset_hashes WHERE asset_key=OLD.uuid; END;")?;
    }
    c.execute_batch(
        "CREATE TRIGGER human_hash_version_insert AFTER INSERT ON meta WHEN NEW.key GLOB 'folder_hash:*'
        BEGIN DELETE FROM human_asset_hashes WHERE asset_key=substr(NEW.key,13); END;
        CREATE TRIGGER human_hash_version_update AFTER UPDATE OF value ON meta
        WHEN NEW.key GLOB 'folder_hash:*' AND NEW.value IS NOT OLD.value
        BEGIN DELETE FROM human_asset_hashes WHERE asset_key=substr(NEW.key,13); END;
        CREATE TRIGGER human_hash_version_delete AFTER DELETE ON meta WHEN OLD.key GLOB 'folder_hash:*'
        BEGIN DELETE FROM human_asset_hashes WHERE asset_key=substr(OLD.key,13); END;",
    )?;
    Ok(())
}
