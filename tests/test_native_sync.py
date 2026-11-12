"""M4 同步验收：只用测试自己的合成照片数据库、文件夹和图库包。"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import json
import sqlite3
import subprocess
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from tests.test_native_folder import NativeFolderHTTPTest
from tests import photos_fixture
from tests.test_native_human_labels import snapshot
from tests.test_read_http import wait_until

# 给那些 worker 不能被当成卡住的测试用的空闲窗口：远长于繁忙机器上 worker 的启动时间（#219）。
# 产品默认值是 10 分钟，保持不变。
WIDE_IDLE_MS=8000
# 宿主显示上一条报告之后，限速的 worker 等多久再发下一条。
PACE_S=.8

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
        self.assertEqual(state['state']['total'],2) # 文件夹 + E；D 已经识别过
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
r=json.loads(Path(sys.argv[1]).read_text())
mode=MODE
def paced_report(stage,processed):
    prog=Path(os.environ['ILLUSTASH_RECOGNITION_PROGRESS']);tmp=prog.with_suffix('.tmp')
    tmp.write_text(json.dumps(dict(format='illustash-recognition-progress/1',task_id=r['identity']['task_id'],stage=stage,total=len(r['assets']),processed=processed,failed=0)))
    tmp.replace(prog)
def wait_for(name):
    # Until the test lets the worker go on; a forgotten worker gives up instead of holding the run forever.
    go=Path(sys.argv[0]).parent/name;until=time.monotonic()+120
    while not go.exists():
        if time.monotonic()>until: sys.exit(8)
        time.sleep(.01)
# The first report goes out before NumPy loads: on a busy machine that import alone takes seconds.
if mode=='paced': paced_report('preparing',0)
import numpy as np
if mode=='hang':
    # Written whole, then renamed: a test that sees the file reads the full pid (#132).
    (Path(r['output'])/'pid.tmp').write_text(str(os.getpid()));os.replace(Path(r['output'])/'pid.tmp',Path(r['output'])/'pid')
    time.sleep(60)
if mode=='fail': sys.exit(7)
if mode=='progress':
    # A stale task's value first (ignored), then this task's, each next one once the test has seen the last.
    prog=Path(os.environ['ILLUSTASH_RECOGNITION_PROGRESS'])
    def report(stage,processed,failed=0,task=r['identity']['task_id']):
        tmp=prog.with_suffix('.tmp')
        tmp.write_text(json.dumps(dict(format='illustash-recognition-progress/1',task_id=task,stage=stage,total=len(r['assets']),processed=processed,failed=failed)))
        tmp.replace(prog)
    report('recognising',len(r['assets']),task='stale-task');time.sleep(.5)
    report('preparing',0);wait_for('go-1')
    report('recognising',1);wait_for('go-2')
    report('recognising',2);wait_for('release')
(Path(sys.argv[0]).parent/'last-request').write_text(json.dumps([a['id'] for a in r['assets']]))
failures=[]
if mode in ('classify','classify-legacy'):
    # Stands in for the native producer (#112): a file it cannot read is unreadable; a PNG read in
    # full without its end chunk, or anything else, is undecodable. Older workers give no reason.
    kept=[]
    for a in r['assets']:
        try: b=Path(a['path']).read_bytes()
        except OSError: failures.append(dict(uuid=a['id'],reason='unreadable')); continue
        if b.startswith(b'\x89PNG') and b.endswith(b'IEND\xaeB`\x82'): kept.append(a)
        else: failures.append(dict(uuid=a['id'],reason='undecodable'))
    if mode=='classify-legacy': failures=[dict(uuid=f['uuid']) for f in failures]
    r['assets']=kept
    if not kept:
        Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=[],failed=failures)))
        sys.exit(0)
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
if mode=='paced':
    # Each next report, and then the result, waits for the test to let it go: the test sets the pace (#219).
    for step in range(1,len(r['assets'])+2):
        wait_for(f'step-{step}')
        if step<=len(r['assets']): paced_report('recognising',step)
p=Path(r['output'])/'result';p.mkdir()
ids=[a['id'] for a in r['assets']]
(p/'labels.jsonl').write_text('\n'.join(json.dumps(dict(asset=u,dim='rating',value='general',confidence=.9)) for u in ids))
specs=SPECS
for model in ('wd','ccip'): np.save(p/(model+'.npy'),np.ones((len(ids),specs[model]['dimension']),dtype='<f4'))
if mode=='content':
    # Vectors from the file's bytes, as a real model: another picture under the same path gives others.
    fills=[[sum(Path(a['path']).read_bytes())%97+1] for a in r['assets']]
    for model in ('wd','ccip'): np.save(p/(model+'.npy'),np.repeat(np.asarray(fills,dtype='<f4'),specs[model]['dimension'],axis=1))
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
if failures: Path(sys.argv[2]).write_text(json.dumps(dict(identity=r['identity'],packages=packages,failed=failures)))
""".replace('MODE',repr(mode)).replace('SPECS',repr(specs)))
        script.chmod(0o700)
        self.env['ILLUSTASH_RECOGNITION_WORKER']=worker_command(script)
        self.process,self.log=self.start(self.lib.path,extra=('--sync-limit',str(per_run)))
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']

    def finish(self):
        import time
        # 比这里最宽的空闲窗口还长：卡住的 worker 由宿主停下，而不是靠这个等待。
        until=time.monotonic()+60
        while time.monotonic()<until:
            response=self.request('/api/sync')
            self.assertEqual(response['status'],200)
            value=json.loads(response['body'])
            if value['state']['status'] not in ('scanning','importing','recognising'): return value
            time.sleep(.02)
        self.fail(f'sync did not finish: {value}')

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
        # 先给 source 加新的活，再让进程失败。不能留下部分完成的正式识别结果。
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
        for uuid in ('E','F'):
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        self.worker('progress')
        self.assertEqual(self.request('/api/sync',{})['body'],b'{"started": true}')
        seen=[]
        def shown(stage,done):
            s=json.loads(self.request('/api/sync')['body'])['state']
            seen.append({k:s.get(k) for k in ('status','stage','done','total','failed')})
            if s['status'] not in ('scanning','importing','recognising'): self.fail(seen[-5:])
            return s['status']=='recognising' and s.get('stage')==stage and s.get('done')==done
        # 测试看到每个值之后 worker 才往下走，不论哪一边跑得多慢（#219）。
        wait_until(lambda: shown('preparing',0),timeout=60);(self.root/'go-1').touch()
        wait_until(lambda: shown('recognising',1),timeout=60);(self.root/'go-2').touch()
        wait_until(lambda: shown('recognising',2),timeout=60)
        total=seen[-1]['total']
        self.assertGreaterEqual(total,3)
        self.assertEqual(seen[-1]['failed'],0)
        recognising=[v for v in seen if v['status']=='recognising']
        # 面板上只出现这个任务不断变化的计数；过期的总数从不出现。
        self.assertIn('preparing',[v['stage'] for v in recognising])
        self.assertTrue(all(v['done'] in (0,1,2) for v in recognising),recognising)
        self.assertFalse((self.root/'release').exists())
        (self.root/'release').touch()
        value=self.finish()['state']
        self.assertEqual(value['status'],'done',value)
        self.assertEqual((value['done'],value['total'],value['imported'],value['import_done'],value['import_total']),
                         (total,total,total,total,total))

    def test_native_library_new_worker_deadline_follows_progress_and_reports_why(self):
        import time
        photos='EFGHIJKLMN'
        for uuid in photos:
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        # worker 每张图报告一次，宿主显示上一条之后，测试稍等片刻再放行下一条，所以这次运行只靠进度就能撑过
        # 空闲窗口。窗口很宽，所以繁忙机器上 worker 启动慢也能及时报告（#219）；时限规则本身在手动拨动的时钟上
        # 做单元测试（app_tasks）。
        self.env['ILLUSTASH_RECOGNITION_TIMEOUT_MS']=str(WIDE_IDLE_MS)
        self.worker('paced')
        self.assertEqual(self.request('/api/sync',{})['body'],b'{"started": true}')
        def shown(stage,done):
            state=json.loads(self.request('/api/sync')['body'])['state']
            if state['status'] not in ('scanning','importing','recognising'): self.fail(state)
            return state['status']=='recognising' and state.get('stage')==stage and state.get('done')==done and state
        total=wait_until(lambda: shown('preparing',0),timeout=60)['total']
        self.assertEqual(total,len(photos)+1)
        first=time.monotonic()
        for step in range(1,total+2):
            time.sleep(PACE_S)
            (self.root/f'step-{step}').touch()
            if step<=total: wait_until(lambda: shown('recognising',step),timeout=60)
        value=self.finish()
        self.assertEqual(value['state']['status'],'done',value)
        self.assertEqual(value['state']['done'],value['state']['total'])
        self.assertGreater(time.monotonic()-first,WIDE_IDLE_MS/1000)
        # 同一个 worker，不放它越过第一条报告，由总时限停下，并保留这个原因。
        for step in self.root.glob('step-*'): step.unlink()
        with sqlite3.connect(self.lib.source) as c: c.execute(f"DELETE FROM labels WHERE uuid IN ({','.join('?'*len(photos))})",tuple(photos))
        self.env.update(ILLUSTASH_RECOGNITION_TIMEOUT_MS='60000',ILLUSTASH_RECOGNITION_MAX_MS='1000')
        self.worker('paced')
        value=self.sync()
        self.assertEqual(value['state']['status'],'interrupted',value)
        self.assertEqual(value['state']['message'],'任务超时：超过 1 秒上限')
        self.assertEqual((value['history'][0]['status'],value['history'][0]['message']),('interrupted','任务超时：超过 1 秒上限'))
        self.env.pop('ILLUSTASH_RECOGNITION_MAX_MS')

    def test_native_library_new_finished_batches_survive_interruption(self):
        for uuid in ('E','F'):
            self.photo(uuid,'com.atebits.Tweetie2')
            original=self.library/f'originals/{uuid}/{uuid}.png';original.parent.mkdir(parents=True);original.write_bytes(b'synthetic')
        # 两批完成后，worker 卡住超过空闲窗口。窗口很宽，
        # 所以繁忙机器上第一批之前加载 NumPy 的时间不会被当成卡住（#219）。
        self.env['ILLUSTASH_RECOGNITION_TIMEOUT_MS']=str(WIDE_IDLE_MS)
        self.worker('stream-hang')
        value=self.sync()
        self.assertEqual(value['state']['status'],'interrupted',value)
        self.assertEqual(value['state']['message'],f'任务超时：{WIDE_IDLE_MS//1000} 秒没有进展')
        first=json.loads((self.root/'last-request').read_text())
        self.assertEqual(len(first),3)
        # 文件夹里的图片在扫描导入时计数；流式送来的一批只计它带来的照片图片。前两批包含哪些图片按键的顺序决定。
        self.assertEqual((value['history'][0]['status'],value['history'][0]['imported']),
                         ('interrupted',1+sum(u in ('E','F') for u in first[:2])))
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(sorted(u for u, in c.execute('SELECT asset FROM recognition_assets')),sorted(first[:2]))
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_imports WHERE status='done'").fetchone()[0],2)
        # 下一次同步只识别剩下的那张图片，运行完成。
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
        other = self.package('other')
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
        # 本地识别数量上限已经去掉（#100）：不论多少新图片，都等本机的 worker 识别。
        self.assertEqual(self.request('/api/startup',{'local_limit':100})['status'],400)
        state=self.sync()
        self.assertEqual(state['state']['status'],'needs_recognition')
        self.assertEqual(state['state']['total'],102)
        self.assertEqual(state['history'][0]['status'],'needs_recognition')

    @unittest.skipIf(os.name == 'nt', 'Apple 照片图库只在 macOS 上提供')
    def test_native_library_new_photos_exact_source_reference(self):
        # 原 Python 照片扫描器在同样输入上的结果已录制（tests/fixed_expectations.py）。
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

    @unittest.skipUnless(shutil.which('sandbox-exec'), 'macOS/Linux only: the Photos permission is a macOS privacy setting, refused here through sandbox-exec')
    def test_native_library_new_photos_permission_refused_is_named(self):
        # macOS 隐私设置以 EPERM 拒绝访问照片数据库，与沙箱拒绝时一样（#202）。
        from tests.test_read_http import SPAWN
        from tools.verify_read_http import enter
        self.photo('D','com.atebits.Tweetie2')
        self.stop(self.process)
        database=os.path.realpath(self.library/'database/Photos.sqlite')
        profile='(version 1)(allow default)(deny file-read* (literal %s))'%json.dumps(database)
        args=[shutil.which('sandbox-exec'),'-p',profile,str(self.binary),'http','--no-auto-sync','--library',str(self.lib.path),'--port',str(self.port),
              '--root',str(self.resources),'--no-names','--no-index-check']
        log=tempfile.TemporaryFile();self.addCleanup(log.close)
        self.process=subprocess.Popen(args,env=self.env,stdout=log,stderr=subprocess.DEVNULL,**SPAWN)
        self.addCleanup(self.stop,self.process)
        enter(self.origin,log,self.process,timeout=5)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        state=self.sync()['state']
        self.assertEqual((state['status'],state['problem']),('error','photos_permission'),state)
        self.assertIn('Photos.sqlite',state['message'])
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(c.execute('SELECT status FROM sync_runs ORDER BY id DESC').fetchone(),('error',))
        # 其他任何失败都不带问题代码：页面按原样显示错误。
        self.stop(self.process)
        self.process,self.log=self.start(self.lib.path)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        with sqlite3.connect(self.library/'database/Photos.sqlite') as c:c.execute('DROP TABLE ZASSET')
        state=self.sync()['state']
        self.assertEqual(state['status'],'error',state)
        self.assertNotIn('problem',state)

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

    def hold_unreadable(self, path):
        """让一个已有文件读不了，同时元数据保持原样：用 POSIX 权限位，在 Windows 上则用一个不共享任何访问的
        打开句柄。返回解除用的函数。"""
        if os.name != 'nt':
            path.chmod(0)
            release = lambda: path.chmod(0o644)
        else:
            import ctypes
            from ctypes import wintypes
            kernel = ctypes.WinDLL('kernel32', use_last_error=True)
            kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID,
                                           wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
            kernel.CreateFileW.restype = wintypes.HANDLE
            kernel.CloseHandle.argtypes = [wintypes.HANDLE]
            handle = kernel.CreateFileW(str(path), 0x80000000, 0, None, 3, 0, None)  # GENERIC_READ, no sharing, OPEN_EXISTING
            if handle in (None, wintypes.HANDLE(-1).value):
                raise ctypes.WinError(ctypes.get_last_error())
            release = lambda: kernel.CloseHandle(handle)
        held = [True]
        def done():
            if held.pop() if held else False:
                release()
        self.addCleanup(done)
        try:
            path.read_bytes()
            done()
            self.skipTest('this account reads files whatever their permissions')
        except OSError:
            pass
        return done

    def test_native_library_new_changed_picture_is_recognised_again_with_its_new_result(self):
        # 同名替换的图片会得到别的向量；同一张图片换一个执行后端也是如此（#270）。旧的识别记录随它的标签一起去掉，
        # 所以导入取新的结果。
        from tests.folder_fixture import image, settle
        folder = self.new_folder
        image(folder / 'changing.png', 'red')
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        self.worker('content')
        def run():
            self.assertEqual(self.request('/api/sync', {})['status'], 200)
            value = self.finish()
            self.assertEqual(value['state']['status'], 'done', value)
            with sqlite3.connect(self.lib.source) as c:
                return c.execute("SELECT e.vec FROM embeddings e JOIN assets a ON a.uuid=e.uuid "
                                 "WHERE a.filename='changing.png' AND e.model='ccip'").fetchone()[0]
        settle(folder)
        before = run()
        image(folder / 'changing.png', 'blue')
        settle(folder)
        after = run()
        self.assertNotEqual(after, before)
        with sqlite3.connect(self.lib.source) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_assets r JOIN assets a ON a.uuid=r.asset "
                                       "WHERE a.filename='changing.png'").fetchone()[0], 1)
        self.assertEqual(run(), after, 'unchanged: nothing is recognised again')

    def test_native_library_new_damaged_pictures_wait_for_a_change_unreadable_ones_retry(self):
        # #112：读不了的图片每次同步都重试；完整读出来却不是有效图片的，在文件改动之前一直跳过；两者分开计数。
        from tests.folder_fixture import image, settle
        folder = self.new_folder
        image(folder / 'good.png', 'red'); image(folder / 'locked.png', 'blue'); image(folder / 'bad.png', 'green', (41, 30))
        whole = (folder / 'bad.png').read_bytes(); (folder / 'bad.png').write_bytes(whole[:len(whole) // 2])
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        with sqlite3.connect(self.lib.db) as c:
            ids = dict(c.execute("SELECT filename,uuid FROM assets WHERE filename LIKE '%.png'").fetchall())
        self.assertEqual(set(ids), {'not-scanned.png', 'good.png', 'locked.png', 'bad.png'})
        for name in ('good.png', 'bad.png'):
            self.assertEqual(self.request('/api/tag', {'ids': [ids[name]], 'tag': '合成人工'})['status'], 200)
        with sqlite3.connect(self.lib.db) as c:
            tags = c.execute('SELECT uuid,tag FROM asset_tags ORDER BY uuid,tag').fetchall()
        self.worker('classify')
        request = self.root / 'last-request'
        def run():
            request.unlink(missing_ok=True)
            response = self.request('/api/sync', {})
            self.assertEqual(response['status'], 200, response['body'])
            value = self.finish()
            self.assertEqual(value['state']['status'], 'done', value)
            sent = set(json.loads(request.read_text())) if request.exists() else set()
            return value, {name for name, u in ids.items() if u in sent}
        settle(folder)
        release = self.hold_unreadable(folder / 'locked.png')
        value, sent = run()
        state = value['state']
        self.assertEqual(sent, {'not-scanned.png', 'good.png', 'bad.png'})
        self.assertEqual((state['failed'], state['damaged'], state['imported']), (1, 2, 1), state)
        self.assertEqual(state['message'], '1 张暂时读不到（可能还在 iCloud），下次同步重试；'
                                           '2 张图片已损坏或格式不支持，留在图库里但不识别；文件改动后会重新识别')
        self.assertEqual(value['damaged']['count'], 2)
        self.assertEqual(sorted(Path(p).name for p in value['damaged']['paths']), ['bad.png', 'not-scanned.png'])
        self.assertEqual(value['history'][0]['failed'], 1)
        # 再来一次：损坏的图片不送出，读不了的那张再试一次。
        value, sent = run()
        self.assertEqual(sent, set())
        self.assertEqual((value['state']['failed'], value['state']['damaged']), (1, 2), value['state'])
        release()
        value, sent = run()
        self.assertEqual(sent, {'locked.png'})
        self.assertEqual((value['state']['failed'], value['state']['damaged'], value['state']['imported']), (0, 2, 1), value['state'])
        self.assertEqual(value['state']['message'], '2 张图片已损坏或格式不支持，留在图库里但不识别；文件改动后会重新识别')
        # 损坏的文件换成一张有效图片：下一次同步识别它，UUID 不变。
        image(folder / 'bad.png', 'yellow', (51, 31))
        settle(folder)
        value, sent = run()
        self.assertEqual(sent, {'bad.png'})
        self.assertEqual((value['state']['failed'], value['state']['damaged'], value['state']['imported']), (0, 1, 1), value['state'])
        self.assertEqual([Path(p).name for p in value['damaged']['paths']], ['not-scanned.png'])
        (folder / 'not-scanned.png').unlink()
        value, sent = run()
        self.assertEqual(sent, set())
        self.assertNotIn('damaged', value['state']); self.assertNotIn('damaged', value)
        with sqlite3.connect(self.lib.db) as c:
            self.assertEqual(dict(c.execute("SELECT filename,uuid FROM assets WHERE filename LIKE '%.png'").fetchall()),
                             {k: v for k, v in ids.items() if k != 'not-scanned.png'})
            self.assertEqual(c.execute('SELECT uuid,tag FROM asset_tags ORDER BY uuid,tag').fetchall(), tags)
            self.assertIsNone(c.execute("SELECT value FROM prefs WHERE key='sync_damaged'").fetchone())
        with sqlite3.connect(self.lib.source) as c:
            done = {u for u, in c.execute('SELECT asset FROM recognition_assets')}
        self.assertEqual(done & set(ids.values()), {ids[n] for n in ('good.png', 'locked.png', 'bad.png')})
        self.assertFalse(self.sentinel.exists())

    def test_native_library_new_worker_without_failure_reasons_keeps_retrying(self):
        # #112 之前的 worker 不给原因：所有失败都照旧可以重试。
        from tests.folder_fixture import image
        image(self.new_folder / 'good.png', 'red')
        self.worker('classify-legacy')
        for _ in range(2):
            (self.root / 'last-request').unlink(missing_ok=True)
            value = self.sync()
            self.assertEqual((value['state']['status'], value['state']['failed']), ('done', 1), value)
            self.assertNotIn('damaged', value['state']); self.assertNotIn('damaged', value)
            with sqlite3.connect(self.lib.db) as c:
                text = c.execute("SELECT uuid FROM assets WHERE filename='not-scanned.png'").fetchone()[0]
            self.assertIn(text, json.loads((self.root / 'last-request').read_text()))
            self.assertEqual(value['state']['message'], '1 张暂时读不到（可能还在 iCloud），下次同步重试')

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
