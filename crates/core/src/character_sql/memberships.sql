INSERT INTO memberships SELECT m.uuid,m.entity,m.status,m.origin,m.sources,m.score,m.source_count,m.ccip_only,
            printf('%d%d%d%020.12f%d',m.status='confirmed',m.source_count<>1,1-m.ccip_only,coalesce(m.score,1),
            NOT EXISTS(SELECT 1 FROM family_confirmed f WHERE f.uuid=m.uuid AND f.entity<>m.entity))
            FROM membership_rules m WHERE NOT EXISTS(SELECT 1 FROM entities t WHERE t.id=m.entity AND t.mascot=1)
