"""首页：按加入日期分组、跟随 R18、每天的角色计数；图库的按天筛选。"""
import unittest
from web.store import Invalid, day_bounds
from tests import test_web_v2


class HomeFeedTest(unittest.TestCase):
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def spread_days(self):
        with self.store.connect() as c:
            c.executemany('UPDATE assets SET added_at=? WHERE uuid=?', [('2026-09-22T10:00:00', 'a'), ('2026-09-22T23:59:59', 'b'), ('2026-09-23T08:00:00', 'c')])

    def test_groups_by_day_newest_first_with_characters(self):
        self.spread_days()
        feed = self.store.home()
        self.assertEqual([d['day'] for d in feed['days']], ['2026-09-23', '2026-09-22'])
        for d in feed['days']:
            ids = [x['uuid'] for x in d['items']]
            self.assertEqual(d['count'], len(ids))
            self.assertIn(d['hero'], ids)
            self.assertEqual(sorted(ids), sorted(self.store.ids({'day': d['day']})))
            for ch in d['characters']:
                owned = sum(any(m['id'] == ch['id'] for m in x['memberships']) for x in d['items'])
                self.assertEqual(ch['count'], owned)
        self.assertEqual(feed['next'], '')

    def test_follows_r18_and_paginates(self):
        self.spread_days()
        everything = {x['uuid'] for d in self.store.home()['days'] for x in d['items']}
        hidden = {x['uuid'] for d in self.store.home('hide')['days'] for x in d['items']}
        only = {x['uuid'] for d in self.store.home('only')['days'] for x in d['items']}
        self.assertEqual(hidden | only, everything)
        self.assertFalse(hidden & only)
        page = self.store.home(days=1)
        self.assertEqual(page['next'], '2026-09-23')
        rest = self.store.home(before=page['next'])
        self.assertEqual([d['day'] for d in rest['days']], ['2026-09-22'])

    def test_day_bounds(self):
        self.assertEqual(day_bounds('2026-09-30'), ('2026-09-30', '2026-10-01'))
        self.assertEqual(day_bounds('2026-12-31'), ('2026-12-31', '2027-01-01'))
        with self.assertRaises(Invalid):
            day_bounds('9月26日')
        with self.assertRaises(Invalid):
            self.store.home(before='nope')


class R18GroupsTest(unittest.TestCase):
    """相似分组的封面和组内数量跟随 R18：a 是 explicit、c 是 questionable、b 是 sensitive（不算 R18）。"""
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def test_similar_groups_follow_r18(self):
        with self.store.connect() as c:
            c.executemany('INSERT INTO similar_groups VALUES (?,1)', [('a',), ('b',), ('c',)])
        sizes = {mode: [(g['size'], sorted(g['covers'])) for g in self.store.similar_groups(nsfw=mode)] for mode in ('', 'hide', 'only')}
        self.assertEqual(sizes[''], [(3, ['a', 'b', 'c'])])
        self.assertEqual(sizes['hide'], [])          # only b is left: one image is not a group
        self.assertEqual(sizes['only'], [(2, ['a', 'c'])])


if __name__ == '__main__':
    unittest.main()
