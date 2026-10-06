"""主动提问：挑哪些图来问、一次回答是一步撤销、问过的不再问、「以后再说」让角色歇一周。"""
import unittest
from web.store import Invalid
from tests import test_web_v2, terms_seed


class AskTest(unittest.TestCase):
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown
    make_single = test_web_v2.V2Test.make_single

    def owners(self, ident):
        return {m['id']: m['status'] for m in self.store.detail(ident)['memberships']}

    def asked(self):
        with self.store.connect() as c:
            return {(r['uuid'], r['answer']) for r in c.execute('SELECT uuid,answer FROM asked')}

    def test_asks_about_characters_without_enough_confirmations(self):
        self.make_single('a', 'b', 'c')
        q = self.store.detail('b')['question']
        # a is confirmed from an album; one confirmation is still short of a learned prototype.
        self.assertEqual((q['kind'], q['entity'], q['confirmed']), ('seed', self.alice, 1))
        self.assertIsNone(self.store.detail('a')['question'])
        batch = self.store.ask_batch()
        self.assertEqual((batch['kind'], batch['entity']), ('seed', self.alice))
        self.assertEqual(sorted(x['uuid'] for x in batch['items']), ['b', 'c'])
        self.assertIsNone(self.store.ask_batch(skip=[self.alice]))

    def test_multi_person_and_excluded_images_are_not_asked(self):
        self.assertIsNone(self.store.detail('b')['question'])   # not a single-person image
        self.make_single('b')
        self.store.curate(['b'], 'exclude')
        self.assertIsNone(self.store.detail('b')['question'])

    def test_answers_are_one_undo_step_and_never_asked_again(self):
        self.make_single('a', 'b', 'c')
        undo = self.store.answer(self.alice, yes=['b'], no=['c'])
        self.assertEqual(undo['label'], f"已确认 1 张是 {self.store.detail('a')['memberships'][0]['name']}，移出 1 张")
        self.assertEqual(self.owners('b')[self.alice], 'confirmed')
        self.assertNotIn(self.alice, self.owners('c'))
        self.assertEqual(self.asked(), {('b', 'yes'), ('c', 'no')})
        self.assertIsNone(self.store.detail('b')['question'])
        self.assertIsNone(self.store.ask_batch())
        self.store.undo(undo['id'])
        self.assertEqual(self.owners('b')[self.alice], 'machine')
        self.assertIn(self.alice, self.owners('c'))

    def test_switch_moves_a_wrong_claim_to_the_runner_up(self):
        self.make_single('c')
        self.store.answer(self.alice, no=['c'], kind='ambiguous', switch=self.bob)
        self.assertEqual(self.owners('c'), {self.bob: 'confirmed'})

    def test_later_is_remembered_without_an_undo_step_and_rests_the_character(self):
        self.make_single('a', 'b', 'c')
        with self.store.connect() as c:
            before = c.execute('SELECT count(*) FROM actions').fetchone()[0]
        self.assertIsNone(self.store.answer(self.alice, later=['b']))
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM actions').fetchone()[0], before)
        self.assertEqual(self.owners('b')[self.alice], 'machine')
        self.assertIsNone(self.store.detail('b')['question'])
        self.assertIsNotNone(self.store.detail('c')['question'])
        self.assertIsNone(self.store.ask_batch())

    def test_rejects_malformed_answers(self):
        for kw in ({}, {'yes': ['a'], 'no': ['a']}, {'yes': ['a'], 'switch': self.bob}, {'yes': ['a'], 'kind': 'guess'},
                   {'yes': 'a'}, {'no': ['a'], 'switch': self.alice}):
            with self.subTest(kw=kw), self.assertRaises(Invalid):
                self.store.answer(self.alice, **kw)
        with self.assertRaises(Invalid):
            self.store.answer(self.variant, yes=['b'])


if __name__ == '__main__':
    unittest.main()


class AuditTest(unittest.TestCase):
    """随机抽查：只抽没人判断过的机器归属，问过或人工改过就离开列表，回答记为 audit。"""
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown

    def open_audit(self):
        # The page lists every sampled image; the open ones carry no answer yet (and are what the sidebar counts).
        return {x['uuid'] for x in self.store.assets(pool='audit', limit=100)['items'] if not x['audit']['answer']}

    def test_sample_skips_judged_images_and_is_drawn_once(self):
        drawn = self.store.make_audit(seed=1)
        with self.store.connect() as c:
            rows = {r['uuid']: r['entity'] for r in c.execute('SELECT uuid,entity FROM audit_sample')}
            judged = {r[0] for r in c.execute('SELECT uuid FROM assertions')}
            machine = {(r[0], r[1]) for r in c.execute("SELECT uuid,entity FROM memberships WHERE status='machine'")}
        self.assertEqual(drawn, len(rows))
        self.assertTrue(rows)
        self.assertFalse(set(rows) & judged)
        self.assertTrue(all((u, e) in machine for u, e in rows.items()))
        self.assertEqual(self.store.make_audit(seed=2), 0)
        self.assertEqual(self.open_audit(), set(rows))
        self.assertEqual(next(p for p in self.store.review_pools() if p['kind'] == 'audit')['count'], len(rows))
        u = sorted(rows)[0]
        self.store.answer_audit(no=[u])
        items = {x['uuid']: x for x in self.store.assets(pool='audit', limit=100)['items']}
        self.assertEqual(items[u]['audit'], {'id': rows[u], 'name': items[u]['audit']['name'], 'answer': 'no'})   # stays listed
        self.assertEqual(next(p for p in self.store.review_pools() if p['kind'] == 'audit')['count'], len(rows) - 1)

    def test_audit_question_comes_first_and_answer_leaves_the_list(self):
        self.store.make_audit(seed=1)
        with self.store.connect() as c:
            u, e = c.execute('SELECT uuid,entity FROM audit_sample ORDER BY uuid LIMIT 1').fetchone()
        q = self.store.detail(u)['question']
        self.assertEqual((q['kind'], q['entity']), ('audit', e))
        self.store.answer(e, no=[u], kind='audit')
        self.assertNotIn(u, self.open_audit())
        with self.store.connect() as c:
            self.assertEqual(tuple(c.execute('SELECT kind,answer FROM asked WHERE uuid=?', (u,)).fetchone()), ('audit', 'no'))

    def test_batch_answer_is_one_step_on_each_sampled_claim(self):
        self.store.make_audit(seed=1)
        with self.store.connect() as c:
            rows = c.execute('SELECT uuid,entity FROM audit_sample ORDER BY uuid').fetchall()
        self.assertGreaterEqual(len(rows), 2)
        (u1, e1), (u2, e2) = rows[:2]
        undo = self.store.answer_audit(yes=[u1], no=[u2])
        self.assertEqual(self.open_audit() & {u1, u2}, set())
        with self.store.connect() as c:
            self.assertEqual({tuple(r) for r in c.execute("SELECT uuid,entity,answer FROM asked WHERE kind='audit'")}, {(u1, e1, 'yes'), (u2, e2, 'no')})
            self.assertEqual(c.execute('SELECT status FROM assertions WHERE uuid=? AND entity=?', (u2, e2)).fetchone()[0], 'rejected')
        self.store.undo(undo['id'])
        self.assertLessEqual({u1, u2}, self.open_audit())
        with self.assertRaises(Invalid):
            self.store.answer_audit(yes=['not-sampled'])

    def test_fixing_another_tag_leaves_the_sampled_claim_open(self):
        self.store.make_audit(seed=1)
        with self.store.connect() as c:
            u, e = c.execute('SELECT uuid,entity FROM audit_sample ORDER BY uuid LIMIT 1').fetchone()
            other = c.execute("SELECT id FROM entities WHERE id<>? AND parent_id IS NULL AND kind<>'work' LIMIT 1", (e,)).fetchone()[0]
        self.store.change([u], 'rejected', other)
        self.assertIn(u, self.open_audit())
        self.store.change([u], 'rejected', e)
        self.assertNotIn(u, self.open_audit())

    def test_undone_answer_returns_to_the_audit(self):
        self.store.make_audit(seed=1)
        with self.store.connect() as c:
            u, e = c.execute('SELECT uuid,entity FROM audit_sample ORDER BY uuid LIMIT 1').fetchone()
        undo = self.store.answer(e, yes=[u], kind='audit')
        self.assertNotIn(u, self.open_audit())
        self.store.undo(undo['id'])
        self.assertIn(u, self.open_audit())
        self.assertEqual(self.store.detail(u)['question']['kind'], 'audit')


class HintTest(unittest.TestCase):
    """学习线索（原型外圈的 weak PROTO claims）优先于可能认错和种子提问；答「是」就成为新种子。"""
    setUp = test_web_v2.V2Test.setUp
    tearDown = test_web_v2.V2Test.tearDown
    make_single = test_web_v2.V2Test.make_single

    def owners(self, ident):
        return {m['id']: m['status'] for m in self.store.detail(ident)['memberships']}

    def hint(self, *rows):
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.executemany("INSERT INTO weak_claims VALUES (?,?,'PROTO',?)", rows)
        self.store = old

    def test_hint_is_asked_first_and_names_the_current_owner(self):
        self.make_single('b', 'c')
        self.hint(('b', self.bob, .8), ('c', self.bob, .7))
        q = self.store.detail('b')['question']
        self.assertEqual((q['kind'], q['entity'], q['current']), ('hint', self.bob, self.alice))
        batch = self.store.ask_batch()
        self.assertEqual((batch['kind'], batch['entity']), ('hint', self.bob))
        self.assertEqual([x['uuid'] for x in batch['items']], ['b', 'c'])   # strongest lead first

    def test_yes_moves_a_single_person_image_to_the_hinted_character(self):
        self.make_single('b', 'c')
        self.hint(('b', self.bob, .8), ('c', self.bob, .7))
        self.store.answer(self.bob, yes=['b'], kind='hint')
        self.assertEqual(self.owners('b'), {self.bob: 'confirmed'})
        self.assertNotEqual(self.store.detail('b')['question'] and self.store.detail('b')['question']['kind'], 'hint')
        self.store.answer(self.bob, no=['c'], kind='hint')
        self.assertEqual(self.owners('c'), {self.alice: 'machine'})
        self.assertIsNone(self.store.ask_batch(skip=[self.alice]))

    def test_hints_skip_owned_rejected_and_multi_person_images(self):
        self.hint(('b', self.bob, .8))                    # b is not a single-person image
        self.assertNotEqual((self.store.detail('b')['question'] or {}).get('kind'), 'hint')
        self.make_single('b')
        self.hint(('b', self.alice, .9))                  # already Alice's
        self.assertEqual(self.store.detail('b')['question']['entity'], self.bob)
        self.store.answer(self.bob, no=['b'], kind='hint')
        self.assertNotEqual((self.store.detail('b')['question'] or {}).get('kind'), 'hint')
