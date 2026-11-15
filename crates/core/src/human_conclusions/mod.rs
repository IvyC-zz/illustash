//! 人工结论的便携快照、匹配与事务合同；不读取图片或操作文件系统。
pub mod export;
pub mod format;
mod format_graph;
pub mod matching;
pub mod state;
mod upgrade;

pub(crate) use upgrade::migrate;

#[cfg(test)]
mod tests;
