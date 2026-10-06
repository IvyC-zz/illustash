"""全库找相似：签名 → 候选 → 两两关系 → 相似组；临时库与人造图片。"""
import sqlite3
import tempfile
import unittest
from pathlib import Path

from web.similar import SimilarScanner
from web.store import Store
from tests.test_relate import picture, jpeg
from tests.test_web import fixture
from tests import terms_seed
from tests.test_web_v2 import assert_memberships_current


class SimilarTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        source = fixture(root)
        base = picture(1, (600, 800))
        variant = base.copy()
        variant.paste(picture(5, (160, 140)), (220, 300))
        base.save(root / 'a.png')
        jpeg(variant, 90).save(root / 'b.png')
        picture(2, (600, 800)).save(root / 'c.png')
        self.store = Store(source, root / 'app.sqlite')
        self.store.refresh()

    def tearDown(self):
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def test_scan_groups_the_variant_and_skips_what_was_compared(self):
        scanner = SimilarScanner(self.store)
        scanner.scan()
        self.assertEqual(scanner.state['status'], 'idle')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM image_sigs WHERE ok=1').fetchone()[0], 3)
            self.assertEqual([tuple(r) for r in c.execute("SELECT a,b,kind FROM relations")], [('a', 'b', 'variant')])
        groups = self.store.similar_groups()
        self.assertEqual([(g['size'], g['kinds']) for g in groups], [(2, {'variant': 1})])
        self.assertEqual(self.store.similar_groups('crop'), [])
        self.assertEqual(sorted(self.store.ids({'pool': 'similar', 'cluster': str(groups[0]['gid'])})), ['a', 'b'])
        # Variants are not folded by default; the settings can fold them like copies, and unfold them again.
        self.assertEqual(self.store.fold_kinds(), ['duplicate', 'identical'])
        visible = lambda: sorted(r['uuid'] for r in self.store.assets(limit=100)['items'])
        self.assertIn('a', visible()), self.assertIn('b', visible())
        self.store.set_fold_kinds(['variant'])
        self.assertTrue({'a', 'b'} <= set(visible()))          # their characters differ: left for the duplicates page
        self.store.change(['b'], 'rejected', self.store.entities('Bob')[0]['id'])
        shown = visible()
        self.assertEqual(len({'a', 'b'} & set(shown)), 1)
        self.assertEqual(sorted(self.store.ids({'pool': 'duplicates'})), ['a', 'b'])
        assert_memberships_current(self, self.store)
        self.store.set_fold_kinds(['duplicate', 'identical'])
        self.assertTrue({'a', 'b'} <= set(visible()))
        with self.assertRaises(Exception):
            self.store.set_fold_kinds(['nonsense'])
        # A second scan compares nothing again.
        scanner.scan()
        self.assertEqual(scanner.state['compared'], 0)
        # ...and does not look for neighbours of images already paired (2026-09-26: 67 s per launch for nothing).
        self.assertEqual(scanner.new, [])
        self.store = terms_seed.startup_input(self.store)
        # Groups survive a migration (relations are kept, groups rebuilt).
        with self.store.connect() as c:
            c.execute("UPDATE meta SET value='16' WHERE key='schema_version'")
        again = Store(self.store.source, self.store.path, legacy=True)
        self.assertEqual(len(again.similar_groups()), 1)
        terms_seed.switch(again)

    def test_new_image_pairs_with_old_ones_and_unreadable_pairs_retry(self):
        scanner = SimilarScanner(self.store)
        with self.store.connect() as c:
            c.execute('CREATE TABLE IF NOT EXISTS similar_paired(uuid TEXT PRIMARY KEY)')
            c.execute("INSERT INTO similar_paired VALUES ('a'),('c')")   # b arrives after a and c were paired
            c.execute("UPDATE assets SET src_path=src_path||'.missing' WHERE uuid='b'")   # b still in iCloud
        scanner.scan()
        self.assertEqual(scanner.new, [])          # unreadable: not marked paired
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM relations').fetchone()[0], 0)
            c.execute("UPDATE assets SET src_path=replace(src_path,'.missing','') WHERE uuid='b'")   # downloaded
        scanner.scan()   # b is read again and pairs with the old images
        self.assertEqual(scanner.new, ['b'])
        with self.store.connect() as c:
            self.assertEqual([tuple(r) for r in c.execute('SELECT a,b,kind FROM relations')], [('a', 'b', 'variant')])
            # A compare that fails (file gone mid-scan) is retried from similar_retry, without re-pairing.
            c.execute("DELETE FROM relations")
            c.execute("INSERT INTO similar_retry VALUES ('a','b')")
        scanner.scan()
        self.assertEqual(scanner.new, [])
        with self.store.connect() as c:
            self.assertEqual([tuple(r) for r in c.execute('SELECT a,b,kind FROM relations')], [('a', 'b', 'variant')])
            self.assertEqual(c.execute('SELECT count(*) FROM similar_retry').fetchone()[0], 0)


if __name__ == '__main__':
    unittest.main()
