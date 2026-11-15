//! 人工结论的便携快照、匹配与事务合同；不读取图片或操作文件系统。
mod definitions;
pub mod export;
pub mod format;
mod format_graph;
pub mod import;
pub mod matching;
pub mod state;
mod upgrade;
pub(crate) mod visibility;

pub(crate) use upgrade::migrate;

#[cfg(test)]
mod tests;
