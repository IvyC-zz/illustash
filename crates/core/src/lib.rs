//! Illustash 连接内共享业务规则；不依赖应用宿主、HTTP、CLI 或平台实现。
#![cfg_attr(not(test), warn(clippy::unwrap_used))]
pub mod backup;
pub mod casefold;
pub mod character_commands;
pub mod character_evidence;
pub mod character_graph;
pub mod character_identity;
pub mod character_import;
pub mod character_json;
pub mod character_labels;
pub mod character_learning;
pub mod character_merge;
pub mod character_names;
pub mod character_profile;
pub mod character_projection;
pub mod character_structure;
pub mod compat_text;
#[path = "http_unicode.rs"]
pub mod compat_unicode;
pub mod error;
pub mod name_codecs;
pub mod names_commands;
pub mod names_search;
pub mod names_view;
pub mod origin;
pub mod package;
#[path = "schema.rs"]
mod schema_rules;
pub mod store;
pub mod strict_json;
pub mod undo_labels;
#[cfg(not(test))]
pub mod schema {
    pub use super::schema_rules::*;
}
#[cfg(test)]
pub use test_support::schema;
#[path = "legacy_artists.rs"]
mod legacy_artists_rules;
#[cfg(not(test))]
pub mod legacy_artists {
    pub use super::legacy_artists_rules::*;
}
#[cfg(test)]
pub use test_support::legacy_artists;
#[path = "legacy_characters.rs"]
mod legacy_characters_rules;
#[cfg(not(test))]
pub mod legacy_characters {
    pub use super::legacy_characters_rules::*;
}
#[cfg(test)]
pub use test_support::legacy_characters;
#[path = "legacy_labels.rs"]
mod legacy_labels_rules;
#[cfg(not(test))]
pub mod legacy_labels {
    pub use super::legacy_labels_rules::*;
}
#[cfg(test)]
pub use test_support::legacy_labels;
#[path = "legacy_names.rs"]
mod legacy_names_rules;
pub use error::{Error, Result};
#[cfg(not(test))]
pub mod legacy_names {
    pub use super::legacy_names_rules::*;
}
use rusqlite::Connection;
pub use schema::{Kind, SCHEMA_VERSION};
use std::time::Duration;
#[cfg(test)]
pub use test_support::legacy_names;
pub fn configure(conn: &Connection) -> Result<()> {
    conn.set_prepared_statement_cache_capacity(128);
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod test_support;
#[cfg(test)]
use test_support::{migrate, open, platform_backup, sqlite};
