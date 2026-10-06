"""#51 acceptance: fresh synthetic packages, computation substitutes and loopback queries only."""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

from contextlib import closing
import http.server
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

from tests import test_native_lifecycle as lifecycle
from tests.test_read_http import SPAWN, wait_until
from tools.verify_read_http import fetch
from provenance import db, export
from web import artists


WORKER = r'''
import json,os,sqlite3,sys,time
from pathlib import Path
r=json.loads(Path(sys.argv[1]).read_text()); d=Path(r['cache']); box=Path(r['mailbox'])
mode=os.environ.get('SYNTHETIC_MODE','ok')
if mode=='hang-first' and r['step']=='local_match':
 while True:time.sleep(.1)
if mode in ('hang','tree'):
 if mode=='tree':
  import subprocess
  p=subprocess.Popen([sys.executable,'-c','import time; time.sleep(300)'])
  Path(os.environ['SYNTHETIC_PID']).write_text(str(p.pid))
 while True: time.sleep(.1)
if mode=='crash': sys.exit(9)
step=r['step']; result={'stopped':''}
if step in ('pixel_hash','iqdb'):
 ids=r['only'] or ['a']
 result=[{'asset':x,'hash':'a'*32 if step=='pixel_hash' else 'iqdb_'+'a'*528} for x in ids]
 if mode=='outside':result=[{'asset':'outside','hash':'a'*32}]
if step=='groups':
 c=sqlite3.connect(d/'provenance.sqlite')
 c.execute("INSERT OR IGNORE INTO artists(id) VALUES ('synthetic-candidate')")
 c.execute("INSERT OR REPLACE INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES ('b','synthetic-candidate','group_run',.3,'{}','candidate')")
 c.commit();c.close();result={'auto':0,'candidates':1,'stopped':''}
if step=='verify' and mode=='query':
 req={'identity':r['identity'],'url':'https://danbooru.donmai.us/posts.json?tags=synthetic'}
 (box/'net-request.json').write_text(json.dumps(req))
 while not (box/'net-response.json').exists():time.sleep(.01)
 result={'asked':1,'stopped':''}
if mode=='wrong':r['identity']['task_id']='old'
if mode=='mutate-runs':
 c=sqlite3.connect(d/'provenance.sqlite');c.execute("DELETE FROM runs");c.commit()
Path(sys.argv[2]).write_text(json.dumps({'format':r['format'],'identity':r['identity'],'step':step,'result':result}))
'''

class NativeProvenanceTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    package = lifecycle.NativeLifecycleTest.package
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    def start(self,path,extra=(),command=None):
        log=tempfile.TemporaryFile();self.addCleanup(log.close)
        process=subprocess.Popen([str(self.binary),'http','--no-names','--no-auto-sync','--library',str(path),'--port',str(self.port),'--root',str(self.resources),*extra],env={**self.env,'ILLUSTASH_NO_KEYCHAIN':'1'},stdout=log,stderr=log, **SPAWN)
        self.addCleanup(self.stop,process)
        wait_until(lambda:fetch(self.origin,{'url':'/api/bootstrap'},self.origin)['status']==200,timeout=8)
        return process,log
    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.worker=self.root/'worker';self.worker.write_text('#!'+sys.executable+'\n'+WORKER);self.worker.chmod(0o755)
        self.calls=[];self.code=200;self.bad=False;self.pixiv_users=False;self.pixiv_code=None
        outer=self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                outer.calls.append(self.path)
                if 'media_assets.json' in self.path:value=[{'pixel_hash':'a'*32,'post':{'id':1,'tag_string_artist':'synthetic_artist','source':'https://x.com/synthetic/status/1'}}]
                elif 'iqdb_queries.json' in self.path:value=[{'score':89.1,'post':{'id':2,'tag_string_artist':'synthetic_other','source':'https://www.pixiv.net/artworks/2'}}]
                elif 'artists.json' in self.path:value=[{'id':7,'name':'synthetic_artist','is_banned':False}]
                elif 'posts.json' in self.path:value=[]
                elif '/ajax/user/extra' in self.path:value={'body':{'following':101}}
                elif '/following' in self.path and outer.pixiv_users:
                    value={'body':{'users':[{'userId':str(i),'userName':'合成名字'} for i in (range(100,200) if 'offset=0' in self.path else range(200,201))],'total':101}}
                elif '/ajax/user/' in self.path:value={'body':{'name':'合成账号','users':[],'total':0}}
                else:value={'body':{'users':[],'total':0}}
                data=b'bad' if outer.bad else json.dumps(value).encode()
                self.send_response(outer.pixiv_code if '/ajax/' in self.path and outer.pixiv_code else outer.code);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        self.service=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        t=threading.Thread(target=self.service.serve_forever,daemon=True);t.start()
        self.addCleanup(self.service.server_close);self.addCleanup(t.join);self.addCleanup(self.service.shutdown)
        self.env.update(ILLUSTASH_PROVENANCE_WORKER=worker_command(self.worker),ILLUSTASH_PROVENANCE_BASE=f'http://127.0.0.1:{self.service.server_port}',ILLUSTASH_PROVENANCE_IDLE_SECONDS='2')
        with sqlite3.connect(self.lib.source) as c:
            c.execute("ALTER TABLE assets ADD COLUMN preset TEXT DEFAULT 'x'")
            c.execute("ALTER TABLE assets ADD COLUMN source_id TEXT DEFAULT 'synthetic'")
        self.env['ILLUSTASH_NO_KEYCHAIN']='1'
        self.restart()
    def restart(self,extra=()):
        self.stop(self.process);self.process,self.log=self.start(self.lib.path,extra)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
    def info(self):
        value=self.request('/api/provenance');self.assertEqual(value['status'],200,value);return json.loads(value['body'])
    def finish(self,status=None):
        wait_until(lambda:self.info()['state']['status'] not in ('running','stopping'),timeout=10)
        value=self.info()
        if status:self.assertEqual(value['state']['status'],status,value)
        return value
    def post(self,body,code=200):
        r=self.request('/api/provenance',body);self.assertEqual(r['status'],code,r);return json.loads(r['body'])
    def sql(self,sql,args=(),cache=False):
        with closing(sqlite3.connect(self.lib.path/('provenance.sqlite' if cache else 'library.sqlite'))) as c:return c.execute(sql,args).fetchall()
    def test_idle_success_bytes_and_all_action_errors(self):
        self.assertEqual(self.request('/api/provenance')['body'],json.dumps({'available':True,'state':{'status':'idle'},'runs':[],'coverage':None,'auto':True,'budgets':{'full':1024,'new':50,'pick':20,'artist':200},'frozen':False,'auto_network':False,'delivery':None},ensure_ascii=False).encode())
        for body in ({},{'action':'bad'},{'action':'lookup','ids':[]},{'action':'lookup','ids':[1]},{'action':'lookup','ids':['a']*501},{'action':'artist','artist':''},{'action':'artist','artist':True}):self.post(body,400)
        self.post({'action':'stop'})
    def test_optional_coordinator_keeps_ordinary_cache_schema_and_minimal_packages(self):
        self.env.pop('ILLUSTASH_PROVENANCE_WORKER');self.restart()
        other,_=self.package('minimal-without-sync')
        with sqlite3.connect(other.db) as c:c.execute('DROP TABLE sync_runs')
        self.assertEqual(self.request('/api/libraries/open',{'path':str(other.path)})['status'],200)
        self.lib=other;self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.info()['state']['status'],'idle');self.assertFalse(db.path_of(other).exists())
        with db.connect(other) as c:before=list(c.iterdump())
        self.assertEqual(self.info()['runs'],[])
        time.sleep(1.1) # periodic recovery also stays read-only without task intent
        with db.connect(other) as c:self.assertEqual(before,list(c.iterdump()))
        self.assertEqual(self.request('/api/assets')['status'],200)
        self.post({'action':'artist','artist':'synthetic'});self.finish('failed')
        self.assertEqual(self.request('/api/assets')['status'],200)

    def test_lookup_stable_queries_candidates_duplicate_and_manual_verdict(self):
        self.post({'action':'lookup','ids':['a']});v=self.finish('done')
        self.assertEqual(v['runs'][0]['kind'],'pick');self.assertEqual(v['delivery']['status'],'done')
        self.assertEqual(len(self.calls),1)
        self.assertTrue(self.sql("SELECT id FROM term_evidence WHERE source='provenance'"))
        self.assertTrue(self.sql("SELECT id FROM actions")==[]) # machine import does not create manual history
        self.assertTrue(self.sql("SELECT asset FROM image_artists WHERE status='candidate'",cache=True))
        count=self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'")
        artist=self.sql("SELECT artist_id FROM image_artists WHERE asset='a'",cache=True)[0][0]
        r=self.request('/api/artist/verdict',{'ids':['a'],'artist':artist,'verdict':'no'});self.assertEqual(r['status'],200,r)
        history=self.sql('SELECT count(*) FROM actions')
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(count,self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'"))
        self.assertEqual(history,self.sql('SELECT count(*) FROM actions'))
        self.assertTrue(self.sql("SELECT verdict FROM term_effective_verdicts WHERE verdict='no'"))
    def test_full_local_consent_and_x_leads(self):
        with db.connect(self.lib) as c:c.execute("INSERT INTO leads(source,system,key) VALUES ('x_following','x_id','123')")
        self.post({'action':'start'});v=self.finish('stopped');self.assertFalse(self.calls)
        self.assertEqual(v['runs'][0]['kind'],'full')
        self.post({'action':'start','allow_network':True});v=self.finish('stopped') # no bound Pixiv account
        self.assertEqual(self.sql("SELECT status FROM leads WHERE key='123'",cache=True),[('resolved',)])
        self.assertTrue(any('artists.json' in s for s in self.calls))
    def test_worker_missing_wrong_identity_crash_and_timeout(self):
        for mode in ('wrong','crash','hang','outside'):
            self.env['SYNTHETIC_MODE']=mode;self.restart()
            self.post({'action':'lookup','ids':['a']});v=self.finish()
            self.assertIn(v['state']['status'],('failed','stopped'))
            self.assertEqual(self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'"),[(0,)])
        self.env.pop('ILLUSTASH_PROVENANCE_WORKER');self.env.pop('SYNTHETIC_MODE');self.restart()
        self.post({'action':'artist','artist':'synthetic'});v=self.finish('failed')
        self.assertEqual(self.request('/api/assets')['status'],200)
    def test_stop_restart_and_process_tree(self):
        self.env.update(SYNTHETIC_MODE='tree',SYNTHETIC_PID=str(self.root/'pid'));self.restart()
        self.post({'action':'artist','artist':'synthetic'});wait_until(lambda:(self.root/'pid').exists(),timeout=5)
        pid=int((self.root/'pid').read_text());self.post({'action':'stop'});self.finish('stopped')
        wait_until(lambda:not worker_exists(pid),timeout=5)
        self.env['SYNTHETIC_MODE']='ok';self.restart();self.post({'action':'lookup','ids':['a']});self.finish('done')
    def test_failure_retry_and_delivery_recovery(self):
        self.code=403;self.post({'action':'lookup','ids':['a']});self.finish('stopped')
        self.assertEqual(len(self.calls),1)
        self.code=200;self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertTrue(self.sql("SELECT last_result FROM lookups WHERE backend='pixel_hash'",cache=True))
        with db.connect(self.lib) as c:c.execute("UPDATE native_delivery SET status='pending'")
        count=self.sql('SELECT count(*) FROM term_evidence');self.restart()
        self.assertEqual(count,self.sql('SELECT count(*) FROM term_evidence'))
        self.assertFalse(self.sql("SELECT task_id FROM native_delivery WHERE status='pending'",cache=True))
    def test_frozen_package_does_not_replay_pending_delivery(self):
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        with db.connect(self.lib) as c:c.execute("UPDATE native_delivery SET status='pending'")
        self.stop(self.process)
        config=dict(self.lib.config);config['frozen']=True
        (self.lib.path/'library.json').write_text(json.dumps(config))
        with sqlite3.connect(self.lib.db) as c:before=list(c.iterdump())
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.post({'action':'start'},400)
        self.assertTrue(self.info()['frozen']);self.assertIsNone(self.info()['delivery'])
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(before,list(c.iterdump()))
        self.assertEqual(self.sql('SELECT status FROM native_delivery',cache=True),[('pending',)])

    def test_same_id_other_package_switch_rejects_late_results(self):
        other,_=self.package('other');config=other.config.copy();config['id']=self.lib.config['id'];(other.path/'library.json').write_text(json.dumps(config))
        self.env['SYNTHETIC_MODE']='hang';self.restart();self.post({'action':'artist','artist':'synthetic'})
        response=self.request('/api/libraries/open',{'path':str(other.path)});self.assertEqual(response['status'],200,response)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.info()['state']['status'],'idle')
        with closing(sqlite3.connect(other.db)) as c:self.assertEqual(c.execute("SELECT count(*) FROM term_evidence WHERE source='provenance'").fetchone(),(0,))
    def test_same_path_reopen_refreshes_state_and_preserves_new(self):
        self.env['SYNTHETIC_MODE']='hang';self.restart()
        self.post({'action':'artist','artist':'synthetic'})
        self.assertEqual(self.request('/api/libraries/open',{'path':str(self.lib.path)})['status'],200)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        wait_until(lambda:self.info()['state']['status']=='stopped',timeout=4)
        self.env['SYNTHETIC_MODE']='hang-first'
        with db.connect(self.lib) as c:c.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')")
        self.restart();wait_until(lambda:self.info()['state'].get('kind')=='new',timeout=4)
        previous=self.sql('SELECT max(id) FROM runs',cache=True)[0][0]
        self.assertEqual(self.request('/api/libraries/open',{'path':str(self.lib.path)})['status'],200)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        wait_until(lambda:self.sql('SELECT max(id) FROM runs',cache=True)[0][0]>previous,timeout=4)
        self.assertEqual(self.info()['state']['kind'],'new');self.post({'action':'stop'});self.finish('stopped')

    def test_export_contract_native_matches_python(self):
        from web import terms_core
        before=self.root/'reference-application.sqlite'
        with sqlite3.connect(self.lib.db) as source,sqlite3.connect(before) as target:source.backup(target)
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        package=self.lib.path/self.sql('SELECT package FROM native_delivery',cache=True)[0][0]
        out=self.root/'reference-evidence';export.write(self.lib,out)
        for name in ('evidence.jsonl','terms.jsonl'):self.assertEqual((Path(package)/name).read_bytes(),(out/name).read_bytes())
        native_manifest=json.loads((Path(package)/'manifest.json').read_text());reference_manifest=json.loads((out/'manifest.json').read_text())
        native_manifest.pop('created_at');reference_manifest.pop('created_at')
        self.assertEqual(native_manifest,reference_manifest) # creation clock is transport metadata
        reference_import=terms_core.import_package(before,self.lib.config['id'],out)
        native_import=self.info()['delivery']
        self.assertEqual({k:v for k,v in native_import.items() if k!='status'},reference_import)
        # Resolve random term IDs through stable links; compare formal evidence, projections,
        # history and durable events against the reference import of identical bytes.
        queries=(
            'SELECT source,method,score,metric,data,asset_id FROM term_evidence ORDER BY source,method,data,asset_id',
            'SELECT system,key FROM term_links ORDER BY system,key',
            'SELECT p.asset_id,t.kind,p.state,p.score FROM term_projection p JOIN terms t ON t.id=p.term_id ORDER BY p.asset_id,t.kind,p.state,p.score',
            'SELECT label,before_json FROM actions ORDER BY id',
            'SELECT kind,status,error FROM backend_effects ORDER BY id',
        )
        with sqlite3.connect(before) as expected,sqlite3.connect(self.lib.db) as actual:
            for query in queries:self.assertEqual(actual.execute(query).fetchall(),expected.execute(query).fetchall(),query)
        native=self.info();reference=artists.runs(self.lib)
        self.assertEqual(native['runs'],reference['runs']);self.assertEqual(native['coverage'],reference['coverage'])
        from provenance import report
        self.assertEqual(native['runs'][0]['report'],report.run(self.lib))

    def test_pixiv_pages_cache_and_expiry_never_grants_background_consent(self):
        account=self.support/'accounts';account.mkdir(parents=True,exist_ok=True)
        (account/'pixiv').write_text('9_synthetic')
        self.pixiv_users=True
        self.post({'action':'start'});self.finish('stopped');self.assertFalse(self.calls)
        self.post({'action':'start','allow_network':True});self.finish('done')
        self.assertEqual(self.sql("SELECT count(*) FROM seeds WHERE source='pixiv_following'",cache=True),[(101,)])
        self.assertEqual(self.sql("SELECT count(*) FROM artist_names WHERE source='pixiv'",cache=True),[(101,)])
        before=len([s for s in self.calls if '/following' in s])
        self.post({'action':'start','allow_network':True});self.finish('done')
        self.assertEqual(before,len([s for s in self.calls if '/following' in s]))
        self.pixiv_code=403;self.post({'action':'start','allow_network':True});self.finish('stopped')
        metadata=json.loads((self.support/'accounts.json').read_text());self.assertNotIn('expired',metadata['pixiv'])
        self.pixiv_code=401;self.post({'action':'start','allow_network':True});self.finish('stopped')
        metadata=json.loads((self.support/'accounts.json').read_text());self.assertIn('expired',metadata['pixiv'])
        self.assertFalse(self.info()['auto_network'])
    def test_new_queue_preemption_resume_and_startup_disable(self):
        self.env['SYNTHETIC_MODE']='hang-first';self.restart()
        with db.connect(self.lib) as c:c.execute("INSERT INTO pending_runs(kind) VALUES ('new')")
        self.restart()
        wait_until(lambda:self.info()['state'].get('kind')=='new',timeout=5)
        self.post({'action':'start'},400)
        self.post({'action':'lookup','ids':['a']})
        self.finish('done')
        wait_until(lambda:self.info()['state'].get('kind')=='new' and self.info()['state']['status']=='running',timeout=5)
        self.post({'action':'stop'});self.finish('stopped')
        with db.connect(self.lib) as c:c.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')")
        self.restart(['--no-provenance']);time.sleep(1.2)
        self.assertNotEqual(self.info()['state']['status'],'running')
        self.post({'action':'start'},400)
    def test_staged_machine_only_receipt_cannot_change_native_runs(self):
        self.env['SYNTHETIC_MODE']='mutate-runs';self.restart()
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(self.sql('SELECT count(*) FROM runs',cache=True),[(1,)])

    def test_desktop_transport_expiry_and_no_cookie_fallback(self):
        calls=[];outer=self
        class Bridge(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_POST(self):self.reply()
            def do_GET(self):self.reply()
            def reply(self):
                calls.append((self.command,self.path,self.headers.get('Cookie')))
                self.assert_token=self.headers.get('X-Illustash-Token')=='synthetic-token'
                if '/pixiv/session' in self.path:value={'loggedIn':True,'userId':'9','loginOpen':False}
                elif '/pixiv/login' in self.path:value={'ok':True}
                elif '/pixiv/fetch' in self.path:
                    from urllib.parse import parse_qs,urlsplit
                    path=parse_qs(urlsplit(self.path).query)['path'][0]
                    data={'body':{'following':0,'name':'模拟桌面账号','users':[],'total':0}}
                    value={'status':outer.pixiv_code or 200,'body':json.dumps(data)}
                else:raise AssertionError(self.path)
                data=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
        bridge=http.server.ThreadingHTTPServer(('127.0.0.1',0),Bridge);thread=threading.Thread(target=bridge.serve_forever,daemon=True);thread.start()
        self.addCleanup(bridge.server_close);self.addCleanup(thread.join);self.addCleanup(bridge.shutdown)
        self.support.mkdir(exist_ok=True,parents=True);(self.support/'accounts.json').write_text(json.dumps({'pixiv':{'via':'app','user_id':'9'}}))
        account=self.support/'accounts';account.mkdir(exist_ok=True);(account/'pixiv').write_text('9_shouldNeverFallback')
        self.env.update(ILLUSTASH_DESKTOP_BRIDGE=f'http://127.0.0.1:{bridge.server_port}',ILLUSTASH_DESKTOP_TOKEN='synthetic-token');self.restart()
        self.post({'action':'start','allow_network':True});self.finish('done')
        self.assertTrue(any('/pixiv/fetch' in path for _,path,_ in calls));self.assertTrue(all(cookie is None for _,_,cookie in calls));self.assertFalse(any('/ajax/' in path for path in self.calls))
        self.pixiv_code=401;self.post({'action':'artist','artist':'synthetic'});self.finish('done')
        # login popup is a mocked local bridge command; no third-party login is executed.
        self.assertTrue(any(method=='POST' and '/pixiv/login' in path for method,path,_ in calls))
        self.env.pop('ILLUSTASH_DESKTOP_BRIDGE');self.env.pop('ILLUSTASH_DESKTOP_TOKEN');self.restart()
        before=len(self.calls);self.post({'action':'start','allow_network':True});self.finish('stopped')
        self.assertFalse(any('/ajax/' in path for path in self.calls[before:]))
    def test_sync_commit_cursor_survives_restart(self):
        self.env['SYNTHETIC_MODE']='hang-first';self.restart()
        self.stop(self.process)
        with sqlite3.connect(self.lib.db) as c:c.execute("INSERT INTO sync_runs(status,imported,finished_at) VALUES ('done',1,datetime('now'))")
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        wait_until(lambda:self.info()['state'].get('kind')=='new',timeout=5)
        self.assertTrue(self.sql("SELECT value FROM native_meta WHERE key='sync_seen'",cache=True))
        self.post({'action':'stop'});self.finish('stopped')

    def test_real_adapter_hash_and_owner_crash_recovery(self):
        from PIL import Image
        from provenance.sources.pixel_hash import pixel_hash
        image=self.root/'synthetic.png';Image.new('RGBA',(2,3),(9,17,21,255)).save(image)
        with sqlite3.connect(self.lib.source) as c:c.execute("UPDATE assets SET src_path=? WHERE uuid='a'",(str(image),))
        cache=self.root/'adapter-cache';cache.mkdir()
        with db.connect(self.lib) as c:
            with sqlite3.connect(cache/'provenance.sqlite') as out:c.backup(out)
        request={'format':'illustash-provenance/1','identity':{'task_id':'synthetic'},'owner_pid':os.getpid(),'library':str(self.lib.path),'cache':str(cache),'mailbox':str(cache),'step':'pixel_hash','only':['a'],'artists':[],'network':False}
        req=cache/'request.json';req.write_text(json.dumps(request));reply=cache/'response.json'
        adapter=Path(__file__).resolve().parents[1]/'provenance/native_worker.py'
        subprocess.run([sys.executable,str(adapter),str(req),str(reply)],check=True,start_new_session=True,env={**self.env,'PATH':os.environ['PATH']})
        self.assertEqual(json.loads((cache/'hashes.json').read_text()),[{'asset':'a','hash':pixel_hash(image)}])
        if os.environ.get('ILLUSTASH_IQDB_BINARY'):
            request['step']='iqdb';req.write_text(json.dumps(request))
            subprocess.run([sys.executable,str(adapter),str(req),str(reply)],check=True,start_new_session=True,env={**self.env,'PATH':os.environ['PATH']})
            rows=json.loads((cache/'hashes.json').read_text());self.assertEqual([r['asset'] for r in rows],['a']);self.assertTrue(rows[0]['hash'].startswith('iqdb_'))
        wrapper=self.root/'owner-worker'
        wrapper.write_text('#!'+sys.executable+'\n'+"import importlib.util,os,subprocess,time\n"+f"spec=importlib.util.spec_from_file_location('adapter',{str(adapter)!r});m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)\n"+"def compute(r):\n p=subprocess.Popen([os.environ['SYNTHETIC_PYTHON'],'-c','import time;time.sleep(300)']);open(os.environ['SYNTHETIC_PID'],'w').write(str(p.pid))\n while True:time.sleep(.1)\nm.execute=compute;m.main()\n")
        wrapper.chmod(0o755);pidfile=self.root/'owner-child'
        self.env.update(ILLUSTASH_PROVENANCE_WORKER=worker_command(wrapper),SYNTHETIC_PYTHON=sys.executable,SYNTHETIC_PID=str(pidfile));self.restart()
        self.post({'action':'artist','artist':'synthetic'});wait_until(pidfile.exists,timeout=5)
        child=int(pidfile.read_text());self.process.kill();self.process.wait(timeout=5)
        wait_until(lambda:not worker_exists(child),timeout=5)
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.info()['state']['status'],'stopped')
        self.assertEqual(self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'"),[(0,)])
