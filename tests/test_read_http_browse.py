"""R3 byte-strict browsing acceptance using only temporary synthetic libraries."""
import json
import os
import unittest
from pathlib import Path
from urllib.parse import urlencode
from tests.read_http_harness import HTTPHarness


class BrowseReadHTTPTest(HTTPHarness):
    def test_page_names_refresh_after_write_and_library_switch_in_hybrid(self):
        root = self.root/'page-name-refresh'; root.mkdir(exist_ok=True)
        self.sync('replacement'); self.publish()
        first = self.pair('/api/assets?limit=200')['body']
        items = json.loads(first)['items']
        member = next(m for row in items for m in row['memberships'])
        self.assertIn('node_type', member)
        self.sync('renamed')
        updated = self.pair('/api/assets?limit=200')['body']
        self.assertNotEqual(first, updated)
        self.assertIn('分页更新🙂', updated.decode())
        for query in ('limit=1', 'limit=200&offset=1', 'offset=1000',
                      'pool=unmatched', 'order=hinted', 'order=suspicious',
                      'nsfw=hide', 'nsfw=only'):
            self.pair('/api/assets?'+query)
        self.publish()
        self.pair('/api/assets?limit=200')

    def test_browse_corpus(self):
        migrated={'/api/assets','/api/assets/count','/api/ids','/api/detail','/api/home','/api/fold','/api/review-pools'}
        requests=self.corpus(sample=2,query_limit=30,keep=lambda r:r['url'].split('?',1)[0] in migrated)
        report=self.compare(requests,require_core=['R3'])
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
            if os.name == 'nt' and query == 'before=9999-12-31':
                # Recorded on POSIX (200); the Windows host cannot represent that local date (500).
                self.rec.skip('response')
                continue
            self.pair('/api/home?'+query)
        for nsfw in ('','hide','only','invalid'):
            self.pair('/api/review-pools?'+urlencode({'nsfw':nsfw}))
        self.pair('/api/detail?id=missing')

    def test_database_error_precedes_review_pool_rating_validation(self):
        corrupt = self.root/'corrupt-temporary.sqlite'
        corrupt.write_bytes(b'not a SQLite database')
        self.publish()
        self.assertEqual(self.pair('/api/review-pools?nsfw=invalid')['status'],500)

    def test_decorated_paging_exact_for_core(self):
        with self.connect() as c:
            entities=[r[0] for r in c.execute("SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE t.kind<>'work' AND r.merged_into IS NULL ORDER BY r.seq")]
            variants=[r[0] for r in c.execute('SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE r.parent_id IS NOT NULL ORDER BY r.parent_id,t.key')]
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
        with self.connect() as c:
            ids=[r[0] for r in c.execute('SELECT uuid FROM assets')]
        for id in ids:self.pair('/api/detail?'+urlencode({'id':id}))
        for nsfw in ('','hide','only'):
            self.pair('/api/home?'+urlencode({'nsfw':nsfw,'days':2}))

    @unittest.skipIf(os.name == 'nt', '固定期望录自 POSIX，包名含 Windows 不允许的 ?')
    def test_expanded_legacy_core_visibility_evidence_and_auxiliary_reads(self):
        name = 'Browse #雪%.pilib' if os.name == 'nt' else 'Browse #雪?.pilib'
        package = self.root/name
        self.sync('expanded'); self.publish()
        source = Path(self.rec.value('source'))
        # The library before it switched to the core (no `meta`) is refused since core v8 (#201, only switched libraries
        # are kept), so its 294 recorded replies are not read live any more. The same 294 requests follow on the library
        # after it switched, restored at this core version; 288 of its recorded replies are byte-identical to the ones
        # before the switch, so those are compared with the live v8 replies as well. The other 6 differ because the
        # switch itself changed the artist attribution (the Python-era artist tables no supported library has): the
        # details of `a` and `extra-0`, artist A's assets, count and ids, and artist A with an unreadable provenance
        # database (500 before the switch, 200 after).
        before = [self.rec.value('response') for _ in range(294)]
        self.sync('switched'); self.publish()
        compared = self.after_switch(before, differ={266, 267, 275, 276, 277, 290})
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
        source_parked=source.with_suffix('.parked')
        source.rename(source_parked)
        try:
            self.assertEqual(self.pair('/api/detail?id=missing')['status'],400)
            self.assertEqual(self.pair('/api/detail?id=a')['status'],500)
            self.pair('/api/assets')
        finally:source_parked.rename(source)
        prov=package/'provenance.sqlite';parked=package/'provenance.parked'
        prov.rename(parked);prov.mkdir()
        try:
            self.assertEqual(self.pair('/api/assets?artist=A')['status'],200)
            self.assertEqual(self.pair('/api/assets?artist=A&day=bad')['status'],400)
            self.assertEqual(self.pair('/api/detail?id=missing')['status'],400)
            self.assertEqual(self.pair('/api/detail?id=a')['status'],500)
        finally:prov.rmdir();parked.rename(prov)
        self.assertEqual(len(compared), 294)


