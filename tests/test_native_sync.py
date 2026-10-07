"""M4 sync acceptance: owned synthetic Photos databases, folders and packages only."""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import json
import sqlite3
import subprocess
import os
import unittest
from tests.test_native_folder import NativeFolderHTTPTest
from tests import photos_fixture
from tests.test_native_human_labels import snapshot

class NativeSyncTest(unittest.TestCase):
    setUpClass = classmethod(NativeFolderHTTPTest.setUpClass.__func__)
    package = NativeFolderHTTPTest.package
    start = NativeFolderHTTPTest.start
    stop = NativeFolderHTTPTest.stop
    request = NativeFolderHTTPTest.request
    sync = NativeFolderHTTPTest.sync
    finish = NativeFolderHTTPTest.finish
    _folder_setup = NativeFolderHTTPTest.setUp
    _fixture_setup = NativeFolderHTTPTest._fixture_setup
    photo = photos_fixture.photo

    def setUp(self):
        self._folder_setup()
        self.stop(self.process)
        self.library = self.root / 'synthetic.photoslibrary'
        (self.library / 'database').mkdir(parents=True)
        with sqlite3.connect(self.library / 'database/Photos.sqlite') as c:
            c.execute('CREATE TABLE ZASSET(Z_PK INTEGER PRIMARY KEY,ZUUID TEXT,ZKIND INTEGER,ZTRASHEDSTATE INTEGER,ZWIDTH INTEGER,ZHEIGHT INTEGER,ZADDEDDATE REAL,ZHIDDEN INTEGER,ZDIRECTORY TEXT,ZFILENAME TEXT,ZADDITIONALATTRIBUTES INTEGER)')
            c.execute('CREATE TABLE ZADDITIONALASSETATTRIBUTES(Z_PK INTEGER PRIMARY KEY,ZIMPORTEDBYBUNDLEIDENTIFIER TEXT,ZORIGINALSTABLEHASH TEXT,ZORIGINALFILESIZE INTEGER)')
        from tests.library_fixture import ensure_source_schema
        with sqlite3.connect(self.lib.source) as c:
            ensure_source_schema(c)
            if 'z_pk' not in {row[1] for row in c.execute('PRAGMA table_info(assets)')}:
                c.execute('ALTER TABLE assets ADD COLUMN z_pk INTEGER')
        self.source={'id':'photos','kind':'photos','path':str(self.library),'apps':{'com.atebits.Tweetie2':'x','net.pxv.iphone':'pixiv'}}
        self.lib.config['sources'].append(self.source)
        (self.lib.path/'library.json').write_text(json.dumps(self.lib.config))
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']

    def test_native_library_new_mixed_source_deletion_restoration_paths_and_notifications(self):
        self.photo('D','com.atebits.Tweetie2')
        self.photo('E','net.pxv.iphone')
        self.photo('F','com.apple.springboard')
        with sqlite3.connect(self.lib.source) as c:c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES ('D','rating','general',1)")
        original=self.library/'originals/D/D.png'
        original.parent.mkdir(parents=True)
        original.write_bytes(b'synthetic')
        before=snapshot(self.library/'database/Photos.sqlite')
        state=self.sync()
        self.assertEqual(state['state']['status'],'needs_recognition',state)
        self.assertEqual(state['state']['total'],2) # folder + E; D already recognised
        self.assertEqual(list(state['history'][0]['sources']),['s1','com.atebits.Tweetie2','net.pxv.iphone','com.apple.springboard'])
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute("SELECT uuid,preset FROM assets WHERE source_id='photos' ORDER BY uuid").fetchall(), [('D','x'),('E','pixiv')])
            self.assertEqual(c.execute("SELECT src_kind,src_path FROM assets WHERE uuid='D'").fetchone(), ('orig',str(original)))
            folder=c.execute("SELECT uuid FROM assets WHERE source_id!='photos'").fetchone()[0]
        self.request('/api/tag',{'ids':['D'],'tag':'合成保留'})
        with sqlite3.connect(self.library/'database/Photos.sqlite') as c:
            c.execute("UPDATE ZASSET SET ZTRASHEDSTATE=1 WHERE ZUUID='D'")
        self.assertEqual(self.sync()['state']['removed'],1)
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT trashed FROM assets WHERE uuid=?',(folder,)).fetchone()[0],0)
        with sqlite3.connect(self.library/'database/Photos.sqlite') as c:
            c.execute("UPDATE ZASSET SET ZTRASHEDSTATE=0 WHERE ZUUID='D'")
        self.assertEqual(self.sync()['state']['found'],1)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT tag FROM asset_tags WHERE uuid='D'").fetchall(), [('合成保留',)])
            self.assertGreater(c.execute("SELECT count(*) FROM backend_effects WHERE kind='imported'").fetchone()[0],0)
        self.assertEqual(snapshot(self.library/'database/Photos.sqlite'),before)
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_source_transaction_failure_and_retry(self):
        self.photo('D','com.atebits.Tweetie2')
        with sqlite3.connect(self.lib.source) as c:
            c.execute("CREATE TRIGGER fail_photo BEFORE INSERT ON asset_links WHEN NEW.system='photos' BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        before=snapshot(self.lib.source)
        result=subprocess.run([str(self.binary),'photos-scan','--db',str(self.lib.source)],input=json.dumps(self.source).encode(),capture_output=True)
        self.assertNotEqual(result.returncode,0)
        self.assertEqual(snapshot(self.lib.source),before)
        with sqlite3.connect(self.lib.source) as c: c.execute('DROP TRIGGER fail_photo')
        self.assertEqual(self.sync()['state']['status'],'needs_recognition')

    def test_native_library_new_original_download_refresh_and_existing_results_retry(self):
        self.photo('D','com.atebits.Tweetie2')
        self.sync()
        original=self.library/'originals/D/D.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        with sqlite3.connect(self.lib.source) as c: c.execute("INSERT INTO labels(uuid,dim,value,conf) VALUES ('D','rating','general',1)")
        with sqlite3.connect(self.lib.db) as c:
            c.execute("CREATE TRIGGER fail_app BEFORE INSERT ON assets WHEN NEW.uuid='D' BEGIN SELECT RAISE(ABORT,'synthetic'); END")
        self.assertEqual(self.sync()['state']['status'],'error')
        with sqlite3.connect(self.lib.db) as c: c.execute('DROP TRIGGER fail_app')
        self.assertEqual(self.sync()['state']['status'],'needs_recognition')
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT rating FROM asset_rating WHERE uuid='D'").fetchall(), [('general',)])
            self.assertEqual(c.execute("SELECT src_path FROM assets WHERE uuid='D'").fetchone()[0],str(original))

    def worker(self, mode='ok', per_run=0):
        import hashlib, sys
        self.stop(self.process)
        (self.resources/'models/ccip').mkdir(parents=True,exist_ok=True)
        (self.resources/'models/wd_swinv2_v3_emb.onnx').write_bytes(b'synthetic WD')
        (self.resources/'models/ccip/model_feat.onnx').write_bytes(b'synthetic CCIP')
        (self.resources/'tags/tags_v3.csv').write_text('name,category\ngeneral,9\nblue_hair,0\nalice,4\n')
        specs={
            'wd': dict(version='swinv2_448',dimension=1024,sha256=hashlib.sha256(b'synthetic WD').hexdigest(),tags_sha256=hashlib.sha256((self.resources/'tags/tags_v3.csv').read_bytes()).hexdigest()),
            'ccip':dict(version='ccip-model_feat',dimension=768,sha256=hashlib.sha256(b'synthetic CCIP').hexdigest())}
        with sqlite3.connect(self.lib.source) as c:
            columns = {row[1] for row in c.execute('PRAGMA table_info(labels)')}
            for col in ('source TEXT DEFAULT "legacy"','model_ver TEXT','run_id TEXT'):
                if col.split()[0] not in columns:
                    c.execute('ALTER TABLE labels ADD COLUMN '+col)
        script=self.root/'synthetic-worker'
        script.write_text('#!'+sys.executable+'\n'+r"""
import hashlib,json,sys,time,os
from pathlib import Path
import numpy as np
r=json.loads(Path(sys.argv[1]).read_text())
mode=MODE
if mode=='hang':
    (Path(r['output'])/'pid').write_text(str(os.getpid()))
    time.sleep(60)
if mode=='fail': sys.exit(7)
if mode=='progress':
    # A stale task's value first (ignored), then this task's; hold until the test releases it.
    prog=Path(os.environ['ILLUSTASH_RECOGNITION_PROGRESS'])
    def report(stage,processed,failed=0,task=r['identity']['task_id']):
        tmp=prog.with_suffix('.tmp')
        tmp.write_text(json.dumps(dict(format='illustash-recognition-progress/1',task_id=task,stage=stage,total=len(r['assets']),processed=processed,failed=failed)))
        tmp.replace(prog)
    report('recognising',len(r['assets']),task='stale-task');time.sleep(.5)
    report('preparing',0);time.sleep(.5)
    report('recognising',1);time.sleep(.5)
    report('recognising',2)
    release=Path(sys.argv[0]).parent/'release'
    for _ in range(1000):
        if release.exists(): break
        time.sleep(.01)
(Path(sys.argv[0]).parent/'last-request').write_text(json.dumps([a['id'] for a in r['assets']]))
if mode in ('stream','stream-hang'):
    # One complete batch per asset, each marked complete only after its files exist.
    specs=SPECS
    for i,a in enumerate(r['assets']):
        if mode=='stream-hang' and i==2:
            time.sleep(60)
        dest=Path(r['output'])/f'batch-{i}';dest.mkdir()
        (dest/'labels.jsonl').write_text(json.dumps(dict(asset=a['id'],dim='rating',value='general',confidence=.9)))
        for model in ('wd','ccip'): np.save(dest/(model+'.npy'),np.ones((1,specs[model]['dimension']),dtype='<f4'))
        m=dict(format='illustash-recognition/1',library_id=r['identity']['library_id'],assets=[a['id']],models=specs,files={f:dict(bytes=(dest/f).stat().st_size,sha256=hashlib.sha256((dest/f).read_bytes()).hexdigest()) for f in ('labels.jsonl','wd.npy','ccip.npy')})
        (dest/'manifest.json').write_text(json.dumps(m))
        (dest/'complete.json').write_text(json.dumps(dict(format='illustash-recognition-batch/1',task_id=r['identity']['task_id'],assets=[a['id']],manifest_sha256=hashlib.sha256((dest/'manifest.json').read_bytes()).hexdigest(),sidecar_sha256=None)))
        time.sleep(.6)
    Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=[str(Path(r['output'])/f'batch-{i}') for i in range(len(r['assets']))],failed=[])))
    sys.exit(0)
if mode=='slow':
    # Steady progress, each step well inside the idle window, the whole run longer than it.
    prog=Path(os.environ['ILLUSTASH_RECOGNITION_PROGRESS'])
    for stage,processed in [('preparing',0)]+[('recognising',i) for i in range(len(r['assets'])+1)]:
        tmp=prog.with_suffix('.tmp')
        tmp.write_text(json.dumps(dict(format='illustash-recognition-progress/1',task_id=r['identity']['task_id'],stage=stage,total=len(r['assets']),processed=processed,failed=0)))
        tmp.replace(prog);time.sleep(.4)
p=Path(r['output'])/'result';p.mkdir()
ids=[a['id'] for a in r['assets']]
(p/'labels.jsonl').write_text('\n'.join(json.dumps(dict(asset=u,dim='rating',value='general',confidence=.9)) for u in ids))
specs=SPECS
for model in ('wd','ccip'): np.save(p/(model+'.npy'),np.ones((len(ids),specs[model]['dimension']),dtype='<f4'))
m=dict(format='illustash-recognition/1',library_id=r['identity']['library_id'],assets=ids,models=specs,files={f:dict(bytes=(p/f).stat().st_size,sha256=hashlib.sha256((p/f).read_bytes()).hexdigest()) for f in ('labels.jsonl','wd.npy','ccip.npy')})
(p/'manifest.json').write_text(json.dumps(m))
if mode=='wrong':r['identity']['generation']+=1
packages=[str(p)]
if mode=='splitbad' and len(ids)>1:
    packages=[]
    for i,u in enumerate(ids):
        dest=Path(r['output'])/('batch'+str(i));dest.mkdir()
        (dest/'labels.jsonl').write_text(json.dumps(dict(asset=u,dim='rating',value='general',confidence=.9)))
        for model in ('wd','ccip'):np.save(dest/(model+'.npy'),np.full((1,specs[model]['dimension']),0 if i==1 else 1,dtype='<f4'))
        manifest=dict(m,assets=[u],files={f:dict(bytes=(dest/f).stat().st_size,sha256=hashlib.sha256((dest/f).read_bytes()).hexdigest()) for f in ('labels.jsonl','wd.npy','ccip.npy')})
        (dest/'manifest.json').write_text(json.dumps(manifest));packages.append(str(dest))
Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=packages,failed=[])))
""".replace('MODE',repr(mode)).replace('SPECS',repr(specs)))
        script.chmod(0o700)
        self.env['ILLUSTASH_RECOGNITION_WORKER']=worker_command(script)
        self.process,self.log=self.start(self.lib.path,extra=('--sync-limit',str(per_run)))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']

    def finish(self):
        import time
        for _ in range(300):
            response=self.request('/api/sync')
            self.assertEqual(response['status'],200)
            value=json.loads(response['body'])
            if value['state']['status'] not in ('scanning','importing','recognising'): return value
            time.sleep(.02)
        self.fail('sync did not finish')

    def test_native_library_new_worker_per_run_failure_retry_and_identity(self):
        self.worker(per_run=1)
        state=self.sync()
        self.assertEqual(state['state']['status'],'done',state)
        self.assertEqual((state['state']['total'],state['state']['done']),(1,1))
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],1)
        self.assertEqual(self.sync()['state']['status'],'done')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],1)
        # New source work, then fail the process. No partial formal recognition.
        self.photo('D','com.atebits.Tweetie2')
        original=self.library/'originals/D/D.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        script=self.root/'synthetic-worker'
        contents=script.read_text();script.write_text(contents.replace("mode='ok'","mode='fail'"))
        self.assertEqual(self.sync()['state']['status'],'error')
        script.write_text(contents.replace("mode='ok'","mode='wrong'"))
        self.assertEqual(self.sync()['state']['status'],'error')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_assets WHERE asset='D'").fetchone()[0],0)
        script.write_text(contents)
        self.assertEqual(self.sync()['state']['status'],'done')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_assets WHERE asset='D'").fetchone()[0],1)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute("SELECT rating FROM asset_rating WHERE uuid='D'").fetchall(),[('general',)])
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_worker_progress_before_exit(self):
        import time
        for uuid in ('E','F'):
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        self.worker('progress')
        self.assertEqual(self.request('/api/sync',{})['body'],b'{"started": true}')
        seen=[]
        for _ in range(1000):
            s=json.loads(self.request('/api/sync')['body'])['state']
            seen.append({k:s.get(k) for k in ('status','stage','done','total','failed')})
            if s['status']=='recognising' and s.get('stage')=='recognising' and s.get('done')==2: break
            time.sleep(.01)
        else:
            self.fail(seen[-5:])
        total=seen[-1]['total']
        self.assertGreaterEqual(total,3)
        self.assertEqual(seen[-1]['failed'],0)
        recognising=[v for v in seen if v['status']=='recognising']
        # Only this task's moving count reaches the panel; the stale full count never does.
        self.assertIn('preparing',[v['stage'] for v in recognising])
        self.assertTrue(all(v['done'] in (0,1,2) for v in recognising),recognising)
        self.assertFalse((self.root/'release').exists())
        (self.root/'release').touch()
        value=self.finish()['state']
        self.assertEqual(value['status'],'done',value)
        self.assertEqual((value['done'],value['total'],value['imported'],value['import_done'],value['import_total']),
                         (total,total,total,total,total))

    def test_native_library_new_worker_deadline_follows_progress_and_reports_why(self):
        for uuid in ('E','F'):
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        # The worker runs about 2 s, longer than the 0.8 s idle window, but never 0.8 s without progress.
        self.env['ILLUSTASH_RECOGNITION_TIMEOUT_MS']='800'
        self.worker('slow')
        value=self.sync()
        self.assertEqual(value['state']['status'],'done',value)
        self.assertEqual(value['state']['done'],value['state']['total'])
        # The same steady worker is stopped by the total limit, with that reason kept.
        with sqlite3.connect(self.lib.source) as c: c.execute("DELETE FROM labels WHERE uuid IN ('E','F')")
        self.env.update(ILLUSTASH_RECOGNITION_TIMEOUT_MS='60000',ILLUSTASH_RECOGNITION_MAX_MS='1000')
        self.worker('slow')
        value=self.sync()
        self.assertEqual(value['state']['status'],'interrupted',value)
        self.assertEqual(value['state']['message'],'任务超时：超过 1 秒上限')
        self.assertEqual((value['history'][0]['status'],value['history'][0]['message']),('interrupted','任务超时：超过 1 秒上限'))
        self.env.pop('ILLUSTASH_RECOGNITION_MAX_MS')

    def test_native_library_new_finished_batches_survive_interruption(self):
        for uuid in ('E','F'):
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        # Two batches finish, then the worker stalls past the idle window.
        self.env['ILLUSTASH_RECOGNITION_TIMEOUT_MS']='2000'
        self.worker('stream-hang')
        value=self.sync()
        self.assertEqual(value['state']['status'],'interrupted',value)
        self.assertEqual(value['state']['message'],'任务超时：2 秒没有进展')
        first=json.loads((self.root/'last-request').read_text())
        self.assertEqual(len(first),3)
        # The folder asset is counted when the scan imports it; a streamed batch counts only the
        # Photos assets it brings. Which assets the first two batches hold follows key order.
        self.assertEqual((value['history'][0]['status'],value['history'][0]['imported']),
                         ('interrupted',1+sum(u in ('E','F') for u in first[:2])))
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(sorted(u for u, in c.execute('SELECT asset FROM recognition_assets')),sorted(first[:2]))
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_imports WHERE status='done'").fetchone()[0],2)
        # The next sync recognises only the asset left, and the run completes.
        self.env.pop('ILLUSTASH_RECOGNITION_TIMEOUT_MS')
        self.worker('stream')
        value=self.sync()
        self.assertEqual(value['state']['status'],'done',value)
        self.assertEqual(json.loads((self.root/'last-request').read_text()),first[2:])
        self.assertEqual((value['state']['imported'],value['state']['import_done'],value['state']['import_total']),(1,1,1))
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],3)

    def test_native_library_new_worker_timeout_and_switch_cancel(self):
        self.env['ILLUSTASH_RECOGNITION_TIMEOUT_MS']='200'
        self.worker('hang')
        value=self.sync()
        self.assertEqual(value['state']['status'],'interrupted')
        self.assertEqual(value['state']['message'],'任务超时：200 毫秒没有进展')
        self.env.pop('ILLUSTASH_RECOGNITION_TIMEOUT_MS')
        self.stop(self.process)
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/sync',{})['status'],200)
        import time
        for _ in range(100):
            if json.loads(self.request('/api/sync')['body'])['state']['status']=='recognising':break
            time.sleep(.01)
        self.assertEqual(self.request('/api/sync',{})['body'],b'{"started": false}')
        other,_=self.package('other')
        other.config['id']=self.lib.config['id']
        (other.path/'library.json').write_text(json.dumps(other.config))
        self.assertEqual(self.request('/api/libraries/open',{'path':str(other.path)})['status'],200)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/sync')['body'])['state']['status'],'off')
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM sync_runs ORDER BY id DESC LIMIT 1').fetchone()[0],'interrupted')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM sqlite_master WHERE name="recognition_assets"').fetchone()[0],0)

    def test_native_library_new_startup_preferences_off_freeze_and_limits(self):
        self.stop(self.process)
        self.process,self.log=self.start(self.lib.path,extra=('--auto-sync',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.finish()['state']['status'],'needs_recognition')
        with sqlite3.connect(self.lib.db) as c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('startup',?)",(json.dumps({"sync":False}),))
        self.stop(self.process)
        self.process,self.log=self.start(self.lib.path,extra=('--auto-sync',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/sync')['body'])['state']['status'],'off')
        self.assertEqual(self.request('/api/sync',{})['status'],400)
        self.stop(self.process)
        self.process,self.log=self.start(self.lib.path,extra=('--no-sync',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/sync',{})['status'],400)
        self.assertEqual(json.loads(self.request('/api/sync')['body'])['state']['status'],'off')
        self.assertIn('sync',json.loads(self.request('/api/settings')['body'])['forced_off'])
        self.stop(self.process)
        self.lib.config['test']={'synthetic':True}
        (self.lib.path/'library.json').write_text(json.dumps(self.lib.config))
        self.process,self.log=self.start(self.lib.path,extra=('--auto-sync',))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/sync',{})['status'],400)
        self.stop(self.process)
        self.lib.config.pop('test')
        with sqlite3.connect(self.lib.db) as c:c.execute("INSERT OR REPLACE INTO prefs VALUES ('startup',?)",(json.dumps({'sync':True}),))
        (self.lib.path/'library.json').write_text(json.dumps(self.lib.config))
        for i in range(101):self.photo('S'+str(i),'com.atebits.Tweetie2')
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/startup',{'local_limit':100})['status'],200)
        state=self.sync()
        self.assertEqual(state['state']['status'],'needs_windows')
        self.assertEqual(state['state']['total'],102)
        self.assertEqual(state['history'][0]['status'],'needs_windows')

    @unittest.skipIf(os.name == 'nt', 'Apple 照片图库只在 macOS 上提供')
    def test_native_library_new_photos_exact_source_reference(self):
        # The former Python Photos scanner's results on the same inputs are recorded (tests/reference_replay.py).
        for u,app in (('D','com.atebits.Tweetie2'),('E','net.pxv.iphone'),('F','com.apple.springboard')): self.photo(u,app)
        for i,app in enumerate(('com.apple.camera','com.apple.mobileslideshow','com.apple.mobilesafari','tv.danmaku.bilianime','ai.x.GrokApp','com.tencent.xin','com.baidu.netdisk','app.nicegram','')):self.photo('N'+str(i),app)
        def pair():
            old=self.root/'reference-source.sqlite'
            with sqlite3.connect(self.lib.source) as src,sqlite3.connect(old) as dest:src.backup(dest)
            result=subprocess.run([str(self.binary),'photos-scan','--db',str(self.lib.source)],input=json.dumps(self.source).encode(),capture_output=True,env=self.env)
            self.assertEqual(result.returncode,0,result.stderr)
            expected=self.rec.value('old scan')
            self.assertEqual(json.loads(result.stdout),expected['result'])
            self.assertEqual(snapshot(self.lib.source),expected['source'])
        pair()
        with sqlite3.connect(self.library/'database/Photos.sqlite') as c:c.execute("UPDATE ZASSET SET ZTRASHEDSTATE=1 WHERE ZUUID='D'")
        pair()
        with sqlite3.connect(self.library/'database/Photos.sqlite') as c:c.execute("UPDATE ZASSET SET ZTRASHEDSTATE=0 WHERE ZUUID='D'")
        original=self.library/'originals/E/E.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        pair()

    def test_native_library_new_unreadable_counts_and_retry(self):
        self.photo('E','net.pxv.iphone')
        self.worker()
        state=self.sync()
        self.assertEqual(state['state']['status'],'done',state)
        self.assertEqual((state['state']['total'],state['state']['done'],state['state']['failed'],state['state']['imported']),(2,2,1,1))
        self.assertIn('iCloud',state['state']['message'])
        original=self.library/'originals/E/E.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        state=self.sync()
        self.assertEqual((state['state']['status'],state['state']['failed'],state['state']['imported']),('done',0,1))
        with sqlite3.connect(self.lib.source) as c:self.assertEqual(c.execute("SELECT count(*) FROM recognition_assets WHERE asset='E'").fetchone()[0],1)

    def test_native_library_new_batch_commit_then_failure_recovers_once(self):
        self.photo('D','com.atebits.Tweetie2')
        original=self.library/'originals/D/D.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        self.worker('splitbad')
        state=self.sync()
        self.assertEqual(state['state']['status'],'error',state)
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],1)
            first=c.execute('SELECT asset FROM recognition_assets').fetchone()[0]
        with sqlite3.connect(self.lib.db) as c:
            before=c.execute('SELECT count(*) FROM term_evidence').fetchone()[0]
            self.assertEqual(c.execute('SELECT rating FROM asset_rating WHERE uuid=?',(first,)).fetchall(),[('general',)])
        script=self.root/'synthetic-worker';script.write_text(script.read_text().replace("mode='splitbad'","mode='ok'"))
        self.assertEqual(self.sync()['state']['status'],'done')
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0],2)
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_imports').fetchone()[0],2)
        self.assertEqual(self.sync()['state']['status'],'done')
        with sqlite3.connect(self.lib.source) as c:self.assertEqual(c.execute('SELECT count(*) FROM recognition_imports').fetchone()[0],2)

    def test_native_library_new_multiple_photos_statistics_accumulate(self):
        self.photo('D','com.atebits.Tweetie2')
        self.stop(self.process)
        second=self.root/'second.photoslibrary';(second/'database').mkdir(parents=True)
        with sqlite3.connect(self.library/'database/Photos.sqlite') as src,sqlite3.connect(second/'database/Photos.sqlite') as dest:src.backup(dest)
        with sqlite3.connect(second/'database/Photos.sqlite') as c:c.execute("UPDATE ZASSET SET ZUUID='G'")
        self.lib.config['sources'].append(dict(self.source,id='second-photos',path=str(second)))
        (self.lib.path/'library.json').write_text(json.dumps(self.lib.config))
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        state=self.sync()
        self.assertEqual(state['history'][0]['sources']['com.atebits.Tweetie2'],{'name':'X','photos':2,'collected':2})
        with sqlite3.connect(self.lib.source) as c:self.assertEqual(c.execute("SELECT uuid,source_id FROM assets WHERE source_id LIKE '%photos' ORDER BY uuid").fetchall(),[('D','photos'),('G','second-photos')])

del NativeFolderHTTPTest
