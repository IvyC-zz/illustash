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
from tests.fixed_expectations import unclocked
from tests.test_native_human_labels import snapshot

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
if mode=='anchors':
 ids=sorted(x.stem for x in (p/'char-index').glob('*.json'))
 (p/'char-anchors.txt').write_text('\n'.join(ids)); (p/'char-anchors.npy').write_bytes(('rows:'+','.join(ids)).encode())
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
        self.hold_posts=False;self.posts_entered=threading.Event();self.release_posts=threading.Event()
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
                elif self.path.startswith('/posts'):
                    if 'dani_' in self.path and parent.hold_posts:
                        parent.posts_entered.set();parent.release_posts.wait(10)
                    data=json.dumps(parent.posts).encode()
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
        self.lib=self.rec.package('work',self.lib.path)
        self.work=self.rec.value('work')
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
        self.index({'action':'check','allow_network':True}); v=self.finish();self.assertEqual(v['state']['status'],'checked',v);return v

    def test_check_requires_consent_on_every_run_without_persisting_it(self):
        before=snapshot(self.lib.db)['prefs']
        for value in (None, False, 'true', 1):
            body={'action':'check'}
            if value is not None: body['allow_network']=value
            r=self.request('/api/index',body);self.assertEqual(r['status'],400,r['body'])
        self.assertEqual(self.calls,[])
        self.check();calls=len(self.calls);self.assertGreater(calls,0)
        r=self.request('/api/index',{'action':'check'});self.assertEqual(r['status'],400,r['body'])
        self.assertEqual(len(self.calls),calls)
        self.assertEqual(snapshot(self.lib.db)['prefs'],before)
        self.stop(self.process);self.restart()
        self.assertEqual(self.request('/api/index',{'action':'check'})['status'],400)
        self.assertEqual(len(self.calls),calls)

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
        other = self.package('other')
        other_work=self.rec.value('other work')
        self.stop(self.process);self.process,self.log=self.start(other.path,extra=('--no-index-check',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.index()['watch'],{'version':2,'works':[],'pending_tags':[],'min_posts':100})
        self.assertEqual(self.index({'action':'watch','works':[other_work],'min_posts':120})['watch']['works'],[other_work])
        self.assertEqual(self.request('/api/index',{'action':'watch','works':[self.work],'min_posts':120})['status'],400)
        self.stop(self.process);self.restart()
        v=self.index()['watch'];self.assertEqual((v['works'],v['min_posts']),([self.work],150))
        self.assertEqual(shared_file.read_bytes(),legacy)

    @unittest.skipIf(os.name == 'nt', 'POSIX 权限位制造复制失败')
    def test_index_files_are_published_as_one_generation(self):
        # #47: readers never see a mix of old and new shared index files.
        self.mode.write_text('anchors')
        names = ('char-anchors.npy', 'char-anchors.txt', 'char-index/carol_example.json')
        published = lambda: {n: (self.shared/n).read_bytes() if (self.shared/n).exists() else None for n in names}
        generation = lambda: (self.shared/'index-generation').read_text() if (self.shared/'index-generation').exists() else None
        # A copy that fails while staging changes no published file and leaves no staged copy behind.
        self.check()
        self.shared.chmod(0o555); self.addCleanup(self.shared.chmod, 0o755)
        self.index({'action':'apply','tags':['carol_example']}); v = self.finish()
        self.shared.chmod(0o755)
        self.assertEqual(v['state']['status'], 'error', v)
        self.assertEqual((published(), generation()), (dict.fromkeys(names), None))
        self.assertEqual(list(self.shared.rglob('*.tmp')), [])
        self.stop(self.process); self.restart(); v = self.finish(); self.assertEqual(v['state']['status'], 'done', v)
        first = published()
        self.assertEqual((first['char-anchors.txt'], generation()), (b'carol_example', '2'))
        # A failure after files were replaced leaves the generation odd until the resume completes the set.
        index = self.shared/'char-index'
        index.chmod(0o555); self.addCleanup(index.chmod, 0o755)
        self.index({'action':'drop','char':'carol_example','reason':'合成放弃'}); v = self.finish()
        index.chmod(0o755)
        self.assertEqual((v['state']['status'], generation()), ('error', '3'), v)
        self.stop(self.process); self.restart(); v = self.finish(); self.assertEqual(v['state']['status'], 'done', v)
        self.assertEqual((published(), generation()), ({'char-anchors.npy': b'rows:', 'char-anchors.txt': b'', 'char-index/carol_example.json': None}, '4'))
        self.assertEqual(list(self.shared.rglob('*.tmp')), [])

    def test_done_is_reported_only_after_the_job_is_cleaned_up(self):
        # #123: a finished job reported done before removing its journal and folder; a poll in between saw both.
        # The test-only pause right after done makes that moment wide enough to observe every time.
        self.env['ILLUSTASH_TEST_INDEX_DONE_PAUSE_MS']='1500';self.stop(self.process);self.restart()
        self.check();v=self.apply();self.assertEqual(v['state']['status'],'done',v)
        self.assertFalse((self.lib.path/'index-job.json').exists())
        self.assertEqual(list((self.lib.path/'index-jobs').iterdir()),[])

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
        self.index({'action':'check','allow_network':True});self.assertEqual(self.finish()['state']['status'],'error');self.assertEqual(self.index()['check'],old)

    def test_repeat_busy_cancel_switch_same_id_and_shutdown(self):
        self.mode.write_text('sleep');self.check();self.index({'action':'apply','tags':['carol_example']})
        self.assertEqual(self.request('/api/index',{'action':'check','allow_network':True})['status'],400)
        self.assertEqual(self.request('/api/sync',{})['status'],400)
        other = self.package('other');other.config['id']=self.lib.config['id'];(other.path/'library.json').write_text(json.dumps(other.config))
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
        self.process,self.log=self.start(self.lib.path,extra=('--index-check','--index-network'))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        v=self.finish();self.assertEqual(v['state']['status'],'checked',v)
        self.assertEqual(v['check']['missing'],[])
        self.assertEqual(v['watch']['works'],[self.work]);self.assertEqual(v['watch']['pending_tags'],['unresolved'])
        # #205: the shared file of earlier versions is read as a starting point, never rewritten.
        self.assertEqual(json.loads((self.shared/'char-patch/watch.json').read_text()),{'works':['example','unresolved'],'min_posts':100})
        self.assertFalse((self.shared/'char-patch/watch-v1.json').exists())
        count=len(self.calls);time.sleep(1.2);self.assertEqual(len(self.calls),count)

    def test_deterministic_get_and_progress_bytes_match_reference(self):
        # The former Python updater's bytes are recorded (tests/fixed_expectations.py); it had its own
        # shared state and the native timings were copied into it, so times compare as <clock>.
        reference=None
        watch=self.index()['watch']
        info=lambda:json.dumps(reference.info(),ensure_ascii=False)
        self.assertEqual(self.request('/api/index')['body'].decode(),self.rec.value('info'))
        self.check();v=self.index()
        self.assertEqual(unclocked(self.request('/api/index')['body'].decode()),unclocked(self.rec.value('checked info')))
        self.assertEqual(unclocked(self.request('/api/index?progress=1')['body'].decode()),
                         unclocked(self.rec.value('progress')))

    def test_real_bundled_calculation_worker_native_download_and_import(self):
        import numpy as np
        from PIL import Image
        # 合成模型特征输入；真实原生 clean/build/scan、下载与导入仍运行。
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
        # 原生计算读取合成的缓存特征；模型输入准备与索引计算分开验收。
        raw=self.shared/'char-patch/raw';raw.mkdir(parents=True,exist_ok=True)
        rows=np.zeros((6,768),np.float32);rows[:,1]=1;rows[:,3]=np.arange(6)*.002
        np.save(raw/'carol_example.npy',rows)
        # 宿主照常下载；启动计算前只补合成缓存的来源元数据，随后运行原生程序。
        wrapper=self.root/'cached-index-worker'
        wrapper.write_text('#!'+os.sys.executable+'\n'+
            'import json,os,subprocess,sys\nfrom pathlib import Path\n'+
            'request=json.loads(Path(sys.argv[1]).read_text());review=Path(request["review"])\n'+
            'for row in request["rows"]:\n'+
            ' meta=dict(row,source="danbooru-patch",posts=list(range(6)),gender={"girl":1})\n'+
            ' (review/"char-index"/(row["short_tag"]+".json")).write_text(json.dumps(meta))\n'+
            'sys.exit(subprocess.run([os.environ["ILLUSTASH_INDEX_COMPUTE_BIN"],*sys.argv[1:]]).returncode)\n')
        self.stop(self.process)
        self.env.update(ILLUSTASH_INDEX_WORKER=worker_command(wrapper),
                        ILLUSTASH_INDEX_TAGS=str(self.resources/'tags/tags_v3.csv'))
        self.restart()
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
            'import subprocess,os\n'+
            'r=json.loads(Path(sys.argv[1]).read_text());Path('+repr(str(receipt))+').write_text(json.dumps(r))\n'+
            'import traceback\ntry: subprocess.run([os.environ["ILLUSTASH_RESEARCH_COMPUTE_BIN"],sys.argv[1],sys.argv[2]],check=True)\nexcept Exception: Path('+repr(str(self.root/'research-error'))+').write_text(traceback.format_exc());raise\n')
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
        self.assertEqual(lifecycle.import_task(self,'/api/recognition/import',{'resume':True})['status'],200)
        # No external request is made: the network gate rejects before contacting a host.
        self.stop(self.process);self.env.pop('ILLUSTASH_COPYRIGHT_BASE');self.restart()
        r=self.request('/api/index',{'action':'check'});self.assertEqual(r['status'],400,r['body'])
        self.assertIn('确认本次检查',r['body'].decode())

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

    def paused(self):
        for _ in range(500):
            v=self.index()
            if v['state']['status']=='paused':return v
            self.assertIn(v['state']['status'],('idle','checking','checked','fetching'),v)
            time.sleep(.02)
        self.fail('index add did not pause')

    def test_add_without_network_after_restart_pauses_keeps_pictures_and_continues(self):
        # #287: a restart resumes an unfinished add without the person's consent to go online. It waits for them
        # instead of failing every minute, keeps the pictures it has, and goes on once they allow it.
        self.env['ILLUSTASH_TEST_INDEX_CONSENT']='1';self.stop(self.process);self.restart()
        port=self.remote.server_port
        self.posts=[{'id':i,'tag_string_general':'solo','media_asset':{'variants':[{'type':'720x720','url':f'http://127.0.0.1:{port}/image/{i}'}]}} for i in range(3)]
        self.check();self.hold_posts=True
        self.index({'action':'apply','tags':['carol_example','dani_example'],'allow_network':True})
        self.assertTrue(self.posts_entered.wait(10))
        job=json.loads((self.lib.path/'index-job.json').read_text());folder=Path(job['folder'])
        self.assertTrue((folder/'char-patch/carol_example-refs.json').is_file())
        self.process.kill();self.process.wait();self.release_posts.set();self.hold_posts=False
        self.assertEqual(sorted(p for p in self.calls if p.startswith('/image/')),['/image/0','/image/1','/image/2'])
        self.restart();v=self.paused()['state']
        self.assertEqual((v['kind'],v['done'],v['total'],v['message']),('add',1,2,'继续下载参考图需要你确认联网'),v)
        self.assertTrue(json.loads((self.lib.path/'index-job.json').read_text())['paused'])
        # The automatic retry leaves it waiting, and a new check or another add waits for the person too.
        calls=len(self.calls);self.stop(self.process);self.restart();self.assertEqual(self.paused()['state'],v)
        self.assertEqual(len(self.calls),calls)
        for body in ({'action':'check','allow_network':True},{'action':'apply','tags':['carol_example'],'allow_network':True}):
            self.assertEqual(self.request('/api/index',body)['status'],400)
        # A sync while it waited changes the source; the add goes on from the library as it is now.
        with sqlite3.connect(self.lib.source) as c:c.execute("UPDATE labels SET conf=.5 WHERE dim='character' AND uuid='b'")
        self.index({'action':'resume','allow_network':True});v=self.finish()
        self.assertEqual((v['state']['status'],v['state']['added']),('done',1),v)
        # Carol's pictures were kept, Dani's fetched once; nothing was downloaded twice.
        images=[p for p in self.calls if p.startswith('/image/')]
        self.assertEqual(len(images),6,images)
        self.assertFalse((self.lib.path/'index-job.json').exists())

    def test_paused_add_can_be_given_up(self):
        self.env['ILLUSTASH_TEST_INDEX_CONSENT']='1';self.stop(self.process);self.restart()
        self.assertEqual(self.request('/api/index',{'action':'discard'})['status'],400)
        self.check();before=snapshot(self.lib.source)
        self.index({'action':'apply','tags':['carol_example']});v=self.paused()['state']
        self.assertEqual((v['done'],v['total']),(0,1),v)
        folder=Path(json.loads((self.lib.path/'index-job.json').read_text())['folder'])
        self.assertEqual(self.index({'action':'discard'})['state'],{'status':'idle'})
        self.assertFalse((self.lib.path/'index-job.json').exists());self.assertFalse(folder.exists())
        for k in ('characters','character_aliases','labels'):self.assertEqual(snapshot(self.lib.source)[k],before[k])
        self.assertEqual(self.check()['check']['missing'],self.remote_rows)

    def test_failure_says_what_failed_without_the_error_kind(self):
        self.check();self.mode.write_text('fail')
        self.index({'action':'apply','tags':['carol_example']});v=self.finish()['state']
        self.assertEqual((v['status'],v['kind'],v['message']),('error','add','角色索引计算 worker 失败'),v)

    def test_apply_source_projection_names_and_history_match_python_chain(self):
        expected=self.rec.value('python chain')
        actual=self.apply();self.assertEqual(actual['state']['status'],'done',actual)
        for table in ('characters','character_aliases','labels'):
            with sqlite3.connect(self.lib.source) as b:
                self.assertEqual(expected['source'][table],sorted(b.execute('SELECT * FROM '+table),key=repr),table)
        self.assertEqual(expected['projection'],projection(self.lib.db))
        self.assertEqual(actual['state']['changed'],expected['info']['state']['changed'])
        a,b=actual['history'][0],expected['info']['history'][0];a.pop('at');b.pop('at')
        canonical(a,self.lib.db)
        self.assertEqual(a,b)



# The character links as the former `links` table had them (core v8 keeps them in term_links, #201).
LINKS="SELECT CASE x.system WHEN 'wd_character' THEN 'wd' ELSE x.system END AS system,x.key AS key,t.key AS entity_id,x.current AS current FROM term_links x JOIN terms t ON t.id=x.term_id JOIN term_tree r ON r.term_id=x.term_id"


def projection(path):
    """Application rows the patch touches, entity ids replaced by their stable links; times as <clock>."""
    with sqlite3.connect(path) as c:
        ids=dict(c.execute("SELECT entity_id,system||':'||key FROM ("+LINKS+") WHERE system IN ('ccip','qualifier')"))
        def canon(rows):return sorted([tuple(unclocked(ids.get(v,v)) for v in r) for r in rows],key=repr)
        return {table:canon(c.execute(sql)) for table,sql in {
          'names':'SELECT t.key,n.lang,n.value,n.source,n.role,n.preferred,n.position FROM term_names n JOIN terms t ON t.id=n.term_id JOIN term_tree r ON r.term_id=n.term_id',
          'links':LINKS,
          'memberships':'SELECT uuid,entity,status FROM memberships',
          'candidates':'SELECT uuid,entity,score,source FROM candidates',
          'assertions':'SELECT uuid,entity,status,origin FROM assertions',
          'actions':'SELECT * FROM actions'}.items()}


def canonical(history,path):
    """A history entry's entity ids replaced by their stable links in that library."""
    with sqlite3.connect(path) as c: ids=dict(c.execute("SELECT entity_id,system||':'||key FROM ("+LINKS+") WHERE system IN ('ccip','qualifier')"))
    for r in history['characters']:
        if r['entity']:
            for key in ('id','work_id','character_id','variant_id','canonical_id'):
                if key in r['entity']:r['entity'][key]=ids.get(r['entity'][key],r['entity'][key])
