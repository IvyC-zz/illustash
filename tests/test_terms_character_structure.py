"""S3 合并、资料、结构命令的双跑。名字相关的字符串（名字语言、可读名、当前显示名）由调用方算好传给核心，
新身份的 id 取旧版建出来的那个，两边才能逐行相同。"""
import unittest
import uuid

from tests import terms_seed

from tests.terms_commands_fixture import CoreCommands
from tests.test_terms_character_learning import snapshot
from web import names


class Structure(CoreCommands):
    policy = None

    def newest(self, kind=None):
        with self.api.connect() as c:
            sql = 'SELECT id FROM entities' + (' WHERE kind=?' if kind else '') + ' ORDER BY rowid DESC LIMIT 1'
            return c.execute(sql, (kind,) if kind else ()).fetchone()[0]

    def made(self, old_call, op, **args):
        """旧版建出一个新身份 / 作品；核心用同一个 id 建。"""
        kept = []
        self.both(lambda: kept.append(old_call()), op, expect_action=False, effect=None,
                  late=lambda: {'id': kept[0]['id']}, **args)
        return kept[0]['id']

    def shown(self, ident):
        with self.api.connect() as c:
            return c.execute('SELECT entity_name(?)', (ident,)).fetchone()[0]

    def test_merge_and_undo(self):
        self.both(lambda: self.api.change(['c'], 'confirmed', self.bob), 'change', asked=['c'], action='confirmed', entity=self.bob)
        with self.api.connect() as c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('pins',?)", (f'["{self.bob}","x"]',))
        with self.new.connect() as c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('pins',?)", (f'["{self.bob}","x"]',))
        self.both(lambda: self.api.merge(self.bob, self.alice), 'merge', source=self.bob, target=self.alice)
        self.undo_all()

    def test_profile_and_link_local_and_undo(self):
        self.both(lambda: self.api.set_profile(self.alice, name='爱丽丝', aliases=['a1', ' a1 ', 'a2']), 'set_profile',
                  entity=self.alice, name='爱丽丝', name_lang='zh', aliases=['a1', ' a1 ', 'a2'])
        self.both(lambda: self.api.set_profile(self.bob, tag='bob_(new_work)', copyright_of=lambda t: 'new_work'), 'set_profile',
                  entity=self.bob, tag='bob_(new_work)', copyright='new_work', readable=names.readable('new_work'),
                  late=lambda: {'work_id': self.newest('work')})
        mine = self.made(lambda: self.api.create_entity('Local Hero'), 'create_entity', name='Local Hero', lang='en', kind='', artist='')
        self.both(lambda: self.api.link_local([{'id': mine, 'tag': 'local_hero'}]), 'link_local', items=[{'id': mine, 'tag': 'local_hero'}])
        self.undo_all()

    def test_new_isolated_entities_keep_existing_ownership_and_match_full_rebuild(self):
        before = snapshot(self.new, ('family_evidence', 'memberships'))
        for name in ('孤立新人物甲', '孤立新人物乙', '孤立新人物丙'):
            self.made(lambda: self.api.create_entity(name), 'create_entity', name=name, lang='zh', kind='', artist='')
            # made 内已有逐表双跑及增量 / 全量对照；这里另钉住已有图的归属不动。
            self.assertEqual(before, snapshot(self.new, ('family_evidence', 'memberships')))

    def test_structure_commands(self):
        w = self.made(lambda: self.api.edit_work('create', name='系列', tag='series'), 'edit_work',
                      action='create', name='系列', lang='zh', tag='series')
        child = self.made(lambda: self.api.edit_work('create', name='续作', parent=w), 'edit_work',
                          action='create', name='续作', lang='zh', parent=w)
        for kw in ({'action': 'move', 'ident': child, 'parent': None}, {'action': 'link', 'ident': child, 'tag': 'sequel'},
                   {'action': 'rename', 'ident': w, 'name': 'Series', 'lang': 'en'},
                   {'action': 'edit', 'ident': child, 'name': '续作二', 'lang': 'zh', 'parent': w, 'tag': ''}):
            old = {k: v for k, v in kw.items() if k != 'lang'}
            self.both(lambda: self.api.edit_work(**old), 'edit_work', expect_action=False, effect=None, **kw)
        self.made(lambda: self.api.edit_work('from_tag', tag='other_game'), 'edit_work', action='from_tag', tag='other_game',
                  readable=names.readable('other_game'))
        self.both(lambda: self.api.set_work(self.alice, child), 'set_work', expect_action=False, effect=None, entity=self.alice, work=child)
        shown = self.shown(self.alice)
        self.both(lambda: self.api.set_kind(self.alice, 'oc', 'someone'), 'set_kind', expect_action=False, effect=None,
                  entity=self.alice, kind='oc', artist='someone', shown=shown, shown_lang=names.lang_of(shown))
        self.both(lambda: self.api.set_kind(self.alice, '', ''), 'set_kind', expect_action=False, effect=None,
                  entity=self.alice, kind='', artist='')
        self.both(lambda: self.api.set_mascot(self.bob, True), 'set_mascot', expect_action=False, effect=None, entity=self.bob, mascot=True)
        self.made(lambda: self.api.shared_oc(), 'shared_oc')
        self.both(lambda: self.api.shared_oc(), 'shared_oc', expect_action=False, effect=None)
        self.made(lambda: self.api.create_linked('carol_(series)', typed='卡罗尔'), 'create_linked', tag='carol_(series)', alias='卡罗尔')

    def automatic_work_input(self, store):
        name = terms_seed.key(self)
        result, _ = terms_seed.load(name, self.root, db=self.root / (store.path.stem + '-automatic.sqlite'))
        tables = ('entities', 'links', 'names', 'display_names', 'name_aliases', 'work_review')
        with store.connect() as src, result.connect() as dst:
            for table in tables:
                rows = list(map(tuple, src.execute(f'SELECT * FROM {table}')))
                dst.execute(f'DELETE FROM {table}')
                if rows:
                    dst.executemany(f'INSERT INTO {table} VALUES ({",".join("?" for _ in rows[0])})', rows)
            dst.execute('UPDATE work_review SET parent_locked=0')
        return terms_seed.switch(result)

    def test_work_evidence_keeps_order_semantics(self):
        for t in ('a_work', 'b_work'):
            self.made(lambda: self.api.edit_work('create', name=t.upper(), tag=t), 'edit_work', action='create', name=t.upper(),
                      lang='en', tag=t)
        # 这一项要测机器作品父级候选，公开手工创建会锁父级。
        # 把已经验收过的创建结果当作旧形态输入，设置候选条件后再做真实启动切换，不能绕过核心写保护。
        cursor = uuid.uuid4.side_effect.n
        self.api, self.new = [self.automatic_work_input(s) for s in (self.api, self.new)]
        uuid.uuid4.side_effect.n = cursor
        evidence = {'tags': [{'name': t, 'category': 3} for t in ('a_work', 'b_work', 'c_work')],
                    'implications': [{'antecedent_name': x, 'consequent_name': y, 'status': 'active'}
                                     for x, y in (('b_work', 'a_work'), ('a_work', 'b_work'), ('a_work', 'c_work'))],
                    'watch_tags': ['c_work']}
        def old():
            return self.api.apply_work_evidence(evidence)
        report, out = self.both(old, 'work_evidence', expect_action=False, effect=None, evidence=evidence,
                                readable={'c_work': names.readable('c_work')}, late=lambda: {'ids': {'c_work': self.newest('work')}})
        self.assertEqual(out, report)


class HybridStructure(Structure):
    policy = 'hybrid'


if __name__ == '__main__':
    unittest.main()
