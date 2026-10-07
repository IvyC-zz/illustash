"""用户标签、分级、WD 题材切到词条核心（docs/TERMS_LABELS.md）：切换前后读到的一样，写和撤销走核心。只用临时库。"""
import contextlib
import io
import json
import os
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from web import terms_core
from web.server import adopt_labels
from web.store import Invalid, Store, _indexes_a_view
from tests.synthetic_source import source_fixture as fixture
from tests import terms_seed
from tests.test_terms_core import core


class IndexGuardTest(unittest.TestCase):
    def test_indexes_on_views_are_recognised(self):
        views = {'asset_tags', 'general_hits'}
        self.assertTrue(_indexes_a_view('\nCREATE INDEX IF NOT EXISTS asset_tags_tag ON asset_tags(tag,uuid)', views))
        self.assertTrue(_indexes_a_view('create unique index x on general_hits(uuid)', views))
        self.assertFalse(_indexes_a_view('CREATE INDEX IF NOT EXISTS candidates_entity ON candidates(entity,uuid)', views))
        self.assertFalse(_indexes_a_view('CREATE TABLE IF NOT EXISTS asset_tags(uuid TEXT)', views))


@unittest.skipIf(os.name == 'nt', 'macOS/Linux only for now')
class LabelsInCoreTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name).resolve()
        self.source = fixture(self.root)
        with sqlite3.connect(self.source) as c:
            c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [
                ('a', 'rating', 'general', 0.8), ('a', 'rating', 'sensitive', 0.1),
                ('b', 'rating', 'explicit', 0.7), ('b', 'rating', 'questionable', 0.2),
                ('c', 'rating', 'sensitive', 0.9),
                ('b', 'general', 'solo', 0.9), ('b', 'general', '1girl', 0.8), ('c', 'general', 'solo', 0.3)])
        (self.root / 'lib').mkdir()
        self.store = Store(self.source, self.root / 'lib' / 'library.sqlite')
        self.store.refresh()
        # 旧标签与原历史作为启动输入；切换后的写入只走公开核心入口。
        self.store = terms_seed.startup_input(self.store)
        with self.store.connect() as c:
            c.execute("INSERT INTO asset_tags(uuid,tag,origin) VALUES ('a','summer','review')")
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)',
                      ('已给 1 张加上标签 summer', json.dumps([{'uuid':'a','tag':'summer','had':None}])))
            self.legacy = Store.last_action(c)

    def tearDown(self):
        self.temp.cleanup()

    def test_native_cutovers_have_recoverable_backups_without_app_wrapper(self):
        terms_core.run('migrate', '--db', self.store.path)
        self.store._deliver_wd(self.store.library_uuids())
        folder = self.store.path.parent / 'library-backups'
        for domain, cutover in [('artists', terms_core.cutover_artists), ('labels', terms_core.cutover_labels),
                                ('characters', terms_core.cutover_characters), ('names', terms_core.cutover_names)]:
            with self.subTest(domain=domain):
                cutover(self.store.path)
                with self.store.connect() as c:
                    self.assertEqual(c.execute('SELECT value FROM meta WHERE key=?', ('terms_' + domain,)).fetchone()[0], 'core')
                backups = list(folder.glob(f'terms-before-cutover-{domain}-*.sqlite'))
                self.assertEqual(len(backups), 1)
                with sqlite3.connect(backups[0]) as c:
                    self.assertIsNone(c.execute('SELECT value FROM meta WHERE key=?', ('terms_' + domain,)).fetchone())
                    self.assertEqual(c.execute('PRAGMA quick_check').fetchone()[0], 'ok')
                    self.assertEqual(c.execute('SELECT count(*) FROM assets').fetchone()[0], 3)
                cutover(self.store.path)
                self.assertEqual(list(folder.glob(f'terms-before-cutover-{domain}-*.sqlite')), backups)

    def test_native_cutover_backup_failure_leaves_old_tables_unchanged(self):
        terms_core.run('migrate', '--db', self.store.path)
        self.store._deliver_wd(self.store.library_uuids())
        blocker = self.store.path.parent / 'library-backups'
        blocker.write_text('阻止创建备份目录')
        with sqlite3.connect(self.store.path) as c:
            before = list(c.iterdump())
        with self.assertRaises((terms_core.Failed, RuntimeError)):
            terms_core.cutover_labels(self.store.path)
        with sqlite3.connect(self.store.path) as c:
            self.assertEqual(list(c.iterdump()), before)
        blocker.unlink()

    def adopt(self):
        adopted = adopt_labels(self.store)
        if adopted:
            terms_seed.switch(self.store)
        return adopted

    def snapshot(self):
        with self.store.connect() as c:
            tables = {t: sorted(map(tuple, c.execute(f'SELECT * FROM {t}'))) for t in ('general_hits', 'asset_rating', 'single_assets', 'lead_assets')}
            tables['asset_tags'] = sorted(map(tuple, c.execute('SELECT uuid,tag FROM asset_tags')))
        s = self.store
        return tables, s.ids({'tag': 'solo'}), s.ids({'nsfw': 'hide'}), s.ids({'utag': 'summer'}), s.user_tags()

    def kind(self, name):
        with self.store.connect() as c:
            return c.execute('SELECT type FROM sqlite_master WHERE name=?', (name,)).fetchone()[0]

    def test_the_switch_reads_the_same_and_writes_through_the_core(self):
        before = self.snapshot()
        self.assertTrue(self.adopt())
        self.assertTrue(self.adopt())                                 # once only
        self.assertEqual([self.kind(n) for n in ('general_hits', 'asset_rating', 'asset_tags')], ['view', 'view', 'view'])
        with self.store.connect() as c:                                            # no frozen copy (terms schema 4)
            self.assertFalse(c.execute("SELECT 1 FROM sqlite_master WHERE name='legacy_asset_tags'").fetchone())
        self.assertEqual(self.snapshot(), before)
        # Tags: the core writes them; both its own step and the old one are undone by it.
        step = self.store.tag(['b'], 'summer')
        self.assertEqual(step['label'], '已给 1 张加上标签 summer')
        self.assertEqual(self.store.user_tags(), [{'tag': 'summer', 'count': 2}])
        after = self.store.undo(step['id'])
        self.assertEqual(after['id'], self.legacy['id'])
        self.assertEqual(sorted(self.store.ids({'utag': 'summer'})), ['a'])
        self.assertIsNone(self.store.undo(self.legacy['id']))
        self.assertEqual(self.store.user_tags(), [])
        removal = self.store.tag(['a'], 'summer', add=False)                     # a no-op removal is still a step
        self.assertEqual(removal['label'], '已从 1 张去掉标签 summer')
        with self.assertRaisesRegex(Invalid, '没有标签'):
            self.store.tag(['a'], 'never-used', add=False)
        # A rebuild of WD hits delivers again (nothing new) and rebuilds what derives from them.
        self.store.refresh_general_hits()
        self.assertEqual(self.snapshot()[0]['general_hits'], before[0]['general_hits'])

    def test_syncing_new_pictures_after_the_switch(self):
        self.adopt()
        with sqlite3.connect(self.source) as c:
            c.execute("INSERT INTO assets VALUES ('d','d.png',?,120,150,'2026-09-29','orig',1,0)", (str(self.root / 'd.png'),))
            c.executemany('INSERT INTO labels VALUES (?,?,?,?)', [('d', 'rating', 'explicit', 0.9), ('d', 'general', 'solo', 0.95)])
        self.assertEqual(self.store.import_changes(added=['d']), {'added': 1, 'removed': 0})
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT rating FROM asset_rating WHERE uuid='d'").fetchone()[0], 'explicit')
            self.assertEqual([r[0] for r in c.execute("SELECT value FROM general_hits WHERE uuid='d'")], ['solo'])
            self.assertTrue(c.execute("SELECT 1 FROM single_assets WHERE uuid='d'").fetchone())   # derived rules see it
        self.assertIn('d', self.store.ids({'tag': 'solo'}))
        self.assertNotIn('d', self.store.ids({'nsfw': 'hide'}))
        self.store.import_changes(removed=['b'])
        with self.store.connect() as c:
            self.assertIsNone(c.execute("SELECT 1 FROM general_hits WHERE uuid='b'").fetchone())

    def test_materialized_selection_matches_direct_count_page_and_decoration(self):
        self.adopt()
        for nsfw in ('hide', 'only'):
            for tag in ('', 'solo'):
                for order in ('oldest', 'newest', 'suspicious', 'hinted'):
                    for offset in (0, 1, 20):
                        for include_total in (True, False):
                            filters = dict(nsfw=nsfw, tag=tag, order=order)
                            with self.store.connect() as c:
                                c.execute('BEGIN')
                                where, args, ordering = self.store._selection(c, filters)
                                total = c.execute(f'SELECT count(*) FROM assets a WHERE {where}', args).fetchone()[0] if include_total else None
                                rows = [dict(r) for r in c.execute(f'SELECT a.uuid,a.width,a.height FROM assets a WHERE {where} ORDER BY {ordering} LIMIT 1 OFFSET ?', [*args, offset])]
                                expected = dict(items=self.store._decorate(c, rows), total=total, offset=offset)
                            self.assertEqual(self.store.assets(**filters, limit=1, offset=offset, include_total=include_total), expected)

    def test_combined_core_selection_equals_original_views(self):
        self.adopt()
        for nsfw in ('hide', 'only'):
            for tag in ('solo', 'blue_hair', '1girl'):
                for order in ('oldest', 'newest', 'suspicious', 'hinted'):
                    for offset in (0, 1, 20):
                        filters = dict(nsfw=nsfw, tag=tag, order=order)
                        with mock.patch.object(terms_core, 'labels_in_core', return_value=False):
                            expected = self.store.assets(**filters, limit=1, offset=offset)
                        self.assertEqual(self.store.assets(**filters, limit=1, offset=offset), expected)

    def pending(self):
        return terms_core.run('wd-pending', '--db', self.store.path, '--source', self.source,
                              stdin=json.dumps({'uuids': sorted(self.store.library_uuids()), 'library_id': self.store.library_id()}))['pending']

    def test_unchanged_wd_skips_packages_with_original_content_fingerprint(self):
        self.adopt()
        before = self.snapshot()
        self.assertEqual(self.pending(), [])
        with mock.patch('web.wd_export.packages', side_effect=AssertionError('不应重交未变化的证据')):
            self.assertEqual(self.store._deliver_wd(self.store.library_uuids()), 0)
        self.assertEqual(self.snapshot(), before)
        # 采集时间不在原内容指纹里；改变它也不能偷偷改出新的内容版本。
        with sqlite3.connect(self.source) as c:
            c.execute('ALTER TABLE labels ADD COLUMN ts TEXT')
            c.execute("UPDATE labels SET ts='2026-09-30 08:00:00'")
        self.assertEqual(self.pending(), [])

    def test_changed_wd_score_still_uses_original_conflict_and_is_atomic(self):
        self.adopt()
        before = self.snapshot()
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE labels SET conf=0.77 WHERE uuid='a' AND dim='rating' AND value='general'")
        self.assertEqual(self.pending(), ['a'])
        with self.assertRaisesRegex(Invalid, '已有不同的结果'):
            self.store._deliver_wd(self.store.library_uuids())
        self.assertEqual(self.snapshot(), before)

    def test_new_wd_version_and_method_are_delivered_and_then_skipped(self):
        self.adopt()
        before = self.snapshot()
        with sqlite3.connect(self.source) as c:
            c.execute('ALTER TABLE labels ADD COLUMN model_ver TEXT')
            c.execute("ALTER TABLE labels ADD COLUMN source TEXT DEFAULT 'wd'")
            c.execute("UPDATE labels SET model_ver='v2',source='wd_new' WHERE uuid='b'")
        self.assertEqual(self.pending(), ['b'])
        self.assertGreater(self.store._deliver_wd(self.store.library_uuids()), 0)
        self.assertEqual(self.pending(), [])
        self.assertEqual(self.snapshot(), before)

    def test_removed_registration_is_not_mistaken_for_delivered_live_asset(self):
        self.adopt()
        # 库里的图片仍存在，但核心登记被标成移除，原交付会恢复登记；预检必须保留交付。
        with sqlite3.connect(self.store.path) as c:
            c.execute("UPDATE term_assets SET removed_at=CURRENT_TIMESTAMP WHERE namespace='library' AND key='b'")
        self.assertEqual(self.pending(), ['b'])
        self.store._deliver_wd(['b'], new_assets=True)
        self.assertEqual(self.pending(), [])

    def test_duplicate_source_rows_and_invalid_binding_do_not_bypass_package_validation(self):
        self.adopt()
        with sqlite3.connect(self.source) as c:
            c.execute("INSERT INTO labels SELECT * FROM labels WHERE uuid='b' AND dim='rating' AND value='explicit'")
        self.assertEqual(self.pending(), ['b'])
        with self.assertRaises(Invalid):
            self.store._deliver_wd(['b'])
        with sqlite3.connect(self.source) as c:
            c.execute("DELETE FROM labels WHERE rowid=(SELECT max(rowid) FROM labels)")
        (self.store.path.parent / 'library.json').write_text(json.dumps({'id': ''}))
        self.assertEqual(self.pending(), ['a', 'b', 'c'])
        with self.assertRaises(Invalid):
            self.store._deliver_wd(['a'])

    def test_invalid_source_field_is_left_to_original_export_validation(self):
        self.adopt()
        with sqlite3.connect(self.source) as c:
            c.execute("ALTER TABLE labels ADD COLUMN source TEXT DEFAULT 'wd'")
            c.execute("UPDATE labels SET source=NULL WHERE uuid='b'")
        self.assertEqual(self.pending(), ['b'])
        with self.assertRaises(Invalid):
            self.store._deliver_wd(['b'])

    def test_without_the_core_the_library_opens_read_only(self):
        with mock.patch.dict(os.environ, {'ILLUSTASH_TERMS_BIN': str(self.root / 'missing')}), contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(adopt_labels(self.store))
        self.assertEqual(self.kind('general_hits'), 'table')                  # 旧表留着
        self.assertIn('找不到词条核心程序', self.store.read_only)
        with self.assertRaises(Invalid) as refused:
            self.store.tag(['b'], 'summer')
        self.assertEqual(str(refused.exception), self.store.read_only)
        with self.assertRaises(sqlite3.DatabaseError), self.store.connect() as c:
            c.execute("INSERT INTO asset_tags(uuid,tag,origin) VALUES ('b','summer','review')")
        with self.store.connect() as c:
            self.assertEqual(sorted(r[0] for r in c.execute('SELECT uuid FROM asset_tags')), ['a'])

    def test_a_failed_switch_leaves_the_old_tables_read_only(self):
        with mock.patch.object(terms_core, 'cutover_labels', side_effect=terms_core.Failed('boom')), contextlib.redirect_stdout(io.StringIO()):
            self.assertFalse(adopt_labels(self.store))
        self.assertEqual(self.kind('asset_tags'), 'table')
        self.assertIn('boom', self.store.read_only)
        self.store = Store(self.store.source, self.store.path)                    # next start tries again
        self.assertTrue(self.adopt())


if __name__ == '__main__':
    unittest.main()


class RequiredBackupCliTest(unittest.TestCase):
    """Direct shipped CLI must refuse file-backed upgrades/cutovers when backup cannot be created."""
    def test_upgrade_and_all_cutovers_refuse_backup_failure_without_modification(self):
        binary = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not binary or not Path(binary).is_file():
            self.skipTest('set ILLUSTASH_TERMS_BIN explicitly for native CLI acceptance')
        import subprocess
        with tempfile.TemporaryDirectory(prefix='illustash-required-backup-cli-') as tmp:
            root = Path(tmp)
            for command in ('migrate', 'cutover-artists', 'cutover-labels', 'cutover-characters', 'cutover-names'):
                with self.subTest(command=command):
                    folder = root / command
                    folder.mkdir()
                    db = folder / 'library.sqlite'
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        c.executescript("CREATE TABLE synthetic_rows(value BLOB); INSERT INTO synthetic_rows(rowid,value) VALUES (27,x'0001ff');")
                    prepared = subprocess.run([binary, 'migrate', '--db', str(db)], capture_output=True, text=True)
                    self.assertEqual(prepared.returncode, 0, prepared.stderr)
                    if command == 'migrate':
                        with contextlib.closing(sqlite3.connect(db)) as c, c:
                            c.execute("UPDATE meta SET value='6' WHERE key='terms_schema'")
                    blocker = folder / 'library-backups'
                    blocker.write_text('synthetic backup blocker')
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        before = list(c.iterdump())
                    refused = subprocess.run([binary, command, '--db', str(db)], capture_output=True, text=True)
                    self.assertNotEqual(refused.returncode, 0)
                    self.assertEqual(refused.stdout, '')
                    with contextlib.closing(sqlite3.connect(db)) as c, c:
                        self.assertEqual(list(c.iterdump()), before)
                        self.assertEqual(c.execute('SELECT rowid,value FROM synthetic_rows').fetchone(), (27, b'\x00\x01\xff'))
                    self.assertEqual(blocker.read_text(), 'synthetic backup blocker')
