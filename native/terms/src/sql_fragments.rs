//! SQL fragments the HTTP reads share (#205): each condition is written once here. A fragment taking an alias
//! `a` reads the picture column `a.uuid` (or `a.entity`); the text is exactly what each reader used before.
use crate::{Error, Result};

/// The ratings that count as NSFW (R18): an SQL list for `IN (…)`.
pub(crate) const NSFW_RATINGS: &str = "'questionable', 'explicit'";

/// Picture `a` is rated NSFW, by the `asset_rating` view.
pub(crate) fn nsfw(a: &str) -> String {
    format!("{a}.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ({NSFW_RATINGS}))")
}
/// The content filter `nsfw` (`""`, `"hide"` or `"only"`) on picture `a`, as a clause appended with ` AND `;
/// empty without a filter.
pub(crate) fn rating(a: &str, nsfw: &str) -> Result<String> {
    match nsfw {
        "" => Ok(String::new()),
        "hide" => Ok(format!(" AND NOT coalesce({},0)", self::nsfw(a))),
        "only" => Ok(format!(" AND coalesce({},0)", self::nsfw(a))),
        _ => Err(Error::Invalid("未知内容筛选".into())),
    }
}
/// Picture `a` is not a hidden (folded) duplicate.
pub(crate) fn unfolded(a: &str) -> String {
    format!("NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid={a}.uuid AND h.hidden=1)")
}
/// Picture `a` is neither a hidden duplicate nor excluded.
pub(crate) fn visible(a: &str) -> String {
    format!("{} AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid={a}.uuid)", unfolded(a))
}
/// Picture `a` belongs to no identity and was not deferred: the unmatched pool, without the tag identities.
pub(crate) fn unclaimed(a: &str) -> String {
    format!(
        "NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid={a}.uuid) \
         AND NOT EXISTS(SELECT 1 FROM deferred d WHERE d.uuid={a}.uuid)"
    )
}
/// Every lead on a picture (`uuid`, `entity`, `score`, `weak`): family evidence, then weak claims.
pub(crate) const HINTS: &str =
    "(SELECT uuid,entity,score,0 AS weak FROM family_evidence UNION ALL SELECT uuid,entity,score,1 FROM weak_claims)";
/// The pair (`a.uuid`, `a.entity`) was not rejected by a person.
pub(crate) fn unrejected(a: &str) -> String {
    format!(
        "NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid={a}.uuid AND s.entity={a}.entity AND s.status='rejected')"
    )
}
/// The leads of `HINTS` that a person has not rejected.
pub(crate) fn leads() -> String {
    format!("(SELECT v.* FROM {HINTS} v WHERE {})", unrejected("v"))
}
/// Nobody has been asked about the pair (`a.uuid`, `a.entity`) yet.
pub(crate) fn unasked(a: &str) -> String {
    format!("NOT EXISTS(SELECT 1 FROM asked k WHERE k.uuid={a}.uuid AND k.entity={a}.entity)")
}
/// Identity `a.entity` can be learned: it is not a label-only mark.
pub(crate) fn learnable(a: &str) -> String {
    format!(
        "NOT EXISTS(
  SELECT 1
  FROM terms t
  JOIN term_tree l ON l.term_id=t.id
  WHERE t.key={a}.entity AND l.label_only=1)"
    )
}
/// Prototype hints on single pictures still worth asking about, selecting `columns` of `w` (`weak_claims`).
pub(crate) fn proto_hints(columns: &str) -> String {
    format!(
        "SELECT {columns}
FROM weak_claims w
JOIN single_assets o ON o.uuid=w.uuid
WHERE w.source='PROTO'
  AND {unasked}
  AND {learnable}
  AND NOT EXISTS(SELECT 1 FROM memberships m WHERE m.uuid=w.uuid AND m.entity=w.entity)
  AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=w.uuid AND (s.status='confirmed' OR s.entity=w.entity))
  AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=w.uuid)",
        unasked = unasked("w"),
        learnable = learnable("w")
    )
}
/// All four modules are in the core: one row, true or false.
pub(crate) const SWITCHED: &str = "
    SELECT count(*)=4
    FROM meta
    WHERE key IN ('terms_artists','terms_labels','terms_characters','terms_names') AND value='core'";
/// The library uses the hybrid identity policy (`identity_nodes` and the identity reads apply).
pub(crate) fn hybrid(c: &rusqlite::Connection) -> Result<bool> {
    use rusqlite::OptionalExtension;
    let policy = "SELECT value FROM meta WHERE key='identity_policy'";
    Ok(c.query_row(policy, [], |r| r.get::<_, String>(0)).optional()?.as_deref() == Some("hybrid"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragments_keep_the_text_each_reader_used() {
        assert_eq!(
            rating("m", "hide").unwrap(),
            " AND NOT coalesce(m.uuid IN (SELECT uuid FROM asset_rating \
             WHERE rating IN ('questionable', 'explicit')),0)"
        );
        assert_eq!(
            rating("a", "only").unwrap(),
            " AND coalesce(a.uuid IN (SELECT uuid FROM asset_rating WHERE rating IN ('questionable', 'explicit')),0)"
        );
        assert_eq!(rating("a", "").unwrap(), "");
        assert!(rating("a", "x").is_err());
        assert_eq!(
            visible("m"),
            "NOT EXISTS(SELECT 1 FROM duplicates h WHERE h.uuid=m.uuid AND h.hidden=1) \
             AND NOT EXISTS(SELECT 1 FROM excluded x WHERE x.uuid=m.uuid)"
        );
        assert_eq!(
            leads(),
            "(SELECT v.* FROM (SELECT uuid,entity,score,0 AS weak FROM family_evidence UNION ALL SELECT uuid,entity,\
             score,1 FROM weak_claims) v WHERE NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=v.uuid AND \
             s.entity=v.entity AND s.status='rejected'))"
        );
    }
}
