-- Existing Python migration SQL; keep rules unchanged.

CREATE VIEW IF NOT EXISTS ambiguous_claims AS
 SELECT m.uuid,m.entity,r.entity AS rival,t.ccip-r.ccip AS margin
 FROM memberships m JOIN single_assets o ON o.uuid=m.uuid
 JOIN family_evidence t ON t.uuid=m.uuid AND t.entity=m.entity AND t.rank=1
 JOIN family_evidence r ON r.uuid=m.uuid AND r.rank=2
 WHERE m.status='machine' AND ((t.wd IS NOT NULL AND t.ccip IS NOT NULL)*4 + (coalesce(t.wd,0)>=0.9 OR coalesce(t.ccip,0)>=0.95)*2 + (t.wd IS NOT NULL))=((r.wd IS NOT NULL AND r.ccip IS NOT NULL)*4 + (coalesce(r.wd,0)>=0.9 OR coalesce(r.ccip,0)>=0.95)*2 + (r.wd IS NOT NULL))
 AND (t.ccip IS NULL OR r.ccip IS NULL OR t.ccip-r.ccip<0.03)
 AND NOT EXISTS(SELECT 1 FROM assertions s WHERE s.uuid=m.uuid AND s.entity=r.entity AND s.status='rejected')
 AND NOT EXISTS(SELECT 1 FROM entities x WHERE x.id=r.entity AND x.mascot=1)
;
