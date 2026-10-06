"""Batch reads preserve browse contracts and avoid per-result SQL work."""
from contextlib import contextmanager
from pathlib import Path
import tempfile
import sqlite3
import json
import unittest
from unittest.mock import patch

from tests.test_web import fixture
from tests import terms_seed
from web.store import Store


class BrowsePerformanceTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.store = Store(fixture(root), root / 'app.sqlite')
        self.store.refresh()
        self.alice = self.store.entities('Alice')[0]['id']
        self.bob = self.store.entities('Bob')[0]['id']
        work = self.store.edit_work('create', name='作品')['id']
        self.store.set_work(self.alice, work)
        self.skin = self.store.create_entity('衣装')['id']
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("UPDATE entities SET kind='skin',parent_id=?,variant='costume' WHERE id=?", (self.alice, self.skin))
                # 标签连同既有旧历史构成启动输入，避免把核心历史信封当成旧标签步骤。
                c.executemany("INSERT INTO asset_tags(uuid,tag,origin) VALUES ('b',?,'user')", [('z',),('a',)])
                c.executemany('INSERT INTO actions(label,before_json) VALUES (?,?)',
                              [(f'已给 1 张加上标签 {tag}', json.dumps([{'uuid':'b','tag':tag,'had':None}])) for tag in ('z','a')])
        self.store = old
        self.store.change(['b'], 'add_variant', self.skin)
        self.store.change(['c'], 'unknown')
        self.store.curate(['c'], 'exclude')
        with self.store.connect() as c:
            c.execute("INSERT INTO duplicates(uuid,grp,size,pos,grp_added,rep) VALUES ('b','b',2,0,'',1)")

    def hybrid(self):
        self.store = terms_seed.startup_input(self.store)
        self.store.activate_identity(mode='hybrid')
        terms_seed.switch(self.store)

    def traced(self, fn):
        statements = []
        connect = self.store.connect
        @contextmanager
        def traced_connect():
            with connect() as c:
                c.set_trace_callback(statements.append)
                yield c
        with patch.object(self.store, 'connect', traced_connect):
            result = fn()
        return result, statements

    def test_snapshot_display_matches_single_reads_in_both_identity_modes(self):
        self.store.create_entity('原创角色', kind='oc')
        for active in (False, True):
            if active:
                self.hybrid()
            with self.store.connect() as c:
                c.execute('BEGIN')
                rows = [dict(r) for r in c.execute('SELECT id,kind,work FROM entities')]
                expected = [(Store._display(r, c), Store._display(r, c, skin=True)) for r in rows]
                display = Store._display_reader(c)
                self.assertEqual([(display(r), display(r, skin=True)) for r in rows], expected)

    def test_single_character_skips_unrelated_variants_and_missing_skips_all(self):
        unrelated = self.store.create_entity('无关衣装')['id']
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("UPDATE entities SET kind='skin',parent_id=?,variant='other' WHERE id=?", (self.bob, unrelated))
        self.store = old
        all_rows = self.store.characters()
        original = Store._display
        seen = []
        def display(row, *args, **kwargs):
            seen.append(row['id'])
            return original(row, *args, **kwargs)
        with patch.object(Store, '_display', side_effect=display):
            self.assertEqual(self.store.characters(ident=self.alice), [r for r in all_rows if r['id'] == self.alice])
            self.assertNotIn(unrelated, seen)
            seen.clear()
            self.assertEqual(self.store.characters(ident='missing'), [])
            self.assertEqual(seen, [])
        self.assertEqual(self.store.characters(ident=self.alice, minimum=999), [])

    def test_single_variant_query_does_not_materialize_the_whole_view(self):
        _, statements = self.traced(lambda: self.store.characters(ident=self.alice))
        query = next(s for s in statements if 'FROM variant_memberships v' in s)
        with self.store.connect() as c:
            plan = [r[3] for r in c.execute('EXPLAIN QUERY PLAN ' + query)]
        self.assertFalse(any('MATERIALIZE variant_memberships' in step for step in plan), plan)

    def test_assets_match_legacy_per_image_reads(self):
        for active in (False, True):
            if active:
                self.hybrid()
            for filters in ({}, {'pool': 'excluded'}, {'offset': 1, 'limit': 1}, {'entity': self.alice}):
                for row in self.store.assets(**filters)['items']:
                    ident = row['uuid']
                    with self.store.connect() as c:
                        memberships = self.store._memberships(c, ident)
                        self.assertEqual(row['memberships'], memberships)
                        self.assertEqual(row['confirmed'], [m for m in memberships if m['status'] == 'confirmed'])
                        self.assertEqual(row['unknown'], bool(c.execute('SELECT 1 FROM deferred WHERE uuid=?', (ident,)).fetchone()))
                        self.assertEqual(row['user_tags'], [r[0] for r in c.execute('SELECT tag FROM asset_tags WHERE uuid=? ORDER BY tag', (ident,))])
                        dup = c.execute('SELECT grp,size,pos FROM duplicates WHERE uuid=?', (ident,)).fetchone()
                        self.assertEqual(row['dup'], dict(dup) if dup else None)
                        excluded = c.execute('SELECT reason FROM excluded WHERE uuid=?', (ident,)).fetchone()
                        self.assertEqual(row['excluded'], excluded[0] if excluded else None)
        self.assertEqual(self.store.assets(offset=999)['items'], [])

    def test_query_counts_do_not_grow_with_result_count(self):
        self.hybrid()
        _, small_chars = self.traced(self.store.characters)
        _, small_assets = self.traced(lambda: self.store.assets(limit=1))
        extra = [f'extra-{n}' for n in range(100)]
        with sqlite3.connect(self.store.source) as c:
            c.executemany("INSERT INTO assets VALUES (?,?,'',1,1,'','orig',1,0)", [(i,i) for i in extra])
        self.store.import_changes(added=extra)
        for ident in extra:
            entity = self.store.create_entity(ident)['id']
            self.store.change([ident], 'confirmed', entity)
        _, large_chars = self.traced(self.store.characters)
        _, large_assets = self.traced(lambda: self.store.assets(limit=200))
        self.assertLessEqual(len(large_chars), len(small_chars) + 1)
        self.assertLessEqual(len(large_assets), len(small_assets) + 6)
        self.assertLess(len(large_chars), 20)
        self.assertLess(len(large_assets), 25)

    def test_new_request_sees_name_and_membership_changes(self):
        before = self.store.characters(ident=self.alice)[0]
        self.store.set_profile(self.alice, name='新名字')
        after = self.store.characters(ident=self.alice)[0]
        self.assertNotEqual(before['name'], after['name'])
        self.assertEqual(after['name'], '新名字')
        initial = next(r for r in self.store.assets()['items'] if r['uuid'] == 'a')
        action = self.store.change(['a'], 'confirmed', self.bob)
        changed = next(r for r in self.store.assets()['items'] if r['uuid'] == 'a')
        self.assertIn(self.bob, [m['id'] for m in changed['confirmed']])
        self.store.undo(action['id'])
        restored = next(r for r in self.store.assets()['items'] if r['uuid'] == 'a')
        self.assertEqual(restored, initial)

class DirectorySummaryTest(unittest.TestCase):
    setUp = BrowsePerformanceTest.setUp
    traced = BrowsePerformanceTest.traced
    hybrid = BrowsePerformanceTest.hybrid
    def test_summary_preserves_visible_fields_and_skips_variant_counts(self):
        self.hybrid()
        # 分级是源输入；准备新旧库启动边界后再运行核心目录查询。
        with sqlite3.connect(self.store.source) as c:
            c.execute("INSERT INTO labels VALUES ('b','rating','explicit',1)")
        self.store = terms_seed.startup_input(self.store)
        with self.store.connect() as c:
            c.execute("INSERT OR REPLACE INTO asset_rating VALUES ('b','explicit')")
        terms_seed.switch(self.store)
        for nsfw in ('', 'hide', 'only'):
            for filters in ({}, {'ident': self.alice}, {'query': 'Alice'}, {'minimum': 1}):
                full = self.store.characters(nsfw=nsfw, **filters)
                summary, sql = self.traced(lambda: self.store.characters(nsfw=nsfw, summary=True, **filters))
                self.assertEqual([r['id'] for r in summary], [r['id'] for r in full])
                for brief, complete in zip(summary, full):
                    self.assertTrue(brief['summary'])
                    for key, value in brief.items():
                        if key not in ('summary', 'variants'):
                            self.assertEqual(value, complete[key], key)
                    self.assertEqual(brief['variants'], [{'id': v['id'], 'name': v['name']} for v in complete['variants']])
                self.assertFalse(any('FROM variant_memberships' in q for q in sql))

    def test_summary_tracks_changes_undo_names_and_tag_only(self):
        self.store.refresh()
        before = self.store.characters(summary=True)
        action = self.store.change(['b'], 'rejected', self.alice)
        self.assertNotEqual(before, self.store.characters(summary=True))
        self.store.undo(action['id'])
        self.assertEqual(before, self.store.characters(summary=True))
        self.store.set_profile(self.alice, name='新名字')
        self.assertEqual(self.store.characters(ident=self.alice, summary=True)[0]['name'], '新名字')
        self.store.set_mascot(self.alice, True)
        self.assertNotIn(self.alice, [r['id'] for r in self.store.characters(summary=True)])
        self.assertEqual(self.store.characters(ident=self.alice, summary=True)[0]['count'],
                         self.store.characters(ident=self.alice)[0]['count'])

    def test_single_character_union_uses_entity_indexes(self):
        for entity in (self.alice, self.bob, 'missing'):
            _, statements = self.traced(lambda: self.store.characters(ident=entity))
            sql = next(q for q in statements if 'UNION ALL SELECT * FROM tag_memberships' in q)
            with self.store.connect() as c:
                plan = [r[3] for r in c.execute('EXPLAIN QUERY PLAN ' + sql)]
            self.assertFalse(any('SCAN memberships' in row or 'SCAN tag_memberships' in row for row in plan), plan)
