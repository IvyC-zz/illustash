"""「1girl + 1boy」图：只有一个有名字的角色，排第二以后的弱证据不算归属（普通模式与 hybrid 身份模式）。"""
import unittest
import sqlite3
import hashlib
from web import identity
from web.store import STRONG_CCIP, STRONG_WD
from tests import test_web_v2, terms_seed


class LeadRuleTest(unittest.TestCase):
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def tag(self, *pairs):
        with sqlite3.connect(self.source) as c:
            c.executemany("INSERT INTO labels VALUES (?,'general',?,1)", pairs)
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.executemany('INSERT OR IGNORE INTO general_hits VALUES (?,?)', pairs)
                old._refresh_single(c)
                old._refresh_memberships(c)
        self.store = old

    def hybrid(self):
        self.store = terms_seed.startup_input(self.store)
        activated = self.store.activate_identity(mode='hybrid')
        terms_seed.switch(self.store)
        return activated

    def wd(self, score):
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE labels SET conf=? WHERE uuid='b' AND dim='character' AND value='bob_tag'", (score,))
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("UPDATE candidates SET score=? WHERE uuid='b' AND entity=? AND source='WD'", (score, self.bob))
                old._refresh_evidence(c, ['b'])
                old._refresh_memberships(c, ['b'])
        self.store = old

    def owners(self, ident):
        with self.store.connect() as c:
            return {e for (e,) in c.execute('SELECT entity FROM memberships WHERE uuid=?', (ident,))}

    def check(self):
        # b: alice first, bob second on WD 0.7 only (weak).
        self.assertEqual(self.owners('b'), {self.alice, self.bob})
        self.tag(('b', '1girl'), ('b', '1boy'))
        self.assertEqual(self.owners('b'), {self.alice})
        # A strong runner-up survives: give bob a strong WD score on b.
        self.wd(0.95)
        self.assertEqual(self.owners('b'), {self.alice, self.bob})
        self.wd(0.7)
        self.assertEqual([x['id'] for x in self.store.detail('b')['alternatives']], [self.bob])
        self.tag(('b', 'multiple_girls'))   # a second girl: several named characters again
        self.assertEqual(self.owners('b'), {self.alice, self.bob})

    def test_plain_rule(self):
        self.check()

    def test_hybrid_rule(self):
        self.assertEqual(self.hybrid()['policy'], 'hybrid')
        self.check()

    def test_solo_images_are_not_lead_images(self):
        self.tag(('b', 'solo'), ('b', '1girl'), ('b', '1boy'))
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM lead_assets').fetchone()[0], 0)

    def test_thresholds_match_the_strong_evidence_constants(self):
        self.assertIn(f'coalesce(v.wd,0)<{STRONG_WD}', identity.LEAD_RULE)
        self.assertIn(f'coalesce(v.ccip,0)<{STRONG_CCIP}', identity.LEAD_RULE)


if __name__ == '__main__':
    unittest.main()


class AsTagTest(unittest.TestCase):
    """「当作标签」：不算角色（不进任何角色视图、不算人头、不学习），但仍挂在图上、可以按它筛选；再点一次改回角色。"""
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown
    tag = LeadRuleTest.tag
    owners = LeadRuleTest.owners
    wd = LeadRuleTest.wd
    hybrid = LeadRuleTest.hybrid

    def tags(self, ident):
        with self.store.connect() as c:
            return {e for (e,) in c.execute('SELECT entity FROM tag_memberships WHERE uuid=?', (ident,))}

    def check(self):
        # b: alice first (WD+CCIP), bob second (weak WD). As a single-person image only alice counts.
        self.tag(('b', 'solo'))
        self.assertEqual(self.owners('b'), {self.alice})
        # bob as a tag: weak evidence still does not count on a single-person image; strong evidence rides along as a tag.
        self.assertEqual(self.store.set_mascot(self.bob, True), {'id': self.bob, 'mascot': True})
        self.assertEqual((self.owners('b'), self.tags('b')), ({self.alice}, set()))
        self.wd(0.95)
        self.assertEqual((self.owners('b'), self.tags('b')), ({self.alice}, {self.bob}))
        # Not a character anywhere, but its own page and the gallery filter still find its images.
        self.assertNotIn(self.bob, [c['id'] for c in self.store.characters()])
        self.assertIn('b', [x['uuid'] for x in self.store.assets(entity=self.bob)['items']])
        self.assertGreaterEqual(self.store.characters(ident=self.bob)[0]['count'], 1)
        self.assertEqual([t['id'] for t in self.store.detail('b')['tag_identities']], [self.bob])
        self.assertEqual([t['id'] for t in self.store.tagged_identities()], [self.bob])
        self.store.set_mascot(self.bob, False)
        self.assertEqual((self.owners('b'), self.tags('b')), ({self.alice}, set()))   # second on a single-person image
        # alice as a tag: bob moves up to the character slot, alice (strong CCIP) stays as a tag.
        self.store.set_mascot(self.alice, True)
        self.assertEqual((self.owners('b'), self.tags('b')), ({self.bob}, {self.alice}))
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT entity FROM family_evidence WHERE uuid=? AND rank=1', ('b',)).fetchone()[0], self.bob)
            self.assertEqual(c.execute('SELECT count(*) FROM ambiguous_claims WHERE rival=?', (self.alice,)).fetchone()[0], 0)
        self.store.set_mascot(self.alice, False)
        self.assertEqual((self.owners('b'), self.tags('b')), ({self.alice}, set()))

    def test_plain(self):
        self.check()

    def test_hybrid(self):
        self.hybrid()
        self.check()

    def test_an_image_with_only_tags_is_not_unmatched(self):
        before = self.store.review_pools()[0]['count']
        self.store.set_mascot(self.alice, True)   # c is alice's only (CCIP) image
        self.assertEqual(self.owners('c'), set())
        self.assertEqual(self.tags('c'), {self.alice})
        self.assertNotIn('c', [x['uuid'] for x in self.store.assets(pool='unmatched')['items']])
        self.assertEqual(self.store.review_pools()[0]['count'], before)

    def test_rejects_skins_and_bad_values(self):
        from web.store import Invalid
        with self.assertRaises(Invalid):
            self.store.set_mascot(self.variant, True)
        with self.assertRaises(Invalid):
            self.store.set_mascot(self.bob, 1)


class DefaultTagsTest(unittest.TestCase):
    """新图库：老师、博士这类玩家化身默认只当标签；用户改过的不再被默认值覆盖。"""
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def state(self, entity):
        with self.store.connect() as c:
            return tuple(c.execute('SELECT mascot,mascot_origin FROM entities WHERE id=?', (entity,)).fetchone())

    def link(self, entity, key):
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("INSERT INTO links(system,key,entity_id,source) VALUES ('danbooru',?,?,'import')", (key, entity))
        self.store = old

    def test_listed_identities_start_as_tags_on_refresh(self):
        self.link(self.bob, 'sensei_(blue_archive)')
        self.store.refresh()
        self.assertEqual(self.state(self.bob), (1, 'default'))
        self.assertNotIn(self.bob, [c['id'] for c in self.store.characters()])
        self.assertEqual(self.state(self.alice), (0, None))

    def test_the_users_choice_wins(self):
        self.link(self.bob, 'sensei_(blue_archive)')
        self.store.refresh()
        self.store.set_mascot(self.bob, False)
        self.store.refresh()
        self.assertEqual(self.state(self.bob), (0, 'user'))

    def test_migration_keeps_existing_tags_as_the_users(self):
        from web.store import Store
        self.store.set_mascot(self.alice, True)
        self.link(self.bob, 'doctor_(arknights)')
        self.store = terms_seed.startup_input(self.store)
        with self.store.connect() as c:
            c.execute("UPDATE entities SET mascot_origin=NULL")
            c.execute("UPDATE meta SET value='35' WHERE key='schema_version'")
        store = Store(self.source, self.store.path, legacy=True)
        with store.connect() as c:
            got = dict(c.execute('SELECT id,mascot_origin FROM entities WHERE id IN (?,?)', (self.alice, self.bob)).fetchall())
        self.assertEqual(got, {self.alice: 'user', self.bob: 'default'})
        terms_seed.switch(store)
        self.store = store
        test_web_v2.assert_memberships_current(self, store)
