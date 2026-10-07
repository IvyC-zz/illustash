"""#52: optional real calculator and controlled failures on generated packages only.

The existing Python calculators' results on the same inputs (similar scan, clustering, relations,
OC groups) and the prepared package were recorded through tests/reference_replay.py (#9;
``ILLUSTASH_REFERENCE=record`` re-runs them).
"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import json
import os
from pathlib import Path
import signal
import sqlite3
import sys
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
import numpy as np
from PIL import Image
from tests import test_native_lifecycle as lifecycle
from tests.reference_replay import RECORD, unclocked
from tests.test_read_http import wait_until


class NativeResearchTest(unittest.TestCase):
    setUpClass = lifecycle.NativeLifecycleTest.__dict__['setUpClass']
    package = lifecycle.NativeLifecycleTest.package
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    pair = lifecycle.NativeLifecycleTest.pair
    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.stop(self.process)
        self.repo = Path(__file__).resolve().parents[1]
        self.worker = self.root/'research-worker'
        self.mode = self.root/'mode'
        self.pid = self.root/'worker.pid'
        self.runs = self.root/'worker.runs'
        self.mode.write_text('ok')
        self.worker.write_text('#!'+sys.executable+'\n'+f'''import sys,os,json,time,subprocess
from pathlib import Path
sys.path.insert(0,{str(self.repo)!r})
r=json.loads(Path(sys.argv[1]).read_text())
Path({str(self.pid)!r}).write_text(str(os.getpid()))
with open({str(self.runs)!r},'a') as f: f.write(r['version']+'\\n')
mode=Path({str(self.mode)!r}).read_text()
if mode=='hang':
    subprocess.Popen([sys.executable,'-c','import time;time.sleep(60)'])
    time.sleep(60)
if mode=='crash': sys.exit(7)
if mode=='slow': time.sleep(1)
from web.research_worker import run
v=run(r)
if mode=='wrong': v['identity']['generation']+=1
if mode=='bad': v['tables']['relations']=[['a','foreign','identical',0,'','2026-10-01']]
if mode=='invalid': v['tables']['cluster_info']=[['invalid']]
Path(sys.argv[2]).write_text(json.dumps(v,allow_nan=False))
''')
        self.worker.chmod(0o755)
        self.env.update(ILLUSTASH_RESEARCH_WORKER=worker_command(self.worker),ILLUSTASH_RESEARCH_TIMEOUT_MS='10000',ILLUSTASH_RESEARCH_RETRY_MS='300')
        with sqlite3.connect(self.lib.source) as c:
            c.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
            v=np.zeros(768,np.float32);v[0]=1
            for u in 'abc': c.execute('INSERT OR REPLACE INTO embeddings VALUES (?,?,?)',(u,'ccip',v.tobytes()))
            for i in range(9):
                u=('seed'+str(i)) if i<6 else 'unmatched'+str(i)
                v=np.zeros(768,np.float32);v[0 if i<6 else 1]=1
                c.execute('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)',(u,u+'.png',str(self.lib.path/(u+'.png')),120,150,'2026-10-01','orig',1,0))
                c.execute('INSERT OR REPLACE INTO embeddings VALUES (?,?,?)',(u,'ccip',v.tobytes()))
        if RECORD:
            self.store.refresh()
            entity=self.store.entities('Alice')[0]['id']
            self.store.change(['seed'+str(i) for i in range(6)],'confirmed',entity)
        self.lib=self.rec.package('prepared',self.lib.path)
        self.alice=self.rec.value('alice',lambda:self.store.entities('Alice')[0]['id'])
        pixels=np.random.default_rng(1).integers(0,255,(150,120,3),dtype=np.uint8)
        for u in list('abc')+[('seed'+str(i)) if i<6 else 'unmatched'+str(i) for i in range(9)]: Image.fromarray(pixels).save(self.lib.path/(u+'.png'))
        self.restart()
        self.finished()

    def restart(self):
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']

    def state(self): return json.loads(self.request('/api/similar')['body'])['state']
    def finished(self, status='idle'):
        wait_until(lambda:self.state()['status'] in ('idle','error'),timeout=20)
        self.assertEqual(self.state()['status'],status,self.state())
    def rows(self, tables):
        with sqlite3.connect(self.lib.db) as c:
            return {t:c.execute('SELECT * FROM '+t+' ORDER BY 1').fetchall() for t in tables}
    def pending(self):
        with sqlite3.connect(self.lib.db) as c: return c.execute("SELECT count(*) FROM backend_effects WHERE kind IN ('verdict','imported','index_changed') AND status='pending'").fetchone()[0]
    def versions(self): return self.runs.read_text().split() if self.runs.exists() else []
    def clustered(self):
        with sqlite3.connect(self.lib.db) as c: return {u for (u,) in c.execute('SELECT uuid FROM clusters')}
    def poke_event(self):
        with sqlite3.connect(self.lib.db) as c:c.execute("INSERT INTO backend_effects(kind) VALUES ('imported')")
    def test_real_similar_relate_cluster_and_oc_routes(self):
        # Existing actual algorithms against the same generated inputs, exact tables except wall-clock fields.
        tables=['relations','image_sigs','similar_paired','similar_retry','similar_groups','duplicates']
        native=self.rows(tables)
        self.assertGreater(len(self.rows(['prototypes'])['prototypes']),0)
        def scanned():
            from web.features import Features
            from web.similar import SimilarScanner
            SimilarScanner(self.store,Features(self.lib.source).load()).scan()
            return self.rows(tables)
        # Wall-clock columns (when a relation was computed) are compared as <clock>.
        masked=lambda rows:{t:[tuple(unclocked(v) for v in r) for r in rs] for t,rs in rows.items()}
        self.assertEqual(masked(native),masked(self.rec.value('scanned',scanned)))
        clusters=self.rows(['clusters','cluster_info'])
        self.assertGreater(len(clusters['clusters']),0)
        def clustered():
            from web import learn
            from web.features import Features
            learn.cluster(self.store,Features(self.lib.source).load())
            return self.rows(['clusters','cluster_info'])
        expected=self.rec.value('clustered',clustered)
        self.assertEqual(clusters['clusters'],expected['clusters'])
        self.assertEqual([r[:4]+r[5:] for r in clusters['cluster_info']], [r[:4]+r[5:] for r in expected['cluster_info']])
        def related():
            from web import relate
            return relate.relate_all({u:relate.load(self.lib.path/(u+'.png')) for u in 'ab'})|{'missing':[]}
        expected=self.rec.value('related',lambda:json.dumps(related(),ensure_ascii=False))
        actual=self.request('/api/relate',{'ids':['a','b','a']})
        self.assertEqual(actual['status'],200,actual)
        self.assertEqual(actual['body'],expected.encode())
        self.assertEqual(self.request('/api/relate',{'ids':['a']})['status'],400)
        entity=self.alice
        def groups():
            from web import learn
            from web.features import Features
            return json.dumps(learn.oc_groups(self.store,Features(self.lib.source).load(),entity),ensure_ascii=False)
        expected=self.rec.value('oc groups',groups)
        actual=self.request('/api/oc/groups?entity='+entity)
        self.assertEqual(actual['status'],200,actual)
        self.assertEqual(actual['body'],expected.encode())
        self.assertEqual(self.request('/api/regroup',{})['body'],b'{"started": true}')
        self.finished()
        for url in ['/api/identity/queue','/api/assets/count','/api/identity/count']:
            self.pair(url)
        self.assertFalse(self.sentinel.exists())

    def test_wrong_malformed_crash_timeout_results_do_not_publish_and_retry(self):
        tables=['relations','image_sigs','clusters','cluster_info','actions','assertions']
        before=self.rows(tables)
        for mode in ['wrong','bad','crash','hang']:
            self.mode.write_text(mode)
            if mode=='hang':self.env['ILLUSTASH_RESEARCH_TIMEOUT_MS']='200'
            self.stop(self.process);self.restart();self.finished('error')
            self.assertEqual(before,self.rows(tables))
            with sqlite3.connect(self.lib.db) as c:
                self.assertGreater(c.execute("SELECT count(*) FROM research_runs WHERE status='pending'").fetchone()[0],0)
            self.assertEqual(self.request('/api/assets/count')['status'],200)
        self.mode.write_text('ok');self.env['ILLUSTASH_RESEARCH_TIMEOUT_MS']='10000'
        self.stop(self.process);self.restart();self.finished()
        self.assertEqual(self.pending(),0)

    def test_inflight_human_change_cancel_switch_exit_no_process_group(self):
        self.mode.write_text('slow')
        self.request('/api/regroup',{})
        wait_until(lambda:self.pid.exists() and self.state()['status']=='learning',timeout=5)
        self.request('/api/tag',{'ids':['a'],'tag':'合成'})
        self.finished()
        self.mode.write_text('hang')
        self.pid.unlink(missing_ok=True)
        self.request('/api/similar',{})
        wait_until(self.pid.exists,timeout=5)
        pid=int(self.pid.read_text())
        with sqlite3.connect(self.lib.db) as c: pending=c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        for _ in range(3):self.assertEqual(self.request('/api/similar',{})['body'],b'{"started": true}')
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(c.execute('SELECT count(*) FROM research_runs').fetchone()[0],pending)
        self.assertEqual(self.request('/api/similar',{'action':'cancel'})['status'],200)
        wait_until(lambda:not group_exists(pid),timeout=5)
        self.assertEqual(self.state()['status'],'idle')
        self.request('/api/similar',{})
        wait_until(lambda:self.pid.exists() and int(self.pid.read_text())!=pid,timeout=5)
        pid=int(self.pid.read_text())
        replacement,_=self.package('same-id')
        config=replacement.config;config['id']=self.lib.config['id'];(replacement.path/'library.json').write_text(json.dumps(config))
        self.assertEqual(self.request('/api/libraries/open',{'path':str(replacement.path)})['status'],200)
        wait_until(lambda:not group_exists(pid),timeout=5)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        with sqlite3.connect(replacement.db) as c:self.assertEqual(c.execute("SELECT count(*) FROM research_runs WHERE id='startup'").fetchone()[0],1)
        self.request('/api/similar',{'action':'cancel'})
        self.pid.unlink(missing_ok=True)
        self.request('/api/similar',{})
        wait_until(self.pid.exists,timeout=5)
        pid=int(self.pid.read_text());self.stop(self.process)
        wait_until(lambda:not group_exists(pid),timeout=5)

    def test_inflight_input_change_retries_on_fresh_snapshot(self):
        # #194: a human verdict lands while the worker computes; the stale result is rejected,
        # never published, and the task finishes by itself on a newer snapshot.
        self.assertIn('unmatched6',self.clustered())
        before=self.rows(['clusters','cluster_info','relations','prototypes'])
        runs=len(self.versions())
        self.mode.write_text('slow')
        self.request('/api/regroup',{})
        wait_until(lambda:len(self.versions())>runs and self.state()['status']=='learning',timeout=5)
        self.assertEqual(self.request('/api/change',{'ids':['unmatched6'],'action':'confirmed','entity':self.alice})['status'],200)
        seen=[]
        def settled():
            s=self.state()
            if s['status']=='waiting':
                self.assertEqual(self.rows(list(before)),before)
                self.assertIn('自动重试',s['message'])
            seen.append(s['status'])
            return s['status'] in ('idle','error')
        wait_until(settled,timeout=20)
        self.assertEqual(self.state()['status'],'idle',self.state())
        self.assertIn('waiting',seen)
        versions=self.versions()[runs:]
        self.assertGreaterEqual(len(versions),2)
        self.assertNotEqual(versions[0],versions[-1])
        # Published groups are the existing calculator's result on the post-verdict library.
        native=self.rows(['clusters'])
        self.assertNotIn('unmatched6',self.clustered())
        def clustered():
            from web import learn
            from web.features import Features
            learn.cluster(self.store,Features(self.lib.source).load())
            return self.rows(['clusters'])
        self.assertEqual(native,self.rec.value('clustered',clustered))
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM research_runs WHERE status='pending'").fetchone()[0],0)

    def test_continuous_input_change_retries_are_bounded_then_manual_retry(self):
        # Six slow runs plus backoff outlast the default fixture task limit.
        self.stop(self.process);self.env['ILLUSTASH_RESEARCH_TIMEOUT_MS']='60000';self.restart();self.finished()
        before=self.rows(['clusters','cluster_info','relations','prototypes'])
        runs=len(self.versions())
        self.mode.write_text('slow')
        self.request('/api/regroup',{})
        wait_until(lambda:len(self.versions())>runs,timeout=5)
        stop=threading.Event()
        def write():
            i=0
            while not stop.is_set():
                i+=1;self.request('/api/tag',{'ids':['a'],'tag':f'合成持续{i}'});time.sleep(0.15)
        writer=threading.Thread(target=write);writer.start()
        try:
            wait_until(lambda:self.state()['status']=='error',timeout=40)
        finally:
            stop.set();writer.join()
        state=self.state()
        self.assertIn('自动重试 5 次',state['message'])
        self.assertEqual(len(self.versions())-runs,6)
        self.assertEqual(self.rows(list(before)),before)
        with sqlite3.connect(self.lib.db) as c:
            self.assertIn('持续变化',c.execute("SELECT error FROM research_runs WHERE status='pending'").fetchone()[0])
        # Exhausted retries wait for an explicit start instead of looping on recovery.
        time.sleep(3)
        self.assertEqual(self.state(),state)
        self.assertEqual(len(self.versions())-runs,6)
        self.mode.write_text('ok')
        self.assertEqual(self.request('/api/regroup',{})['body'],b'{"started": true}')
        def drained():
            with sqlite3.connect(self.lib.db) as c:
                return self.state()['status']=='idle' and c.execute("SELECT count(*) FROM research_runs WHERE status='pending'").fetchone()[0]==0
        wait_until(drained,timeout=20)
        self.assertEqual(self.pending(),0)

    def test_ack_failure_replay_is_atomic_and_restart_recovers(self):
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'synthetic ack'); END")
        with sqlite3.connect(self.lib.db) as c:c.execute("INSERT INTO backend_effects(kind) VALUES ('index_changed')")
        self.finished() # scheduler hasn't started yet
        wait_until(lambda:self.state()['status']=='error',timeout=15)
        self.assertGreater(self.pending(),0)
        before=self.rows(['relations','prototypes','proto_claims','assertions','actions'])
        with sqlite3.connect(self.lib.db) as c:c.execute('DROP TRIGGER fail_ack')
        self.stop(self.process);self.restart();self.finished()
        self.assertEqual(self.pending(),0)
        after=self.rows(['relations','prototypes','proto_claims','assertions','actions'])
        self.assertEqual(before,after)

    def test_recognition_dispatch_persists_intent_and_duplicate_does_not_repeat(self):
        import hashlib
        from tests import test_native_recognition as recognition
        self.stop(self.process)
        self.resources=self.root/'worker-resources'
        (self.resources/'models/ccip').mkdir(parents=True)
        (self.resources/'models/wd_swinv2_v3_emb.onnx').write_bytes(b'synthetic WD')
        (self.resources/'models/ccip/model_feat.onnx').write_bytes(b'synthetic CCIP')
        (self.resources/'tags').mkdir()
        (self.resources/'tags/tags_v3.csv').write_text('name,category\ngeneral,9\nblue_hair,0\nalice,4\n')
        self.specs={'wd':dict(version='swinv2_448',dimension=1024,sha256=hashlib.sha256(b'synthetic WD').hexdigest(),tags_sha256=hashlib.sha256((self.resources/'tags/tags_v3.csv').read_bytes()).hexdigest()),'ccip':dict(version='ccip-model_feat',dimension=768,sha256=hashlib.sha256(b'synthetic CCIP').hexdigest())}
        with sqlite3.connect(self.lib.source) as c:
            for column in ('source TEXT DEFAULT "legacy"','model_ver TEXT','run_id TEXT'):c.execute('ALTER TABLE labels ADD COLUMN '+column)
        self.manifest=lambda p,ids=('a','b'):recognition.NativeRecognitionTest.manifest(self,p,ids)
        package=recognition.NativeRecognitionTest.incoming(self)
        self.restart();self.finished()
        history=self.rows(['assertions','actions'])
        with sqlite3.connect(self.lib.db) as c:before=c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        self.assertEqual(self.request('/api/recognition/import',{'path':str(package)})['status'],200)
        def completed():
            with sqlite3.connect(self.lib.db) as c:return c.execute("SELECT count(*) FROM research_runs WHERE status='done'").fetchone()[0]>before
        wait_until(completed,timeout=20)
        self.assertEqual(history,self.rows(['assertions','actions']))
        with sqlite3.connect(self.lib.db) as c:count=c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        # Confirmation failure re-dispatches the same stored event; intent key must remain stable.
        with sqlite3.connect(self.lib.db) as c:
            c.execute("INSERT INTO backend_effects(kind) VALUES ('imported')")
            c.execute("CREATE TRIGGER fail_dispatch_ack BEFORE UPDATE ON backend_effects WHEN NEW.status='done' BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        self.mode.write_text('crash')
        self.stop(self.process);self.restart();self.finished('error')
        with sqlite3.connect(self.lib.db) as c:first=c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        self.stop(self.process);self.restart();self.finished('error')
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(first,c.execute('SELECT count(*) FROM research_runs').fetchone()[0])
            c.execute('DROP TRIGGER fail_dispatch_ack')
        self.mode.write_text('ok');self.stop(self.process);self.restart();self.finished()
        with sqlite3.connect(self.lib.db) as c:count=c.execute('SELECT count(*) FROM research_runs').fetchone()[0]
        self.assertEqual(self.request('/api/recognition/import',{'path':str(package)})['status'],200)
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(c.execute('SELECT count(*) FROM research_runs').fetchone()[0],count)

    def test_worker_missing_dependency_and_frozen_package_preserve_basics(self):
        self.stop(self.process);self.env['ILLUSTASH_RESEARCH_WORKER']=str(self.root/'missing')
        self.restart();self.finished('error')
        self.assertEqual(self.request('/api/assets/count')['status'],200)
        self.assertEqual(self.request('/api/tag',{'ids':['a'],'tag':'合成缺依赖'})['status'],200)
        self.stop(self.process)
        self.env['ILLUSTASH_RESEARCH_WORKER']=worker_command(self.worker)
        config=self.lib.config;config['frozen']=True;(self.lib.path/'library.json').write_text(json.dumps(config))
        before=self.rows(['assertions','actions','relations','prototypes'])
        self.restart()
        self.assertEqual(self.request('/api/regroup',{})['status'],400)
        self.assertEqual(self.request('/api/assets/count')['status'],200)
        self.assertEqual(before,self.rows(['assertions','actions','relations','prototypes']))


def group_exists(pid):
    return worker_exists(pid)
