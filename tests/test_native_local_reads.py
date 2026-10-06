"""#41 native local queries: only disposable synthetic packages and original byte contracts."""
from contextlib import closing
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sqlite3
import unittest
from unittest.mock import patch
from urllib.parse import urlencode

from tests import test_native_human_labels as labels, test_native_human_curation as curation, terms_seed
from tools.verify_read_http import difference, fetch
from web import names, library
from web.store import Store
from provenance import accounts


class NativeLocalReadsTest(unittest.TestCase):
    setUpClass = classmethod(curation.NativeHumanCurationTest.setUpClass.__func__)
    clone = labels.NativeHumanLabelsTest.clone
    start = labels.NativeHumanLabelsTest.start
    stop = labels.NativeHumanLabelsTest.stop
    seed_both = labels.NativeHumanLabelsTest.seed_both
    pair = curation.NativeHumanCurationTest.pair

    def setUp(self):
        labels.NativeHumanLabelsTest.setUp(self)
        # Both sides have no research workers; mirror native's explicit forced-off names.
        self.reference.names = names.NameUpdater(self.store, enabled=False)
        from web.index_update import IndexUpdater
        self.reference.indexer=IndexUpdater(self.store)
        self.env['ILLUSTASH_NO_KEYCHAIN'] = '1'
        self.env.pop('ILLUSTASH_DESKTOP_BRIDGE', None)
        self.env.pop('ILLUSTASH_DESKTOP_TOKEN', None)
        for key, value in [('APP_SUPPORT', self.root/'support')]:
            scoped=patch.object(accounts,key,value);scoped.start();self.addCleanup(scoped.stop)
        scoped=patch.dict(os.environ, {'ILLUSTASH_NO_KEYCHAIN':'1'});scoped.start();self.addCleanup(scoped.stop)
        def seed(store):
            with closing(sqlite3.connect(self.left.source)) as c:
                c.executemany('INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed) VALUES (?,?,?,?,?,?,?,?,?)',
                              [(f'SYN{i:02}',f'synthetic-{i}.png',str(self.root/f'synthetic-{i}.png'),120+i,150+i,'2026-09-22','orig',1,0) for i in range(20)])
                c.executemany('INSERT INTO labels VALUES (?,?,?,?)',[(f'SYN{i:02}',dim,tag,score) for i in range(20)
                              for dim,tag,score in [('character','alice_tag',.95),('general','solo',1.)]])
                c.commit()
            store.refresh()
        self.seed_both(seed)

    def get_pair(self,url,expected=200):
        before=[labels.states(lib) for lib in (self.left,self.right)]
        responses=[fetch(base,{'url':url},self.origin) for base in (self.direct,self.origin)]
        for r in responses:self.assertEqual(r['status'],expected,(url,r['body']))
        self.assertEqual(responses[1]['route'],'core')
        if expected<400:
            if url=='/api/names':
                # #1 extends only GET /api/names. Keep the frozen reference bytes
                # intact and append the exact unauthorized status for this fixture;
                # compare the complete native response, including the new field.
                self.assertTrue(responses[0]['body'].endswith(b'}'))
                responses[0]['body']=(responses[0]['body'][:-1]
                                      + b', "network": {"authorized": false, "granted_at": null, "this_run": false}}')
                responses[0]['headers']['Content-Length']=str(len(responses[0]['body']))
            self.assertIsNone(difference(*responses),(url,difference(*responses)))
            for h in labels.HEADERS:self.assertEqual(responses[0]['headers'].get(h),responses[1]['headers'].get(h),h)
        self.assertEqual(before,[labels.states(lib) for lib in (self.left,self.right)])
        self.assertFalse(self.sentinel.exists())
        return json.loads(responses[1]['body']) if expected<400 else None

    def test_retired_routes_are_unavailable_without_mutating_library(self):
        before=labels.states(self.right)
        for route in ('prefs','identity/status','identity/relations','identity/pairs','identity/assignments'):
            response=fetch(self.origin,{'url':'/api/'+route},self.origin)
            self.assertEqual(response['status'],400,(route,response['body']))
        for route in ('prefs','correct','character-work','identity/uncertain'):
            response=fetch(self.origin,{'url':'/api/'+route,'method':'POST','body':'{}','headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}},self.origin)
            self.assertEqual(response['status'],502,(route,response['body']))
        self.assertEqual(labels.states(self.right),before)
        self.assertFalse(self.sentinel.exists())

    def ask(self,query='',kind=None,entity=None,allowed=None):
        before=labels.states(self.right)
        for base in (self.direct,self.origin):
            r=fetch(base,{'url':'/api/ask'+('?' + query if query else '')},self.origin)
            self.assertEqual(r['status'],200,r['body'])
            if base==self.origin:self.assertEqual(r['route'],'core')
            batch=json.loads(r['body'])['batch']
            if kind is None:self.assertIsNone(batch);continue
            self.assertEqual(batch['kind'],kind)
            self.assertEqual(batch['entity'],entity)
            with self.store.connect() as c:
                self.assertEqual(batch['name'],c.execute('SELECT entity_name(?)',(entity,)).fetchone()[0])
                self.assertEqual(batch['confirmed'],c.execute("SELECT count(*) FROM memberships WHERE entity=? AND status='confirmed'",(entity,)).fetchone()[0])
                valid={u:(w,h) for u,w,h in c.execute('SELECT uuid,width,height FROM assets')}
            ids=[x['uuid'] for x in batch['items']]
            self.assertEqual(len(ids),len(set(ids)));self.assertTrue(0<len(ids)<=6)
            self.assertLessEqual(set(ids),set(allowed))
            for x in batch['items']:self.assertEqual((x['width'],x['height']),valid[x['uuid']])
        self.assertEqual(before,labels.states(self.right))
        return batch

    def test_seed_sample_skip_later_visibility_and_answer_loop(self):
        eligible={f'SYN{i:02}' for i in range(20)}
        batch=self.ask(kind='seed',entity=self.alice,allowed=eligible)
        self.ask(urlencode({'skip':self.alice}))
        self.get_pair('/api/ask?'+urlencode({'skip':','.join(['x']*51)}),400)
        self.get_pair('/api/ask?nsfw=wrong',400)
        chosen=batch['items'][0]['uuid']
        self.pair('/api/ask/answer',{'entity':self.alice,'yes':[chosen]})
        self.ask(kind='seed',entity=self.alice,allowed=eligible-{chosen})
        seeds={chosen,*sorted(eligible-{chosen})[:4]}
        def confirmed(store):store.answer(self.alice,yes=sorted(seeds-{chosen}),kind='seed')
        self.seed_both(confirmed)
        self.ask(kind='sample',entity=self.alice,allowed=eligible-seeds)
        later=sorted(eligible-seeds)[0];excluded=sorted(eligible-seeds)[1]
        self.seed_both(lambda store:store.answer(self.alice,later=[later]))
        self.ask()
        def expired(store):
            with store.connect() as c:c.execute("UPDATE asked SET asked_at=datetime('now','-8 days') WHERE answer='later'")
            store.curate([excluded],'exclude')
        self.seed_both(expired)
        self.ask(kind='sample',entity=self.alice,allowed=eligible-seeds-{later,excluded})

    def test_hints_first_score_order_skip_and_nsfw(self):
        def hints(store):
            with terms_seed.staged(store) as old:
                with old.connect() as c:
                    c.executemany("INSERT INTO weak_claims VALUES (?,?,'PROTO',?)",[('SYN00',self.bob,.9),('SYN01',self.bob,.8),('SYN02',self.bob,.7)])
            with closing(sqlite3.connect(old.path)) as src, closing(sqlite3.connect(store.path)) as dst:src.backup(dst)
        self.seed_both(hints)
        batch=self.ask(kind='hint',entity=self.bob,allowed={'SYN00','SYN01','SYN02'})
        self.assertEqual([x['uuid'] for x in batch['items']],['SYN00','SYN01','SYN02'])
        self.ask(urlencode({'skip':self.bob}),kind='seed',entity=self.alice,allowed={f'SYN{i:02}' for i in range(20)})
        self.ask('nsfw=only')
        self.ask('nsfw=hide',kind='hint',entity=self.bob,allowed={'SYN00','SYN01','SYN02'})

    def test_suggest_ids_filters_rejections_order_and_errors(self):
        for body in ({'ids':['b']},{'ids':['a','a','b','c']},{'filter':{}},{'filter':{},'exclude':['a']},
                     {'ids':['SYN00']*2001},{'filter':{'entity':self.alice}},{'filter':{'q':'synthetic'}}):
            self.pair('/api/suggest',body)
        for body in ({},{'ids':[]},{'ids':['missing']},{'ids':[1]},{'ids':'b'},{'filter':[]},
                     {'filter':{'unknown':'x'}},{'ids':['b'],'filter':{}},{'ids':['b'],'exclude':['a']},{'ids':['b'],'exclude':[1]}):
            self.pair('/api/suggest',body,expected=400)
        self.seed_both(lambda store:store.change(['b'],'rejected',self.bob))
        self.assertNotIn(self.bob,[r['id'] for r in self.pair('/api/suggest',{'ids':['b']})])

    def test_cached_groups_clusters_sync_and_worker_off(self):
        def cached(store):
            with store.connect() as c:
                c.execute("INSERT INTO relations(a,b,kind,changed,note) VALUES ('SYN00','SYN01','duplicate',0.9,'fixture')")
                c.executemany('INSERT INTO similar_groups VALUES (?,17)',[('SYN00',),('SYN01',)])
                c.execute("INSERT INTO cluster_info VALUES (9,2,NULL,NULL,'2026-10-01',NULL,'[\"合成\"]')")
                c.executemany('INSERT INTO clusters VALUES (?,9)',[('SYN18',),('SYN19',)])
                c.execute("INSERT INTO sync_runs(status,sources) VALUES ('done','[\"s1\"]')")
                c.execute("INSERT INTO sync_runs(status,sources) VALUES ('interrupted','')")
            store.change(['SYN18','SYN19'],'rejected',self.alice)
        self.seed_both(cached)
        for url in ['/api/similar','/api/similar?kind=duplicate','/api/clusters','/api/sync','/api/index',
                    '/api/index?progress=1','/api/provenance','/api/names','/api/names?progress=1']:
            if url=='/api/provenance':
                # #51 now supplies a coordinator even without computation workers.
                # Use the real idle Python coordinator for the original byte contract.
                from web.provenance_job import Runner
                self.reference.provenance=Runner(self.left)
                self.addCleanup(self.reference.provenance.close)
            result=self.get_pair(url)
            if url=='/api/similar':self.assertTrue(result['groups'])
            if url=='/api/clusters':self.assertTrue(result['clusters'])
        for url in ['/api/similar?kind=wrong','/api/similar?nsfw=wrong','/api/clusters?nsfw=wrong','/api/oc/groups?entity='+self.alice]:self.get_pair(url,400)

    def test_names_configuration_coverage_and_accounts(self):
        def config(store):
            with store.connect() as c:
                c.executemany('INSERT INTO name_settings VALUES (?,?)',[('deepseek_key','synthetic-key-1234'),('deepseek_model','synthetic:model')])
        self.seed_both(config)
        result=self.get_pair('/api/names');self.assertEqual(result['model_api']['hint'],'1234')
        support=self.root/'support';(support/'accounts').mkdir(exist_ok=True)
        (support/'accounts'/'saucenao').write_text('synthetic-placeholder')
        if os.name == 'nt':
            # Seed the native DPAPI format through its local API; the reference
            # still reads the existing synthetic plaintext file.
            response = fetch(self.origin, {
                'url': '/api/accounts/saucenao', 'method': 'POST',
                'body': json.dumps({'value': 'a' * 40}).encode('utf-8'),
                'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': self.token},
            }, self.origin)
            self.assertEqual(response['status'], 200, response['body'])
            self.assertTrue((support/'accounts'/'saucenao.dpapi').is_file())
        (support/'accounts.json').write_text(json.dumps({'pixiv':{'via':'app','name':'Synthetic User'},'saucenao':{'checked_at':'2026-10-01'}}))
        result=self.get_pair('/api/accounts');self.assertTrue(result['pixiv']['bound']);self.assertTrue(result['saucenao']['bound'])
        self.assertFalse(result['desktop'])

    def test_names_coverage_link_fallbacks(self):
        for system in ('photos_album', 'ccip', 'ccip_base'):
            with self.subTest(system=system):
                def links(store):
                    with terms_seed.staged(store) as old:
                        with old.connect() as c:
                            c.execute('DELETE FROM links WHERE entity_id IN (?,?)', (self.alice, self.bob))
                            for entity, key in ((self.alice, 'synthetic_alice'), (self.bob, 'synthetic_bob')):
                                value = json.dumps([key, 'synthetic_work']) if system == 'ccip_base' else key
                                c.execute("INSERT INTO links(system,key,entity_id,current,source) VALUES (?,?,?,1,'synthetic')", (system, value, entity))
                    with closing(sqlite3.connect(old.path)) as src, closing(sqlite3.connect(store.path)) as dst:
                        src.backup(dst)
                self.seed_both(links)
                self.get_pair('/api/names')

    def test_about_reports_compiled_host_without_python(self):
        r=fetch(self.origin,{'url':'/api/about'},self.origin)
        self.assertEqual((r['status'],r['route']),(200,'core'))
        info=json.loads(r['body'])
        self.assertEqual((info['name'],info['version'],info['channel'],info['schema']),('Illustash','0.1.0','预览版',36))
        self.assertIsNone(info['python']);self.assertTrue(info['runtime'].startswith('rustc '))
        self.assertRegex(info['build'],r'^[0-9a-f]{7,40}(?:-dirty)?$');self.assertIn('(',info['platform'])
        self.assertTrue(all(v is None for v in info['libraries'].values()))
        self.assertFalse(self.sentinel.exists())

    def test_concurrent_cached_reads_and_suggest_then_switch(self):
        urls=['/api/similar','/api/clusters','/api/sync','/api/names','/api/about','/api/ask']*4
        before=labels.states(self.right)
        with ThreadPoolExecutor(max_workers=8) as pool:
            result=list(pool.map(lambda u:fetch(self.origin,{'url':u},self.origin),urls))
        self.assertTrue(all((r['status'],r['route'])==(200,'core') for r in result))
        self.assertEqual(before,labels.states(self.right))
        replacement=self.clone(self.left,'replacement')
        new_store=Store.open(replacement.source,replacement.db);new_store.curate([f'SYN{i:02}' for i in range(20)],'exclude')
        response=fetch(self.origin,{'url':'/api/libraries/open','method':'POST','body':json.dumps({'path':str(replacement.path)}),
                        'headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}},self.origin)
        self.assertEqual(response['status'],200,response['body'])
        self.assertIsNone(json.loads(fetch(self.origin,{'url':'/api/ask'},self.origin)['body'])['batch'])
        old=fetch(self.origin,{'url':'/api/suggest','method':'POST','body':'{"ids":["b"]}',
                   'headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}},self.origin)
        self.assertEqual(old['status'],403)


    def test_suggest_2000_cap_nine_limit_and_no_copy_expansion(self):
        # a/b are synthetic duplicates; selecting a alone must not include b's alternatives.
        self.assertEqual(self.pair('/api/suggest',{'ids':['a']}),[])
        targets=[]
        def seed(store):
            targets.extend(store.create_entity('Synthetic alternative '+str(i))['id'] for i in range(11))
            with closing(sqlite3.connect(self.left.source)) as c:
                c.executemany('INSERT INTO assets(uuid,filename,src_path,width,height,added_at,src_kind,in_scope,trashed) VALUES (?,?,?,?,?,?,?,?,?)',
                              [(f'SUG{i:04}',f'synthetic-suggest-{i}.png',str(self.root/f'synthetic-suggest-{i}.png'),10,20,'2026-10-01','orig',1,0) for i in range(2001)])
                c.commit()
            store.refresh()
            with terms_seed.staged(store) as old:
                with old.connect() as c:
                    c.executemany("INSERT INTO weak_claims VALUES (?,?,'PROTO',?)",[(f'SUG{i:04}',targets[0],.8) for i in range(2000)]+[('SUG2000',targets[1],.99)]+[('SUG0000',t,.5) for t in targets[2:]])
            with closing(sqlite3.connect(old.path)) as src,closing(sqlite3.connect(store.path)) as dst:src.backup(dst)
        self.seed_both(seed)
        ids=[f'SUG{i:04}' for i in range(2001)]
        result=self.pair('/api/suggest',{'ids':ids})
        self.assertEqual(len(result),9);self.assertEqual(result[0]['count'],2000)
        self.assertNotIn(targets[1],[r['id'] for r in result])
        result=self.pair('/api/suggest',{'ids':list(reversed(ids))})
        self.assertIn(targets[1],[r['id'] for r in result]);self.assertEqual(result[0]['count'],1999)

    def test_partial_suggest_body_cannot_cross_library_switch(self):
        import http.client
        connection=http.client.HTTPConnection('127.0.0.1',self.port,timeout=5)
        body=b'{"ids":["b"]}'
        connection.putrequest('POST','/api/suggest');connection.putheader('Host',self.origin.split('://')[1])
        connection.putheader('Origin',self.origin);connection.putheader('X-CSRF-Token',self.token)
        connection.putheader('Content-Type','application/json');connection.putheader('Content-Length',str(len(body)))
        connection.endheaders();connection.send(body[:3])
        replacement=self.clone(self.left,'partial-replacement');before=labels.states(replacement)
        try:
            opened=fetch(self.origin,{'url':'/api/libraries/open','method':'POST','body':json.dumps({'path':str(replacement.path)}),
                         'headers':{'Content-Type':'application/json','X-CSRF-Token':self.token}},self.origin)
            self.assertEqual(opened['status'],200,opened['body'])
            connection.send(body[3:]);response=connection.getresponse();response.read();self.assertEqual(response.status,403)
            self.assertEqual(before,labels.states(replacement))
        finally:connection.close()

    def test_hint_top_twelve_pool_after_skip(self):
        ids=[]
        def seed(store):
            ids.extend(store.create_entity('Synthetic hint '+str(i))['id'] for i in range(13))
            with terms_seed.staged(store) as old:
                with old.connect() as c:
                    c.executemany("INSERT INTO weak_claims VALUES (?,?,'PROTO',?)",[(f'SYN{j:02}',entity,.9-j*.01) for i,entity in enumerate(ids) for j in range(i+2)])
            with closing(sqlite3.connect(old.path)) as src,closing(sqlite3.connect(store.path)) as dst:src.backup(dst)
        self.seed_both(seed)
        for base in (self.direct,self.origin):
            for _ in range(24):
                r=fetch(base,{'url':'/api/ask'},self.origin);self.assertEqual(r['status'],200)
                batch=json.loads(r['body'])['batch'];self.assertEqual(batch['kind'],'hint')
                self.assertIn(batch['entity'],ids[1:]);self.assertLessEqual(len(batch['items']),6)
        self.ask(urlencode({'skip':','.join(ids[1:])}),kind='hint',entity=ids[0],allowed={'SYN00','SYN01'})

    def test_read_sql_failures_status_only_and_no_partial_data(self):
        for lib in (self.left,self.right):
            with closing(sqlite3.connect(lib.db)) as c:c.execute('DROP TABLE sync_runs');c.commit()
        self.get_pair('/api/sync',500)
        self.pair('/api/suggest',{'ids':['b']},headers={'X-CSRF-Token':'bad'},expected=403)
        self.pair('/api/suggest',{'ids':['b']},headers={'Origin':'https://synthetic.invalid'},expected=403)
