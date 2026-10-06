"""详情卡片的收藏统计：数量与「角色」页一致、跟随 R18 设置、最近图片排除当前这张。"""
import unittest
from web.store import Invalid
from tests import test_web_v2


class CharacterStatsTest(unittest.TestCase):
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def test_rating_set_matches_exists_with_null_and_missing_rows(self):
        import sqlite3
        from web.store import rating
        with sqlite3.connect(':memory:') as c:
            c.executescript("""
                CREATE TABLE assets(uuid TEXT);
                CREATE TABLE asset_rating(uuid TEXT,rating TEXT);
                INSERT INTO assets VALUES ('a'),('b'),('c'),(NULL);
                INSERT INTO asset_rating VALUES ('a','explicit'),('b','sensitive'),(NULL,'explicit');
            """)
            for mode in ('hide', 'only'):
                old = "NOT " if mode == 'hide' else ""
                old += "EXISTS(SELECT 1 FROM asset_rating r WHERE r.uuid=a.uuid AND r.rating IN ('questionable','explicit'))"
                self.assertEqual(c.execute('SELECT ' + old + ' FROM assets a').fetchall(),
                                 c.execute('SELECT ' + rating('a', mode)[5:] + ' FROM assets a').fetchall())

    def count(self, ident, nsfw=''):
        rows = self.store.characters(ident=ident, nsfw=nsfw)
        return rows[0]['count'] if rows else 0

    def test_counts_match_character_list_and_follow_r18(self):
        for ident in (self.alice, self.bob):
            for mode in ('', 'hide', 'only'):
                stats = self.store.character_stats(ident, mode)
                self.assertEqual(stats['count'], self.count(ident, mode), (ident, mode))
                self.assertEqual(len(stats['months']), 12)
                self.assertLessEqual(sum(m['count'] for m in stats['months']), stats['count'])

    def test_recent_skips_current_image_and_is_capped(self):
        stats = self.store.character_stats(self.alice)
        self.assertLessEqual(len(stats['recent']), 4)
        if stats['recent']:
            current = stats['recent'][0]
            again = self.store.character_stats(self.alice, exclude=current)
            self.assertNotIn(current, again['recent'])

    def test_first_month_and_missing_id(self):
        stats = self.store.character_stats(self.alice)
        if stats['count']:
            self.assertRegex(stats['first'], r'^\d{4}-\d{2}$')
        with self.assertRaises(Invalid):
            self.store.character_stats('')

    def test_activity_recent_and_last_follow_r18(self):
        today = __import__('time').strftime('%Y-%m-%dT10:00:00')
        with self.store.connect() as c:
            c.executemany('UPDATE assets SET added_at=? WHERE uuid=?', [(today, 'a'), ('2020-01-02T00:00:00', 'b'), (today, 'c')])
        for mode in ('', 'hide', 'only'):
            act = self.store.character_activity(mode)
            for ident in (self.alice, self.bob):
                if self.count(ident, mode):
                    self.assertIn(ident, act)
                    self.assertLessEqual(act[ident]['recent'], self.count(ident, mode))
                    self.assertRegex(act[ident]['last'], r'^\d{4}-\d{2}-\d{2}$')
                else:
                    self.assertNotIn(ident, act)
        everything = self.store.character_activity()
        with self.store.connect() as c:
            owners = {e: {u for (u,) in c.execute('SELECT uuid FROM memberships WHERE entity=?', (e,))} for e in everything}
        for e, uuids in owners.items():
            fresh = uuids & {'a', 'c'}
            self.assertEqual(everything[e]['recent'], len(fresh))
            self.assertEqual(everything[e]['last'], today[:10] if fresh else '2020-01-02')

    def test_cover_prefers_solo_confirmed_pictures_and_is_visible(self):
        for mode in ('', 'hide', 'only'):
            rows = self.store.characters(minimum=1, nsfw=mode)
            with self.store.connect() as c:
                for r in rows:
                    owned = [u for (u,) in c.execute('SELECT uuid FROM memberships WHERE entity=?', (r['id'],))]
                    visible = {x['uuid'] for x in self.store.assets(entity=r['id'], nsfw=mode, limit=500, include_total=False)['items']}
                    self.assertIn(r['cover'], visible, (r['name'], mode))
                    solo = [u for u in owned if u in visible and c.execute('SELECT count(*) FROM memberships WHERE uuid=?', (u,)).fetchone()[0] == 1]
                    if solo:
                        self.assertIn(r['cover'], solo, (r['name'], mode))


if __name__ == '__main__':
    unittest.main()
