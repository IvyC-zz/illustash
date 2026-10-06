import hashlib
from pathlib import Path
import tempfile
import unittest
from tests.test_web import fixture
from tests import terms_seed
from web.store import Store
from web import identity


class IdentityTest(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name); self.source=fixture(self.root)
        self.store=Store(self.source,self.root/'app.sqlite',identity='hybrid'); self.store.refresh()
        with self.store.connect() as c:
            ids=dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
        self.a,self.b=ids['alice'],ids['bob']
        self.digest=hashlib.sha256(self.source.read_bytes()).hexdigest()

    def tearDown(self):
        self.assertEqual(hashlib.sha256(self.source.read_bytes()).hexdigest(),self.digest)

    def relation(self,a,b,kind='duplicate_of',status='accepted'):
        return self.store.decide_identity(a=a,b=b,kind=kind,status=status,reason='test')

    def confirmed(self,uuid):
        """人工确认的归属（hybrid 下机器归属照常存在，这里只看关系层怎么投影人的结论）。"""
        return [r for r in self.store.identity_assignments(uuid)['assignments'] if r['status']=='confirmed']

    def test_duplicate_is_projection_no_records_move_and_undo_restores(self):
        self.store.change(['b'],'confirmed',self.b)
        with self.store.connect() as c:
            before={t:list(map(tuple,c.execute('SELECT * FROM '+t))) for t in ('assertions','candidates','links','names')}
        action=self.relation(self.b,self.a)
        with self.store.connect() as c:
            for t,rows in before.items():
                self.assertEqual(list(map(tuple,c.execute('SELECT * FROM '+t))),rows,t)
        self.assertEqual({r['character_id'] for r in self.confirmed('b')},{self.a})
        self.store.undo(action['id'])
        self.assertEqual({r['character_id'] for r in self.confirmed('b')},{self.b})

    def test_new_group_claim_is_pending_after_split(self):
        self.relation(self.b,self.a)
        self.store.change(['c'],'confirmed',self.a)
        self.relation(self.b,self.a,status='proposed')
        result=self.store.identity_assignments('c')
        self.assertEqual(result['assignments'],[])
        self.assertTrue(any(r['reason']=='equivalence_split' for r in result['undetermined']))
        # Human can resolve the ambiguity without erasing its old evidence.
        self.store.change(['c'],'confirmed',self.a)
        self.assertEqual(len(self.store.identity_assignments('c')['assignments']),1)

    def test_redundant_edges_keep_group_until_actual_split(self):
        third=self.store.create_entity('Third')['id']
        self.relation(self.a,self.b)
        self.relation(self.b,third)
        self.relation(self.a,third)
        self.store.change(['c'],'confirmed',third)
        self.relation(self.a,third,status='rejected')
        with self.store.connect() as c:
            self.assertEqual(len(set(dict(c.execute('SELECT entity,canonical FROM identity_canonical')).values())),1)
            self.assertEqual(c.execute('SELECT count(*) FROM identity_suspended').fetchone()[0],0)
        self.relation(self.b,third,status='rejected')
        self.assertEqual(self.store.identity_assignments('c')['assignments'],[])

    def test_form_keeps_id_flattens_chain_and_rejects_cycle(self):
        child=self.store.create_entity('Child')['id']
        self.relation(child,self.a,'form_of')
        self.store.change(['b'],'confirmed',child)
        assignment=self.confirmed('b')[0]
        self.assertEqual((assignment['character_id'],assignment['variant_id']),(self.a,child))
        other=self.store.create_entity('Subform')['id']
        self.relation(other,child,'form_of')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?',(other,)).fetchone()[0],self.a)
        with self.assertRaises(ValueError): self.relation(self.a,other,'form_of')
        self.store.refresh()
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?',(child,)).fetchone()[0],self.a)

    def test_form_reinterpretation_applies_to_duplicate_component_without_moving_ids(self):
        parent=self.store.create_entity('Person')['id']
        self.relation(self.a,self.b)
        self.relation(self.b,parent,'form_of')
        with self.store.connect() as c:
            nodes={r[0]:tuple(r[1:]) for r in c.execute('SELECT id,node_type,character_id,variant_id FROM identity_nodes')}
        self.assertEqual(nodes[self.a],('variant',parent,self.b))
        self.assertEqual(nodes[self.b],('variant',parent,self.b))
        with self.assertRaises(ValueError):self.relation(parent,self.a)
        # Undoing duplication restores the unattached spelling's independent ID.
        self.relation(self.a,self.b,status='proposed')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT node_type FROM identity_nodes WHERE id=?',(self.a,)).fetchone()[0],'character')
            self.assertEqual(c.execute('SELECT parent_id FROM identity_nodes WHERE id=?',(self.b,)).fetchone()[0],parent)

    def test_different_from_blocks_transitive_equivalence(self):
        self.relation(self.a,self.b,'different_from')
        with self.assertRaises(ValueError): self.relation(self.a,self.b)
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM identity_relations WHERE kind='duplicate_of'").fetchone()[0],0)

    def test_ambiguity_is_explicit_reversible_and_not_training_truth(self):
        action=self.store.uncertain_identity(['a'],[self.a,self.b])
        self.assertEqual(self.store.identity_assignments('a')['assignments'],[])
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM family_confirmed').fetchone()[0],0)
            self.assertEqual(c.execute('SELECT count(*) FROM assertions').fetchone()[0],1)
        self.store.undo(action['id'])
        self.assertEqual(len(self.store.identity_assignments('a')['assignments']),1)

    def test_conflicting_assertions_do_not_train(self):
        self.store.change(['b'],'confirmed',self.a)
        self.store.change(['b'],'rejected',self.b)
        self.relation(self.b,self.a)
        self.assertEqual(self.store.identity_assignments('b')['assignments'],[])

    def test_conflicting_variant_alias_verdict_does_not_confirm_parent(self):
        variant=self.store.create_entity('Variant alias')['id']
        self.relation(self.b,self.a,'form_of');self.relation(variant,self.a,'form_of')
        self.store.change(['b'],'confirmed',self.b)
        self.store.change(['b'],'rejected',variant)
        self.relation(self.b,variant)
        self.assertEqual(self.confirmed('b'),[])
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM assertions WHERE uuid='b'").fetchone()[0],2)

    def test_raw_candidate_ids_survive_refresh_after_merge(self):
        self.relation(self.b,self.a)
        self.store.refresh()
        with self.store.connect() as c:
            self.assertIsNotNone(c.execute('SELECT 1 FROM candidates WHERE entity=?',(self.b,)).fetchone())

    def test_undo_manual_resolution_restores_group_pending_scope(self):
        self.relation(self.b,self.a)
        self.store.change(['c'],'confirmed',self.a)
        self.relation(self.b,self.a,status='proposed')
        action=self.store.change(['c'],'confirmed',self.a)
        self.store.undo(action['id'])
        self.assertEqual(self.store.identity_assignments('c')['assignments'],[])
        self.assertTrue(any(r['reason']=='equivalence_split' for r in self.store.identity_assignments('c')['undetermined']))

    def test_legacy_destructive_merge_migrates_to_revocable_edge(self):
        # Separate legacy fixture avoids manufacturing state under v2 triggers.
        store=Store(self.source,self.root/'legacy.sqlite');store.refresh()
        with store.connect() as c:ids=dict(c.execute("SELECT key,entity_id FROM links WHERE system='ccip'"))
        a,b=ids['alice'],ids['bob']
        store.change(['b'],'confirmed',b);store.merge(b,a)
        store=terms_seed.startup_input(store)
        store.activate_identity()
        terms_seed.switch(store)
        store.decide_identity(a=b,b=a,kind='duplicate_of',status='proposed',reason='撤销旧合并')
        self.assertEqual(store.identity_assignments('b')['assignments'],[])
        self.assertTrue(store.identity_assignments('b')['undetermined'])

    def test_inherited_variant_relation_can_be_revoked_without_losing_id(self):
        (self.root / 'hierarchy').mkdir()
        store=terms_seed.base(self.root / 'hierarchy', db=self.root / 'hierarchy' / 'library.sqlite')
        # 固定旧输入中布置继承层级，再验收真实启动迁移。
        with store.connect() as c:
            a=c.execute("SELECT entity_id FROM links WHERE system='ccip' AND key='alice'").fetchone()[0]
            c.execute("INSERT INTO entities(id,name,kind,parent_id,variant) VALUES ('legacy-skin','','skin',?,'summer')",(a,))
        store.activate_identity()
        with store.connect() as c:
            rel=c.execute("SELECT id FROM identity_relations WHERE a='legacy-skin' AND kind='form_of'").fetchone()[0]
        terms_seed.switch(store)
        store.decide_identity(relation_id=rel,status='rejected',reason='旧层级错误')
        with store.connect() as c:
            row=c.execute("SELECT id,node_type,parent_id FROM identity_nodes WHERE id='legacy-skin'").fetchone()
            self.assertEqual(tuple(row),('legacy-skin','character',None))

    def test_new_evidence_flags_old_decision_but_identical_refetch_does_not(self):
        self.store.decide_identity(a=self.a,b=self.b,kind='duplicate_of',status='rejected',reason='人工确认不同',
                                   evidence={'fact':'one','response_ids':[1]})
        self.store._core({'op':'propose','a':self.a,'b':self.b,'kind':'duplicate_of','evidence':{'fact':'one','response_ids':[2]},'source':'manual'})
        row=self.store.identity_relations()['items'][0]
        self.assertFalse(row['new_evidence_requires_review'])
        self.store._core({'op':'propose','a':self.a,'b':self.b,'kind':'duplicate_of','evidence':{'fact':'two','response_ids':[3]},'source':'manual'})
        row=self.store.identity_relations()['items'][0]
        self.assertEqual(row['status'],'rejected')
        self.assertTrue(row['new_evidence_requires_review'])

if __name__=='__main__': unittest.main()
