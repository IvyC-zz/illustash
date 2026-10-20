//! 身份之间的关系（重复、皮肤从属、确认不同）：关系在 `term_relations`，关系证据是带 `relation_id` 的
//! `term_evidence`（方法 `identity_evidence`），决议是带 `relation_id` 的 `term_verdicts`，旧编号经 `term_legacy_ids`
//! 对应。核心第 8 版（#201）起它们是唯一权威，命令直接写核心；原来的兼容表 `identity_relations`、`identity_evidence`、
//! `identity_decisions`、`identity_decision_evidence` 在升级时逐行核对后删除（`legacy_characters::retire_relations`）。
//!
//! 下面四个片段给出与原表同形的行，列序同原表；读者照原来的写法读它们。

/// 与原来的 `identity_relations` 同形：`id` 是旧编号，`a`、`b`、`kind` 是旧的写法（`contains` 写成 `b form_of a`，
/// `different_from` 两端按 key 排序），`status` 由关系最新的一条决议算出。另带核心关系的 id `core_id`。按 `id` 找走
/// `term_legacy_ids` 的主键。
pub const RELATIONS: &str = "(SELECT m.legacy_id AS id,
    CASE r.kind
      WHEN 'contains' THEN tb.key
      WHEN 'different_from' THEN min(ta.key, tb.key)
      ELSE ta.key
    END AS a,
    CASE r.kind
      WHEN 'contains' THEN ta.key
      WHEN 'different_from' THEN max(ta.key, tb.key)
      ELSE tb.key
    END AS b,
    CASE r.kind WHEN 'same_as' THEN 'duplicate_of' WHEN 'contains' THEN 'form_of' ELSE 'different_from' END AS kind,
    CASE l.verdict WHEN 'yes' THEN 'accepted' WHEN 'no' THEN 'rejected' ELSE 'proposed' END AS status,
    r.id AS core_id
  FROM term_legacy_ids m
  JOIN term_relations r ON r.id = m.core_id
  JOIN terms ta ON ta.id = r.a
  JOIN terms tb ON tb.id = r.b
  LEFT JOIN term_relation_latest l ON l.relation_id = r.id
  WHERE m.kind = 'relation')";

/// 与原来的 `identity_evidence` 同形：`relation_id` 是关系的旧编号，指纹、载荷、时间在证据的 `data` 里。
pub const EVIDENCE: &str = "(SELECT m.legacy_id AS relation_id,
    json_extract(e.data, '$.legacy_fingerprint') AS fingerprint,
    e.source AS source,
    json_extract(e.data, '$.payload') AS payload,
    json_extract(e.data, '$.created_at') AS created_at
  FROM term_evidence e
  JOIN term_legacy_ids m ON m.kind = 'relation' AND m.core_id = e.relation_id
  WHERE e.method = 'identity_evidence')";

/// 与原来的 `identity_decisions` 同形：`id`、`relation_id` 是旧编号，`previous_status` 是同一关系上一条决议的状态
/// （第一条是 `proposed`），`clear` 写成 `proposed`。
pub const DECISIONS: &str = "(SELECT d.legacy_id AS id,
    m.legacy_id AS relation_id,
    coalesce(lag(s.status) OVER (PARTITION BY v.relation_id ORDER BY v.id), 'proposed') AS previous_status,
    s.status AS status,
    v.reason AS reason,
    v.created_at AS created_at
  FROM term_legacy_ids d
  JOIN term_verdicts v ON v.id = d.core_id
  JOIN term_legacy_ids m ON m.kind = 'relation' AND m.core_id = v.relation_id
  JOIN (SELECT 'yes' AS verdict, 'accepted' AS status
        UNION ALL SELECT 'no', 'rejected'
        UNION ALL SELECT 'clear', 'proposed') s ON s.verdict = v.verdict
  WHERE d.kind = 'decision')";

/// 与原来的 `identity_decision_evidence` 同形：一条决议复核过的证据（决议的 `reviewed`）的指纹。
pub const DECISION_EVIDENCE: &str = "(SELECT d.legacy_id AS decision_id,
    json_extract(e.data, '$.legacy_fingerprint') AS fingerprint
  FROM term_legacy_ids d
  JOIN term_verdicts v ON v.id = d.core_id, json_each(v.reviewed) j
  JOIN term_evidence e ON e.id = j.value
  WHERE d.kind = 'decision')";

/// 原来的四张表、它们的列和同形片段，被引用的在前。升级时的核对和学习指纹按它读。
pub const TABLES: [(&str, &str, &str); 4] = [
    ("identity_relations", "id, a, b, kind, status", RELATIONS),
    ("identity_evidence", "relation_id, fingerprint, source, payload, created_at", EVIDENCE),
    ("identity_decisions", "id, relation_id, previous_status, status, reason, created_at", DECISIONS),
    ("identity_decision_evidence", "decision_id, fingerprint", DECISION_EVIDENCE),
];
