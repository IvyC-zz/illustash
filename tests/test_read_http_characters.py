"""R4 status/raw HTTP acceptance; all mutable inputs are temporary synthetic fixtures."""
import json
import unittest
from urllib.parse import urlencode
from tests.read_http_harness import HTTPHarness

CORE = {'characters', 'tagged', 'character/profile', 'character/activity', 'character/stats',
        'identity/count', 'identity/queue',
        }

class CharacterReadHTTPTest(HTTPHarness):
    def test_directory_and_identity_corpus(self):
        requests = self.corpus(sample=6, query_limit=30,
                               keep=lambda r: r['stage'] == 'R4' and r['url'].split('?', 1)[0].removeprefix('/api/') in CORE)
        report = self.compare(requests, require_core=['R4'])
        self.assertEqual(report['differ'], 0, report['differences'])
        self.assertEqual(report['compared'], report['core_equal'])
        self.assertGreater(report['core_equal'], 100)
        self.assertTrue(json.loads(self.pair('/api/characters')['body']))

    def test_error_and_query_parameters(self):
        for path, key in [('characters', 'min'),
                          ('identity/queue', 'offset')]:
            for n in ['bad', '1.2', '-1', '0', '201', ' １_０ ', '\x1c2\x1f', '9'*100]:
                self.pair('/api/' + path + '?' + urlencode({key:n}))
        for path in ['characters', 'tagged', 'character/activity', 'character/stats', 'identity/count', 'identity/queue']:
            self.pair('/api/' + path + '?nsfw=bad')
        for query in ['min=bad&min=', 'min=bad&min=0', 'q=Alice&q=&q=雪', 'q=%FF%FE', 'q=+%1C+']:
            self.pair('/api/characters?' + query)
        self.pair('/api/character/profile?entity=no-such')
        self.pair('/api/characters?work=no-such')

    def test_eager_errors_do_not_read_db(self):
        cases = ['/api/characters?min=-1',
                  '/api/identity/queue?limit=0',
                 '/api/identity/queue?nsfw=bad', '/api/identity/count?nsfw=bad',
                  '/api/character/stats',
                 '/api/character/stats?id=a&nsfw=bad']
        missing = self.root/'never-created.sqlite'
        self.publish()
        for path in cases:
            self.assertEqual(self.pair(path)['status'], 400)
        self.assertFalse(missing.exists())
    def test_nonempty_identity_queue_first_hit_paging_and_detail(self):
        root = self.root/'r4-hybrid'; root.mkdir(exist_ok=True)
        self.sync('hybrid'); self.publish()
        with self.connect() as c:
            ids = [r[0] for r in c.execute("SELECT entity,count(*) FROM memberships GROUP BY entity ORDER BY count(*) DESC")]
        self.assertGreaterEqual(len(ids), 2)
        a, b = ids[:2]
        self.sync('proposed')
        for path in ['identity/queue', 'identity/count', 'detail?id=a']:
            self.pair('/api/' + path)
            for query in ['limit=1&offset=0', 'limit=1&offset=1', 'limit=1&offset=1000']:
                self.pair('/api/' + path + '?' + query)
        for nsfw in ['', 'hide', 'only']:
            self.pair('/api/identity/queue?' + urlencode({'nsfw':nsfw}))
            self.pair('/api/identity/count?' + urlencode({'nsfw':nsfw}))
        queue = json.loads(self.pair('/api/identity/queue')['body'])
        self.assertEqual(queue['total'], 1)
        self.assertEqual(queue['items'][0]['distance'], 0.11)
        self.assertEqual(queue['items'][0]['hint']['subject'], 'first_tag')
        self.assertEqual(queue['items'][0]['a']['id'], a)
        for e in (a,b):
            for summary in ('0','1'):
                self.pair('/api/characters?' + urlencode({'id':e,'summary':summary}))
            self.pair('/api/character/profile?' + urlencode({'entity':e}))
            self.pair('/api/character/stats?' + urlencode({'id':e,'exclude':'a'}))
        covers={r['id']:r['cover'] for r in json.loads(self.pair('/api/characters')['body'])}
        self.assertEqual(covers[a], 'a')  # Solo, confirmed image wins over the shared machine image.
        self.assertEqual(covers[b], 'b')
        self.assertNotEqual(covers[a], covers[b])
        self.sync('mascot')
        tagged=json.loads(self.pair('/api/tagged')['body'])
        self.assertTrue(any(r['id']==b and r['count'] for r in tagged))
        self.pair('/api/characters?'+urlencode({'id':b}))
        self.sync('profile')
        profile=json.loads(self.pair('/api/character/profile?'+urlencode({'entity':a}))['body'])
        self.assertEqual(profile['user_name'], '合成用户名字')
        self.assertEqual(profile['aliases'], ['后插入','先排序'])
        for uuid in ('a','b','c'):
            self.pair('/api/detail?id='+uuid)
        self.sync('skipped')
        self.assertEqual(json.loads(self.pair('/api/identity/count')['body'])['total'],0)
        self.sync('decided')
        self.pair('/api/identity/count')
        self.sync('uncertain')
        response = json.loads(self.pair('/api/detail?id=a')['body'])['identity']
        self.assertTrue(response['undetermined'])

    def test_unset_policy_legacy_names_stable_ties_and_cover_reuse(self):
        other = self.root/'r4-legacy'; other.mkdir(exist_ok=True)
        self.sync('legacy'); self.publish()
        for path in ['characters', 'characters?summary=1', 'tagged', 'identity/queue', 'identity/count', 'detail?id=a']:
            self.pair('/api/' + path)
        with self.connect() as c:
            ids=[r[0] for r in c.execute("SELECT id FROM entities WHERE kind='character' AND parent_id IS NULL AND merged_into IS NULL LIMIT 3")]
            for e in ids:
                c.execute("DELETE FROM names WHERE entity_id=?",(e,))
                c.execute("INSERT INTO names(entity_id,lang,value,source,role,preferred,position) VALUES (?,'zh','平局','user','display',1,0)",(e,))
            c.execute("DELETE FROM meta WHERE key='identity_policy'")
            c.commit()
        for path in ['characters', 'characters?summary=1', 'detail?id=a']:
            self.pair('/api/' + path)
        for e in ids:
            self.pair('/api/characters?' + urlencode({'id':e}))
            self.pair('/api/character/profile?' + urlencode({'entity':e}))

if __name__ == '__main__':
    unittest.main()
