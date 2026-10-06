"""S3 普通裁决命令的双跑：change / answer / answer_audit / reassign / sweep，连同它们的撤销和连续撤销。"""
import unittest

from tests.terms_commands_fixture import CoreCommands
from web import terms_core


class Verdicts(CoreCommands):
    policy = None

    def change(self, ids, action, entity=None, **kw):
        return self.both(lambda: self.api.change(ids, action, entity, **kw), 'change',
                         asked=ids, action=action, entity=entity, **kw)

    def test_change_every_action_with_copies_and_undo_each(self):
        self.change(['b'], 'confirmed', self.bob)             # b、c 是复制品：两张都写
        self.change(['a'], 'rejected', self.alice)            # 相册确认之上的否定
        self.change(['a', 'b'], 'unknown')
        self.change(['b'], 'resume')
        self.change(['c'], 'add_variant', self.skin)
        self.change(['c'], 'remove_variant', self.skin)
        self.both(lambda: self.api.change(['a'], 'confirmed', entities=[self.alice, self.bob]), 'change',
                  asked=['a'], action='confirmed', entities=[self.alice, self.bob])
        self.undo_all()

    def test_refusals_leave_nothing(self):
        self.change(['b'], 'rejected', self.alice)
        self.refused(lambda: self.api.change(['b'], 'add_variant', self.skin), 'change',
                     asked=['b'], action='add_variant', entity=self.skin)     # 请先恢复该图片的父角色归属
        self.refused(lambda: self.api.change(['nope'], 'confirmed', self.alice), 'change',
                     asked=['nope'], action='confirmed', entity=self.alice)
        self.refused(lambda: self.api.change(['a'], 'add_variant', self.alice), 'change',
                     asked=['a'], action='add_variant', entity=self.alice)   # 请选择子标签

    def test_answers_audits_reassign_and_sweeps(self):
        self.both(lambda: self.api.answer(self.alice, yes=['b'], no=['a'], kind='seed'), 'answer',
                  entity=self.alice, yes=['b'], no=['a'], later=[], kind='seed')
        self.both(lambda: self.api.answer(self.bob, no=['c'], switch=self.alice, kind='ambiguous'), 'answer',
                  entity=self.bob, yes=[], no=['c'], later=[], switch=self.alice, kind='ambiguous')
        self.both(lambda: self.api.answer(self.alice, later=['a'], kind='sample'), 'answer', expect_action=False,
                  entity=self.alice, yes=[], no=[], later=['a'], kind='sample')
        self.both(lambda: self.api.answer_audit(yes=['b'], no=['c']), 'answer_audit', yes=['b'], no=['c'])
        self.both(lambda: self.api.reassign(['a'], self.alice, [self.bob]), 'reassign',
                  asked=['a'], old_entity=self.alice, new_entity=[self.bob]) if self.member('a', self.alice) else None
        self.both(lambda: self.api.sweep(self.alice), 'sweep', entity=self.alice)
        self.both(lambda: self.api.sweep(self.alice), 'sweep', entity=self.alice)   # 零成员：仍是一步，仍写 reviews
        self.undo_all()

    def member(self, uuid, entity):
        with self.api.connect() as c:
            return bool(c.execute('SELECT 1 FROM memberships WHERE uuid=? AND entity=?', (uuid, entity)).fetchone())

    def test_a_failure_inside_the_core_transaction_leaves_nothing(self):
        with self.new.connect() as c:
            c.execute("CREATE TRIGGER injected BEFORE INSERT ON memberships BEGIN SELECT RAISE(ABORT,'injected'); END")
        before = self.dump()
        with self.assertRaisesRegex(RuntimeError, 'injected'):
            terms_core.character_command(self.new.path, {'op': 'change', 'asked': ['b'], 'action': 'confirmed',
                                                         'entity': self.bob, 'label': 'x', 'effect': 'verdict'})
        self.assertEqual(self.dump(), before)
        with self.new.connect() as c:
            c.execute('DROP TRIGGER injected')

    def test_history_text_follows_renames_like_python(self):
        self.change(['b'], 'confirmed', self.bob)
        self.both(lambda: self.api.set_profile(self.bob, name='鲍勃'), 'set_profile', entity=self.bob, name='鲍勃', name_lang='zh')
        self.both(lambda: self.api.sweep(self.alice), 'sweep', entity=self.alice)
        self.both(lambda: self.api.set_profile(self.alice, name='爱丽丝'), 'set_profile', entity=self.alice, name='爱丽丝', name_lang='zh')

    def test_reassign_machine_membership_and_refusal(self):
        self.assertTrue(self.member('b', self.alice))
        self.both(lambda: self.api.reassign(['b'], self.alice, self.bob), 'reassign',
                  asked=['b'], old_entity=self.alice, new_entity=self.bob)
        self.refused(lambda: self.api.reassign(['b'], self.alice, self.bob), 'reassign',
                     asked=['b'], old_entity=self.alice, new_entity=self.bob)   # 原角色归属已变化
        self.undo_all()


class HybridVerdicts(Verdicts):
    policy = 'hybrid'

    def test_uncertain(self):
        """锚点修正的入口已随 v2 删除；旧锚点步骤的撤销见 tests/test_terms_character_history.py。"""
        self.both(lambda: self.api.uncertain_identity(['b'], [self.alice, self.bob]), 'uncertain',
                  asked=['b'], options=[self.alice, self.bob], reason='人工暂不判断')
        self.change(['b'], 'confirmed', self.alice)          # 人工结论清掉未定
        self.undo_all()


if __name__ == '__main__':
    unittest.main()
