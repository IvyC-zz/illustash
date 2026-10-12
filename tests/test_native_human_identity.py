"""#36 native identity/merge: disposable synthetic HTTP and full three-DB comparisons."""
from contextlib import closing
import http.client
import json
import sqlite3
import time
import unittest

from tests import test_native_human_labels as labels
from tests import test_native_human_curation as curation
from tests.fixed_expectations import unclocked
from tools.verify_read_http import fetch
from tests.test_read_http import wait_until


class NativeHumanIdentityTest(unittest.TestCase):
    setUpClass = classmethod(curation.NativeHumanCurationTest.setUpClass.__func__)
    clone = labels.NativeHumanLabelsTest.clone
    start = labels.NativeHumanLabelsTest.start
    stop = labels.NativeHumanLabelsTest.stop
    seed_both = labels.NativeHumanLabelsTest.seed_both
    undo_latest = labels.NativeHumanLabelsTest.undo_latest
    latest_action = labels.NativeHumanLabelsTest.latest_action
    reference_response = labels.NativeHumanLabelsTest.reference_response
    def wait_timestamp_pair(self, url):
        # The existing core stamps newly proposed evidence with SELECT CURRENT_TIMESTAMP.
        # Start both real calls early in the same second, without changing stored/output timestamps.
        if url in ('/api/identity/decision','/api/merge'):
            phase = time.time() % 1
            if phase > .5:
                time.sleep(1 - phase)
    def pair(self, url, body, **kwargs):
        NativeHumanIdentityTest.wait_timestamp_pair(self,url)
        return curation.NativeHumanCurationTest.pair(self,url,body,**kwargs)

    def setUp(self):
        self.hybrid = 'no_policy' not in self._testMethodName
        labels.NativeHumanLabelsTest.setUp(self)
        self.seed_both()
        self.third, self.work, self.variant = self.rec.value('augmented ids')
        self.assertEqual(self.rec.state('seeded'), labels.states(self.right))

    def decide(self, **kwargs):
        return self.pair('/api/identity/decision', {'a':self.alice,'b':self.bob,'kind':'duplicate_of',
                         'status':'accepted','reason':'  合成关系理由  ', **kwargs})

    def test_relation_states_evidence_graph_and_undo(self):
        self.decide(evidence={'z':'证据','a':[1,0.125], 'response_ids':[7]})
        with closing(sqlite3.connect(self.right.db)) as c:
            relation = c.execute("SELECT id FROM identity_relations WHERE kind='duplicate_of'").fetchone()[0]
        for status in ('rejected','proposed','accepted'):
            self.decide(relation_id=relation, status=status, a=None, b=None, kind=None)
        for _ in range(4):
            self.undo_latest()
        self.decide(kind='different_from', status='accepted')
        self.pair('/api/merge', {'source':self.bob,'target':self.alice}, expected=400)
        self.decide(status='proposed')
        self.undo_latest(); self.undo_latest()
        self.decide(a=self.third, b=self.alice, kind='form_of')
        self.pair('/api/identity/decision', {'a':self.alice,'b':self.third,'kind':'form_of',
                  'status':'accepted','reason':'cycle'}, expected=400)
        self.undo_latest()

    def test_skip_repeat_unicode_arbitrary_keys_and_restore(self):
        events = labels.snapshot(self.right.db)['backend_effects']
        self.pair('/api/identity/skip', {'a':self.alice,'b':self.bob})
        self.pair('/api/identity/skip', {'a':self.bob,'b':self.alice})
        self.pair('/api/identity/skip', {'a':'未登记角色','b':'角色|乙'})
        self.assertEqual(labels.snapshot(self.right.db)['backend_effects'], events)
        for _ in range(3): self.undo_latest()
        self.assertNotEqual(labels.snapshot(self.right.db)['backend_effects'], events)

    def test_hybrid_merge_is_relation_preserves_profiles_and_split_restores(self):
        before = labels.snapshot(self.right.db)
        self.pair('/api/merge', {'source':self.bob,'target':self.alice})
        after = labels.snapshot(self.right.db)
        for table in ('assertions','candidates','names','links','prefs'):
            self.assertEqual(before[table], after[table], table)
        self.pair('/api/merge', {'source':self.alice,'target':self.bob})
        self.undo_latest(); self.undo_latest()
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM entities WHERE merged_into IS NOT NULL').fetchone()[0], 0)

    def test_no_policy_merge_moves_children_assertions_names_pins_and_undo(self):
        before = labels.snapshot(self.right.db)
        for lib in (self.left,self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute("CREATE TRIGGER fail_merge BEFORE INSERT ON term_tree BEGIN SELECT RAISE(ABORT,'synthetic merge failure'); END"); c.commit()
        self.pair('/api/merge', {'source':self.bob,'target':self.alice}, expected=500)
        for lib in (self.left,self.right):
            with closing(sqlite3.connect(lib.db)) as c: c.execute('DROP TRIGGER fail_merge'); c.commit()
        self.pair('/api/merge', {'source':self.bob,'target':self.alice})
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute('SELECT merged_into FROM entities WHERE id=?', (self.bob,)).fetchone()[0], self.alice)
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?', (self.variant,)).fetchone()[0], self.alice)
        # A previously merged source resolves to its surviving role; undo the chain in order.
        self.pair('/api/merge', {'source':self.bob,'target':self.alice}, expected=400)
        self.pair('/api/merge', {'source':self.bob,'target':self.third})
        self.undo_latest()
        self.undo_latest()
        after = labels.snapshot(self.right.db)
        # Physical restoration is compared to the real reference after undo, not assumed identical to pre-merge rowids.
        for table in ('entities','prefs'):
            self.assertEqual(before[table], after[table],table)
        self.pair('/api/merge', {'source':self.bob,'target':self.bob}, expected=400)
        self.pair('/api/merge', {'source':self.variant,'target':self.alice}, expected=400)
        self.pair('/api/identity/decision', {'a':self.alice,'b':self.bob,'kind':'duplicate_of','status':'accepted','reason':'x'}, expected=400)
        self.pair('/api/identity/skip', {'a':self.bob,'b':self.alice}); self.undo_latest()

    def test_cross_type_recent_undo_and_error_validation(self):
        first = self.decide()['undo']
        self.pair('/api/tag', {'ids':['c'],'tag':'第二标签'})
        self.pair('/api/artist/verdict', {'ids':['b'],'artist':self.artist,'verdict':'yes'})
        self.pair('/api/curate', {'ids':['c'],'action':'exclude'})
        self.pair('/api/identity/skip', {'a':self.alice,'b':self.bob})
        self.pair('/api/merge', {'source':self.third,'target':self.alice})
        self.pair('/api/undo', {'id':first['id']},expected=400)
        for _ in range(6): self.undo_latest()
        for url, bodies in (
            ('/api/identity/skip', ({},{'a':'x','b':'x'},{'a':1,'b':'x'},{'a':'','b':'x'})),
            ('/api/identity/decision', ({},{'relation_id':True},{'relation_id':1.0},{'relation_id':'1'},
              {'relation_id':-1},{'relation_id':0},{'relation_id':2**100},{'relation_id':999999,'status':'accepted','reason':'x'},
              {'a':'missing','b':self.bob,'kind':'duplicate_of','status':'accepted','reason':'x'},
              {'a':self.alice,'b':self.bob,'kind':'bad','status':'accepted','reason':'x'},
              {'a':self.alice,'b':self.bob,'kind':'duplicate_of','status':None,'reason':'x'},
              {'a':self.alice,'b':self.bob,'kind':'duplicate_of','status':'accepted','reason':' '})),

            ('/api/merge', ({},{'source':self.alice,'target':self.alice},{'source':'missing','target':self.alice},
              {'source':self.work,'target':self.alice},{'source':[],'target':self.alice})),
        ):
            for body in bodies:
                with self.subTest(url=url,body=body): self.pair(url,body,expected=400)

    def test_transaction_faults_rollback(self):
        for table,url,body in (
            ('term_verdicts','/api/identity/decision',{'a':self.alice,'b':self.third,'kind':'duplicate_of','status':'accepted','reason':'x'}),

            ('actions','/api/identity/skip',{'a':self.alice,'b':self.bob}),
            ('backend_effects','/api/merge',{'source':self.bob,'target':self.alice}),
        ):
            for lib in (self.left,self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute(f"CREATE TRIGGER fail_identity BEFORE INSERT ON {table} BEGIN SELECT RAISE(ABORT,'synthetic failure'); END"); c.commit()
            self.pair(url,body,expected=500)
            for lib in (self.left,self.right):
                with closing(sqlite3.connect(lib.db)) as c: c.execute('DROP TRIGGER fail_identity'); c.commit()

    def test_postcommit_ack_failure_recovers_without_http(self):
        for url,body in (
            ('/api/identity/decision',{'a':self.alice,'b':self.third,'kind':'different_from','status':'accepted','reason':'x'}),

            ('/api/merge',{'source':self.bob,'target':self.alice}),
        ):
            for lib in (self.left,self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects BEGIN SELECT RAISE(ABORT,'synthetic ack failure'); END"); c.commit()
            before = [self.rec.state('before'), labels.states(self.right)]
            item={'url':url,'method':'POST','body':json.dumps(body).encode(),'headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}}
            self.wait_timestamp_pair(url)
            # The old reference answered 500 although the operation had committed; by user decision on
            # 2026-10-11 (#40) it answers 200 and the event stays pending for recovery.
            responses=[self.reference_response('response', item), fetch(self.origin,item,self.origin)]
            self.assertEqual([r['status'] for r in responses],[500,200])
            after=[self.rec.state('after'), labels.states(self.right)]
            self.assertEqual(after[0],after[1]); self.assertNotEqual(before[1],after[1])
            for lib in (self.left,self.right):
                with closing(sqlite3.connect(lib.db)) as c:
                    if lib is self.right:
                        self.assertEqual(c.execute('SELECT status FROM backend_effects ORDER BY id DESC LIMIT 1').fetchone()[0],'pending')
                    c.execute('DROP TRIGGER fail_ack'); c.commit()
            drained = self.rec.state('drained')
            wait_until(lambda:drained==labels.states(self.right),timeout=5)
            self.undo_latest()

    def test_write_lock_refuses_without_partial_commit(self):
        with closing(sqlite3.connect(self.left.db)) as left, closing(sqlite3.connect(self.right.db)) as right:
            left.execute('BEGIN IMMEDIATE'); right.execute('BEGIN IMMEDIATE')
            self.pair('/api/merge', {'source':self.bob,'target':self.alice}, expected=500)
            left.rollback(); right.rollback()
        self.pair('/api/merge', {'source':self.bob,'target':self.alice}); self.undo_latest()

    def test_security_and_inflight_switch_isolation(self):
        commands = [('/api/identity/decision', {'a':self.alice,'b':self.bob,'kind':'different_from','status':'accepted','reason':'x'}),
                    ('/api/identity/skip',{'a':self.alice,'b':self.bob}),

                    ('/api/merge',{'source':self.bob,'target':self.alice})]
        for url,body in commands:
            self.pair(url,body,expected=403,headers={'X-CSRF-Token':'bad'})
        for index,(url,body) in enumerate(commands):
            self.stop(self.process)
            newer=self.clone(self.right,'switched-'+str(index))
            self.process=self.start(self.right)
            token=json.loads(fetch(self.origin,{'url':'/api/bootstrap'},self.origin)['body'])['token']
            before_old,before_new=labels.states(self.right),labels.states(newer)
            data=json.dumps(body).encode()
            conn=http.client.HTTPConnection('127.0.0.1',self.port,timeout=5); self.addCleanup(conn.close)
            conn.putrequest('POST',url,skip_host=True)
            for k,v in {'Host':self.origin.split('://')[1],'Origin':self.origin,'Content-Type':'application/json',
                        'X-CSRF-Token':token,'Content-Length':str(len(data))}.items(): conn.putheader(k,v)
            conn.endheaders(); conn.send(data[:1])
            switched=fetch(self.origin,{'url':'/api/libraries/open','method':'POST','body':json.dumps({'path':str(newer.path)}).encode(),
                                       'headers':{'Content-Type':'application/json','X-CSRF-Token':token}},self.origin)
            self.assertEqual(switched['status'],200,switched['body'])
            conn.send(data[1:]); stale=conn.getresponse(); self.assertEqual(stale.status,403,stale.read())
            self.assertEqual(labels.states(self.right),before_old); self.assertEqual(labels.states(newer),before_new)
        self.assertFalse(self.sentinel.exists())
