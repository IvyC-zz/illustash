"""C3 名字写入：固定旧输入经真实启动切换后，逐步对照旧结果快照。

两份库均为完整核心形态，一份逐条提交，另一份验证批量、部分失败与重试。原有逐行比较、Python 选名参考、
全量重建、历史、并发读快照与写保护断言全部保留；NameUpdater 不再使用旧 Python 写表分支。
"""
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tests.terms_names_fixture import snapshot
from tests import terms_seed
from tests.terms_c3_snapshots import Checkpoints, deterministic
from tests.test_terms_core import core
from tests.test_terms_names_projection import projected
from web import names, terms_core
from web.server import adopt_characters, adopt_labels, adopt_names
from web.store import Store


class NameWrites(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        deterministic(self)
        self.checkpoints = Checkpoints(self)
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        serial, self.ids = terms_seed.load('names_writes', root, db=root / 'serial.sqlite')
        terms_seed.switch(serial)
        with sqlite3.connect(serial.path) as src, sqlite3.connect(root / 'new.sqlite') as dst:
            src.backup(dst)
        new = Store.open(serial.source, root / 'new.sqlite')
        self.assertTrue(adopt_names(new))
        self.assertTrue(adopt_names(new))                       # 已切库再次启动：仍只切一次
        self.serial, self.new = serial, new
        self.updaters = [names.NameUpdater(s, enabled=False) for s in (serial, new)]

    def tables(self, store):
        with store.connect() as c:
            return {t: sorted(map(tuple, c.execute(f'SELECT * FROM {t}'))) for t in ('names', 'display_names', 'name_aliases')}

    def same(self):
        self.checkpoints.capture(self.serial, ('names', 'display_names', 'name_aliases'))
        self.assertEqual(self.tables(self.serial), self.tables(self.new))
        with self.new.connect() as c:
            snap = snapshot(c)                                   # 旧 View 在切换那份的数据上算
            entity, skin_only, choice = projected(c)
        self.assertEqual(entity, snap['entity'])
        self.assertEqual(skin_only, snap['skin_only'])
        self.assertEqual({k: choice[k] for k in snap['choose']}, snap['choose'])
        self.assertTrue(terms_core.run('verify-names', '--db', self.new.path)['committed'])

    def both(self, call):
        for updater in self.updaters:
            call(updater)
        self.same()

    @mock.patch.object(names.NameUpdater, 'wiki', return_value=None)
    def test_name_updater_writes(self, empty_wiki):
        self.same()
        self.both(lambda u: u.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '新 b 名'), ('ja', '新 j 名')]))
        self.both(lambda u: u.replace('character', 'prio_(series_q)', 'model', []))            # 成功的空结果
        self.both(lambda u: u.replace('work', 'series_q', 'index', [('ja', '新シリーズ'), ('zh', '  ')]))
        self.both(lambda u: u.replace('skin', 'x_style', 'bangumi', [('zh', '新样式')]))
        self.both(lambda u: u.replace('character', 'no_such_tag', 'bangumi', [('zh', '无主')]))
        for kind, tag, values in (('character', 'prio_(series_q)', ['alias a', 'alias b']),
                                  ('work', 'series_q', ['Q 系列', 'Q2']), ('skin', 'swimsuit', ['swim'])):
            with mock.patch.object(names, 'danbooru_names', return_value=values):
                self.both(lambda u, k=kind, t=tag: u.aliases(k, t, fetch=True))
        # danbooru_names 的实参会先调用 wiki()，不能只 mock 别名解析器。
        empty_wiki.assert_any_call('prio_(series_q)')
        with self.assertRaises(ValueError):
            self.updaters[1].replace('character', 'prio_(series_q)', 'bogus', [('zh', 'x')])
        with self.assertRaises(ValueError):
            self.updaters[0].replace('character', 'prio_(series_q)', 'bogus', [('zh', 'x')])
        self.same()                                                                            # 拒绝时两边都没写

    def test_worker_batches_keep_full_name_results(self):
        for updater in self.updaters:
            with mock.patch.object(updater, 'aliases', return_value=[]), \
                    mock.patch.object(updater, 'work', side_effect=lambda key, _: updater.replace('work', key, 'bangumi', [('zh', '批量作品')])), \
                    mock.patch.object(updater, 'character', side_effect=lambda key, work, _: updater.replace('character', key, 'bangumi', [('zh', '批量角色')])), \
                    mock.patch.object(updater, 'translate', return_value=None):
                updater.run(refresh=True)
            self.assertEqual(updater.state['status'], 'done', updater.state)
        self.same()
        self.assertEqual(self.updaters[0].state, self.updaters[1].state)
        for updater in self.updaters:
            with updater.store.connect() as c:
                done = {r[0] for r in c.execute("SELECT key FROM name_cache WHERE key LIKE '[\"done\",%'")}
            self.assertEqual(len(done), updater.state['done'])

    def test_batch_failure_replays_partial_success_without_false_done(self):
        old, new = self.updaters
        bad = [('bangumi', [('zh', '保留部分成功')]), ('bogus', [('zh', '拒绝')]), ('wiki', [('zh', '不得执行')])]
        with self.assertRaises(ValueError):
            for source, values in bad:
                old.replace('character', 'prio_(series_q)', source, values)
        old.replace('character', 'pref_(series_q)', 'bangumi', [('zh', '其他任务成功')])
        jobs = []
        for key, replacements in [('prio_(series_q)', bad), ('pref_(series_q)', [('bangumi', [('zh', '其他任务成功')])])]:
            new._replacements.pending = []
            for source, values in replacements:
                new.replace('character', key, source, values)
            jobs.append((('character', key, ''), new._replacements.pending, True))
        new._replacements.pending = None
        with mock.patch.object(new, '_mark_done', wraps=new._mark_done) as done:
            new._flush_replacements(jobs)
        self.assertEqual(done.call_args_list, [mock.call('character', 'pref_(series_q)', '')])
        self.assertIn('error', new.state)
        self.same()

    def test_incomplete_job_flushes_partial_writes_but_is_retryable(self):
        old, new = self.updaters
        old.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '网络失败之前')])
        new._replacements.pending = []
        new.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '网络失败之前')])
        jobs = [(('character', 'prio_(series_q)', ''), new._replacements.pending, False)]
        new._replacements.pending = None
        with mock.patch.object(new, '_mark_done') as done:
            new._flush_replacements(jobs)
        done.assert_not_called()
        self.same()

    def test_model_enabled_mid_run_sees_pending_names(self):
        old, new = self.updaters
        old.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '已选中文')])
        new._replacements.pending = []
        new.replace('character', 'prio_(series_q)', 'bangumi', [('zh', '已选中文')])
        with mock.patch.object(new, 'settings', return_value={'key': 'test-key'}), \
                mock.patch.object(new, 'completion', side_effect=AssertionError('已有中文不应调用模型')):
            new.translate('character', 'prio_(series_q)')
        self.assertEqual(new._replacements.pending, [])
        new._replacements.pending = None
        self.same()

    def test_random_name_changes_equal_full_rebuild(self):
        import random
        rng = random.Random(730)
        # 固定前缀确保父名与合并借名真的变动，避免随机来源被更高优先级遮住。
        self.both(lambda updater: updater.replace('character', 'prio_(series_q)', 'user', [('zh', '父名确实改变')]))
        self.both(lambda updater: updater.replace('character', 'rep_old', 'bangumi', [('zh', '借名确实改变')]))
        keys = [('character', 'prio_(series_q)'), ('character', 'rep_old'),
                ('character', 'pref_(series_q)'), ('work', 'series_q'),
                ('skin', 'x_style'), ('skin', 'swimsuit'), ('work', 'missing_work')]
        for step in range(40):
            kind, key = rng.choice(keys)
            source = rng.choice(['bangumi', 'model', 'wiki', 'user'])
            values = [] if step % 4 == 0 else [('zh', f'随机名{step}'), ('ja', f'かな{step}')]
            with self.subTest(step=step, kind=kind, key=key, source=source):
                self.both(lambda updater: updater.replace(kind, key, source, values))
                with self.new.connect() as c:
                    incremental = projected(c)
                terms_core.run('names-project', '--db', self.new.path)
                with self.new.connect() as c:
                    self.assertEqual(incremental, projected(c))

    def test_intermediate_projection_keeps_history_and_later_changes(self):
        for name, tag in [('第一次改名', None), ('第二次改名', 'prio_(series_q)'), ('第三次改名', None)]:
            for store in (self.serial, self.new):
                result = store.set_profile('c-prio', name=name, tag=tag)
                self.assertEqual(result['undo']['label'], f'已更新 {name} 的资料')
            self.same()
            with self.new.connect() as c:
                self.assertEqual(c.execute("SELECT value FROM meta WHERE key='names_version'").fetchone()[0],
                                 c.execute("SELECT value FROM meta WHERE key='names_display_projected'").fetchone()[0])

    def test_seed_and_album_aliases(self):
        for u in self.updaters:
            with u.store.connect() as c:
                rows = list(names.scope(c).values()) if isinstance(names.scope(c), dict) else names.scope(c)
            u.seed(rows)
        self.same()
        for s in (self.serial, self.new):
            s._album_titles(add_aliases=True)
        self.same()

    def test_character_commands_keep_the_projection(self):
        for s in (self.serial, self.new):
            s.set_profile('c-prio', name='改过的名字', aliases=['别名甲'])
        self.same()
        for s in (self.serial, self.new):
            with mock.patch('uuid.uuid4', side_effect=[__import__('uuid').UUID(int=7)]):
                s.create_entity('又一个新角色')
        self.same()

    def test_search_uses_the_callers_read_snapshot_during_a_concurrent_rename(self):
        needle = '并发写入后的独有名字'
        with self.new.connect() as c:
            c.execute('PRAGMA journal_mode=WAL')
            c.execute('BEGIN')
            before = names.search_hits_python(c, needle)
            self.assertEqual(before, {})
            self.new.set_profile('c-prio', name=needle)
            self.assertEqual(names.search_hits(c, needle), before)
        with self.new.connect() as c:
            self.assertIn('c-prio', names.search_hits(c, needle))

    def test_daily_commands_repair_preexisting_stale_names(self):
        calls = [lambda s: s.change(['a'], 'confirmed', 'c-prio'),
                 lambda s: s.sweep('c-prio'),
                 lambda s: s.answer('c-prio', yes=['a'])]
        for index, call in enumerate(calls):
            for store in (self.serial, self.new):
                with store.connect() as c:
                    c.execute("INSERT OR REPLACE INTO meta VALUES ('terms_writer','1')")
                    c.execute("DELETE FROM names WHERE entity_id='c-prio' AND source='user'")
                    c.execute("INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES ('c-prio','zh',?,'user','display',1,0)",
                              (f'待同步名字{index}',))
                    c.execute("DELETE FROM meta WHERE key='terms_writer'")
                call(store)
            self.same()

    def test_python_writes_to_name_tables_are_refused_after_the_switch(self):
        with sqlite3.connect(self.new.path) as c:
            for sql in ("INSERT INTO names(entity_id,lang,value,source,role) VALUES ('c-prio','zh','x','user','display')",
                        "DELETE FROM display_names", "UPDATE name_aliases SET position=1"):
                with self.subTest(sql=sql), self.assertRaisesRegex(sqlite3.IntegrityError, 'written by the terms core'):
                    c.execute(sql)
            c.execute("INSERT OR REPLACE INTO name_cache VALUES ('k','v')")                     # 外围表照常可写


if __name__ == '__main__':
    unittest.main()
