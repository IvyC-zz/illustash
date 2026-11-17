"""#51 验收：只用新建的合成图库包、计算替身和回环地址上的查询。"""
from tests.native_worker_fixture import command as worker_command, source as worker_source, exists as worker_exists

import contextlib
from contextlib import closing
import http.server
import json
import os
from pathlib import Path
import shutil
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
from tests.fixed_expectations import unclocked
from tests.test_read_http import SPAWN, wait_until
from tools.verify_read_http import enter, fetch
from tests import provenance_fixture as db
from tools.wait_limit import WAIT


def store_credential(support, kind, value):
    """合成的账号凭据，格式与宿主在本平台上读取的一致。

    Windows 从不读明文文件：凭据是绑定到账号类型的 DPAPI blob（crates/platform/src/app_dpapi.rs），
    这里用同样的方式写入。
    """
    directory = support / 'accounts'
    directory.mkdir(parents=True, exist_ok=True)
    if os.name != 'nt':
        (directory / kind).write_text(value)
        return
    import ctypes
    from ctypes import wintypes

    class Blob(ctypes.Structure):
        _fields_ = [('len', wintypes.DWORD), ('data', ctypes.POINTER(ctypes.c_char))]

    def blob(data):
        buffer = ctypes.create_string_buffer(data, len(data))
        return Blob(len(data), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_char))), buffer

    (plain, plain_buffer), (purpose, purpose_buffer) = blob(value.encode()), blob(f'Illustash/accounts/v1/{kind}'.encode())
    out = Blob()
    if not ctypes.windll.crypt32.CryptProtectData(ctypes.byref(plain), None, ctypes.byref(purpose), None, None, 1, ctypes.byref(out)):
        raise ctypes.WinError()
    try:
        (directory / f'{kind}.dpapi').write_bytes(ctypes.string_at(out.data, out.len))
    finally:
        ctypes.windll.kernel32.LocalFree(out.data)


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
  # Written whole, then renamed: a test that sees the file reads the full pid (#132).
  Path(os.environ['SYNTHETIC_PID']+'.tmp').write_text(str(p.pid));os.replace(os.environ['SYNTHETIC_PID']+'.tmp',os.environ['SYNTHETIC_PID'])
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
 urls=(
  'https://danbooru.donmai.us/posts.json?tags=synthetic',
  'https://cdn.donmai.us/synthetic-danbooru.jpg',
  'https://www.pixiv.net/ajax/user/12/profile/all',
  'https://i.pximg.net/synthetic-pixiv.jpg',
 )
 response=box/'net-response.json'
 for url in urls:
  req={'identity':r['identity'],'url':url}
  pending=box/'net-request.tmp'
  pending.write_text(json.dumps(req));pending.replace(box/'net-request.json')
  while not response.exists():time.sleep(.01)
  reply=json.loads(response.read_text())
  if reply.get('status')!=200:raise RuntimeError(reply)
  response.unlink()
 result={'asked':len(urls),'stopped':''}
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
        enter(self.origin,log,process)
        return process,log
    def setUp(self):
        lifecycle.NativeLifecycleTest.setUp(self)
        self.worker=self.root/'worker';self.worker.write_text('#!'+sys.executable+'\n'+WORKER);self.worker.chmod(0o755)
        self.calls=[];self.referers=[];self.code=200;self.bad=False;self.pixiv_users=False;self.pixiv_code=None
        outer=self
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                outer.calls.append(self.path)
                outer.referers.append((self.path, self.headers.get_all('Referer')))
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
        # 只有超时用例依赖空闲时限；负载高时（并行跑套件、Windows #56）光是 worker 启动就可能超过 2 秒，
        # 所以其余用例用一个永远不会让运行结束的时限。
        self.env.update(ILLUSTASH_PROVENANCE_WORKER=worker_command(self.worker),ILLUSTASH_PROVENANCE_BASE=f'http://127.0.0.1:{self.service.server_port}',ILLUSTASH_PROVENANCE_IDLE_SECONDS='30')
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
        # 按结束等待时的状态判断：排队的运行可能在第二次读取之前就开始（#56）。
        seen=[]
        def settled():
            seen[:]=[self.info()];return seen[0]['state']['status'] not in ('running','stopping')
        wait_until(settled)
        value=seen[0]
        if status:self.assertEqual(value['state']['status'],status,value)
        return value
    def post(self,body,code=200):
        r=self.request('/api/provenance',body);self.assertEqual(r['status'],code,r);return json.loads(r['body'])
    def sql(self,sql,args=(),cache=False):
        with closing(sqlite3.connect(self.lib.path/('provenance.sqlite' if cache else 'library.sqlite'))) as c:return c.execute(sql,args).fetchall()

    def test_direct_query_referer_is_only_sent_to_pixiv_image_host(self):
        self.env['SYNTHETIC_MODE']='query';self.restart()
        self.post({'action':'artist','artist':'synthetic'})
        wait_until(lambda: len(self.referers) >= 4)
        self.assertEqual(self.referers, [
            ('/posts.json?tags=synthetic', None),
            ('/synthetic-danbooru.jpg', None),
            ('/ajax/user/12/profile/all', None),
            ('/synthetic-pixiv.jpg', ['https://www.pixiv.net/']),
        ])

    def test_idle_success_bytes_and_all_action_errors(self):
        self.assertEqual(self.request('/api/provenance')['body'],json.dumps({'available':True,'state':{'status':'idle'},'runs':[],'coverage':None,'auto':True,'budgets':{'full':1024,'new':50,'pick':20,'artist':200},'frozen':False,'auto_network':False,'delivery':None},ensure_ascii=False).encode())
        for body in ({},{'action':'bad'},{'action':'lookup','ids':[]},{'action':'lookup','ids':[1]},{'action':'lookup','ids':['a']*501},{'action':'artist','artist':''},{'action':'artist','artist':True}):self.post(body,400)
        self.post({'action':'stop'})
    def test_optional_coordinator_keeps_ordinary_cache_schema_and_minimal_packages(self):
        self.env.pop('ILLUSTASH_PROVENANCE_WORKER');self.restart()
        other = self.package('minimal-without-sync')
        with sqlite3.connect(other.db) as c:c.execute('DROP TABLE sync_runs')
        self.assertEqual(self.request('/api/libraries/open',{'path':str(other.path)})['status'],200)
        self.lib=other;self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.info()['state']['status'],'idle');self.assertFalse(db.path_of(other).exists())
        with db.connect(other) as c:before=list(c.iterdump())
        self.assertEqual(self.info()['runs'],[])
        time.sleep(1.1) # 没有任务意图时，定期恢复也保持只读
        with db.connect(other) as c:self.assertEqual(before,list(c.iterdump()))
        self.assertEqual(self.request('/api/assets')['status'],200)
        self.post({'action':'artist','artist':'synthetic'});self.finish('failed')
        self.assertEqual(self.request('/api/assets')['status'],200)

    def test_lookup_stable_queries_candidates_duplicate_and_manual_verdict(self):
        self.post({'action':'lookup','ids':['a']});v=self.finish('done')
        self.assertEqual(v['runs'][0]['kind'],'pick');self.assertEqual(v['delivery']['status'],'done')
        self.assertEqual(len(self.calls),1)
        self.assertTrue(self.sql("SELECT id FROM term_evidence WHERE source='provenance'"))
        self.assertTrue(self.sql("SELECT id FROM actions")==[]) # 机器导入不产生人工历史
        self.assertTrue(self.sql("SELECT asset FROM image_artists WHERE status='candidate'",cache=True))
        count=self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'")
        artist=self.sql("SELECT artist_id FROM image_artists WHERE asset='a'",cache=True)[0][0]
        r=self.request('/api/artist/verdict',{'ids':['a'],'artist':artist,'verdict':'no'});self.assertEqual(r['status'],200,r)
        history=self.sql('SELECT count(*) FROM actions')
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(count,self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'"))
        self.assertEqual(history,self.sql('SELECT count(*) FROM actions'))
        self.assertTrue(self.sql("SELECT verdict FROM term_effective_verdicts WHERE verdict='no'"))
    def test_changed_picture_bytes_are_looked_up_again(self):
        # #27：同一 UUID 下图片的字节变了，基于旧字节的像素结果随之去掉。
        with closing(sqlite3.connect(self.lib.source)) as c:
            c.execute('ALTER TABLE assets ADD COLUMN stable_hash TEXT')
            c.execute("UPDATE assets SET stable_hash='first-'||uuid");c.commit()
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(len(self.calls),1)
        artist=self.sql("SELECT artist_id FROM image_artists WHERE asset='a'",cache=True)[0][0]
        r=self.request('/api/artist/verdict',{'ids':['a'],'artist':artist,'verdict':'no'});self.assertEqual(r['status'],200,r)
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertEqual(len(self.calls),1)  # 字节没变时复用先前的结果
        with closing(sqlite3.connect(self.lib.source)) as c:
            c.execute("UPDATE assets SET stable_hash='second' WHERE uuid='a'");c.commit()
        self.post({'action':'lookup','ids':['a']});v=self.finish('done')
        self.assertEqual(len(self.calls),2)
        self.assertEqual(v['delivery']['status'],'done')
        self.assertEqual(self.sql("SELECT hash FROM content_versions WHERE asset='a'",cache=True),[('second',)])
        self.assertTrue(self.sql("SELECT verdict FROM term_effective_verdicts WHERE verdict='no'"))
    def test_full_local_consent_and_x_leads(self):
        with db.connect(self.lib) as c:c.execute("INSERT INTO leads(source,system,key) VALUES ('x_following','x_id','123')")
        self.post({'action':'start'});v=self.finish('stopped');self.assertFalse(self.calls)
        self.assertEqual(v['runs'][0]['kind'],'full')
        self.post({'action':'start','allow_network':True});v=self.finish('stopped') # 没有绑定 Pixiv 账号
        self.assertEqual(self.sql("SELECT status FROM leads WHERE key='123'",cache=True),[('resolved',)])
        self.assertTrue(any('artists.json' in s for s in self.calls))
    def test_worker_missing_wrong_identity_crash_and_timeout(self):
        self.env['ILLUSTASH_PROVENANCE_IDLE_SECONDS']='2'
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
        self.post({'action':'artist','artist':'synthetic'});wait_until(lambda:(self.root/'pid').exists())
        pid=int((self.root/'pid').read_text());self.post({'action':'stop'});self.finish('stopped')
        wait_until(lambda:not worker_exists(pid))
        self.env['SYNTHETIC_MODE']='ok';self.restart();self.post({'action':'lookup','ids':['a']});self.finish('done')
    def test_failure_retry_and_delivery_recovery(self):
        self.code=403;self.post({'action':'lookup','ids':['a']});self.finish('stopped')
        self.assertEqual(len(self.calls),1)
        self.code=200;self.post({'action':'lookup','ids':['a']});self.finish('done')
        self.assertTrue(self.sql("SELECT last_result FROM lookups WHERE backend='pixel_hash'",cache=True))
        # 只在宿主停下时设为待处理：运行中的宿主会在下一次空闲轮询时重放它（#56、#143）。
        self.stop(self.process)
        with db.connect(self.lib) as c:c.execute("UPDATE native_delivery SET status='pending'")
        count=self.sql('SELECT count(*) FROM term_evidence');self.restart()
        self.assertEqual(count,self.sql('SELECT count(*) FROM term_evidence'))
        self.assertFalse(self.sql("SELECT task_id FROM native_delivery WHERE status='pending'",cache=True))
    def large_library_delivery(self):
        """用 unittest 显式点名执行。真实宿主与导入事务，合成 worker 和回环服务。"""
        for path in (self.lib.db, self.lib.source):
            with closing(sqlite3.connect(path)) as c, c:
                c.execute("""WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<100000)
                    INSERT INTO assets(uuid,filename,width,height,added_at,src_kind)
                    SELECT printf('0-%06d',i),'synthetic.png',120,150,'2026-01-01','orig' FROM n""")
        [(total,)] = self.sql('SELECT count(*) FROM assets')
        self.post({'action': 'lookup', 'ids': ['a']})
        result = self.finish('done')
        self.assertEqual(result['delivery']['status'], 'done', result)
        self.assertGreater(result['delivery']['added'], 0)
        [(parts, path)] = self.sql("SELECT parts,package FROM native_delivery WHERE status='done'", cache=True)
        self.assertEqual(parts, 2)
        sizes = []
        for part in range(1, parts + 1):
            manifest = self.lib.path / path / f'part-{part}' / 'manifest.json'
            sizes.append(len(json.loads(manifest.read_text(encoding='utf-8'))['assets']))
        self.assertEqual(sizes, [100000, total - 100000])
        self.assertTrue(self.sql("""SELECT 1 FROM term_current_evidence e JOIN term_assets a ON a.id=e.asset_id
            WHERE a.key='a' AND e.source='provenance'"""))

    def test_refused_pending_package_is_resealed_and_later_deliveries_proceed(self):
        # #28：因暂时性失败而留在待处理的包，涉及一张在重试前被移除的图片。
        with closing(sqlite3.connect(self.lib.db)) as hold:
            hold.execute('BEGIN IMMEDIATE')  # 运行交付它的包时图库正忙
            self.post({'action':'lookup','ids':['a']});v=self.finish('done')
            # 在图库空闲之前停下：宿主的定期重试（每 60 秒）不能在图片移除之前交付待处理的包（#143）。
            self.stop(self.process)
            hold.rollback()
        self.assertEqual(v['delivery']['status'],'failed')
        [(old,status)]=self.sql('SELECT task_id,status FROM native_delivery',cache=True)
        self.assertEqual(status,'pending')
        kept="SELECT count(*) FROM term_evidence e JOIN term_assets t ON t.id=e.asset_id WHERE t.key='b'"
        evidence_b=self.sql(kept)
        # 按同步移除图片的方式移除：删掉它的行，core 把它标为已移除。
        with closing(sqlite3.connect(self.lib.db)) as c:
            c.execute("DELETE FROM assets WHERE uuid='b'")
            c.execute("UPDATE term_assets SET removed_at=CURRENT_TIMESTAMP WHERE namespace='library' AND key='b'")
            c.commit()
        self.restart()
        rows=dict(self.sql('SELECT task_id,status FROM native_delivery',cache=True))
        self.assertEqual(rows[old],'stale')
        self.assertIn('已移除',self.sql('SELECT error FROM native_delivery WHERE task_id=?',(old,),cache=True)[0][0])
        resealed=[task for task in rows if task.startswith('resealed-')]
        self.assertEqual([rows[task] for task in resealed],['done'])
        self.assertEqual(self.info()['delivery']['status'],'done')
        # 之后的研究正常交付；没有留下待处理的包，被移除的图片保留它的证据。
        self.post({'action':'lookup','ids':['a']});v=self.finish('done')
        self.assertEqual(v['delivery']['status'],'done')
        self.assertFalse(self.sql("SELECT task_id FROM native_delivery WHERE status='pending'",cache=True))
        self.assertEqual(self.sql(kept),evidence_b)
        # 过期的包不再重试，之后重启也不会封新的包。
        count=self.sql('SELECT count(*) FROM native_delivery',cache=True);self.restart()
        self.assertEqual(self.sql('SELECT count(*) FROM native_delivery',cache=True),count)
        # 目录已经不在的待处理包，也照同样的方式了结。
        latest=self.sql('SELECT task_id,package FROM native_delivery ORDER BY rowid DESC LIMIT 1',cache=True)[0]
        self.stop(self.process)
        with db.connect(self.lib) as c:c.execute("UPDATE native_delivery SET status='pending' WHERE task_id=?",(latest[0],))
        shutil.rmtree(self.lib.path/latest[1]);self.restart()
        self.assertEqual(self.sql('SELECT status FROM native_delivery WHERE task_id=?',(latest[0],),cache=True),[('stale',)])
        self.assertEqual(self.info()['delivery']['status'],'done')
        self.assertFalse(self.sql("SELECT task_id FROM native_delivery WHERE status='pending'",cache=True))
    def test_frozen_package_does_not_replay_pending_delivery(self):
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        wait_until(lambda:self.sql('SELECT status FROM native_delivery',cache=True)==[('done',)])
        # 运行中、未冻结的宿主会在下一次空闲轮询时重放待处理的交付（#56），所以要等宿主停下后才设置待处理状态。
        self.stop(self.process)
        with db.connect(self.lib) as c:c.execute("UPDATE native_delivery SET status='pending'")
        config=dict(self.lib.config);config['frozen']=True
        (self.lib.path/'library.json').write_text(json.dumps(config))
        with sqlite3.connect(self.lib.db) as c:before=list(c.iterdump())
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.post({'action':'start'},400)
        self.assertTrue(self.info()['frozen']);self.assertIsNone(self.info()['delivery'])
        with sqlite3.connect(self.lib.db) as c:self.assertEqual(before,list(c.iterdump()))
        self.assertEqual(self.sql('SELECT status FROM native_delivery',cache=True),[('pending',)])

    def test_same_id_other_package_switch_rejects_late_results(self):
        other = self.package('other');config=other.config.copy();config['id']=self.lib.config['id'];(other.path/'library.json').write_text(json.dumps(config))
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
        wait_until(lambda:self.info()['state']['status']=='stopped')
        self.env['SYNTHETIC_MODE']='hang-first'
        with db.connect(self.lib) as c:c.execute("INSERT OR IGNORE INTO pending_runs(kind) VALUES ('new')")
        self.restart();wait_until(lambda:self.info()['state'].get('kind')=='new')
        previous=self.sql('SELECT max(id) FROM runs',cache=True)[0][0]
        self.assertEqual(self.request('/api/libraries/open',{'path':str(self.lib.path)})['status'],200)
        self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        wait_until(lambda:self.sql('SELECT max(id) FROM runs',cache=True)[0][0]>previous)
        self.assertEqual(self.info()['state']['kind'],'new');self.post({'action':'stop'});self.finish('stopped')

    def test_export_contract_native_matches_python(self):
        # 原 Python 导出器、导入器和运行报告在同一次查找上的结果已录制（tests/fixed_expectations.py）；
        # 类似时钟的文本按 <clock> 比较。
        before=self.root/'reference-application.sqlite'
        with sqlite3.connect(self.lib.db) as source,sqlite3.connect(before) as target:source.backup(target)
        self.post({'action':'lookup','ids':['a']});self.finish('done')
        package=self.lib.path/self.sql('SELECT package FROM native_delivery',cache=True)[0][0]
        expected=self.rec.value('python export')
        for name in ('evidence.jsonl','terms.jsonl'):
            self.assertEqual(unclocked((Path(package)/name).read_text(encoding='utf-8')),unclocked(expected['files'][name]))
        # 创建时间是传输用的元数据；文件摘要覆盖上面比较过的（带时钟的）字节。
        self.assertEqual(manifest_shape(json.loads((Path(package)/'manifest.json').read_text())),expected['manifest'])
        native_import=self.info()['delivery']
        self.assertEqual({k:v for k,v in native_import.items() if k!='status'},expected['import'])
        # 通过稳定的链接解析随机的词条 ID；把正式证据、投影、历史和持久事件，与参照导入相同字节的结果比较。
        with sqlite3.connect(self.lib.db) as actual:
            for query,rows in zip(EXPORT_QUERIES,expected['rows']):
                self.assertEqual(json.loads(unclocked(json.dumps(actual.execute(query).fetchall(),ensure_ascii=False))),rows,query)
        native=json.loads(unclocked(json.dumps(self.info(),ensure_ascii=False)))
        self.assertEqual(native['runs'],expected['runs']);self.assertEqual(native['coverage'],expected['coverage'])
        self.assertEqual(native['runs'][0]['report'],expected['report'])


    def test_pixiv_pages_cache_and_expiry_never_grants_background_consent(self):
        store_credential(self.support,'pixiv','9_synthetic')
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
        wait_until(lambda:self.info()['state'].get('kind')=='new')
        self.post({'action':'start'},400)
        self.post({'action':'lookup','ids':['a']})
        # 被抢占的运行在查找一结束就恢复，所以查找的结果从它自己的运行里读。
        wait_until(lambda:any(r['kind']=='pick' and r['status'] not in ('running','stopping') for r in self.info()['runs']))
        self.assertEqual([r['status'] for r in self.info()['runs'] if r['kind']=='pick'],['done'])
        wait_until(lambda:self.info()['state'].get('kind')=='new' and self.info()['state']['status']=='running')
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
        # 登录弹窗是 mock 的本地桥接命令；不执行任何第三方登录。
        self.assertTrue(any(method=='POST' and '/pixiv/login' in path for method,path,_ in calls))
        self.env.pop('ILLUSTASH_DESKTOP_BRIDGE');self.env.pop('ILLUSTASH_DESKTOP_TOKEN');self.restart()
        before=len(self.calls);self.post({'action':'start','allow_network':True});self.finish('stopped')
        self.assertFalse(any('/ajax/' in path for path in self.calls[before:]))
    def test_sync_commit_cursor_survives_restart(self):
        self.env['SYNTHETIC_MODE']='hang-first';self.restart()
        self.stop(self.process)
        with sqlite3.connect(self.lib.db) as c:c.execute("INSERT INTO sync_runs(status,imported,finished_at) VALUES ('done',1,datetime('now'))")
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        wait_until(lambda:self.info()['state'].get('kind')=='new')
        self.assertTrue(self.sql("SELECT value FROM native_meta WHERE key='sync_seen'",cache=True))
        self.post({'action':'stop'});self.finish('stopped')

    def test_real_adapter_hash_and_owner_crash_recovery(self):
        from PIL import Image
        import hashlib
        image=self.root/'synthetic.png';Image.new('RGBA',(2,3),(9,17,21,255)).save(image)
        with sqlite3.connect(self.lib.source) as c:c.execute("UPDATE assets SET src_path=? WHERE uuid='a'",(str(image),))
        cache=self.root/'adapter-cache';cache.mkdir()
        with db.connect(self.lib) as c:
            with sqlite3.connect(cache/'provenance.sqlite') as out:c.backup(out)
        request={'format':'illustash-provenance/1','identity':{'task_id':'synthetic'},'owner_pid':os.getpid(),'library':str(self.lib.path),'cache':str(cache),'mailbox':str(cache),'step':'pixel_hash','only':['a'],'artists':[],'network':False}
        req=cache/'request.json';req.write_text(json.dumps(request));reply=cache/'response.json'
        from tests.test_native_provenance_production import candidate_worker
        import shutil
        adapter=cache / ('illustash-provenance.exe' if os.name=='nt' else 'illustash-provenance')
        shutil.copy2(candidate_worker(),adapter)
        subprocess.run([str(adapter),str(req),str(reply)],check=True,start_new_session=True,env={**self.env,'PATH':os.environ['PATH']})
        self.assertEqual(json.loads((cache/'hashes.json').read_text()),[{'asset':'a','hash':hashlib.md5(b'P7\nWIDTH 2\nHEIGHT 3\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n'+bytes([9,17,21,255])*6).hexdigest()}])
        if os.environ.get('ILLUSTASH_IQDB_BINARY'):
            request['step']='iqdb';req.write_text(json.dumps(request))
            subprocess.run([str(adapter),str(req),str(reply)],check=True,start_new_session=True,env={**self.env,'PATH':os.environ['PATH']})
            rows=json.loads((cache/'hashes.json').read_text());self.assertEqual([r['asset'] for r in rows],['a']);self.assertTrue(rows[0]['hash'].startswith('iqdb_'))
        # 和真实 worker 的 check_owner 一样：宿主不再是父进程就收掉自己的子进程并退出。
        # Windows 上 worker 经 .cmd 启动、父进程不是宿主，宿主退出时由 Job 收掉整棵树。
        wrapper=self.root/'owner-worker'
        wrapper.write_text('#!'+sys.executable+'\n'+"import json,os,subprocess,sys,time\n"+
                           "owner=json.loads(open(sys.argv[1]).read())['owner_pid']\n"+
                           "p=subprocess.Popen([os.environ['SYNTHETIC_PYTHON'],'-c','import time;time.sleep(300)'])\n"+
                           "out=os.environ['SYNTHETIC_PID'];open(out+'.tmp','w').write(f'{os.getpid()} {p.pid}');os.replace(out+'.tmp',out)\n"+
                           "while os.name=='nt' or os.getppid()==owner:time.sleep(.1)\n"+
                           "p.kill();p.wait()\n")
        wrapper.chmod(0o755);pidfile=self.root/'owner-child'
        self.env.update(ILLUSTASH_PROVENANCE_WORKER=worker_command(wrapper),SYNTHETIC_PYTHON=sys.executable,SYNTHETIC_PID=str(pidfile));self.restart()
        self.post({'action':'artist','artist':'synthetic'});wait_until(pidfile.exists)
        worker,child=map(int,pidfile.read_text().split())
        gone=[]
        def reap():
            # 断言没走到时兜底，不把 worker 留成孤儿；已确认退出的不再按 pid 发信号。
            if gone:return
            for pid,group in ((worker,True),(child,False)):
                with contextlib.suppress(OSError):
                    if os.name=='nt':os.kill(pid,signal.SIGTERM)
                    else:(os.killpg if group else os.kill)(pid,signal.SIGKILL)
        self.addCleanup(reap)
        self.process.kill();self.process.wait(timeout=WAIT)
        # POSIX 上 worker_exists 按进程组查，worker 是组长，孙进程同组，一并覆盖。
        wait_until(lambda:not worker_exists(worker) and not worker_exists(child));gone.append(True)
        self.process,self.log=self.start(self.lib.path);self.token=json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.info()['state']['status'],'stopped')
        self.assertEqual(self.sql("SELECT count(*) FROM term_evidence WHERE source='provenance'"),[(0,)])


def manifest_shape(manifest):
    manifest.pop('created_at')
    for entry in (manifest.get('files') or {}).values():
        if isinstance(entry, dict):
            entry.pop('sha256', None)
    return manifest


EXPORT_QUERIES=(
    'SELECT source,method,score,metric,data,asset_id FROM term_evidence ORDER BY source,method,data,asset_id',
    'SELECT system,key FROM term_links ORDER BY system,key',
    'SELECT p.asset_id,t.kind,p.state,p.score FROM term_projection p JOIN terms t ON t.id=p.term_id ORDER BY p.asset_id,t.kind,p.state,p.score',
    'SELECT label,before_json FROM actions ORDER BY id',
    'SELECT kind,status,error FROM backend_effects ORDER BY id',
)


MACHINE = ('artists', 'artist_links', 'artist_names', 'image_sources', 'image_artists', 'seeds', 'lookups', 'marks',
           'ref_sigs', 'ext_cache')


class ProvenanceRetentionTest(unittest.TestCase):
    """#31：运行目录和机器缓存的写入。复用上面的测试框架，但不把它的测试再跑一遍；
    本类不录制，所以准备阶段的值来自本模块第一个录制的测试。"""
    setUpClass = NativeProvenanceTest.__dict__['setUpClass']
    package = NativeProvenanceTest.package
    stop = NativeProvenanceTest.stop
    request = NativeProvenanceTest.request
    start = NativeProvenanceTest.start
    setUp = NativeProvenanceTest.setUp
    restart = NativeProvenanceTest.restart
    info = NativeProvenanceTest.info
    finish = NativeProvenanceTest.finish
    post = NativeProvenanceTest.post
    sql = NativeProvenanceTest.sql

    def runs(self):
        root = self.lib.path / '.provenance'
        return sorted(p.name for p in root.iterdir()) if root.exists() else []

    def newest_delivered(self):
        return self.sql("SELECT task_id FROM native_delivery WHERE status='done' ORDER BY rowid DESC LIMIT 1",
                        cache=True)[0][0]

    def test_only_pending_and_newest_delivered_packages_stay(self):
        self.post({'action': 'lookup', 'ids': ['a']}); self.finish('done')
        first = self.newest_delivered()
        self.assertEqual(self.runs(), [first])
        self.assertEqual([p.name for p in (self.lib.path / '.provenance' / first).iterdir()], ['evidence'])
        # 每个包都替换 core 里的上一个快照：只保留最新交付的那个。
        self.post({'action': 'lookup', 'ids': ['a']}); self.finish('done')
        second = self.newest_delivered()
        self.assertNotEqual(first, second)
        self.assertEqual(self.runs(), [second])
        # 在接收关口之前失败的运行绝不导入；它留着供检查，直到下一次打开或运行。
        self.env['SYNTHETIC_MODE'] = 'crash'; self.restart()
        self.post({'action': 'lookup', 'ids': ['a']}); v = self.finish()
        self.assertIn(v['state']['status'], ('failed', 'stopped'))
        failed = [name for name in self.runs() if name != second]
        self.assertEqual(len(failed), 1)
        self.assertTrue((self.lib.path / '.provenance' / failed[0] / 'provenance.sqlite').exists())
        self.env['SYNTHETIC_MODE'] = 'ok'; self.restart()
        self.assertEqual(self.runs(), [second])
        # 交付失败的包处于待处理：重启后仍保留，直到交付为止。
        with closing(sqlite3.connect(self.lib.db)) as hold:
            hold.execute('BEGIN IMMEDIATE')
            self.post({'action': 'lookup', 'ids': ['a']}); v = self.finish('done')
            # 在图库忙时停下，所以定期重试无法在重启之前交付它（#143）。
            self.stop(self.process)
            hold.rollback()
        self.assertEqual(v['delivery']['status'], 'failed')
        [(pending,)] = self.sql("SELECT task_id FROM native_delivery WHERE status='pending'", cache=True)
        self.assertEqual(self.runs(), sorted([second, pending]))
        self.restart()
        self.assertEqual(self.sql('SELECT status FROM native_delivery WHERE task_id=?', (pending,), cache=True),
                         [('done',)])
        self.assertEqual(self.runs(), [pending])

    def test_done_is_reported_only_after_working_files_are_removed(self):
        # #143：把清理前的那一刻拉长；读到结束状态的人只会看到封好的包。
        self.env['ILLUSTASH_TEST_PROVENANCE_CLEANUP_PAUSE_MS'] = '1500'; self.restart()
        self.post({'action': 'lookup', 'ids': ['a']}); self.finish('done')
        task = self.newest_delivered()
        self.assertEqual([p.name for p in (self.lib.path / '.provenance' / task).iterdir()], ['evidence'])

    def test_publication_writes_only_rows_the_run_changed(self):
        from collections import Counter
        self.post({'action': 'lookup', 'ids': ['a']}); self.finish('done')
        cache = self.lib.path / 'provenance.sqlite'
        with closing(sqlite3.connect(cache)) as c:
            c.executemany("INSERT INTO ref_sigs(site,post_id,page,ratio,grid) VALUES ('synthetic',?,0,1.0,?)",
                          [(str(i), bytes([i % 256]) * 256) for i in range(2000)])
            c.executemany("INSERT INTO lookups(target,backend,attempts,last_result) VALUES (?,'synthetic',1,'miss')",
                          [('synthetic-' + str(i),) for i in range(2000)])
            c.execute('CREATE TABLE synthetic_writes(tbl TEXT,op TEXT)')
            for table in MACHINE:
                for op in ('INSERT', 'UPDATE', 'DELETE'):
                    c.execute(f"CREATE TRIGGER synthetic_{table}_{op} AFTER {op} ON {table} "
                              f"BEGIN INSERT INTO synthetic_writes VALUES ('{table}','{op}'); END")
            c.commit()

        def rows():
            with closing(sqlite3.connect(cache)) as c:
                return {t: c.execute('SELECT * FROM ' + t + ' ORDER BY rowid').fetchall() for t in MACHINE}
        before = rows()
        self.post({'action': 'lookup', 'ids': ['a']}); self.finish('done')
        after = rows()
        written = self.sql('SELECT tbl,op FROM synthetic_writes', cache=True)
        # 4,000 条合成行既没有重写也没有挪动；而完整重写会把每一行都再写一遍。
        self.assertLess(len(written), 20, Counter(written))
        self.assertFalse([w for w in written if w[0] in ('ref_sigs',)], written)
        self.assertEqual(after['ref_sigs'], before['ref_sigs'])
        synthetic = lambda rows: [r for r in rows if r[1] == 'synthetic']
        self.assertEqual(synthetic(after['lookups']), synthetic(before['lookups']))
