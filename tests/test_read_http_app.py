"""App reads through the same HTTP byte contract and synthetic request context."""
import json
from urllib.parse import urlencode
from tests.test_read_http_core import HTTPHarness


class AppReadHTTPTest(HTTPHarness):
    def test_bootstrap_prefs_and_history_rendering(self):
        for nsfw in ('', 'hide', 'only', 'bad'):
            self.pair('/api/bootstrap?' + urlencode({'nsfw': nsfw}))
        self.pair('/api/prefs')
        with self.store.connect() as c:
            ids = [r[0] for r in c.execute("SELECT id FROM entities WHERE parent_id IS NULL AND kind<>'work' AND merged_into IS NULL LIMIT 2")]
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('pins',?)", (json.dumps(ids + ids + ['missing']),))
        self.assertEqual(json.loads(self.pair('/api/prefs')['body'])['pins'], ids)
        samples = [
            ('literal-history', []),
            ('已将 2 张设为 stale', [{'entity': ids[0]}]),
            ('已将 2 张移出 stale', [{'entity': ids[0]}]),
            ('已给 2 张加上 stale', [{'entity': ids[0]}]),
            ('已从 2 张去掉 stale', [{'entity': ids[0]}]),
            ('已将 2 张从 old 换成 new', [{'entity': i} for i in ids]),
            ('已将 stale 标为已检查（2 张）', [{'entity': ids[0]}]),
            ('已将 stale 标为已检查（2 张）', []),
            ('merge', [{'merge': {'source': ids[0], 'target': ids[1], 'moved': ['a', 'b']}}]),
            ('first empty merge retained', [{'merge': {}}, {'merge': {'source': ids[0], 'target': ids[1], 'moved': []}}]),
            ('first null merge retained', [{'merge': None}, {'merge': {'source': ids[0], 'target': ids[1], 'moved': []}}]),
            ('已将 2 张设为 stale', [{'terms': {'v': 2, 'before': [{'entity': ids[0]}]}}]),
        ]
        for label, before in samples:
            with self.store.connect() as c:
                ident = c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)', (label, json.dumps(before))).lastrowid
                if not before:
                    c.execute('INSERT OR REPLACE INTO reviews(entity,action_id) VALUES (?,?)', (ids[0], ident))
            self.pair('/api/bootstrap')
        old = self.store.read_only
        try:
            self.store.read_only = 'synthetic refusal 雪'
            self.publish()
            self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['read_only'], self.store.read_only)
        finally:
            self.store.read_only = old
            self.publish()

    def test_private_session_refresh(self):
        original = self.server.token
        try:
            self.server.token = 'replacement-synthetic-token'
            self.publish()
            self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['token'], self.server.token)
        finally:
            self.server.token = original
            self.publish()
