"""Learning fixture: fixed old-form library input switched to the core, plus synthetic 8-dimensional features."""
import tempfile
from pathlib import Path

import numpy as np

from tests.research_fixture import Features
from tests.research_fixture import seed
MIXED = """EXISTS(SELECT 1 FROM duplicates y WHERE y.grp=d.grp AND y.uuid<>d.uuid AND (
 EXISTS(SELECT entity FROM memberships WHERE uuid=d.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=y.uuid) OR
 EXISTS(SELECT entity FROM memberships WHERE uuid=y.uuid EXCEPT SELECT entity FROM memberships WHERE uuid=d.uuid)))"""


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
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.store, attrs = seed(self, root)
        self.alice, self.bob = attrs['alice'], attrs['bob']
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
        with self.store.connect() as c:
            return [r[0] for r in c.execute('SELECT entity FROM memberships WHERE uuid=? ORDER BY entity', (uuid,))]

    def learned_snapshot(self):
        with self.store.connect() as c:
            return {table: [tuple(r) for r in c.execute(f'SELECT * FROM {table} ORDER BY 1,2')]
                    for table in ('prototypes', 'proto_claims')}
