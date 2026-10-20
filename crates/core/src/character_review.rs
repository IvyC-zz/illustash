//! 作品复核：`term_tree` 的复核三列 `review_status`、`review_note`、`parent_locked`（三列同为空就是没有复核行）。核心
//! 第 8 版（#201）起它是唯一权威，命令直接读写它；原来的兼容表 `work_review` 在升级时核对后删除
//! （`legacy_characters::retire_work_review`）。

/// 与原来的 `work_review` 同形的读法：`entity_id` 是身份 id（`terms.key`），只含有复核行的词条。
pub const REVIEWS: &str = "(SELECT t.key AS entity_id, r.review_status AS status, r.review_note AS note,
    r.parent_locked AS parent_locked
  FROM term_tree r
  JOIN terms t ON t.id = r.term_id
  WHERE r.review_status IS NOT NULL)";

/// 一个身份的复核行（身份 id 是参数 `?1`）：改它就是原来的 `UPDATE work_review … WHERE entity_id = ?`，没有这一行时
/// 什么都不改。
pub const OF: &str = "term_id = (SELECT id FROM terms WHERE key = ?1) AND review_status IS NOT NULL";

/// 新作品的复核行（原来的 `INSERT INTO work_review`；没写的列取原来的默认值：状态 pending、备注空、不锁父级）。参数
/// 依次是身份 id、状态、备注、是否锁父级。
pub const OPEN: &str = "UPDATE term_tree SET review_status = ?2, review_note = ?3, parent_locked = ?4
  WHERE term_id = (SELECT id FROM terms WHERE key = ?1)";
