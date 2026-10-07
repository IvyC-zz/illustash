"""App reads through the same HTTP byte contract and synthetic request context."""
import json
from urllib.parse import urlencode
from tests.read_http_harness import HTTPHarness


class AppReadHTTPTest(HTTPHarness):
    def test_bootstrap_history_and_legacy_pins_retention(self):
        for nsfw in ('', 'hide', 'only', 'bad'):
            self.pair('/api/bootstrap?' + urlencode({'nsfw': nsfw}))
        with self.connect() as c:
            ids = [r[0] for r in c.execute("SELECT id FROM entities WHERE parent_id IS NULL AND kind<>'work' AND merged_into IS NULL LIMIT 2")]
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('pins',?)", (json.dumps(ids + ids + ['missing']),))
            c.commit()
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
            with self.connect() as c:
                # A fixed time instead of the column's CURRENT_TIMESTAMP default: compared with recorded bytes.
                ident = c.execute("INSERT INTO actions(label,before_json,ts) VALUES (?,?,'2026-10-01 00:00:00')",
                                  (label, json.dumps(before))).lastrowid
                if not before:
                    c.execute('INSERT OR REPLACE INTO reviews(entity,action_id) VALUES (?,?)', (ids[0], ident))
                c.commit()
            self.pair('/api/bootstrap')
        self.publish()
        self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['read_only'], 'synthetic refusal 雪')

    def test_private_session_refresh(self):
        self.publish()
        self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['token'], 'replacement-synthetic-token')
