//! 现有的平台能力。应用策略和发布仍由宿主负责。
#![cfg_attr(not(test), warn(clippy::unwrap_used))]
use illustash_core::backup;
#[cfg(test)]
use illustash_core::store;
pub use illustash_core::{Error, Result};
pub mod app_file_lock;
pub mod app_paths;
pub mod capabilities;
pub mod picker;
pub mod platform_backup;
pub mod platform_credentials;
pub mod platform_filesystem;
pub mod platform_folder;
pub mod platform_photos;
pub mod platform_process;
pub mod platform_research_math;
pub mod platform_system;
pub mod platform_time;
pub mod source_rows;
pub mod sqlite;
#[cfg(test)]
mod test_wait;
