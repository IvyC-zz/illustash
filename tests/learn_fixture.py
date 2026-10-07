"""Learning fixture: fixed old-form library input switched to the core, plus synthetic 8-dimensional features."""
import tempfile
from pathlib import Path

import numpy as np

from web.features import Features
from tests import terms_seed
from web.store import MIXED


def assert_memberships_current(test, store):
    """The stored memberships must equal a fresh evaluation of the ownership rules."""
    cols = 'uuid,entity,status,origin,sources,score,source_count,ccip_only'
    with store.connect() as c:
        stored = c.execute(f'SELECT {cols} FROM (SELECT * FROM memberships UNION ALL SELECT * FROM tag_memberships) ORDER BY uuid,entity').fetchall()
        fresh = c.execute(f'SELECT {cols} FROM membership_rules ORDER BY uuid,entity').fetchall()
    test.assertEqual([tuple(r) for r in stored], [tuple(r) for r in fresh])
    with store.connect() as c:
        stale = c.execute(f'SELECT count(*) FROM duplicates AS d WHERE hidden<>(rep=0 AND NOT {MIXED})').fetchone()[0]
    test.assertEqual(stale, 0)
    with store.connect() as c:
        stored = c.execute('SELECT uuid FROM excluded ORDER BY 1').fetchall()
        fresh = c.execute("""SELECT a.uuid FROM assets a LEFT JOIN curation k ON k.uuid=a.uuid
            LEFT JOIN auto_rules r ON r.uuid=a.uuid AND r.kind='exclude'
            WHERE k.status='excluded' OR (r.uuid IS NOT NULL AND k.uuid IS NULL) ORDER BY 1""").fetchall()
    test.assertEqual([tuple(r) for r in stored], [tuple(r) for r in fresh])


def unit(v):
    v = np.asarray(v, np.float32)
    return v / np.linalg.norm(v)


class Learning:
    """setUp/tearDown/owners/learned_snapshot for learning fixtures (former tests/test_learn.py)."""
    def setUp(self, *, seed=None):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        default_seed = seed is None
        seed = seed or 'test_terms_character_learning_LearningContract_test_learning_changes_and_continuous_old_undo_double_run'
        self.store, attrs = terms_seed.load(seed, root)
        self.alice, self.bob = attrs['alice'], attrs['bob']
        if default_seed:
            if self._testMethodName == 'test_rejected_skin_is_not_a_parent_negative':
                with self.store.connect() as c:
                    c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('summer','','skin',?,'summer')", (self.alice,))
            terms_seed.switch(self.store)
        rng = np.random.default_rng(0)
        A, B = unit([1, 0, 0, 0, 0, 0, 0, 0]), unit([0, 0, 0, 0, 1, 0, 0, 0])
        near = lambda centre, spread: unit(centre + rng.normal(0, spread, 8))
        vectors = {f's{i}': near(A, 0.08) for i in range(6)}                  # alice's confirmed images
        vectors |= {'close': near(A, 0.05), 'edge': unit(A + 0.35 * unit([0, 1, 0, 0, 0, 0, 0, 0])),
                    'far': unit([0, 0, 1, 0, 0, 0, 0, 0])}
        vectors |= {f'u{i}': near(B, 0.05) for i in range(4)}                 # a look-alike group, nobody yet
        vectors |= {'ring': unit([0, 0, 0, 0, 0, 0, 0, 1]), 'hintb': unit([0, 0, 0, 0, 0, 0, 1, 1])}      # placed per test
        vectors |= {'a': unit([0, 0, 0, 1, 0, 0, 0, 0]), 'b': unit([0, 0, 0, 0, 0, 1, 0, 0]), 'c': unit([0, 0, 0, 0, 0, 0, 1, 0])}
        self.features = Features(self.store.source, np.stack(list(vectors.values())), list(vectors))

    def tearDown(self):
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def owners(self, uuid):
        return [m['id'] for m in self.store.detail(uuid)['memberships']]

    def learned_snapshot(self):
        with self.store.connect() as c:
            return {table: [tuple(r) for r in c.execute(f'SELECT * FROM {table} ORDER BY 1,2')]
                    for table in ('prototypes', 'proto_claims')}
