INSERT INTO single_assets SELECT DISTINCT uuid FROM general_hits g WHERE value='solo'
            AND NOT EXISTS(SELECT 1 FROM general_hits m WHERE m.uuid=g.uuid AND m.value IN (?,?,?,?,?,?,?,?,?,?,?,?,?))
