"""S3 acceptance helper. The epsilon exception is opt-in and never used by legacy double runs.

Only ownership scores/skeys after learning publication qualify. Every other table,
row key, and column is exact, including family_evidence.ccip/rank and model outputs.
"""
import math
import sqlite3

OWNERSHIP = {'memberships', 'tag_memberships', 'term_projection'}


def assert_rebuilt_equal(test, incremental, rebuilt, *, command, suppressed_pairs=()):
    """Named rows, keyed by (uuid, entity). suppressed_pairs are known affected ownership
    pairs from sub-epsilon claim changes, not a blanket whitelist of all image scores.
    term_projection rows must include the library uuid and term key as uuid/entity.
    """
    test.assertEqual(incremental.keys(), rebuilt.keys(), 'projection tables differ')
    suppressed_pairs = set(suppressed_pairs)
    with sqlite3.connect(':memory:') as sql:
        for table, rows in incremental.items():
            if table not in OWNERSHIP or command != 'publish-learning':
                test.assertEqual(rows, rebuilt[table], table)
                continue
            def keyed(values):
                out = {(r['uuid'], r['entity']): r for r in values}
                test.assertEqual(len(out), len(values), f'{table}: duplicate row keys')
                return out
            left, right = keyed(rows), keyed(rebuilt[table])
            test.assertEqual(left.keys(), right.keys(), f'{table}: row set changed')
            for key, a in left.items():
                b = right[key]
                if a == b:
                    continue
                label = f'{table} {key}'
                test.assertIn(key, suppressed_pairs, f'{label}: not a suppressed learning score')
                test.assertEqual(a.keys(), b.keys(), f'{label}: columns changed')
                for col in a.keys() - {'score', 'skey'}:
                    test.assertEqual(a[col], b[col], f'{label}: {col}')
                for value in (a['score'], b['score']):
                    test.assertIsInstance(value, (float, int), label)
                    test.assertTrue(math.isfinite(value), label)
                test.assertNotEqual(a['score'], b['score'], f'{label}: skey changed without score')
                test.assertLessEqual(abs(a['score'] - b['score']), 1e-6, label)
                if 'skey' in a:
                    # printf's width is a minimum, not a truncation. Recompute each full
                    # key using its own score; the last (family-confirmed) flag must match.
                    test.assertTrue(a['skey'] and b['skey'], label)
                    test.assertEqual(a['skey'][-1], b['skey'][-1], f'{label}: confirmed-family flag')
                    test.assertIn(a['skey'][-1], '01', label)
                    for row in (a, b):
                        expected, = sql.execute("SELECT printf('%d%d%d%020.12f%d',?,?,?,?,?)", (
                            row['status'] == 'confirmed', row['source_count'] != 1, 1 - row['ccip_only'],
                            row['score'], int(row['skey'][-1]))).fetchone()
                        test.assertEqual(row['skey'], expected, f'{label}: invalid skey')
