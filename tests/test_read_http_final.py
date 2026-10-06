"""Final read-route contract: same synthetic DB, bytes, and an unavailable Python upstream."""
import json
import os
import subprocess

from tests.test_read_http import free_port, stop_process, wait_until
from tests.test_read_http_core import HTTPHarness
from tools import verify_read_http as verify
from web import terms_core
from tests.reference import host
from web.server import switch_failed


CORE = {
    'user-tags', 'general-tags', 'artists', 'artist', 'artist-audit', 'entities', 'works',
    'assets', 'assets/count', 'ids', 'detail', 'home', 'fold', 'review-pools',
    'characters', 'tagged', 'character/profile', 'character/activity', 'character/stats',
    'identity/count', 'identity/queue', 'bootstrap',
}


class FinalReadHTTPTest(HTTPHarness):
    def restart_proxy(self, upstream, *args):
        stop_process(self.proxy)
        type(self).proxy = subprocess.Popen(
            [str(host.binary()), 'http', '--port', self.origin.rsplit(':', 1)[1],
             '--upstream', str(upstream), *args],
            env={**os.environ, 'ILLUSTASH_HTTP_STATE': str(self.state)},
            stdout=self.log, stderr=self.log,
        )
        wait_until(lambda: verify.fetch(self.origin, {'url': '/api/bootstrap'}, self.origin)['status'] == 200)

    def test_all_routes_same_corpus_without_python_upstream(self):
        requests = [r for r in verify.corpus(self.store.path, sample=1, query_limit=220)
                    if r['url'].split('?', 1)[0].removeprefix('/api/') in CORE]
        saved = self.root / 'final-requests.json'
        saved.write_text(json.dumps(requests, ensure_ascii=False))
        requests = json.loads(saved.read_text())
        self.assertEqual({r['url'].split('?', 1)[0].removeprefix('/api/') for r in requests}, CORE)
        report = verify.compare(self.direct, self.origin, self.origin, requests, ['R1', 'R2', 'R3', 'R4'])
        self.assertEqual(report['differ'], 0, report['differences'])
        self.assertEqual(report['core_equal'], len(requests))
        expected = [verify.fetch(self.direct, item, self.origin) for item in requests]
        self.restart_proxy(free_port())
        try:
            for item, left in zip(requests, expected):
                right = verify.fetch(self.origin, item, self.origin)
                self.assertIsNone(verify.difference(left, right), item)
                self.assertEqual(right['route'], 'core', item)
            unavailable = verify.fetch(self.origin, {'url': '/api/similar'}, self.origin)
            self.assertEqual(unavailable['status'], 502)
            self.assertEqual(unavailable['route'], 'unavailable')
        finally:
            self.restart_proxy(self.server.server_port)

    def test_single_route_override_and_http_assembly(self):
        self.restart_proxy(self.server.server_port, '--route', 'GET:/api/fold=forward')
        try:
            self.pair('/api/fold?ignored=1', route='forward')
            self.pair('/api/assets?limit=2&count=0')
            self.pair('/api/assets/count?offset=bad&limit=bad')
            self.pair('/api/bootstrap?nsfw=hide')
            for method in ('HEAD', 'OPTIONS', 'PUT'):
                item = {'url': '/api/fold', 'method': method}
                left = verify.fetch(self.direct, item, self.origin)
                right = verify.fetch(self.origin, item, self.origin)
                self.assertIsNone(verify.difference(left, right), method)
                self.assertEqual(right['route'], 'forward')
        finally:
            self.restart_proxy(self.server.server_port)

    def test_forwarded_write_core_read_and_readonly_refusal(self):
        def post(path, payload, token=None):
            return verify.fetch(self.origin, {
                'url': path, 'method': 'POST', 'body': json.dumps(payload),
                'headers': {'Content-Type': 'application/json',
                            'X-CSRF-Token': token or self.server.token},
            }, self.origin)

        response = post('/api/tag', {'ids': ['a'], 'tag': 'final-合成标签'})
        self.assertEqual(response['status'], 200, response['body'])
        self.assertEqual(response['route'], 'forward')
        tags = json.loads(self.pair('/api/user-tags?q=final-')['body'])
        self.assertEqual(tags, [{'tag': 'final-合成标签', 'count': 1}])
        self.assertEqual(post('/api/tag', {'ids': ['a'], 'tag': 'rejected'}, 'expired-token')['status'], 403)
        original = self.store.read_only
        try:
            self.store.refuse_writes('temporary synthetic readonly')
            self.publish()
            self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['read_only'], self.store.read_only)
            self.assertEqual(post('/api/tag', {'ids': ['a'], 'tag': 'rejected'})['status'], 409)
            self.assertEqual(json.loads(self.pair('/api/user-tags?q=rejected')['body']), [])
            self.pair('/api/assets?limit=2')
        finally:
            self.store.read_only = original
            terms_core.allow_writes(self.store.path)
            self.publish()

    def test_bootstrap_observes_cutover_failure_after_readiness_was_published(self):
        original = self.store.read_only
        with self.store.connect() as c:
            c.execute("DELETE FROM meta WHERE key='terms_characters'")
        try:
            # main() publishes first, then adopt_terms runs in the existing background
            # thread. No republish follows a failure; Python owns its live refusal.
            self.publish()
            switch_failed(self.store, 'synthetic cutover failure after startup')
            data = json.loads(self.pair('/api/bootstrap', route='forward')['body'])
            self.assertEqual(data['read_only'], self.store.read_only)
            self.assertIn('synthetic cutover failure after startup', data['read_only'])
            self.pair('/api/user-tags')
        finally:
            self.store.read_only = original
            terms_core.allow_writes(self.store.path)
            with self.store.connect() as c:
                c.execute("INSERT OR REPLACE INTO meta VALUES ('terms_characters','core')")
            self.publish()
