"""Work identity, tree, filtering and watch migration invariants."""
import json
import unittest
from tests import test_names_migration, terms_seed
from web.index_update import IndexUpdater
from web.store import Store


class WorksTest(unittest.TestCase):
    setUp = test_names_migration.NamesMigrationTest.setUp
    def chain(self):
        parent = None
        ids = []
        for name in ('系列', '分支', '续作', '最新篇'):
            parent = self.store.edit_work('create', name=name, parent=parent)['id']
            ids.append(parent)
        return ids

    def test_arbitrary_depth_search_filter_and_cycle_atomicity(self):
        ids = self.chain()
        self.store.set_work(self.alice, ids[-1])
        for ident in ids:
            self.assertEqual([r['id'] for r in self.store.characters(work=ident)], [self.alice])
            self.assertEqual(self.store.assets(work=ident)['total'], 2)
        self.assertEqual([r['id'] for r in self.store.characters('系列')], [self.alice])
        self.assertEqual(self.store.assets(query='系列')['total'], 2)
        before = self.store.works()
        with self.assertRaises(ValueError):
            self.store.edit_work('edit', ids[0], '不可保存', ids[-1], 'should_not_exist')
        self.assertEqual(self.store.works(), before)
        self.store.edit_work('move', ids[-1], parent=None)
        self.assertEqual(self.store.characters(work=ids[0]), [])
        self.assertEqual(self.store.assets(work=ids[0])['total'], 0)

    def test_works_never_become_characters_or_accept_assertions(self):
        work = self.chain()[0]
        self.assertNotIn(work, {r['id'] for r in self.store.entities('系列')})
        self.assertNotIn(work, {r['id'] for r in self.store.characters()})
        with self.assertRaises(ValueError):
            self.store.change(['a'], 'confirmed', work)
        with self.assertRaises(ValueError):
            self.store.merge(work, self.alice)

    def test_unknown_import_does_not_fabricate_work_and_same_names_are_distinct(self):
        one = self.store.edit_work('create', name='同名')['id']
        two = self.store.edit_work('create', name='同名')['id']
        self.assertNotEqual(one, two)
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                e = Store._entity(c, 'ccip', 'new_character', '同名', 'unknown_suffix')
        self.store = old
        with self.store.connect() as c:
            self.assertIsNone(c.execute('SELECT work_id FROM entities WHERE id=?', (e,)).fetchone()[0])
            self.assertFalse(c.execute("SELECT 1 FROM links WHERE system='qualifier' AND key='unknown_suffix'").fetchone())

    def test_original_and_skin_assignment(self):
        work = self.chain()[-1]
        oc = self.store.create_entity(None, kind='oc')['id']
        with self.store.connect() as c:
            self.assertEqual(tuple(c.execute('SELECT kind,original FROM entities WHERE id=?', (oc,)).fetchone()), ('character', 1))
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("INSERT INTO entities(id,name,kind,parent_id) VALUES ('skin','','skin',?)", (self.alice,))
        self.store = old
        with self.assertRaises(ValueError):
            self.store.set_work(oc, work)
        with self.assertRaises(ValueError):
            self.store.set_work('skin', work)
        self.store.set_work(self.alice, work)
        with self.store.connect() as c:
            self.assertEqual(self.store._display({'id':'skin','work':''}, c)['work_id'], work)
        self.store.set_kind(self.alice, 'oc')
        self.assertEqual(self.store.characters(work=work), [])

    def test_watch_ids_follow_tag_rename_and_reject_unlinked_works(self):
        w = self.store.edit_work('create', name='已确认', tag='old_tag')['id']
        missing = self.store.edit_work('create', name='未知')['id']
        calls = []
        updater = IndexUpdater(self.store, finder=lambda tags, *args: calls.append(tags) or [])
        with self.assertRaises(ValueError):
            updater.set_watch([missing], 100)
        updater.set_watch([w], 100)
        self.store.edit_work('link', w, tag='new_tag')
        updater._check()
        self.assertEqual(calls, [['new_tag']])
        self.assertEqual(updater.watch()['works'], [w])
        self.store.edit_work('edit', w, name='已确认', tag='')
        updater._check()
        self.assertEqual(calls[-1], [])
        self.store.edit_work('link', w, tag='new_tag')
        updater._check()
        self.assertEqual(calls[-1], ['new_tag'])
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT current FROM links WHERE key='old_tag'").fetchone()[0], 0)

    def test_legacy_watch_preserves_unresolved_tags_and_backup(self):
        w = self.store.edit_work('create', name='确认作品', tag='confirmed')['id']
        updater = IndexUpdater(self.store)
        old = {'works':['confirmed','nikke'], 'min_posts':150}
        updater._write('watch.json', old)
        self.assertEqual(updater.watch(), {'version':2,'works':[w],'pending_tags':['nikke'],'min_posts':150})
        self.assertEqual(json.loads((updater.dir/'watch-v1.json').read_text()), old)
        self.assertEqual(updater.watch()['works'], [w])

    def test_work_link_conflict_rolls_back_creation(self):
        self.store.edit_work('create', name='A', tag='same')
        before = self.store.works()
        with self.assertRaises(ValueError):
            self.store.edit_work('create', name='B', tag='same')
        self.assertEqual(self.store.works(), before)

    def test_evidence_exact_match_and_manual_parent_protection(self):
        w = self.store.edit_work('create', name='Child')['id']
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                Store.link(c, w, 'qualifier', 'child')
                c.execute('UPDATE work_review SET parent_locked=0 WHERE entity_id=?', (w,))
        self.store = old
        evidence = {'tags':[{'name':x,'category':3} for x in ('child','parent','grandparent')],
                    'implications':[{'antecedent_name':a,'consequent_name':b,'status':'active'} for a,b in [('child','parent'),('parent','grandparent')]]}
        self.store.apply_work_evidence(evidence)
        before = self.store.works()
        self.store.apply_work_evidence(evidence)
        self.assertEqual(before, self.store.works())
        self.store.edit_work('move', w, parent=None)
        self.store.apply_work_evidence(evidence)
        with self.store.connect() as c:
            self.assertIsNone(c.execute('SELECT parent_id FROM entities WHERE id=?',(w,)).fetchone()[0])

    def test_nonwork_tag_and_multiple_parents_remain_pending(self):
        w = self.store.edit_work('create', name='Pending')['id']
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                Store.link(c,w,'qualifier','nikke')
                c.execute('UPDATE work_review SET parent_locked=0 WHERE entity_id=?',(w,))
        self.store = old
        report = self.store.apply_work_evidence({'tags':[{'name':'nikke','category':0}]})
        with self.store.connect() as c:
            self.assertEqual(len(report['pending']),1)
            self.assertFalse(c.execute("SELECT 1 FROM links WHERE entity_id=? AND system='danbooru'",(w,)).fetchone())
