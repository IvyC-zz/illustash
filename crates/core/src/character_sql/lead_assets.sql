INSERT INTO lead_assets SELECT DISTINCT uuid FROM general_hits g WHERE value='1girl'
            AND EXISTS(SELECT 1 FROM general_hits b WHERE b.uuid=g.uuid AND b.value='1boy')
            AND NOT EXISTS(SELECT 1 FROM single_assets o WHERE o.uuid=g.uuid)
            AND NOT EXISTS(SELECT 1 FROM general_hits m WHERE m.uuid=g.uuid AND m.value IN (?,?,?,?,?,?,?,?,?,?,?,?,?))
