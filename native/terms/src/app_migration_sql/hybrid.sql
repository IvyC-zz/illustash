-- Existing Python migration SQL; keep rules unchanged.
DROP VIEW IF EXISTS identity_assignments;
DROP VIEW IF EXISTS variant_memberships;
DROP VIEW IF EXISTS membership_rules;
DROP VIEW IF EXISTS family_confirmed;
DROP VIEW IF EXISTS effective_assertions;

CREATE VIEW effective_assertions AS
 SELECT s.uuid,n.canonical_id AS entity,n.character_id,n.variant_id,s.status,s.origin,s.entity AS raw_entity
 FROM assertions s JOIN identity_nodes n ON n.id=s.entity JOIN assets a ON a.uuid=s.uuid
 WHERE NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=s.uuid AND h.entity=s.entity)
 AND NOT EXISTS(SELECT 1 FROM identity_uncertain u WHERE u.uuid=s.uuid);
CREATE VIEW family_confirmed AS
 SELECT DISTINCT s.uuid,s.character_id AS entity FROM effective_assertions s
 WHERE s.status='confirmed' AND NOT EXISTS(SELECT 1 FROM effective_assertions r
 WHERE r.uuid=s.uuid AND r.status='rejected' AND
 (r.entity=s.entity OR (r.character_id=s.character_id AND r.variant_id IS NULL)));
CREATE VIEW membership_rules AS
 SELECT f.uuid,f.entity,'confirmed' AS status,
 (SELECT s.origin FROM effective_assertions s WHERE s.uuid=f.uuid AND s.character_id=f.entity
 AND s.status='confirmed' ORDER BY s.variant_id IS NOT NULL,s.origin LIMIT 1) AS origin,
 coalesce(v.sources,'') AS sources,v.score,coalesce(v.source_count,0) AS source_count,coalesce(v.ccip_only,0) AS ccip_only
 FROM family_confirmed f LEFT JOIN family_evidence v ON v.uuid=f.uuid AND v.entity=f.entity
 UNION ALL
 SELECT v.uuid,v.entity,'machine','machine',v.sources,v.score,v.source_count,v.ccip_only
 FROM family_evidence v
 WHERE NOT EXISTS(SELECT 1 FROM effective_assertions s WHERE s.uuid=v.uuid AND s.character_id=v.entity AND s.variant_id IS NULL AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM identity_uncertain u WHERE u.uuid=v.uuid)
 AND NOT EXISTS(SELECT 1 FROM identity_suspended h WHERE h.uuid=v.uuid)
 AND NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity=v.entity)
 AND NOT (v.ccip_only=1 AND EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity))
 AND NOT (EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=v.uuid) AND NOT (EXISTS(SELECT 1 FROM entities x WHERE x.id=v.entity AND x.mascot=1)
  AND (coalesce(v.wd,0)>=0.9 OR coalesce(v.ccip,0)>=0.95)) AND
  (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))
 AND NOT (EXISTS(SELECT 1 FROM lead_assets o WHERE o.uuid=v.uuid) AND NOT (EXISTS(SELECT 1 FROM entities x WHERE x.id=v.entity AND x.mascot=1)
  AND (coalesce(v.wd,0)>=0.9 OR coalesce(v.ccip,0)>=0.95))
  AND coalesce(v.wd,0)<0.9 AND coalesce(v.ccip,0)<0.95
  AND (v.rank>1 OR EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=v.uuid AND f.entity<>v.entity)))AND NOT (v.sources='CCIP' AND coalesce(v.ccip,0)<0.925);
CREATE VIEW variant_memberships AS
 SELECT m.uuid,e.id AS entity,e.parent_id,e.variant,
 CASE WHEN s.status='confirmed' AND s.origin<>'album' THEN 'confirmed' ELSE 'machine' END AS status
 FROM memberships m JOIN entities e ON e.parent_id=m.entity AND e.merged_into IS NULL
 LEFT JOIN assertions s ON s.uuid=m.uuid AND s.entity=e.id
 WHERE (s.status='confirmed' AND s.origin<>'album') OR (coalesce(s.status,'')<>'rejected' AND EXISTS
 (SELECT 1 FROM candidates v JOIN entities x ON x.id=v.entity WHERE v.uuid=m.uuid AND v.source<>'WD_EXT' AND (x.id=e.id OR x.merged_into=e.id)));
CREATE VIEW identity_assignments AS
 SELECT m.uuid,m.entity AS character_id,v.entity AS variant_id,m.status,m.origin
 FROM memberships m LEFT JOIN variant_memberships v ON v.uuid=m.uuid AND v.parent_id=m.entity;
;
