"""Loopback HTTP contract/security tests against disposable data only."""
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest.mock import patch
from PIL import Image
from web.server import Server
from web.store import Store
from tests.test_web import fixture


class HTTPTest(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();root=Path(self.temp.name)
        source=fixture(root)
        Image.new('RGB',(60,90),'#8da195').save(root/'a.png')
        if self._testMethodName == 'test_identity_count_preserves_queue_filter_skip_and_undo':
            with sqlite3.connect(source) as c:
                c.execute("INSERT INTO labels VALUES ('b','rating','explicit',1)")
        self.store=Store(source,root/'app.sqlite',identity='hybrid' if 'identity' in self._testMethodName else None);self.store.refresh()
        self.server=Server(('127.0.0.1',0),self.store,root/'cache')
        self.thread=threading.Thread(target=self.server.serve_forever,daemon=True);self.thread.start()
        self.url=self.server.origin

    def tearDown(self):
        self.server.shutdown();self.thread.join();self.server.server_close();self.temp.cleanup()

    def request(self,path,body=None,headers=None):
        h={}
        if body is not None:h={'Content-Type':'application/json','Origin':self.url,'X-CSRF-Token':self.server.token}
        h.update(headers or {})
        req=urllib.request.Request(self.url+path, data=json.dumps(body).encode() if body is not None else None,headers=h)
        try:
            with urllib.request.urlopen(req,timeout=5) as r:return r.status,r.read(),r.headers
        except urllib.error.HTTPError as e:return e.code,e.read(),e.headers

    def test_recognition_import_endpoint_is_csrf_protected(self):
        self.assertEqual(self.request('/api/recognition/import', {'path': '/unused'}, {'X-CSRF-Token':'bad'})[0], 403)
        self.assertEqual(self.request('/api/recognition/import', {'path': '/unused'})[0], 400)
        from unittest.mock import Mock
        self.server.library = Mock()
        with patch('web.results.import_package', return_value={'status':'done'}) as importer:
            status, body, _ = self.request('/api/recognition/import', {'path': '/unused'})
            self.assertEqual(status, 200)
            self.assertEqual(json.loads(body), {'status':'done'})
            importer.assert_called_once_with(self.server.library, '/unused', self.store)

    def test_name_settings_token_redaction_and_refresh(self):
        from tests.test_names import Providers
        self.server.names.transport = Providers()
        self.server.names.interval = 0
        key = 'sk-local-secret-4321'
        self.assertEqual(self.request('/api/names', {'action': 'model_api', 'key': key},
                                      {'X-CSRF-Token': 'bad'})[0], 403)
        status, body, _ = self.request('/api/names', {'action': 'model_api', 'key': key})
        self.assertEqual(status, 200)
        self.assertNotIn(key.encode(), body)
        info = json.loads(self.request('/api/names')[1])
        self.assertEqual(info['model_api']['hint'], '4321')
        self.assertNotIn(key, json.dumps(info))
        self.assertTrue(json.loads(self.request('/api/names', {'action': 'model_api_test'})[1])['ok'])
        entity = self.store.characters()[0]['id']
        self.assertEqual(self.request('/api/names', {'action': 'refresh', 'entity': entity})[0], 200)
        self.server.names.thread.join(5)
        self.assertEqual(self.request('/api/names', {'action': 'refresh', 'entity': 'missing'})[0], 400)
        self.assertEqual(self.request('/api/names', {'action': 'model_api', 'key': None})[0], 200)
        self.assertFalse(json.loads(self.request('/api/names')[1])['model_api']['configured'])

    def test_name_progress_poll_does_not_query_library(self):
        self.server.names.state = {'status': 'running', 'done': 12, 'total': 50}
        with patch.object(self.store, 'connect', side_effect=AssertionError('unexpected DB query')):
            status, body, _ = self.request('/api/names?progress=1')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'state': {'status': 'running', 'done': 12, 'total': 50}})
        info = json.loads(self.request('/api/names')[1])
        self.assertIn('counts', info)
        self.assertIn('model_api', info)

    def test_index_progress_poll_does_not_load_works_or_history(self):
        from web.index_update import IndexUpdater
        indexer = IndexUpdater.__new__(IndexUpdater)
        indexer.lock = threading.Lock()
        indexer.state = {'status': 'checking', 'done': 12, 'total': 50}
        self.server.indexer = indexer
        with patch.object(indexer, 'info', side_effect=AssertionError('full index read')):
            status, body, _ = self.request('/api/index?progress=1')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'state': indexer.state})
        snapshot = indexer.progress()
        snapshot['state']['done'] = 99
        self.assertEqual(indexer.state['done'], 12)

    def test_static_api_and_thumbnail(self):
        status,body,headers=self.request('/')
        self.assertEqual(status,200);self.assertIn(b'app.js',body);self.assertIn("frame-ancestors 'none'",headers['Content-Security-Policy'])
        self.assertEqual(self.request('/api/bootstrap')[0],200)
        self.assertEqual(self.request('/image?id=a')[0],200)
        self.assertEqual(self.request('/image?id=not-present')[0],404)
        self.assertEqual(self.request('/image?id=a&size=raw')[0],400)
        for size in ('medium','preview','full'):
            self.assertEqual(self.request('/image?id=a&size='+size)[0],200)
        self.assertEqual(self.request('/image?id=../../etc/passwd')[0],404)
        self.assertEqual(self.request('/../review/pi.db')[0],404)

    def test_identity_backend_without_review_ui(self):
        chars=self.store.characters();a,b=[r['id'] for r in chars[:2]]
        for endpoint in ('/api/identity/count','/api/identity/queue','/api/detail?id=a'):
            self.assertEqual(self.request(endpoint)[0],200)
        body={'a':a,'b':b,'kind':'duplicate_of','status':'accepted','reason':'人工确认'}
        self.assertEqual(self.request('/api/identity/decision',body,{'X-CSRF-Token':'bad'})[0],403)
        status,data,_=self.request('/api/identity/decision',body)
        self.assertEqual(status,200)
        action=json.loads(data)['undo']
        self.assertEqual(self.request('/api/undo',{'id':action['id']})[0],200)
        self.store.uncertain_identity(['a'],[a,b])
        pending=json.loads(self.request('/api/detail?id=a')[1])['identity']
        self.assertEqual(pending['assignments'],[])
        self.assertTrue(pending['undetermined'])

    def test_host_origin_and_csrf(self):
        self.assertEqual(self.request('/api/bootstrap',headers={'Host':'evil.example'})[0],403)
        self.assertEqual(self.request('/api/bootstrap',headers={'Origin':'https://evil.example'})[0],403)
        self.assertEqual(self.request('/api/change',{'ids':['a'],'action':'unknown'}, {'X-CSRF-Token':'bad'})[0],403)
        self.assertEqual(self.request('/api/change',{'ids':['a'],'action':'unknown'}, {'Origin':'https://evil.example'})[0],403)
        self.assertFalse(self.store.detail('a')['unknown'])

    def test_save_and_undo(self):
        status,body,_=self.request('/api/change',{'ids':['a','b'],'action':'unknown'})
        self.assertEqual(status,200);action=json.loads(body)['undo']
        self.assertTrue(self.store.detail('a')['unknown'])
        self.assertEqual(self.request('/api/undo',{'id':action['id']})[0],200)
        self.assertFalse(self.store.detail('a')['unknown'])

    def test_curated_names_general_tags_and_correction(self):
        status,body,_=self.request('/api/entities?q=Alice%20album')
        self.assertEqual(status,200)
        self.assertEqual(json.loads(body)[0]['name'],'Alice')
        self.assertEqual(json.loads(body)[0]['matched'],'Alice album')
        self.assertEqual(self.request('/api/general-tags?q=blue_hair')[0],200)
        self.assertEqual(json.loads(self.request('/api/assets?tag=blue_hair')[1])['total'],1)
        self.assertEqual(self.request('/api/review-pools')[0],200)
        alice=self.store.entities('Alice')[0]['id'];bob=self.store.entities('Bob')[0]['id']
        status,body,_=self.request('/api/reassign',{'ids':['a'],'old':alice,'new':bob})
        self.assertEqual(status,200)
        action=json.loads(body)['undo']
        self.assertEqual(self.store.detail('a')['assertions'][0]['status'],'rejected')
        self.assertEqual(self.request('/api/undo',{'id':action['id']})[0],200)
        self.assertEqual(self.store.detail('a')['assertions'][0]['name'],'Alice')

    def test_v2_characters_ids_filters_and_sweep(self):
        alice = self.store.entities('Alice')[0]['id']
        status, body, _ = self.request('/api/characters?min=1')
        self.assertEqual(status, 200)
        row = next(r for r in json.loads(body) if r['id'] == alice)
        self.assertEqual((row['count'], row['unchecked']), (2, 1))
        self.assertEqual(json.loads(self.request('/api/ids?entity=' + alice + '&limit=5000')[1]), ['a', 'b'])
        status, body, _ = self.request('/api/change', {'filter':{'entity':alice}, 'exclude':['a'], 'entity':alice, 'action':'rejected'})
        self.assertEqual(status, 200)
        self.assertEqual(self.store.assets(entity=alice)['total'], 1)
        ownership = json.loads(self.request('/api/detail?id=b')[1])['ownership']
        removed = next(r for r in ownership if r['id'] == alice)
        self.assertEqual(removed['status'], 'rejected')
        self.assertEqual(removed['evidence']['sources'], ['CCIP', 'WD'])
        self.request('/api/undo', {'id':json.loads(body)['undo']['id']})
        status, body, _ = self.request('/api/sweep', {'entity':alice})
        self.assertEqual(status, 200)
        self.assertEqual(self.store.assets(entity=alice, status='machine')['total'], 0)
        self.request('/api/undo', {'id':json.loads(body)['undo']['id']})
        self.assertEqual(self.store.assets(entity=alice, status='machine')['total'], 1)
        for path in ['/api/assets?status=pending', '/api/assets?pool=conflicts', '/api/ids?limit=5001', '/api/ids?order=nope']:
            self.assertEqual(self.request(path)[0], 400)
        self.assertEqual(self.request('/api/tasks')[0], 404)
        self.assertEqual([p['kind'] for p in json.loads(self.request('/api/review-pools')[1])], ['unmatched', 'skipped', 'duplicates', 'excluded', 'pending', 'recent', 'audit'])

    def test_suggest_tags_and_character_lookup(self):
        alice=self.store.entities('Alice')[0]['id'];bob=self.store.entities('Bob')[0]['id']
        rows=json.loads(self.request('/api/characters?id='+bob)[1])
        self.assertEqual([r['id'] for r in rows],[bob])
        status,body,_=self.request('/api/suggest',{'ids':['a','b']})
        self.assertEqual(status,200);self.assertIsInstance(json.loads(body),list)
        status,body,_=self.request('/api/tag',{'filter':{'q':'a.png'},'tag':'  收藏  '})
        self.assertEqual(status,200)
        self.assertEqual(json.loads(self.request('/api/user-tags')[1]),[{'tag':'收藏','count':1}])
        self.assertEqual(json.loads(self.request('/api/assets?utag=%E6%94%B6%E8%97%8F')[1])['total'],1)
        self.assertEqual(self.request('/api/undo',{'id':json.loads(body)['undo']['id']})[0],200)
        self.assertEqual(json.loads(self.request('/api/user-tags')[1]),[])
        self.assertEqual(self.request('/api/tag',{'ids':['a'],'tag':' '})[0],400)

    def test_bad_parameters(self):
        for url in ['/api/assets?limit=9999','/api/assets?offset=-1','/api/assets?limit=bad','/api/detail?id=missing']:
            self.assertEqual(self.request(url)[0],400)
        self.assertEqual(self.request('/api/change',{'ids':['a'],'action':'invalid'})[0],400)
        self.assertEqual(self.request('/api/entities',{'name':''})[0],400)
        self.assertEqual(self.request('/api/entities',{'name':'New character'})[0],200)
        status,body,_=self.request('/api/entities',{'name':'Mina','kind':'oc','artist':'someartist'})
        self.assertEqual(status,200)
        self.assertEqual(self.request('/api/kind',{'entity':json.loads(body)['id'],'kind':'oc','artist':'renamed'})[0],200)
        self.assertEqual(json.loads(self.request('/api/artists')[1]),[{'artist':'renamed','characters':1}])
        self.assertEqual(self.request('/api/kind',{'entity':'missing','kind':'oc'})[0],400)

    def test_relate_selected_images(self):
        self.assertEqual(self.request('/api/relate',{'ids':['a']})[0],400)
        self.assertEqual(self.request('/api/relate',{'ids':['a','b']})[0],400)          # b's file does not exist
        root=Path(self.temp.name)
        Image.open(root/'a.png').resize((40,60)).save(root/'b.png')
        status,body,_=self.request('/api/relate',{'ids':['a','b','c']})
        self.assertEqual(status,200)
        result=json.loads(body)
        self.assertEqual((result['missing'],result['same']),(['c'],[['a','b']]))
        self.assertEqual(result['pairs'][0]['kind'],'duplicate')

    def test_sync_settings_and_backup(self):
        state=json.loads(self.request('/api/sync')[1])
        self.assertEqual(state['state'],{'status':'off'});self.assertEqual(state['history'],[])
        self.assertEqual(self.request('/api/sync',{})[0],400)             # started with --no-sync
        self.assertEqual(self.request('/api/sync',{}, {'X-CSRF-Token':'bad'})[0],403)
        info=json.loads(self.request('/api/settings')[1])
        self.assertEqual(set(info),{'package','source','library','thumbnails','backups','backup_count','startup','forced_off','tasks'})
        self.assertIsNone(info['package'])  # a bare source/app pair, not a library package
        status,body,_=self.request('/api/backup',{})
        self.assertEqual(status,200);self.assertTrue(Path(json.loads(body)['path']).exists())
        self.assertEqual(json.loads(self.request('/api/settings')[1])['backup_count'],1)


    def test_provenance_and_artist_verdicts(self):
        self.assertEqual(json.loads(self.request('/api/provenance')[1]),{'available':False})   # not a library package
        status,body,_=self.request('/api/provenance',{'action':'start'})
        self.assertEqual(status,400)
        # 画师先经 worker 的证据包公开导入核心；裸库的 provenance 可用性仍保持 False。
        from provenance import db, export
        from web import library, terms_core
        root = Path(self.temp.name)
        incoming = library.create(root / '画师输入.pilib')
        incoming.config['id'] = self.store.library_id()
        with sqlite3.connect(self.store.path) as src, sqlite3.connect(Path(incoming.path) / 'library.sqlite') as dst:
            src.backup(dst)
        with db.connect(incoming) as c:
            c.execute("INSERT INTO artists(id) VALUES ('ART')")
        terms_core.import_package(self.store.path, self.store.library_id(), export.write(incoming, root / '画师证据'))
        status,body,_=self.request('/api/artist/verdict',{'ids':['a'],'artist':'ART','verdict':'no','name':'某画师'})
        self.assertEqual(status,200);self.assertEqual(json.loads(body)['undo']['label'],'已标记 1 张不是 某画师 画的')
        self.assertEqual(self.store.artist_verdicts('a'),{'ART':'no'})
        self.assertEqual(self.request('/api/artist/verdict',{'ids':['a'],'artist':'ART','verdict':'perhaps'})[0],400)
        self.assertEqual(self.request('/api/undo',{'id':json.loads(body)['undo']['id']})[0],200)
        self.assertEqual(self.store.artist_verdicts('a'),{})

    def test_startup_settings_and_cache(self):
        info=json.loads(self.request('/api/settings')[1])
        self.assertEqual(info['startup'],{'sync':True,'similar':True,'index_check':True,'names':True,'provenance':True,'local_limit':3000})
        self.assertEqual(info['tasks']['similar'],{'status':'off'})
        status,body,_=self.request('/api/startup',{'similar':False,'local_limit':5000})
        self.assertEqual(status,200)
        self.assertEqual(json.loads(body)['startup'],{'sync':True,'similar':False,'index_check':True,'names':True,'provenance':True,'local_limit':5000})
        self.assertEqual(self.store.startup()['local_limit'],5000)                  # persisted in the library
        for bad in ({'local_limit':5},{'local_limit':True},{'sync':'yes'},{'unknown':True},{}):
            self.assertEqual(self.request('/api/startup',bad)[0],400,bad)
        self.assertEqual(self.store.startup()['similar'],False)                     # bad requests change nothing
        self.assertEqual(self.request('/api/startup',{'sync':False},{'X-CSRF-Token':'bad'})[0],403)
        self.request('/image?id=a&size=thumb')
        cache=Path(self.server.cache);cache.mkdir(exist_ok=True);(cache/'x.jpg').write_bytes(b'x');(cache/'keep.txt').write_text('x')
        status,body,_=self.request('/api/cache',{'action':'clear'})
        self.assertEqual(status,200);self.assertGreaterEqual(json.loads(body)['removed'],1)
        self.assertEqual(list(cache.glob('*.jpg')),[]);self.assertTrue((cache/'keep.txt').exists())
        self.assertEqual(self.request('/api/cache',{'action':'drop'})[0],400)

    def test_about(self):
        import web
        from web.store import SCHEMA_VERSION
        status,body,_=self.request('/api/about')
        info=json.loads(body)
        self.assertEqual(status,200)
        self.assertEqual((info['name'],info['version'],info['schema']),(web.NAME,web.__version__,SCHEMA_VERSION))
        self.assertEqual(set(info['libraries']),{'Pillow','NumPy','ONNX Runtime'})
        self.assertNotIn(str(Path.home()),body.decode())                                  # no local paths or user data


    def test_work_tree_http_and_selection(self):
        status, body, _ = self.request('/api/works', {'action':'create','name':'HTTP 系列'})
        self.assertEqual(status,200)
        parent=json.loads(body)['id']
        status, body, _ = self.request('/api/works', {'action':'create','name':'HTTP 子作品','parent':parent})
        self.assertEqual(status,200)
        child=json.loads(body)['id']
        alice=self.store.entities('Alice')[0]['id']
        self.assertEqual(self.request('/api/character/profile', {'entity':alice,'work':child})[0],200)
        self.assertEqual(json.loads(self.request('/api/assets?work='+parent)[1])['total'],2)
        self.assertEqual(len(json.loads(self.request('/api/characters?work='+parent)[1])),1)
        self.assertEqual(self.request('/api/works', {'action':'move','id':parent,'parent':child})[0],400)

if __name__=='__main__':unittest.main()
