"""R1–R2 raw HTTP acceptance. All databases, state and responses are synthetic/temporary."""
import gzip
import hashlib
import http.client
import json
import os
import unittest
from urllib.parse import urlencode

from tests.read_http_harness import HTTPHarness
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
        state = json.loads(self.state.read_text()); state['tags'] = str(csv)
        self.state.write_text(json.dumps(state))
        self.assertEqual(json.loads(self.pair('/api/general-tags')['body']), tags[:30])
        for q in ('STRASSE', 'i\u0307', 'σασ', '雪🙂', 'comma,', 'line', 'ＡＢ', 'not_general'):
            self.pair('/api/general-tags?'+urlencode({'q': q}))
        self.sync('tagged')
        self.assertTrue(json.loads(self.pair('/api/user-tags')['body']))
        for q in ('雪', 'tie_', ' ', '%', '标签', '\x1c雪\x1f'):
            self.pair('/api/user-tags?'+urlencode({'q':q}))

    def test_repeated_search_no_request_local_temp_state_and_library_generation(self):
        for q in ('Alice', 'nobody', '', '东方', '', 'Alice'):
            self.pair('/api/entities?'+urlencode({'q':q}))
        before = self.pair('/api/entities')['body']
        other = self.root/'other'; other.mkdir(exist_ok=True)
        self.sync('other'); self.publish()
        response = self.pair('/api/entities')
        self.assertNotEqual(response['body'], before)

    def test_equal_sort_keys_and_legacy_name_fallback(self):
        ties=self.root/'ties'; ties.mkdir(exist_ok=True)
        self.sync('ties'); self.publish()
        # The library before the switch (no `meta`, see `after_switch`): all 6 recorded replies equal the ones after it.
        before = [self.rec.value('response') for _ in range(6)]
        self.sync('switched'); self.publish()
        compared = self.after_switch(before, differ=set())
        for path in ('/api/entities', '/api/entities?q='+urlencode({'q':'排序平局'}).split('=',1)[1], '/api/works', '/api/works?'+urlencode({'q':'作品平局'})):
            self.pair(path)
        # Explicitly check repeated requests with identical SQL sort keys.
        first=self.pair('/api/entities?'+urlencode({'q':'排序平局'}))['body']
        self.assertEqual(first,self.pair('/api/entities?'+urlencode({'q':'排序平局'}))['body'])
        self.assertEqual(len(compared), 6)

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
            a,b=self.rec.value('response'),fetch(self.origin)
            self.assertEqual(b['route'], 'core')
            self.assertEqual(a['headers'].get('Content-Encoding'), b['headers'].get('Content-Encoding'))
            if compressed: self.assertEqual(gzip.decompress(b['body']), baseline)
            else: self.assertIsNone(verify.difference(a,b))


class ArtistReadHTTPTest(HTTPHarness):
    @unittest.skipIf(os.name == 'nt', '固定期望录自 POSIX，包名含 Windows 不允许的 ?')
    def test_auxiliary_artist_reads_legacy_then_core_with_fixed_hash_order(self):
        package = self.root/'Artists #雪?.pilib'
        ids = self.rec.value('ids')
        self.sync('artists'); self.publish()
        # The library before the switch (no `meta`, see `after_switch`): 32 recorded replies.
        before = [self.rec.value('response') for _ in range(32)]
        self.sync('switched'); self.publish()
        # The first 9 were recorded only before the switch: the audit with unusual evidence counts in the provenance
        # database (json.loads accepts nonfinite values and integers beyond f64 precision). Before the switch the reason
        # quoted that count ("nan", "inf", "True" … 张); after it the count comes from the core's attribution, which the
        # switch also changed (artist A: 2 images before, 1 after). So they cannot equal the replies recorded before;
        # on v8 the evidence count must not change the reply at all.
        prov = package/'provenance.sqlite'
        with self.connect(prov) as pc:
            evidence = pc.execute("SELECT rowid,evidence FROM image_artists WHERE artist_id='C'").fetchall()
        replies = []
        for count in ('NaN','Infinity','-Infinity','1e400','123456789012345678901234567890',
                      '{"z":"雪🙂", "a":[1.0,true,null]}', 'null', 'true', '"text"'):
            with self.connect(prov) as pc:
                pc.execute("UPDATE image_artists SET evidence=? WHERE artist_id='C'",('{"count":'+count+'}',)); pc.commit()
            replies.append(verify.fetch(self.origin, {'url': '/api/artist-audit', 'headers': {}}, self.origin))
        with self.connect(prov) as pc:
            pc.executemany("UPDATE image_artists SET evidence=? WHERE rowid=?", [(e, r) for r, e in evidence]); pc.commit()
        self.assertEqual({(r['status'], r['body']) for r in replies}, {(200, replies[0]['body'])})
        # The other 23 follow after the switch. 14 equal the replies recorded before it. The 9 others differ because the
        # switch changed the artists: M's name (a legacy artist table name before, "未知画师" after), the image counts
        # in the audits (A: 2 → 1, and extra homes), and with the provenance database parked or unreadable the legacy
        # reads went through it (an empty pool, no name, a 500) while the core's do not.
        compared = self.after_switch(before[9:], differ={3, 10, 11, 12, 13, 16, 17, 18, 21})
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
        parked=package/'parked-provenance.sqlite'
        prov.rename(parked)
        try:
            no_aux=json.loads(self.pair('/api/artist-audit')['body'])
            self.assertEqual(no_aux['pool'],2)
            self.assertTrue(no_aux['items']); self.assertTrue(all(not x['posts'] for x in no_aux['items']))
            self.pair('/api/artist?id=A')
            prov.mkdir()  # exists, but cannot be opened as a SQLite database
            self.assertEqual(self.pair('/api/artist-audit?n=bad')['status'],400)
            self.assertEqual(self.pair('/api/artist-audit')['status'],500)
            self.assertEqual(self.pair('/api/artist?id=A')['status'],200)
            self.pair('/api/entities')  # an unrelated auxiliary failure must not affect local reads
        finally:
            if prov.is_dir(): prov.rmdir()
            parked.rename(prov)
        self.assertEqual(len(compared), 23)

