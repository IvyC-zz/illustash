"""Final read-route contract: every core read route on the same synthetic DB, byte for byte."""
import json

from tests.read_http_harness import RECORD, HTTPHarness


CORE = {
    'user-tags', 'general-tags', 'artists', 'artist', 'artist-audit', 'entities', 'works',
    'assets', 'assets/count', 'ids', 'detail', 'home', 'fold', 'review-pools',
    'characters', 'tagged', 'character/profile', 'character/activity', 'character/stats',
    'identity/count', 'identity/queue', 'bootstrap',
}


class FinalReadHTTPTest(HTTPHarness):
    def test_all_routes_same_corpus(self):
        requests = self.corpus(sample=1, query_limit=220,
                               keep=lambda r: r['url'].split('?', 1)[0].removeprefix('/api/') in CORE)
        self.assertEqual({r['url'].split('?', 1)[0].removeprefix('/api/') for r in requests}, CORE)
        report = self.compare(requests, ['R1', 'R2', 'R3', 'R4'])
        self.assertEqual(report['differ'], 0, report['differences'])
        self.assertEqual(report['core_equal'], len(requests))

    def test_query_assembly(self):
        self.pair('/api/fold?ignored=1')
        self.pair('/api/assets?limit=2&count=0')
        self.pair('/api/assets/count?offset=bad&limit=bad')
        self.pair('/api/bootstrap?nsfw=hide')

    def test_written_tag_and_readonly_context_are_read(self):
        if RECORD:
            self.store.tag(['a'], 'final-合成标签')
        self.sync('tagged')
        tags = json.loads(self.pair('/api/user-tags?q=final-')['body'])
        self.assertEqual(tags, [{'tag': 'final-合成标签', 'count': 1}])
        if RECORD:
            self.store.read_only = 'temporary synthetic readonly'
        self.publish()
        self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['read_only'], 'temporary synthetic readonly')
        self.assertEqual(json.loads(self.pair('/api/user-tags?q=rejected')['body']), [])
        self.pair('/api/assets?limit=2')
