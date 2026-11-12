"""App 的读取走同一份 HTTP 字节契约和合成请求上下文。"""
import json
from urllib.parse import urlencode
from tests.read_http_harness import HTTPHarness


class AppReadHTTPTest(HTTPHarness):
    def test_bootstrap_history_and_legacy_pins_retention(self):
        for nsfw in ('', 'hide', 'only', 'bad'):
            self.pair('/api/bootstrap?' + urlencode({'nsfw': nsfw}))
        with self.connect() as c:
            ids = [r[0] for r in c.execute("SELECT t.key FROM term_tree r JOIN terms t ON t.id=r.term_id WHERE r.parent_id IS NULL AND t.kind<>'work' AND r.merged_into IS NULL ORDER BY t.key LIMIT 2")]
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
        # core v8 之前的步骤：v8 的升级给带计数的步骤补上了类型和数量（#201）。
        counted = {'已将 2 张设为 stale': 'set', '已将 2 张移出 stale': 'remove', '已给 2 张加上 stale': 'add',
                   '已从 2 张去掉 stale': 'drop', '已将 2 张从 old 换成 new': 'swap', '已将 stale 标为已检查（2 张）': 'sweep'}
        for label, before in samples:
            with self.connect() as c:
                # 用固定时间代替该列的 CURRENT_TIMESTAMP 默认值：要与录制的字节比较。
                kind = counted.get(label)
                ident = c.execute("INSERT INTO actions(label,before_json,ts,label_kind,label_count) "
                                  "VALUES (?,?,'2026-10-01 00:00:00',?,?)",
                                  (label, json.dumps(before), kind, kind and '2')).lastrowid
                if not before:
                    c.execute('INSERT OR REPLACE INTO reviews(entity,action_id) VALUES (?,?)', (ids[0], ident))
                c.commit()
            self.pair('/api/bootstrap')
        self.publish()
        self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['read_only'], 'synthetic refusal 雪')

    def test_private_session_refresh(self):
        self.publish()
        self.assertEqual(json.loads(self.pair('/api/bootstrap')['body'])['token'], 'replacement-synthetic-token')
