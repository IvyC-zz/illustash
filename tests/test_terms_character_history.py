"""S3 连续撤销等价：收编之前旧代码留下一串历史（各种旧形状），收编之后两边从最新一步撤到最早一步，每一步都相等。"""
import unittest

from tests.terms_commands_fixture import CoreCommands


class LegacyHistory(CoreCommands):
    policy = None

    def test_undo_every_old_step(self):
        self.undo_all()

    def test_new_steps_on_top_then_undo_everything(self):
        self.both(lambda: self.api.change(['b'], 'confirmed', self.alice), 'change', asked=['b'], action='confirmed', entity=self.alice)
        self.both(lambda: self.api.sweep(self.alice), 'sweep', entity=self.alice)
        self.undo_all()


class HybridLegacyHistory(LegacyHistory):
    """激活之前的历史（含破坏式合并）加上激活之后的关系决议、未定、旧的锚点修正步骤。"""
    policy = 'hybrid'


if __name__ == '__main__':
    unittest.main()
