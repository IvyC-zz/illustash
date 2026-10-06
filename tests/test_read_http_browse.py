"""R3 byte-strict browsing acceptance using only temporary synthetic libraries."""
import json
import os
from urllib.parse import urlencode
from tests.test_read_http_core import HTTPHarness
from tools import verify_read_http as verify


class BrowseReadHTTPTest(HTTPHarness):
    def test_page_names_refresh_after_write_and_library_switch_in_hybrid(self):
        from tests import terms_seed
        root = self.root/'page-name-refresh'; root.mkdir()
        replacement = terms_seed.base(root)
        replacement.activate_identity('hybrid')
        terms_seed.switch(replacement)
        original = self.server.store
        try:
            self.server.store = replacement; self.publish()
            first = self.pair('/api/assets?limit=200')['body']
            items = json.loads(first)['items']
            member = next(m for row in items for m in row['memberships'])
            self.assertIn('node_type', member)
            replacement.set_profile(member['id'], name='分页更新🙂')
            updated = self.pair('/api/assets?limit=200')['body']
            self.assertNotEqual(first, updated)
            self.assertIn('分页更新🙂', updated.decode())
            for query in ('limit=1', 'limit=200&offset=1', 'offset=1000',
                          'pool=unmatched', 'order=hinted', 'order=suspicious',
                          'nsfw=hide', 'nsfw=only'):
                self.pair('/api/assets?'+query)
        finally:
            self.server.store = original; self.publish()
        self.pair('/api/assets?limit=200')

    def test_browse_corpus(self):
        migrated={'/api/assets','/api/assets/count','/api/ids','/api/detail','/api/home','/api/fold','/api/review-pools'}
        requests=[r for r in verify.corpus(self.store.path,sample=2,query_limit=30)
                  if r['url'].split('?',1)[0] in migrated]
        report=verify.compare(self.direct,self.origin,self.origin,requests,require_core=['R3'])
        self.assertEqual(report['differ'],0,report['differences'])
        self.assertEqual(report['compared'],report['core_equal'])
        self.assertGreater(report['compared'],70)

    def test_filter_validation_and_precedence(self):
        for path in ('assets','assets/count','ids'):
            for query in ({'pool':'invalid'},{'nsfw':'invalid'},{'entity':'missing'},{'variant':'missing'},
                          {'status':'invalid'},{'status':'maybe'},{'tag':'missing'},
                          {'reason':'manual'},{'dup':'invalid'},{'order':'invalid'},
                          {'work':'missing'},{'day':'invalid'},{'pool':'cluster'},
                          {'pool':'similar'},{'pool':'cluster','cluster':'²'},
                          {'pool':'cluster','cluster':'①'},{'pool':'similar','cluster':'9'*100},
                          {'pool':'cluster','cluster':'9'*100,'nsfw':'bad'},
                          {'pool':'cluster','cluster':'9'*100,'order':'bad'},
                          {'q':'雪🙂'},{'q':'%_\\'},{'artist':'missing'}):
                self.pair('/api/'+path+'?'+urlencode(query))
            for query in ('unknown=1','offset=bad&limit=bad','limit=bad&offset=0',
                          'limit=201','limit=0','offset=-1','offset=10000001',
                          'offset=&limit=','pool=bad&nsfw=bad','status=maybe&pool=bad'):
                self.pair('/api/'+path+'?'+query)
        for query in ('days=bad','days=0','days=21','before=bad&days=0','before=bad&days=bad',
                      'nsfw=bad&before=bad','before=2024-02-29','before=2023-02-29',
                      'before=2024-2-9','before=0001-01-01','before=9999-12-31'):
            self.pair('/api/home?'+query)
        for nsfw in ('','hide','only','invalid'):
            self.pair('/api/review-pools?'+urlencode({'nsfw':nsfw}))
        self.pair('/api/detail?id=missing')

    def test_database_error_precedes_review_pool_rating_validation(self):
        original = self.store.path
        corrupt = self.root/'corrupt-temporary.sqlite'
        corrupt.write_bytes(b'not a SQLite database')
        try:
            self.store.path = corrupt; self.publish()
            self.assertEqual(self.pair('/api/review-pools?nsfw=invalid')['status'],500)
        finally:
            self.store.path = original; self.publish()

    def test_decorated_paging_exact_for_core(self):
        with self.store.connect() as c:
            entities=[r[0] for r in c.execute("SELECT id FROM entities WHERE kind<>'work' AND merged_into IS NULL")]
            variants=[r[0] for r in c.execute('SELECT id FROM entities WHERE parent_id IS NOT NULL')]
        for nsfw in ('','hide','only'):
            for order in ('newest','oldest','suspicious','hinted'):
                for pool in ('','unmatched','skipped','duplicates','excluded','pending','recent','audit'):
                    filters={'nsfw':nsfw,'order':order,'pool':pool}
                    assets=json.loads(self.pair('/api/assets?'+urlencode({**filters,'limit':2,'count':0}))['body'])
                    ids=json.loads(self.pair('/api/ids?'+urlencode({**filters,'limit':2}))['body'])
                    self.assertEqual([r['uuid'] for r in assets['items']],ids)
                    self.pair('/api/assets?'+urlencode({**filters,'limit':2,'offset':1}))
                    self.pair('/api/assets/count?'+urlencode(filters))
        for entity in entities:
            for status in ('all','machine','confirmed','maybe','ambiguous','unknown'):
                filters={'entity':entity,'status':status,'order':'suspicious'}
                self.pair('/api/assets?'+urlencode(filters))
                self.pair('/api/ids?'+urlencode(filters))
        for variant in variants:
            self.pair('/api/assets?'+urlencode({'variant':variant}))
        with self.store.connect() as c:
            ids=[r[0] for r in c.execute('SELECT uuid FROM assets')]
        for id in ids:self.pair('/api/detail?'+urlencode({'id':id}))
        for nsfw in ('','hide','only'):
            self.pair('/api/home?'+urlencode({'nsfw':nsfw,'days':2}))

    def test_expanded_legacy_core_visibility_evidence_and_auxiliary_reads(self):
        import sqlite3
        from web import artists, library
        from provenance import db
        from tests import terms_names_fixture, terms_seed
        from web.server import adopt_artists
        # Keep the POSIX question mark; Windows still exercises space, Unicode, # and %.
        name = 'Browse #雪%.pilib' if os.name == 'nt' else 'Browse #雪?.pilib'
        lib=library.create(self.root/name)
        seed=self.root/'browse-expanded'; seed.mkdir()
        store,attrs=terms_names_fixture.build(seed,legacy=True)
        alice,bob=attrs['alice'],attrs['bob']
        with sqlite3.connect(store.source) as c:
            for i,date in enumerate(('2026-09-23 01:00:00','2026-09-23 01:00:00','2026-09-24',
                                     '2026-09-24','2026-09-25','2026-09-25','2026-09-25','2026-09-25')):
                id=f'extra-{i}'
                c.execute('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)',(id,id+'.png','',80+i,90+i,date,'orig',1,0))
                c.execute("INSERT INTO labels VALUES (?,'general','blue_hair',?)",(id,.9))
                c.execute("INSERT INTO labels VALUES (?,'rating',?,.99)",(id,'explicit' if i%2 else 'general'))
            c.executemany("INSERT INTO labels VALUES ('a','general',?,.8)",[(f'float-tie-{i}',) for i in range(28)])
        with store.connect() as c:
            with sqlite3.connect(store.source) as src:
                extra=src.execute("SELECT uuid,filename,src_path,width,height,added_at,src_kind FROM assets WHERE uuid LIKE 'extra-%'").fetchall()
            c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,?)',extra)
            for id,entity in [('extra-0',alice),('extra-1',alice),('extra-2',alice),('extra-3',bob)]:
                c.execute("INSERT INTO assertions VALUES (?,?,'confirmed','review')",(id,entity))
            c.execute("INSERT INTO candidates VALUES ('extra-4',?,'WD',.91)",(alice,))
            c.execute("INSERT INTO candidates VALUES ('extra-4',?,'CCIP',.92)",(alice,))
            c.execute("INSERT INTO weak_claims VALUES ('extra-6',?,'PROTO',.6)",(alice,))
            c.execute("INSERT INTO weak_claims VALUES ('extra-7',?,'PROTO',.6)",(bob,))
            c.execute("INSERT INTO deferred VALUES ('extra-5')")
            c.execute("INSERT INTO curation(uuid,status,origin) VALUES ('extra-3','excluded','review')")
            c.execute("INSERT INTO excluded VALUES ('extra-3','manual','review')")
            c.execute("INSERT INTO auto_rules VALUES ('extra-4','pending','text')")
            c.execute("INSERT INTO sync_added VALUES ('extra-6',1)")
            c.execute("INSERT INTO audit_sample(uuid,entity,stratum,solo) VALUES ('extra-0',?,'uniform',1)",(alice,))
            for id,grp,pos,rep in [('extra-0','same',0,1),('extra-1','same',1,0),('extra-2','mixed',0,1),('extra-3','mixed',1,0)]:
                c.execute('INSERT INTO duplicates VALUES (?,?,2,?,?,?,0)',(id,grp,pos,'2026-09-24',rep))
            c.execute('INSERT INTO clusters VALUES (\'extra-6\',7),(\'extra-7\',7)')
            c.execute('INSERT INTO similar_groups VALUES (\'extra-0\',9),(\'extra-1\',9),(\'extra-2\',9)')
            store._refresh_memberships(c)
            c.execute("INSERT INTO general_hits SELECT uuid,'blue_hair' FROM assets WHERE uuid LIKE 'extra-%'")
            c.execute("INSERT INTO asset_rating SELECT uuid,CASE WHEN CAST(substr(uuid,-1) AS INTEGER)%2 THEN 'explicit' ELSE 'general' END FROM assets WHERE uuid LIKE 'extra-%'")
        # Library sqlite used by artists._core is this very same application database.
        with store.connect() as src,sqlite3.connect(lib.db) as dst:src.backup(dst)
        store.path=lib.db
        with db.connect(lib) as c:
            c.execute("INSERT INTO artists(id) VALUES ('A'),('B'),('M')")
            c.execute("UPDATE artists SET merged_into='A' WHERE id='M'")
            c.execute("INSERT INTO artist_links(system,key,artist_id,url,source) VALUES ('pixiv','42','A','unused','test')")
            c.executemany('INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,?,?,?,?)',[
                ('a','A','pixel_hash',1,'{}','auto'),('a','B','group_oc',.5,'{"count":2}','candidate'),
                ('extra-0','M','gallery_match',.95,'{}','auto'),('extra-1','B','ocr',.6,'{}','candidate')])
            c.executemany('INSERT INTO image_sources(asset,site,post_id,url,artist_id,method) VALUES (?,?,?,?,?,?)',[
                ('a','pixiv','2','https://example.test/2','B','gallery_match'),
                ('a','pixiv','1','https://example.test/1','A','pixel_hash'),
                ('a','x','3','https://example.test/x','A','gallery_match')])
        with store.connect() as c:
            c.execute("INSERT INTO artist_verdicts(uuid,artist,verdict) VALUES ('a','B','no'),('extra-1','B','yes')")
            c.execute("INSERT INTO actions(label,before_json) VALUES ('synthetic artist verdicts',?)",(json.dumps([{'artist_verdict':{'uuid':'a','artist':'B','had':None}},{'artist_verdict':{'uuid':'extra-1','artist':'B','had':None}}]),))
        store.artist_assets=lambda artist:artists.assets_of(lib,artist,rejected=store.artist_verdicts())
        original=self.server.store
        try:
            self.server.store=store;self.server.library=lib;self.publish()
            for core in (False,True):
                if core:
                    self.assertTrue(adopt_artists(store,lib));terms_seed.switch(store,artists=False);self.publish()
                for nsfw in ('','hide','only'):
                    for pool in ('','duplicates','excluded','unmatched','pending','recent','audit'):
                        for order in ('newest','oldest','hinted','suspicious'):
                            filters={'pool':pool,'order':order,'nsfw':nsfw}
                            self.pair('/api/assets?'+urlencode({**filters,'limit':3}))
                            self.pair('/api/ids?'+urlencode({**filters,'limit':3}))
                            self.pair('/api/assets/count?'+urlencode(filters))
                    self.pair('/api/review-pools?'+urlencode({'nsfw':nsfw}))
                    self.pair('/api/home?'+urlencode({'nsfw':nsfw,'days':3}))
                    self.pair('/api/assets?'+urlencode({'tag':'blue_hair','nsfw':nsfw}))
                for filters in ({'dup':'mixed'},{'pool':'cluster','cluster':'7'},{'pool':'similar','cluster':'9'},
                                {'pool':'excluded','reason':'manual'},{'day':'2026-09-23'}):
                    self.pair('/api/assets?'+urlencode(filters))
                for id in ('a','extra-0','extra-1','extra-2','extra-3','extra-4','extra-5','extra-6','extra-7'):
                    detail=json.loads(self.pair('/api/detail?'+urlencode({'id':id}))['body'])
                    if id=='a':self.assertTrue(detail['posts']);self.assertTrue(detail['artists'])
                for artist in ('A','B','M','missing'):
                    for path in ('assets','assets/count','ids'):
                        self.pair('/api/'+path+'?'+urlencode({'artist':artist}))
                # Auxiliary failure cannot mask missing images or invalid earlier filters.
                source=store.source;source_parked=source.with_suffix('.parked')
                source.rename(source_parked)
                try:
                    self.assertEqual(self.pair('/api/detail?id=missing')['status'],400)
                    self.assertEqual(self.pair('/api/detail?id=a')['status'],500)
                    self.pair('/api/assets')
                finally:source_parked.rename(source)
                prov=lib.path/'provenance.sqlite';parked=lib.path/'provenance.parked'
                prov.rename(parked);prov.mkdir()
                try:
                    self.assertEqual(self.pair('/api/assets?artist=A')['status'],200 if core else 500)
                    self.assertEqual(self.pair('/api/assets?artist=A&day=bad')['status'],400)
                    self.assertEqual(self.pair('/api/detail?id=missing')['status'],400)
                    self.assertEqual(self.pair('/api/detail?id=a')['status'],500)
                finally:prov.rmdir();parked.rename(prov)
        finally:
            self.server.store=original;self.server.library=None;self.publish()
