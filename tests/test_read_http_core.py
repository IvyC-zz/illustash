"""R1–R2 raw HTTP acceptance. All databases, state and responses are synthetic/temporary."""
import gzip
import hashlib
import http.client
import json
import os
import unittest
from urllib.parse import urlencode

from tests.read_http_harness import RECORD, HTTPHarness
from tools import verify_read_http as verify


class CoreReadHTTPTest(HTTPHarness):
    def test_local_names_corpus_nonempty_empty_unicode_and_order(self):
        requests = self.corpus(sample=1, query_limit=220, keep=lambda r: r['stage'] in ('R1', 'R2'))
        report = self.compare(requests, require_core=['R1', 'R2'])
        self.assertEqual(report['differ'], 0, report['differences'])
        self.assertEqual(report['compared'], report['core_equal'])
        self.assertGreater(report['core_equal'], 700)
        self.assertTrue(json.loads(self.pair('/api/entities')['body']))
        self.assertTrue(json.loads(self.pair('/api/works')['body']))
        for path in ('entities', 'works', 'user-tags', 'general-tags'):
            self.assertEqual(json.loads(self.pair(f'/api/{path}?q=no-such-雪🙂')['body']), [])
            for q in ('Ｓｅｒｉｅｓ', 'Ⅰ', 'Straße', 'İ', 'ΣΑΣ', '蓝档案', 'シリーズ', '雪🙂', ' · ', '\x1cAlice\x1f', '%_\\', 'A'*200):
                self.pair(f'/api/{path}?'+urlencode({'q': q}))

    def test_repeat_blank_and_error_parameters_security_remote(self):
        for path in ('entities', 'works', 'user-tags', 'general-tags'):
            for q in ('q=Alice&q=&q=雪', 'q=Alice&q=', 'q=&q=', 'q=%FF%FE', 'q=+%1C+', 'q=Alice;other=雪', 'q=Alice#q=雪'):
                self.pair(f'/api/{path}?{q}')
        for n in ('0', '-1', '501', '1.2', 'bad', '0x10', '+', '--1', '1__0', "a'b", '"', ' １_０ ', '١٢', '1_', '_1', '9'*100, '9'*5000, '\nNaN\t', '\u200b', '\u0378', '\U000e0001', '\u2028', '\x1c2\x1f', '\x852\x85', '\U00011f50\U00011f52'):
            self.pair('/api/artist-audit?'+urlencode({'n': n}))
        for query in ('n=', 'n=bad&n=', 'n=bad&n=2', 'n=2&n=bad', 'n=+%2B2+'):
            self.pair('/api/artist-audit?'+query)
        for path in ('entities', 'works'):
            self.pair(f'/api/{path}?remote=', route='core')

    def test_csv_order_unicode_casefold_and_nonempty_tags(self):
        csv = self.root/'tags.csv'
        tags = ['z_first', 'Straße', 'İstanbul', 'ΣΑΣ', '雪🙂', 'comma,tag', 'line\nbreak', 'ＡＢ'] + [f'tag_{i:02}' for i in range(35)]
        import csv as csv_module
        with csv.open('w', newline='') as f:
            writer=csv_module.writer(f); writer.writerow(['name', 'category']);
            for tag in tags: writer.writerow([tag, '0'])
            writer.writerow(['not_general', '1'])
        if RECORD:
            self.store.tag_names = tags
        state = json.loads(self.state.read_text()); state['tags'] = str(csv)
        self.state.write_text(json.dumps(state))
        self.assertEqual(json.loads(self.pair('/api/general-tags')['body']), tags[:30])
        for q in ('STRASSE', 'i\u0307', 'σασ', '雪🙂', 'comma,', 'line', 'ＡＢ', 'not_general'):
            self.pair('/api/general-tags?'+urlencode({'q': q}))
        if RECORD:
            ids = list(self.store.uuids())[:2]
            self.store.tag(ids, '雪\t标签')
            self.store.tag(ids[:1], 'tie_a')
            self.store.tag(ids[1:], 'tie_z')
        self.sync('tagged')
        self.assertTrue(json.loads(self.pair('/api/user-tags')['body']))
        for q in ('雪', 'tie_', ' ', '%', '标签', '\x1c雪\x1f'):
            self.pair('/api/user-tags?'+urlencode({'q':q}))

    def test_repeated_search_no_request_local_temp_state_and_library_generation(self):
        for q in ('Alice', 'nobody', '', '东方', '', 'Alice'):
            self.pair('/api/entities?'+urlencode({'q':q}))
        before = self.pair('/api/entities')['body']
        other = self.root/'other'; other.mkdir(exist_ok=True)
        if RECORD:
            from tests import terms_seed
            # Reuse the existing fixture definition rather than any real library.
            self.server.store = terms_seed.switch(terms_seed.base(other))
        self.sync('other'); self.publish()
        response = self.pair('/api/entities')
        self.assertNotEqual(response['body'], before)

    def test_equal_sort_keys_and_legacy_name_fallback(self):
        ties=self.root/'ties'; ties.mkdir(exist_ok=True)
        if RECORD:
            from tests import terms_names_fixture, terms_seed
            replacement,_=terms_names_fixture.build(ties,legacy=True)
            with replacement.connect() as c:
                ids=[r[0] for r in c.execute("SELECT id FROM entities WHERE parent_id IS NULL AND kind<>'work' AND merged_into IS NULL LIMIT 3")]
                for id in ids:
                    c.execute("UPDATE entities SET original=1 WHERE id=?",(id,))
                    c.execute("DELETE FROM names WHERE entity_id=?",(id,))
                    c.execute("INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES (?,'zh','排序平局','user','display',1,0)",(id,))
                for (id,) in c.execute("SELECT id FROM entities WHERE kind='work' AND merged_into IS NULL").fetchall():
                    c.execute("DELETE FROM names WHERE entity_id=? AND source='user'",(id,))
                    c.execute("INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES (?,'zh','作品平局','user','display',1,0)",(id,))
            self.server.store=replacement
        self.sync('ties'); self.publish()
        for core in (False,True):
            if core:
                if RECORD: terms_seed.switch(replacement)
                self.sync('switched'); self.publish()
            for path in ('/api/entities', '/api/entities?q='+urlencode({'q':'排序平局'}).split('=',1)[1], '/api/works', '/api/works?'+urlencode({'q':'作品平局'})):
                self.pair(path)
            # Explicitly check repeated requests with identical SQL sort keys.
            first=self.pair('/api/entities?'+urlencode({'q':'排序平局'}))['body']
            self.assertEqual(first,self.pair('/api/entities?'+urlencode({'q':'排序平局'}))['body'])

    def test_gzip_accept_encoding_retains_headers_and_core_execution(self):
        baseline = self.pair('/api/entities')['body']
        self.assertGreater(len(baseline), 1024)
        for accepted, compressed in [('gzip', True), ('GZip;q=1', True), ('*;q=0.5', True),
                                     ('gzip;q=0,*;q=1', False), ('gzip;q=bad', False), ('identity', False)]:
            item={'url':'/api/entities', 'headers':{'Accept-Encoding':accepted}}
            def fetch(base):
                conn=http.client.HTTPConnection('127.0.0.1',int(base.rsplit(':',1)[1]))
                try:
                    conn.request('GET',item['url'],headers={'Host':self.origin.split('://')[1], 'Origin':self.origin, **item['headers']})
                    response=conn.getresponse()
                    return {'status':response.status,'body':response.read(),'headers':dict(response.getheaders()),'route':response.getheader('X-Illustash-Route')}
                finally:conn.close()
            a,b=self.rec.value('response', lambda: fetch(self.direct)),fetch(self.origin)
            self.assertEqual(b['route'], 'core')
            self.assertEqual(a['headers'].get('Content-Encoding'), b['headers'].get('Content-Encoding'))
            if compressed: self.assertEqual(gzip.decompress(b['body']), baseline)
            else: self.assertIsNone(verify.difference(a,b))


class ArtistReadHTTPTest(HTTPHarness):
    @unittest.skipIf(os.name == 'nt', '固定期望录自 POSIX，包名含 Windows 不允许的 ?')
    def test_auxiliary_artist_reads_legacy_then_core_with_fixed_hash_order(self):
        package = self.root/'Artists #雪?.pilib'
        if RECORD:
            self.build_artists(package)
        ids = self.rec.value('ids', lambda: self.ids)
        self.sync('artists'); self.publish()
        for core in (False,True):
            if core:
                if RECORD:
                    from tests import terms_seed
                    from web.server import adopt_artists
                    self.assertTrue(adopt_artists(self.server.store,self.server.library))
                    terms_seed.switch(self.server.store,artists=False)
                self.sync('switched'); self.publish()
            if not core:
                # json.loads accepts nonfinite values and integers beyond f64 precision.
                for count in ('NaN','Infinity','-Infinity','1e400','123456789012345678901234567890',
                              '{"z":"雪🙂", "a":[1.0,true,null]}', 'null', 'true', '"text"'):
                    with self.connect(package/'provenance.sqlite') as pc:
                        pc.execute("UPDATE image_artists SET evidence=? WHERE artist_id='C'",('{"count":'+count+'}',)); pc.commit()
                    self.pair('/api/artist-audit')
                with self.connect(package/'provenance.sqlite') as pc:
                    pc.execute("UPDATE image_artists SET evidence='{}' WHERE artist_id='C'"); pc.commit()
            for id in ('A','B','C','M','D','N','missing','雪🙂',''):
                self.pair('/api/artist?'+urlencode({'id':id}))
            self.assertTrue(json.loads(self.pair('/api/artists')['body']))
            for n in ('1','2','50','500','0','bad'):
                self.pair('/api/artist-audit?'+urlencode({'n':n}))
            result=json.loads(self.pair('/api/artist-audit')['body'])
            self.assertEqual(result['pool'],2)
            self.assertEqual([x['uuid'] for x in result['items']],sorted(ids[:2],key=lambda a:hashlib.sha1(f'artist-audit-v1:{a}'.encode()).hexdigest()))
            sample=next(x for x in result['items'] if x['uuid']==ids[0])
            self.assertTrue(sample['artists']); self.assertEqual(len(sample['maybe']),2)
            self.assertTrue(sample['artists'][0]['homes']); self.assertTrue(sample['posts'])
            prov=package/'provenance.sqlite'; parked=package/'parked-provenance.sqlite'
            prov.rename(parked)
            try:
                no_aux=json.loads(self.pair('/api/artist-audit')['body'])
                self.assertEqual(no_aux['pool'],2 if core else 0)
                if core: self.assertTrue(no_aux['items']); self.assertTrue(all(not x['posts'] for x in no_aux['items']))
                self.pair('/api/artist?id=A')
                prov.mkdir()  # exists, but cannot be opened as a SQLite database
                self.assertEqual(self.pair('/api/artist-audit?n=bad')['status'],400)
                self.assertEqual(self.pair('/api/artist-audit')['status'],500)
                self.assertEqual(self.pair('/api/artist?id=A')['status'],200 if core else 500)
                self.pair('/api/entities')  # an unrelated auxiliary failure must not affect local reads
            finally:
                if prov.is_dir(): prov.rmdir()
                parked.rename(prov)

    def build_artists(self, package):
        """Recording only: the synthetic artist inputs, written through the former Python application."""
        from provenance import db
        from tests import terms_seed
        from web import library
        lib = library.create(package)
        store = terms_seed.startup_input(self.store, db=lib.db)
        with store.connect() as c:
            # OC statistics and stable tied ordering, irrespective of provenance artists.
            ids_stats=[r[0] for r in c.execute("SELECT id FROM entities WHERE parent_id IS NULL AND kind<>'work' AND merged_into IS NULL LIMIT 4")]
            for id,artist in zip(ids_stats,('雪画师','A画师','雪画师','Z画师')):
                c.execute("UPDATE entities SET original=1,artist=? WHERE id=?",(artist,id))
        with db.connect(lib) as c:
            for id in ('A','B','C','M','D','N'):
                c.execute('INSERT INTO artists(id) VALUES (?)', (id,))
            c.execute("UPDATE artists SET merged_into='A' WHERE id='M'")
            for system, key, id in [('x','雪_account','A'),('x_id','123','A'),('pixiv','42','A'),('danbooru','snow_artist','A'),
                                    ('x_id','99','B'),('danbooru','fallback_name','B'),('name','Name key','C'),('pixiv','77','D')]:
                c.execute('INSERT INTO artist_links(system,key,artist_id,url,source) VALUES (?,?,?,\'unused\',\'test\')',(system,key,id))
            c.execute("INSERT INTO artist_names(artist_id,source,name,updated_at) VALUES ('A','pixiv','雪🙂\"\\名','2026-09-30 00:00:00')")
            ids=list(store.uuids())[:3]
            self.assertGreaterEqual(len(ids),3)
            c.executemany('INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?)',[
                (ids[0],'A','pixel_hash',1.0,'{}','auto'),(ids[0],'B','iqdb',0.99,'{}','auto'),
                (ids[0],'C','group_oc',0.5,'{"z":0,"count":1.0,"a":{"z":2,"a":3}}','candidate'),
                (ids[0],'D','group_run',0.5,'{"count":5.72e-5}','candidate'),
                (ids[1],'M','gallery_match',0.99,'{}','auto'),(ids[1],'B','ocr',0.8,'broken-json','candidate'),
                (ids[2],'B','ocr',0.1,'{}','candidate'),('removed-asset','A','pixel_hash',1.0,'{}','auto')])
            for asset,site,post,id,method in [(ids[0],'danbooru','01','A','pixel_hash'),(ids[0],'pixiv','2','B','gallery_match'),
                (ids[0],'pixiv','1','A','pixel_hash'),(ids[0],'x','3','B','gallery_match'),(ids[1],'unknown','1','M','gallery_match')]:
                c.execute('INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?,?,?,?,?,?)',(asset,site,post,f'https://example.test/{site}/{post}',id,method))
        with store.connect() as c:
            c.execute('INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES (?,\'B\',\'no\')',(ids[0],))
            c.execute('INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES (?,\'B\',\'yes\')',(ids[2],))
            c.execute('INSERT INTO actions(label,before_json) VALUES (\'synthetic artist verdicts\',?)',
                      (json.dumps([{'artist_verdict':{'uuid':ids[0],'artist':'B','had':None}}, {'artist_verdict':{'uuid':ids[2],'artist':'B','had':None}}]),))
        self.ids = ids
        self.server.store=store; self.server.library=lib
