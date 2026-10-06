"""S3 同步、刷新、重扫的双跑：旧 Store 与「Python 准备输入 + 核心一个事务」逐表相同。"""
import json
import sqlite3
import unittest
from unittest import mock

from tests.terms_commands_fixture import CoreCommands
from web import terms_characters, terms_core


class Import(CoreCommands):
    policy = None
    labels = True

    def setUp(self):
        super().setUp()
        original_index, original_import = terms_characters._index, terms_characters.import_changes
        def index(src, new_ids=None):
            return original_index(src, self.fixed_ids() if new_ids is None else new_ids)
        def imported(store, src, c, added, removed, run_id, new_ids=None):
            supplied = self.fixed_ids() if store is self.api and new_ids is None else new_ids
            return original_import(store, src, c, added, removed, run_id, supplied)
        for owner, attr, replacement in ((terms_characters, '_index', index),
                                          (terms_characters, 'import_changes', imported)):
            patch = mock.patch.object(owner, attr, side_effect=replacement)
            patch.start()
            self.addCleanup(patch.stop)

    def fixed_ids(self):
        """仅固定导入的随机 ID 输入：使用旧快照实际记录的新链接，不改任何输出比较或业务规则。"""
        delta = self.checkpoints.want[len(self.checkpoints.record)].get('links', {})
        columns = self.checkpoints.previous['links'][0]
        rows = delta.get('+', [])
        if 'replaced' in delta:
            columns, *rows = delta['replaced']
        out = {}
        for row in rows:
            data = dict(zip(columns, row))
            out.setdefault(data['system'], {})[data['key']] = data['entity_id']
        return out

    def links(self, store):
        with store.connect() as c:
            return {tuple(r) for r in c.execute('SELECT system,key,entity_id FROM links')}

    def entity_ids(self, store):
        with store.connect() as c:
            return {r[0] for r in c.execute('SELECT id FROM entities')}

    def created(self, before_entities):
        """旧版这一次新建的身份：它们的链接 → id（核心按建它时的那条链接取同一个 id）。"""
        fresh = self.entity_ids(self.api) - before_entities
        out = {}
        for system, key, ident in self.links(self.api):
            if ident in fresh:
                out.setdefault(system, {})[key] = ident
        return out

    def meta(self, key):
        with self.api.connect() as c:
            row = c.execute('SELECT value FROM meta WHERE key=?', (key,)).fetchone()
            return row[0] if row else None

    def source(self, *statements):
        with sqlite3.connect(self.api.source) as src:
            for sql in statements:
                src.execute(sql)

    def core(self, build, effect=None):
        with self.new.original() as src, self.new.connect() as c:
            command = build(src, c)
        if effect:
            command['effect'] = effect
        return terms_core.character_command(self.new.path, command)

    def import_changes(self, added=(), removed=(), run_id=None):
        self.aligned()
        before = self.entity_ids(self.api)
        self.api.import_changes(list(added), list(removed), run_id=run_id)
        ids, indexed = self.created(before), self.meta('indexed')
        if added:
            self.new._deliver_wd(list(added), new_assets=True)
        def build(src, c):
            command = terms_characters.import_changes(self.new, src, c, list(added), list(removed), run_id, ids)
            return command | {'indexed': indexed}
        self.core(build, effect='imported')
        self.same()

    def reindex(self, uuids):
        self.aligned()
        before = self.entity_ids(self.api)
        self.api.reindex_characters(uuids)
        ids, indexed = self.created(before), self.meta('indexed')
        self.core(lambda src, c: terms_characters.reindex(self.new, src, c, uuids, ids) | {'indexed': indexed})
        self.same()

    def refresh(self):
        self.aligned()
        before = self.entity_ids(self.api)
        self.api.refresh()
        ids, indexed = self.created(before), self.meta('indexed')
        self.core(lambda src, c: terms_characters.refresh(self.new, src, c, ids) | {'indexed': indexed})
        self.new.album_titles = self.new._album_titles(add_aliases=True)
        self.new._deliver_wd(self.new.library_uuids())
        with self.new.original() as src:
            realistic = self.new._realistic(src, 'main')
        terms_core.character_command(self.new.path, {'op': 'rederive', 'realistic': realistic,
                                                     'rating_indexed': self.meta('rating_indexed'),
                                                     'general_indexed': self.meta('general_indexed')})
        self.same()

    def add_images(self):
        with sqlite3.connect(self.api.source) as c:
            c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)', [
                ('d', 'd.png', '', 240, 300, '2026-09-23', 'orig', 1, 0), ('e', 'e.png', '', 120, 150, '2026-09-23', 'orig', 1, 0),
                ('f', 'f.png', '', 120, 150, '2026-09-23', 'orig', 1, 0), ('g', 'g.png', '', 120, 150, '2026-09-23', 'orig', 1, 0)])
            c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [
                ('d', 'character', 'alice_tag', 0.9), ('d', 'general', 'blue_hair', 0.98), ('d', 'rating', 'general', 0.9),
                ('e', 'character', 'bob_tag', 0.8), ('e', 'character', 'alice_tag', 0.5), ('e', 'general', 'solo', 0.9),
                ('e', 'rating', 'explicit', 0.7), ('e', 'char_ext', 'bob', 0.97),
                ('f', 'general', 'chat_log', 0.8), ('f', 'rating', 'general', 0.9),
                ('g', 'character', 'unknown_tag', 0.6), ('g', 'rating', 'sensitive', 0.6)])
            c.execute("INSERT INTO decisions VALUES ('g','character','alice_album','album:Alice album pk=1')")

    def test_unchanged_migration_candidates_are_reused_but_changed_scores_are_new(self):
        def count():
            with self.new.connect() as c:
                return c.execute("SELECT count(*) FROM term_evidence WHERE method IN ('wd_character','char_ext')").fetchone()[0]
        before = count()
        self.refresh()
        self.assertEqual(count(), before)
        self.source("UPDATE labels SET conf=0.91 WHERE uuid='a' AND dim='character'")
        self.refresh()
        self.assertGreater(count(), before)

    def test_new_images_removal_and_return_then_undo_everything(self):
        self.both(lambda: self.api.change(['b'], 'rejected', self.bob), 'change', asked=['b'], action='rejected', entity=self.bob)
        self.add_images()
        self.import_changes(['d', 'e', 'f', 'g', 'nope'], run_id=1)
        self.both(lambda: self.api.change(['g'], 'rejected', self.alice), 'change', asked=['g'], action='rejected', entity=self.alice)
        self.source("UPDATE assets SET trashed=1 WHERE uuid='c'")
        self.import_changes(removed=['c'])
        self.source("UPDATE assets SET trashed=0 WHERE uuid='c'")
        self.import_changes(['c'])
        self.undo_all()

    def test_index_patch_and_drop(self):
        self.source("INSERT INTO characters VALUES ('carol','Carol',NULL,NULL,'Example',NULL)",
                    "INSERT INTO characters VALUES ('sensei_(blue_archive)','Sensei',NULL,NULL,'blue_archive',NULL)",
                    "INSERT INTO labels VALUES ('a','char_ext','sensei_(blue_archive)',0.97)",
                    "DELETE FROM labels WHERE dim='char_ext' AND uuid='b'",
                    "INSERT INTO labels VALUES ('b','char_ext','carol',0.97)", "INSERT INTO labels VALUES ('c','char_ext','carol',0.96)")
        self.reindex(['b', 'c', 'a', 'outside'])
        self.both(lambda: self.api.change(['c'], 'confirmed', self.bob), 'change', asked=['c'], action='confirmed', entity=self.bob)
        self.source("DELETE FROM characters WHERE char_id='carol'", "DELETE FROM labels WHERE value='carol'",
                    "INSERT INTO labels VALUES ('b','char_ext','alice',0.95)")
        self.reindex(['b', 'c'])
        self.undo_all()

    def test_refresh_with_album_and_skin_hierarchy(self):
        self.source("ALTER TABLE characters ADD COLUMN skin TEXT", "ALTER TABLE characters ADD COLUMN base_tag TEXT",
                    "ALTER TABLE characters ADD COLUMN danbooru_tag TEXT",
                    "UPDATE characters SET base_tag='alice',danbooru_tag='alice' WHERE char_id='alice'",
                    "INSERT INTO characters VALUES ('alice_summer','夏日爱丽丝',NULL,NULL,'Example',NULL,'summer','alice','alice_(summer)')",
                    "INSERT INTO characters VALUES ('dana_winter','Dana',NULL,NULL,'Other',NULL,'winter','dana',NULL)",
                    "INSERT INTO labels VALUES ('c','char_ext','alice_summer',0.99)",
                    "INSERT INTO decisions VALUES ('c','character','alice_album','album:Alice album pk=1')")
        self.both(lambda: self.api.change(['c'], 'rejected', self.alice), 'change', asked=['c'], action='rejected', entity=self.alice)
        self.undo()
        self.refresh()
        # 已有的 bob 在索引里成了 alice 的皮肤：策略未设置时照名字规则挂上，hybrid 下已知身份不动。
        # 单独一次刷新：同一次里有新身份时，hybrid 会重建身份图，按基线把层级写回去，看不出区别。
        self.source("UPDATE characters SET skin='casual',base_tag='alice' WHERE char_id='bob'")
        self.refresh()
        self.both(lambda: self.api.change(['c'], 'rejected', self.alice), 'change', asked=['c'], action='rejected', entity=self.alice)
        # bob 不再是皮肤：variant 由索引重置（名字规则不撤回已有的层级）。
        self.source("UPDATE characters SET skin=NULL,base_tag=NULL WHERE char_id='bob'")
        self.refresh()
        self.undo_all()


class HybridImport(Import):
    policy = 'hybrid'


if __name__ == '__main__':
    unittest.main()
