"""#49：只用显式临时合成包、loopback 文本/图片服务及隔离计算替身。"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import io
import json
import os
from pathlib import Path
import signal
import sqlite3
import threading
import time
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from tests import test_native_lifecycle as lifecycle
from tests.test_native_human_labels import snapshot
from web.index_update import IndexUpdater

WORKER = r'''#!/usr/bin/env PYTHON
import json, sqlite3, sys, time
from pathlib import Path
req=json.loads(Path(sys.argv[1]).read_text()); p=Path(req['review']); mode=Path(MODE).read_text()
if mode=='sleep': time.sleep(60)
if mode=='fail': sys.exit(2)
c=sqlite3.connect(p/'pi.db'); c.row_factory=sqlite3.Row
chars=[dict(r) for r in c.execute('SELECT * FROM characters')]
aliases=[list(r)+['index'] for r in c.execute('SELECT char_id,alias FROM character_aliases')]
labels=[list(r) for r in c.execute("SELECT uuid,value,conf FROM labels WHERE dim='char_ext'")]
if req['action']=='apply':
 for row in req['rows']:
  cid=row['short_tag']
  if cid=='dani_example':
   (p/'char-patch/rejected'/f'{cid}.json').write_text(json.dumps({'rejected':'参考图没有一致的主体'})); continue
  (p/'char-index').mkdir(exist_ok=True)
  (p/'char-index'/f'{cid}.json').write_text(json.dumps(dict(row,source='danbooru-patch')))
  (p/'char-index'/f'{cid}.npy').write_bytes(b'synthetic features')
  chars.append(dict(char_id=cid,name_en=None,name_ja=None,name_zh='Carol',copyright='example',wd_tag=None,danbooru_tag=row['tag']))
  labels=[r for r in labels if r[0]!='b']+[['b',cid,.97],['c',cid,.96]]
else:
 cid=req['rows'][0]['short_tag']; chars=[r for r in chars if r['char_id']!=cid]; aliases=[r for r in aliases if r[0]!=cid]; labels=[r for r in labels if r[1]!=cid]+[['b','alice',.95]]
 for ext in ('.json','.npy'):
  (p/'char-index'/f'{cid}{ext}').unlink(missing_ok=True)
 (p/'char-patch/rejected'/f'{cid}.json').write_text(json.dumps({'rejected':'人工看图后放弃：'+req['reason']}))
v=dict(format=req['format'],identity=req['identity'],before=req['before'],characters=chars,aliases=aliases,labels=labels)
if mode=='identity': v['identity']['generation']+=1
if mode=='invalid': v['labels'].append(['not-an-asset','alice',1])
Path(sys.argv[2]).write_text(json.dumps(v))
'''

class NativeIndexTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    package = lifecycle.NativeLifecycleTest.package
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request

    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.stop(self.process)
        self.mode=self.root/'worker-mode'; self.mode.write_text('ok')
        worker=self.root/'worker'; worker.write_text(WORKER.replace('PYTHON',os.sys.executable).replace('MODE',repr(str(self.mode))));worker.chmod(0o755)
        self.remote_rows=[dict(tag='carol_(example)',short_tag='carol_example',copyright='example',post_count=300,overlap=.9),dict(tag='dani_(example)',short_tag='dani_example',copyright='example',post_count=120,overlap=.8)]
        self.fail_network=False;self.delay=0;self.calls=[];self.posts=[]
        parent=self
        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                parent.calls.append(self.path)
                time.sleep(parent.delay)
                if parent.fail_network: self.send_response(500);self.end_headers();return
                if self.path.startswith('/related_tag'):
                    rows=[{'tag':{'name':r['tag'],'post_count':r['post_count']},'overlap_coefficient':r['overlap']} for r in parent.remote_rows]
                    rows += [{'tag':{'name':'alice','post_count':500},'overlap_coefficient':.9},{'tag':{'name':'skin_(alt)_(example)','post_count':500},'overlap_coefficient':.9}]
                    data=json.dumps({'related_tags':rows}).encode()
                elif self.path.startswith('/posts'): data=json.dumps(parent.posts).encode()
                else:
                    from PIL import Image
                    out=io.BytesIO();Image.new('RGB',(8,8),(int(self.path.split('/')[-1])%255,0,0)).save(out,format='PNG');data=out.getvalue()
                self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers()
                try:self.wfile.write(data)
                except BrokenPipeError:pass
            def log_message(self,*a): pass
        self.remote=ThreadingHTTPServer(('127.0.0.1',0),Handler);t=threading.Thread(target=self.remote.serve_forever,daemon=True);t.start()
        self.addCleanup(t.join);self.addCleanup(self.remote.server_close);self.addCleanup(self.remote.shutdown)
        self.env.update(ILLUSTASH_COPYRIGHT_BASE=f'http://127.0.0.1:{self.remote.server_port}',ILLUSTASH_INDEX_WORKER=worker_command(worker))
        self.work=self.store.edit_work('create',name='合成作品',tag='example')['id']
        (self.shared/'char-index').mkdir()
        (self.shared/'char-patch').mkdir(exist_ok=True)
        (self.shared/'char-patch/watch.json').write_text(json.dumps({'version':2,'works':[self.work],'pending_tags':[],'min_posts':100}))
        self.restart()

    def restart(self,extra=()):
        self.process,self.log=self.start(self.lib.path,extra=('--no-index-check',*extra))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']

    def index(self,b=None):
        r=self.request('/api/index',b);self.assertEqual(r['status'],200,r['body']);return json.loads(r['body'])

    def finish(self):
        for _ in range(500):
            v=self.index()
            if v['state']['status'] not in ('checking','fetching','building','importing','cleaning','scanning'):return v
            time.sleep(.02)
        self.fail('index did not finish')

    def check(self):
        self.index({'action':'check'}); v=self.finish();self.assertEqual(v['state']['status'],'checked',v);return v

    def apply(self):
        self.check();self.index({'action':'apply','tags':['carol_example','dani_example','carol_example']});return self.finish()

    def test_check_watch_apply_drop_formal_projection_history_names(self):
        before=snapshot(self.lib.db)['actions']
        self.assertEqual(self.check()['check']['missing'],self.remote_rows)
        v=self.apply();self.assertEqual(v['state']['status'],'done',v);self.assertEqual((v['state']['status'],v['state']['added'],v['state']['changed']),('done',1,2),v)
        self.assertEqual(v['check']['missing'],[])
        add,rejected=v['history'][0]['characters']; self.assertEqual((add['claims'],add['entity']['count']),(2,2))
        self.assertEqual(rejected['reason'],'参考图没有一致的主体')
        self.assertEqual(v['works'],json.loads(self.request('/api/works')['body']))
        self.assertEqual(snapshot(self.lib.db)['actions'],before)
        with sqlite3.connect(self.lib.db) as c:
            self.assertGreater(c.execute("SELECT count(*) FROM backend_effects WHERE kind LIKE 'names:%'").fetchone()[0],0)
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE kind='index_changed' AND status='pending'").fetchone()[0],1)
        self.assertEqual(self.index({'action':'watch','works':[self.work,self.work],'min_posts':150})['watch']['works'],[self.work])
        for b in ({'action':'apply','tags':['bogus']},{'action':'drop','char':'alice','reason':'x'},{'action':'watch','works':[self.work],'min_posts':True}):self.assertEqual(self.request('/api/index',b)['status'],400)
        self.index({'action':'drop','char':'carol_example','reason':'  合成放弃  '});v=self.finish();self.assertEqual(v['state']['status'],'done',v)
        self.assertEqual(v['history'][0]['characters'][0]['status'],'dropped')
        self.assertEqual(self.check()['check']['missing'],[]) # both rejected characters stay excluded

    def test_watch_list_belongs_to_each_library(self):
        # #205: work IDs only exist in their own library, so each library keeps its own list and threshold.
        shared_file=self.shared/'char-patch/watch.json';legacy=shared_file.read_bytes()
        self.assertEqual(self.index()['watch'],{'version':2,'works':[self.work],'pending_tags':[],'min_posts':100})
        self.assertEqual(self.index({'action':'watch','works':[self.work],'min_posts':150})['watch']['min_posts'],150)
        self.assertEqual(shared_file.read_bytes(),legacy)
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(json.loads(c.execute("SELECT value FROM prefs WHERE key='index_watch'").fetchone()[0])['works'],[self.work])
        other,store=self.package('other');other_work=store.edit_work('create',name='另一部合成作品',tag='other_example')['id']
        self.stop(self.process);self.process,self.log=self.start(other.path,extra=('--no-index-check',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.index()['watch'],{'version':2,'works':[],'pending_tags':[],'min_posts':100})
        self.assertEqual(self.index({'action':'watch','works':[other_work],'min_posts':120})['watch']['works'],[other_work])
        self.assertEqual(self.request('/api/index',{'action':'watch','works':[self.work],'min_posts':120})['status'],400)
        self.stop(self.process);self.restart()
        v=self.index()['watch'];self.assertEqual((v['works'],v['min_posts']),([self.work],150))
        self.assertEqual(shared_file.read_bytes(),legacy)

    def test_source_failure_rollback_then_retry(self):
        self.check()
        with sqlite3.connect(self.lib.source) as c:c.execute("CREATE TRIGGER fail_index BEFORE DELETE ON characters BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        before=snapshot(self.lib.source)
        self.index({'action':'apply','tags':['carol_example']});v=self.finish();self.assertEqual(v['state']['status'],'error',v)
        after=snapshot(self.lib.source)
        for k in ('characters','character_aliases','labels'):self.assertEqual(before[k],after[k])
        with sqlite3.connect(self.lib.source) as c:c.execute('DROP TRIGGER fail_index')
        self.stop(self.process);self.restart();v=self.finish();self.assertEqual(v['state']['status'],'done',v)

    def test_source_commit_app_failure_sigkill_restart_idempotent(self):
        with sqlite3.connect(self.lib.db) as c:c.execute("CREATE TRIGGER fail_projection BEFORE INSERT ON meta WHEN NEW.key LIKE 'index_job:%' BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        v=self.apply();self.assertEqual(v['state']['status'],'error',v)
        with sqlite3.connect(self.lib.source) as c:self.assertEqual(c.execute("SELECT count(*) FROM characters WHERE char_id='carol_example'").fetchone()[0],1)
        self.process.kill();self.process.wait()
        with sqlite3.connect(self.lib.db) as c:c.execute('DROP TRIGGER fail_projection')
        self.restart();v=self.finish();self.assertEqual(v['state']['status'],'done',v)
        self.assertEqual(len(v['history']),1)
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(c.execute("SELECT count(*) FROM meta WHERE key LIKE 'index_job:%'").fetchone()[0],1)
        self.stop(self.process);self.restart();self.assertEqual(len(self.index()['history']),1)

    def test_invalid_or_old_worker_results_rejected(self):
        for mode in ('identity','invalid','fail'):
            self.mode.write_text(mode)
            before=snapshot(self.lib.source)
            v=self.apply();self.assertEqual(v['state']['status'],'error',v)
            for k in ('characters','character_aliases','labels'):self.assertEqual(before[k],snapshot(self.lib.source)[k])
            self.stop(self.process)
            (self.lib.path/'index-job.json').unlink()
            self.restart()

    def test_network_failure_no_check_replacement(self):
        old=self.check()['check'];self.fail_network=True
        self.index({'action':'check'});self.assertEqual(self.finish()['state']['status'],'error');self.assertEqual(self.index()['check'],old)

    def test_repeat_busy_cancel_switch_same_id_and_shutdown(self):
        self.mode.write_text('sleep');self.check();self.index({'action':'apply','tags':['carol_example']})
        self.assertEqual(self.request('/api/index',{'action':'check'})['status'],400)
        self.assertEqual(self.request('/api/sync',{})['status'],400)
        other,store=self.package('other');other.config['id']=self.lib.config['id'];(other.path/'library.json').write_text(json.dumps(other.config))
        before=snapshot(other.source)
        start=time.monotonic();r=self.request('/api/libraries/open',{'path':str(other.path)});self.assertEqual(r['status'],200,r['body']);self.assertLess(time.monotonic()-start,3)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.index()['state']['status'],'idle')
        self.assertEqual(snapshot(other.source),before)
        self.assertEqual(self.request('/api/libraries/open',{'path':str(self.lib.path)})['status'],200)
        time.sleep(.1);start=time.monotonic();self.stop(self.process);self.assertLess(time.monotonic()-start,3)

    def test_no_changes_startup_due_and_v1_watch_resolution(self):
        self.remote_rows=[]
        self.stop(self.process)
        (self.shared/'char-patch/check.json').unlink(missing_ok=True)
        (self.shared/'char-patch/watch.json').write_text(json.dumps({'works':['example','unresolved'],'min_posts':100}))
        self.process,self.log=self.start(self.lib.path,extra=('--index-check',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        v=self.finish();self.assertEqual(v['state']['status'],'checked',v)
        self.assertEqual(v['check']['missing'],[])
        self.assertEqual(v['watch']['works'],[self.work]);self.assertEqual(v['watch']['pending_tags'],['unresolved'])
        # #205: the shared file of earlier versions is read as a starting point, never rewritten.
        self.assertEqual(json.loads((self.shared/'char-patch/watch.json').read_text()),{'works':['example','unresolved'],'min_posts':100})
        self.assertFalse((self.shared/'char-patch/watch-v1.json').exists())
        count=len(self.calls);time.sleep(1.2);self.assertEqual(len(self.calls),count)

    def test_deterministic_get_and_progress_bytes_match_reference(self):
        from web import index_update
        from unittest.mock import patch
        # Reference has its own shared state; timings are copied only to compare wire shape.
        old=self.root/'reference-index';old.mkdir()
        with patch.object(index_update,'shared_dir',return_value=old): reference=IndexUpdater(self.store)
        (old/'char-patch').mkdir()
        watch=self.index()['watch'];(old/'char-patch/watch.json').write_text(json.dumps(watch))
        self.assertEqual(self.request('/api/index')['body'],json.dumps(reference.info(),ensure_ascii=False).encode())
        self.check();v=self.index()
        reference.state={'status':'checked','started_at':v['state']['started_at'],'found':v['state']['found'],'finished_at':v['state']['finished_at']}
        reference._write('check.json',v['check'])
        self.assertEqual(self.request('/api/index')['body'],json.dumps(reference.info(),ensure_ascii=False).encode())
        self.assertEqual(self.request('/api/index?progress=1')['body'],json.dumps(reference.progress(),ensure_ascii=False).encode())

    def test_real_bundled_calculation_worker_native_download_and_import(self):
        import numpy as np
        from PIL import Image
        # Numerical/model substitute only: actual clean/build/scan and native downloads/import run.
        with sqlite3.connect(self.lib.source) as c:
            for column in ('anchor_p90 REAL','base_tag TEXT','skin TEXT','post_count INTEGER','gender_conf REAL','anchor_p50 REAL','anchor_p75 REAL'):
                c.execute('ALTER TABLE characters ADD COLUMN '+column)
        for cid,axis in [('alice',0),('bob',2)]:
            rows=np.zeros((6,768),np.float32);rows[:,axis]=1;rows[:,3]=np.arange(6)*.002
            np.save(self.shared/'char-index'/f'{cid}.npy',rows)
            (self.shared/'char-index'/f'{cid}.json').write_text(json.dumps({'short_tag':cid,'tag':cid+'_tag','copyright':'example','gender':{'girl':1},'post_count':300}))
        features=self.lib.path/'features';features.mkdir(exist_ok=True)
        f=np.zeros((3,768),np.float32);f[0,0]=1;f[1:,1]=1;f[1:,3]=.006
        np.save(features/'full-features.npy',f);(features/'full-manifest.csv').write_text('uuid\na\nb\nc\n')
        with sqlite3.connect(self.lib.source) as c:
            c.execute('CREATE TABLE embeddings(uuid TEXT,model TEXT,vec BLOB)')
            for col in ('source','model_ver','run_id'):c.execute('ALTER TABLE labels ADD COLUMN '+col+' TEXT')
        wrapper=self.root/'numerical-worker';wrapper.write_text('#!'+os.sys.executable+'\n'+
            'import sys,types,numpy as np\n'+
            'sys.path.insert(0,'+repr(str(self.resources/'tools'))+')\n'+
            'm=types.ModuleType("ccip")\n'+
            'def cross(a,b,check=True):\n a=np.asarray(a);b=np.asarray(b);return (1-a@b.T/np.linalg.norm(a,axis=1)[:,None]/np.linalg.norm(b,axis=1)[None,:])/2\n'+
            'def features(images):\n a=np.zeros((len(images),768),np.float32);a[:,1]=1;a[:,3]=[im.getpixel((0,0))[0]*.002 for im in images];return a\n'+
            'm.cross=cross;m.features=features;sys.modules["ccip"]=m\n'+
            'import index_worker,traceback\ntry: index_worker.main(*sys.argv[1:])\nexcept Exception:\n open('+repr(str(self.root/'worker-error'))+',"w").write(traceback.format_exc());raise\n')
        wrapper.chmod(0o755)
        self.stop(self.process);self.env['ILLUSTASH_INDEX_WORKER']=worker_command(wrapper);self.restart()
        self.remote_rows=self.remote_rows[:1]
        self.posts=[{'id':i,'tag_string_general':'solo 1girl blue_hair','media_asset':{'variants':[{'type':'720x720','url':f'http://127.0.0.1:{self.remote.server_port}/image/{i}'}]}} for i in range(6)]
        self.check();self.index({'action':'apply','tags':['carol_example']});v=self.finish();self.assertEqual(v['state']['status'],'done',(v,(self.root/'worker-error').read_text() if (self.root/'worker-error').exists() else ''))
        self.assertEqual(v['history'][0]['characters'][0]['status'],'added',v)
        self.assertEqual(v['history'][0]['characters'][0]['claims'],2,v)
        self.assertEqual(sum(p.startswith('/image/') for p in self.calls),6)
        self.assertTrue((self.shared/'char-anchors.npy').exists())
        self.assertFalse((self.lib.path/'index-job.json').exists())
        self.assertEqual(list((self.lib.path/'index-jobs').iterdir()),[])

    def test_index_event_actual_research_consumer_reloads_anchors_after_restart(self):
        self.test_real_bundled_calculation_worker_native_download_and_import()
        self.stop(self.process)
        before=snapshot(self.lib.db)
        worker=self.root/'research-consumer'; receipt=self.root/'research-request.json'
        repo=Path(__file__).resolve().parents[1]
        worker.write_text('#!'+os.sys.executable+'\n'+
            'import sys,json\nfrom pathlib import Path\n'+
            'sys.path.insert(0,'+repr(str(repo))+')\n'+
            'from web.research_worker import run\n'+
            'r=json.loads(Path(sys.argv[1]).read_text());Path('+repr(str(receipt))+').write_text(json.dumps(r))\n'+
            'import traceback\ntry: v=run(r);Path(sys.argv[2]).write_text(json.dumps(v,allow_nan=False))\nexcept Exception: Path('+repr(str(self.root/'research-error'))+').write_text(traceback.format_exc());raise\n')
        worker.chmod(0o755);self.env['ILLUSTASH_RESEARCH_WORKER']=worker_command(worker);self.restart()
        def complete():
            with sqlite3.connect(self.lib.db) as c:
                return c.execute("SELECT count(*) FROM backend_effects WHERE kind='index_changed' AND status='pending'").fetchone()[0]==0 and c.execute("SELECT count(*) FROM research_runs WHERE status='done'").fetchone()[0]>0
        from tests.test_read_http import wait_until
        wait_until(complete,timeout=20)
        request=json.loads(receipt.read_text());self.assertTrue(request['regroup'])
        self.assertIn('carol_example',(Path(request['anchors'])/'char-anchors.txt').read_text())
        after=snapshot(self.lib.db)
        for table in ('assertions','actions'):self.assertEqual(before[table],after[table])

    def test_timeout_worker_missing_and_offline_network_permission(self):
        self.check();self.stop(self.process)
        self.mode.write_text('sleep');self.env['ILLUSTASH_INDEX_TIMEOUT_MS']='100';self.restart()
        before=snapshot(self.lib.source)
        self.index({'action':'apply','tags':['carol_example']});v=self.finish();self.assertEqual(v['state']['status'],'error',v)
        for k in ('characters','character_aliases','labels'):self.assertEqual(snapshot(self.lib.source)[k],before[k])
        self.stop(self.process);(self.lib.path/'index-job.json').unlink();self.env.pop('ILLUSTASH_INDEX_WORKER');self.env.pop('ILLUSTASH_INDEX_TIMEOUT_MS');self.restart()
        self.assertEqual(self.request('/api/index',{'action':'apply','tags':['carol_example']})['status'],400)
        self.assertEqual(self.request('/api/recognition/import',{'resume':True})['status'],200)
        # No external request is made: the network gate rejects before contacting a host.
        self.stop(self.process);self.env.pop('ILLUSTASH_COPYRIGHT_BASE');self.restart()
        self.index({'action':'check'});v=self.finish();self.assertEqual(v['state']['status'],'error',v)
        self.assertIn('尚未授权',v['state']['message'])

    def test_changed_input_rejects_stale_patch_then_new_check_remains_available(self):
        self.check();self.mode.write_text('sleep')
        self.index({'action':'apply','tags':['carol_example']})
        for _ in range(100):
            if self.index()['state']['status']=='building':break
            time.sleep(.01)
        self.process.kill();self.process.wait()
        with sqlite3.connect(self.lib.source) as c:c.execute("UPDATE labels SET conf=.5 WHERE dim='character' AND uuid='b'")
        self.mode.write_text('ok');self.restart();v=self.finish();self.assertEqual(v['state']['status'],'error',v)
        self.assertIn('输入已改变',v['state']['message'])
        self.assertFalse((self.lib.path/'index-job.json').exists())
        self.check();self.index({'action':'apply','tags':['carol_example']});self.assertEqual(self.finish()['state']['status'],'done')

    def test_apply_source_projection_names_and_history_match_python_chain(self):
        from unittest.mock import patch
        from web import index_update
        from web.store import Store
        root=self.root/'old-chain';root.mkdir()
        source=root/'source.sqlite';app=root/'library.sqlite'
        for old,new in ((self.lib.source,source),(self.lib.db,app)):
            with sqlite3.connect(old) as src,sqlite3.connect(new) as dst:src.backup(dst)
        store=Store.open(source,app)
        def finder(works,minimum,per):per('example',2);return self.remote_rows
        def runner(args):
            yield {'step':'fetch','done':0,'total':2}
            with sqlite3.connect(source) as c:
                c.execute("INSERT INTO characters VALUES ('carol_example','Carol',NULL,NULL,'example',NULL)")
                c.execute("DELETE FROM labels WHERE dim='char_ext' AND uuid='b'")
                c.executemany("INSERT INTO labels VALUES (?,'char_ext','carol_example',?)",[('b',.97),('c',.96)])
            (root/'char-index').mkdir()
            (root/'char-index/carol_example.json').write_text(json.dumps({'source':'danbooru-patch'}))
            (root/'char-patch/rejected').mkdir()
            (root/'char-patch/rejected/dani_example.json').write_text(json.dumps({'rejected':'参考图没有一致的主体'}))
            yield {'step':'scan'}
        with patch.object(index_update,'shared_dir',return_value=root):old=IndexUpdater(store,runner=runner,finder=finder)
        old._write('watch.json',self.index()['watch']);old.start_check();old.thread.join(5);old.start_apply(['carol_example','dani_example']);old.thread.join(5)
        expected=old.info();actual=self.apply();self.assertEqual(actual['state']['status'],'done',actual)
        for table in ('characters','character_aliases','labels'):
            with sqlite3.connect(source) as a,sqlite3.connect(self.lib.source) as b:self.assertEqual(sorted(a.execute('SELECT * FROM '+table),key=repr),sorted(b.execute('SELECT * FROM '+table),key=repr),table)
        def projection(path):
            with sqlite3.connect(path) as c:
                ids=dict(c.execute("SELECT entity_id,system||':'||key FROM links WHERE system IN ('ccip','qualifier')"))
                def canon(rows):return sorted([tuple(ids.get(v,v) for v in r) for r in rows],key=repr)
                return {table:canon(c.execute(sql)) for table,sql in {
                  'names':'SELECT entity_id,lang,value,source,role,preferred,position FROM names',
                  'links':'SELECT system,key,entity_id,current FROM links',
                  'memberships':'SELECT uuid,entity,status FROM memberships',
                  'candidates':'SELECT uuid,entity,score,source FROM candidates',
                  'assertions':'SELECT uuid,entity,status,origin FROM assertions',
                  'actions':'SELECT * FROM actions'}.items()}
        self.assertEqual(projection(app),projection(self.lib.db))
        self.assertEqual(actual['state']['changed'],expected['state']['changed'])
        a,b=actual['history'][0],expected['history'][0];a.pop('at');b.pop('at')
        for h,path in ((a,self.lib.db),(b,app)):
            with sqlite3.connect(path) as c: ids=dict(c.execute("SELECT entity_id,system||':'||key FROM links WHERE system IN ('ccip','qualifier')"))
            for r in h['characters']:
                if r['entity']:
                    for key in ('id','work_id','character_id','variant_id','canonical_id'):
                        if key in r['entity']:r['entity'][key]=ids.get(r['entity'][key],r['entity'][key])
        self.assertEqual(a,b)
