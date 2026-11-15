"""#37 合成 HTTP 验收；比较原始字节、DDL、带类型的单元格和 rowid。

原生新建对象时，通过参照已有的 UUID/NeedId 协议提供 UUID 输入。
参照的 fetch_names 回调把观察到的调用录下来，作为原生使用的持久 M4 交接。参照的响应和状态通过
tests/fixed_expectations.py 录制（#9）；只有类似时钟的文本
按 ``<clock>`` 比较。
"""
from contextlib import closing
import json
import sqlite3
import unittest
from unittest.mock import patch

from tests import test_native_human_labels as labels
from tests.fixed_expectations import unclocked
from tools.verify_read_http import cookie, fetch
from tools.wait_limit import WAIT


class NativeProfilesTest(unittest.TestCase):
    setUpClass = classmethod(labels.NativeHumanLabelsTest.setUpClass.__func__)
    clone = labels.NativeHumanLabelsTest.clone
    start = labels.NativeHumanLabelsTest.start
    stop = labels.NativeHumanLabelsTest.stop
    seed_both = labels.NativeHumanLabelsTest.seed_both
    undo_latest = labels.NativeHumanLabelsTest.undo_latest
    latest_action = labels.NativeHumanLabelsTest.latest_action
    reference_response = labels.NativeHumanLabelsTest.reference_response

    def setUp(self):
        labels.NativeHumanLabelsTest.setUp(self)
        self.seed_both()
        self.flag_actions = set()

    def name_request(self, entity=None, refresh=False):
        with self.store.connect() as c:
            c.execute('INSERT INTO backend_effects(kind) VALUES (?)',
                      ('names:'+json.dumps({'entity':entity,'refresh':refresh},ensure_ascii=False),))

    def post(self, base, url, body):
        return fetch(base, {'url':url,'method':'POST','body':json.dumps(body,ensure_ascii=False).encode(),
                           'headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}},self.origin)

    def recorded_state(self):
        state = labels.states(self.right)
        # #428 只扩展了这两个写入的历史。逐条验证新增步骤后，其余表及历史仍与录制比较。
        actions = state.baseline['app']['actions']
        actions['rows'] = [row for row in actions['rows'] if json.loads(row)[0] not in self.flag_actions]
        return state

    def pair(self,url,body,expected=200,*,flag_changed=None):
        before=[self.rec.state('before'),self.recorded_state()]
        with closing(sqlite3.connect(self.right.db)) as c:
            ids={r[0] for r in c.execute('SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id')}
        native=self.post(self.origin,url,body)
        with closing(sqlite3.connect(self.right.db)) as c:
            births=[r[0] for r in c.execute('SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id ORDER BY r.seq') if r[0] not in ids]
        import uuid
        actual_uuid=uuid.uuid4
        pending=iter(births)
        old=self.rec.value('response')
        for r in (old,native):self.assertEqual(r['status'],expected,(url,body,r['body']))
        self.assertEqual(native['route'],'core')
        if expected<400:
            compared = native
            if url in ('/api/kind', '/api/character/mascot'):
                step = json.loads(native['body'])['undo']
                if flag_changed:
                    self.assertEqual(set(step), {'id', 'label', 'ts'})
                    self.assertEqual(step['id'], self.latest_action())
                    self.assertNotIn(step['id'], self.flag_actions)
                    self.flag_actions.add(step['id'])
                else:
                    self.assertIsNone(step)
                compared = {**native, 'body': native['body'].rsplit(b', "undo": ', 1)[0] + b'}'}
            self.assertIsNone(labels.difference(old,compared),(url,body,old['body'],native['body']))
        after=[self.rec.state('after'),self.recorded_state()]
        if expected>=400:
            self.assertEqual(before,after,'failed transaction committed')
        self.assertEqual(after[0],after[1],(url,body))
        self.assertFalse(self.sentinel.exists())
        self.latency.append((url,old['ms'],native['ms']))
        return json.loads(native['body']) if expected<400 else None

    def test_entity_creation_duplicate_names_tags_oc_shared(self):
        created=self.pair('/api/entities',{'name':' Synthetic 名前 '})
        self.pair('/api/entities',{'name':'synthetic 名前'})
        self.pair('/api/entities',{'kind':'oc','artist':' 合成作者 '})
        self.pair('/api/entities',{'kind':'oc','name':''})
        self.pair('/api/oc/shared',{})
        self.pair('/api/oc/shared',{})
        linked=self.pair('/api/entities',{'tag':' Remote Person ','typed':'中文别名'})
        self.pair('/api/entities',{'tag':'remote_person','typed':'忽略'})
        self.pair('/api/entities',{'name':'中文别名'})
        self.pair('/api/entities',{'tag':'fallback_person'})
        self.pair('/api/entities',{'tag':'unknown_person'})
        for body in ({'name':''},{'name':5},{'name':'x','kind':None},{'name':'x','artist':None},{'name':'x','kind':'other'},{'tag':'bad\tname'}):
            self.pair('/api/entities',body,400)
        self.assertNotEqual(created['id'],linked['id'])

    def test_works_all_branches_and_character_structure(self):
        parent=self.pair('/api/works',{'action':'create','name':' 父作品 '})['id']
        child=self.pair('/api/works',{'action':'create','name':'子作品','parent':parent,'tag':'child_work'})['id']
        self.pair('/api/works',{'action':'move','id':child,'parent':None})
        self.pair('/api/works',{'action':'rename','id':child,'name':'新名字'})
        self.pair('/api/works',{'action':'link','id':child,'tag':'new_work'})
        self.pair('/api/works',{'action':'edit','id':child,'name':'编辑作品','parent':parent,'tag':''})
        self.pair('/api/works',{'action':'from_tag','tag':'external_work'})
        self.pair('/api/works',{'action':'from_tag','tag':'external_work'})
        self.pair('/api/works',{'action':'move','id':parent,'parent':child},400)
        self.pair('/api/works',{'action':'link','id':parent,'tag':'new_work'},400)
        self.pair('/api/works',{'action':'bad','id':parent},400)
        self.pair('/api/character/profile',{'entity':self.alice,'work':child})
        self.pair('/api/character/profile',{'entity':self.alice,'work':None})
        self.pair('/api/character/mascot',{'entity':self.alice,'mascot':True},flag_changed=True)
        self.pair('/api/character/mascot',{'entity':self.alice,'mascot':False},flag_changed=True)
        self.pair('/api/character/mascot',{'entity':self.alice,'mascot':1},400)
        self.pair('/api/kind',{'entity':self.alice,'kind':'oc','artist':'作者'},flag_changed=True)
        self.pair('/api/character/profile',{'entity':self.alice,'work':child},400)
        self.pair('/api/kind',{'entity':self.alice},flag_changed=True)
        self.check_flag_undo(child)

    def check_flag_undo(self, work):
        entity = self.alice

        def post(path, body, status=200):
            r = self.post(self.origin, path, body)
            self.assertEqual(r['status'], status, (path, r['body']))
            return json.loads(r['body'])

        def snapshot():
            state = labels.states(self.right).baseline['app']
            return {table: state[table] for table in ('terms', 'term_tree', 'term_names', 'memberships',
                                                      'tag_memberships', 'family_evidence', 'term_projection')}

        def read(path):
            r = fetch(self.origin, {'url': path}, self.origin)
            self.assertEqual(r['status'], 200, (path, r['body']))
            return json.loads(r['body'])

        def views():
            return (read(f'/api/characters?id={entity}'), read('/api/characters?min=1'), read('/api/tagged'),
                    read(f'/api/assets?entity={entity}')['total'], read('/api/review-pools'))

        # 让合成角色有作品、自动显示名的首选标记和图片归属。
        with closing(sqlite3.connect(self.right.db)) as c:
            term = c.execute('SELECT id FROM terms WHERE key=?', (entity,)).fetchone()[0]
            c.execute("INSERT INTO meta(key,value) VALUES ('terms_writer','1')")
            c.execute("DELETE FROM term_names WHERE term_id=? AND source='user' AND role='display'", (term,))
            c.execute("INSERT INTO term_names(term_id,lang,value,role,source,preferred,seq) "
                      "VALUES (?,'en','Alice Synthetic','display','index',1,(SELECT max(seq)+1 FROM term_names))", (term,))
            c.execute("UPDATE term_tree SET mascot_origin='default' WHERE term_id=?", (term,))
            c.execute("DELETE FROM meta WHERE key='terms_writer'")
            c.commit()
        # 资料写入提交时刷新显示投影，再取初始状态。
        post('/api/character/profile', {'entity': entity, 'work': work})
        post('/api/change', {'ids': ['a'], 'entity': entity, 'action': 'confirmed'})
        initial = snapshot()
        initial_views = views()
        previous = self.latest_action()

        oc = post('/api/kind', {'entity': entity, 'kind': 'oc', 'artist': '合成画师'})['undo']
        character = read(f'/api/characters?id={entity}')[0]
        self.assertEqual((character['kind'], character['artist']), ('oc', '合成画师'))
        profile = read(f'/api/character/profile?entity={entity}')
        self.assertIsNone(profile['work_id'])
        self.assertEqual(profile['user_name'], initial_views[0][0]['name'])
        self.assertIsNone(post('/api/kind', {'entity': entity, 'kind': 'oc', 'artist': '合成画师'})['undo'])
        self.assertEqual(self.latest_action(), oc['id'])
        without_oc = post('/api/kind', {'entity': entity})['undo']
        post('/api/undo', {'id': oc['id']}, 400)
        self.assertEqual(post('/api/undo', {'id': without_oc['id']})['undo'], oc)
        self.assertEqual(read(f'/api/characters?id={entity}')[0]['kind'], 'oc')
        self.assertEqual(post('/api/undo', {'id': oc['id']})['undo']['id'], previous)
        self.assertEqual(snapshot(), initial)
        self.assertEqual(views(), initial_views)

        mascot = post('/api/character/mascot', {'entity': entity, 'mascot': True})['undo']
        char, directory, tagged, total, _ = views()
        self.assertTrue(char[0]['mascot'])
        self.assertNotIn(entity, {r['id'] for r in directory})
        self.assertIn(entity, {r['id'] for r in tagged})
        self.assertEqual(total, initial_views[3])
        restored = post('/api/character/mascot', {'entity': entity, 'mascot': False})['undo']
        self.assertEqual(post('/api/undo', {'id': restored['id']})['undo'], mascot)
        self.assertTrue(read(f'/api/characters?id={entity}')[0]['mascot'])
        self.assertEqual(post('/api/undo', {'id': mascot['id']})['undo']['id'], previous)
        self.assertEqual(snapshot(), initial)
        self.assertEqual(views(), initial_views)

        # 自建角色已有用户显示名时，撤销只恢复它的首选标记，保留原有名字行。
        post('/api/character/profile', {'entity': entity, 'name': '合成角色'})
        named = snapshot()
        step = post('/api/kind', {'entity': entity, 'kind': 'oc'})['undo']
        post('/api/undo', {'id': step['id']})
        self.assertEqual(snapshot(), named)

    def test_profile_aliases_tags_links_and_mixed_undo(self):
        one=self.pair('/api/entities',{'name':'合成本地一'})['id']
        two=self.pair('/api/entities',{'name':'合成本地二'})['id']
        self.pair('/api/character/profile',{'entity':one,'name':'新版','aliases':['别名',' 别名 ','第二']})
        self.pair('/api/tag',{'ids':['c'],'tag':'合成 标签'})
        self.pair('/api/character/profile',{'entity':one,'tag':'remote_person'})
        self.pair('/api/character/profile',{'entity':two,'tag':'remote_person'},400)
        for _ in range(3):self.undo_latest()
        self.pair('/api/local-links',{'items':[{'id':one,'tag':'remote_person'},{'id':two,'tag':'fallback_person'}]})
        self.pair('/api/artist/verdict',{'ids':['a'],'artist':self.artist,'verdict':'yes'})
        for _ in range(2):self.undo_latest()
        for body in ({'entity':one,'aliases':[None]}, {'entity':one,'name':''}, {'entity':'missing','tag':''}):
            self.pair('/api/character/profile',body,400)
        self.pair('/api/local-links',{'items':[]},400)
        self.pair('/api/local-links',{'items':[{'id':one,'tag':'unknown_person'},{'id':'missing','tag':'unknown_person'}]},400)

    def test_transaction_fault_rolls_back_creation_and_profiles(self):
        for lib in (self.left,self.right):
            with closing(sqlite3.connect(lib.db)) as c:
                c.execute("CREATE TRIGGER fail_profile BEFORE INSERT ON term_names WHEN NEW.value='故障' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END");c.commit()
        self.pair('/api/entities',{'name':'故障'},500)
        self.pair('/api/character/profile',{'entity':self.alice,'name':'故障'},500)

    def test_profile_with_a_new_work_is_one_transaction(self):
        # #35：选为新建的作品和资料一起建出来，要么都不建。
        def works():
            with closing(sqlite3.connect(self.right.db)) as c:
                return {name: ident for ident, name in c.execute(
                    "SELECT t.key,n.value FROM terms t JOIN term_names n ON n.term_id=t.id AND n.role='display' WHERE t.kind='work'")}
        def work_of(entity):
            with closing(sqlite3.connect(self.right.db)) as c:
                return c.execute('SELECT r.work_id FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE t.key=?', (entity,)).fetchone()[0]
        before = works()
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute("CREATE TRIGGER fail_profile BEFORE INSERT ON term_names WHEN NEW.value='故障' BEGIN SELECT RAISE(ABORT,'synthetic failure'); END"); c.commit()
        failed = self.post(self.origin, '/api/character/profile', {'entity': self.alice, 'name': '故障', 'work': {'create': '合成新作品'}})
        self.assertEqual(failed['status'], 500, failed['body'])
        self.assertEqual(works(), before)
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute('DROP TRIGGER fail_profile'); c.commit()
        saved = self.post(self.origin, '/api/character/profile', {'entity': self.alice, 'work': {'create': '合成新作品'}})
        self.assertEqual(saved['status'], 200, saved['body'])
        self.assertIn('undo', json.loads(saved['body']))
        created = works()
        self.assertEqual(set(created) - set(before), {'合成新作品'})
        self.assertEqual(work_of(self.alice), created['合成新作品'])
        tagged = self.post(self.origin, '/api/character/profile', {'entity': self.alice, 'work': {'tag': 'synthetic_series'}})
        self.assertEqual(tagged['status'], 200, tagged['body'])
        self.assertNotEqual(work_of(self.alice), created['合成新作品'])
        self.assertIn(work_of(self.alice), works().values())
        refused = self.post(self.origin, '/api/character/profile', {'entity': self.alice, 'work': {'bogus': 1}})
        self.assertEqual(refused['status'], 400, refused['body'])
        self.assertFalse(self.sentinel.exists())

    def test_live_synthetic_copyright_cache_and_local_shortcut(self):
        import http.server
        import threading
        import urllib.parse
        requests=[]
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self,*args):pass
            def do_GET(self):
                t=urllib.parse.parse_qs(urllib.parse.urlsplit(self.path).query)['search[query]'][0]
                requests.append(t)
                raw=json.dumps({'related_tags':[{'tag':{'name':'specific','post_count':9},'frequency':.95},
                                               {'tag':{'name':'franchise','post_count':999},'frequency':1.0}]}).encode()
                self.send_response(200);self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
        server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Handler)
        thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
        self.addCleanup(server.server_close);self.addCleanup(thread.join);self.addCleanup(server.shutdown)
        base=f'http://127.0.0.1:{server.server_port}'
        self.rec.alias(base,'@COPYRIGHT@')   # 按它的 URL 缓存
        self.stop(self.process);self.env['ILLUSTASH_COPYRIGHT_BASE']=base;self.process=self.start(self.right)
        self.token=json.loads(fetch(self.origin,{'url':'/api/bootstrap'},self.origin)['body'])['token']
        self.rec.alias(self.token,'@TOKEN@')
        self.pair('/api/entities',{'tag':'live_person','typed':'实体'})
        self.pair('/api/entities',{'tag':'live_person'})
        self.pair('/api/entities',{'tag':'second_person_(specific)'})
        # 每一边各查询一次，之后复用缓存；录制的参照现在不发请求。
        self.assertEqual(requests,['live_person']*(2 if False else 1))

    def test_pending_ack_failure_committed_profile_recovers_on_restart(self):
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute("CREATE TRIGGER fail_ack BEFORE UPDATE ON backend_effects WHEN OLD.kind='verdict' BEGIN SELECT RAISE(ABORT,'ack failure'); END");c.commit()
        response=self.post(self.origin,'/api/character/profile',{'entity':self.alice,'name':'已提交'})
        # 资料保存在事件确认失败之前就已提交；按用户 2026-10-11 的决定（#40），已提交的操作返回 200，
        # 事件保持待处理，留待恢复。
        self.assertEqual(response['status'],200,response['body'])
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute("SELECT n.value FROM term_names n JOIN terms t ON t.id=n.term_id WHERE t.key=? AND n.source='user' AND n.role='display'",(self.alice,)).fetchone()[0],'已提交')
            self.assertEqual(c.execute("SELECT status FROM backend_effects WHERE kind='verdict' ORDER BY id DESC LIMIT 1").fetchone()[0],'pending')
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE kind LIKE 'names:%' AND status='pending'").fetchone()[0],1)
            c.execute('DROP TRIGGER fail_ack');c.commit()
        self.stop(self.process);self.process=self.start(self.right)
        with closing(sqlite3.connect(self.right.db)) as c:
            self.assertEqual(c.execute("SELECT status FROM backend_effects WHERE kind='verdict' ORDER BY id DESC LIMIT 1").fetchone()[0],'done')
            self.assertEqual(c.execute("SELECT count(*) FROM backend_effects WHERE kind LIKE 'names:%' AND status='pending'").fetchone()[0],1)
        self.assertFalse(self.sentinel.exists())

    def test_inflight_profile_isolated_from_switched_library(self):
        import http.client
        newer=self.clone(self.right,'new-session')
        before_old,before_new=labels.states(self.right),labels.states(newer)
        body=json.dumps({'entity':self.alice,'name':'禁止跨库'}).encode()
        conn=http.client.HTTPConnection('127.0.0.1',self.port,timeout=WAIT);self.addCleanup(conn.close)
        conn.putrequest('POST','/api/character/profile',skip_host=True)
        for name,value in {'Host':self.origin.split('://')[1],'Origin':self.origin,'Cookie':cookie(self.origin),'Content-Type':'application/json',
                           'X-CSRF-Token':self.token,'Content-Length':str(len(body))}.items():conn.putheader(name,value)
        conn.endheaders();conn.send(body[:1])
        switched=self.post(self.origin,'/api/libraries/open',{'path':str(newer.path)})
        self.assertEqual(switched['status'],200,switched['body'])
        conn.send(body[1:]);stale=conn.getresponse();self.assertEqual(stale.status,403,stale.read())
        self.assertEqual(labels.states(self.right),before_old);self.assertEqual(labels.states(newer),before_new)

    def test_names_event_enqueue_fault_is_atomic_and_compat_drain_preserves_handoff(self):
        before=labels.states(self.right)
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute("CREATE TRIGGER fail_names BEFORE INSERT ON backend_effects WHEN NEW.kind LIKE 'names:%' BEGIN SELECT RAISE(ABORT,'synthetic names enqueue'); END");c.commit()
        with_trigger=labels.states(self.right)
        response=self.post(self.origin,'/api/character/profile',{'entity':self.alice,'name':'全部回滚'})
        self.assertEqual(response['status'],500)
        self.assertEqual(labels.states(self.right),with_trigger)
        with closing(sqlite3.connect(self.right.db)) as c:
            c.execute('DROP TRIGGER fail_names');c.commit()
        self.assertEqual(labels.states(self.right),before)
        self.pair('/api/character/profile',{'entity':self.alice,'name':'保留交接'})

    def test_profile_identity_artist_and_tag_continuous_undo(self):
        import time
        person=self.pair('/api/entities',{'name':'跨类型合成角色'})['id']
        self.pair('/api/character/profile',{'entity':person,'name':'跨类型资料','aliases':['合成别名']})
        # 关系证据用真实的 CURRENT_TIMESTAMP。开始时要离整秒边界远一些。
        while time.time()%1 > .3:time.sleep(.02)
        self.pair('/api/change',{'ids':['c'],'action':'confirmed','entity':person})
        self.pair('/api/merge',{'source':person,'target':self.alice})
        self.pair('/api/artist/verdict',{'ids':['a'],'artist':self.artist,'verdict':'yes'})
        self.pair('/api/tag',{'ids':['c'],'tag':'第二标签'})
        for _ in range(5):self.undo_latest()

    def test_unused_nonfinite_and_large_integers_are_ignored(self):
        extra={'unused':[float('nan'),float('inf'),2**1000]}
        self.pair('/api/entities',{'name':'unused 合成',**extra})
        self.pair('/api/character/profile',{'entity':self.alice,'aliases':['合成'],**extra})
        self.pair('/api/oc/shared',extra)
        self.pair('/api/kind',{'entity':self.alice,'kind':'',**extra})

        linked=self.pair('/api/entities',{'tag':'remote_person','name':float('nan'),'kind':float('inf')})['id']
        self.pair('/api/entities',{'tag':'unknown_person','typed':float('nan')})
        self.pair('/api/works',{'action':'create','name':'unused作品','id':float('nan')},400)
        work=self.pair('/api/works',{'action':'create','name':'unused作品'})['id']
        self.pair('/api/works',{'action':'move','id':work,'name':float('nan'),'tag':float('inf')},400)
        self.pair('/api/local-links',{'items':[{'id':linked,'tag':'remote_person',**extra}]})
