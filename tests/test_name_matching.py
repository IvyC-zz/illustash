"""从缓存摘出的名字回归输入；仅使用临时库与固定网络回答。"""
import sqlite3
import tempfile
import threading
import time
import unittest
import urllib.parse
from pathlib import Path
from unittest.mock import patch

from tests.synthetic_source import source_fixture as fixture
from tests import terms_seed
from web.names import BG, DB, NameUpdater
from web.store import Store

WIKIS = {
    'mika_(blue_archive)': ['聖園ミカ', 'Misono_Mika', '圣园未花', '未花', 'ミカ', '미소노미카', 'mika', '聖園彌香', '圣园弥香', 'MisonoMika', '美園ミカ'],
    'hakurei_reimu': ['博丽灵梦', '博麗霊夢', '霊夢', '青霊夢', '博麗靈夢', '靈夢', 'レイマリ'],
    'amiya_(arknights)': ['阿米娅', 'アーミヤ', 'アーミヤ(アークナイツ)', 'Amiya'],
    'raiden_shogun': ['雷电将军', 'RaidenShogun', '雷电影', 'Raiden_Ei', '巴尔泽布', 'Beelzebul', '一心净土', '雷電将軍', '雷電將軍', '雷電将軍(原神)', '雷電影', '雷神バアル', 'EiMiko', '影ちゃん'],
    'blue_archive': ['ブルアカ', 'ブルーアーカイブ', 'ブルアカイラスト部', '蔚藍檔案', '碧蓝档案', '碧藍檔案', 'BA', '蔚蓝档案', 'ぶるあか'],
    'genshin_impact': ['原神', '原神project', '원신', 'gnsnプラス', 'Genshin'],
    'touhou': ['東方Project', 'おは東方'],
}


class MatchingTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        self.store = Store(self.source, self.root / 'library.sqlite')
        self.store.refresh()
        self.calls = []
        self.results = {}
        self.subjects = []
        self.related = {}
        self.updater = NameUpdater(self.store, self.transport, interval=0)

    def tearDown(self):
        self.temp.cleanup()

    def transport(self, url, body=None, key=None):
        self.calls.append((url, body))
        if url.startswith(DB):
            tag = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)['search[title]'][0]
            return [{'title': tag, 'other_names': WIKIS.get(tag, [])}]
        if '/search/characters' in url:
            return {'data': self.results.get(body['keyword'], [])}
        if '/search/subjects' in url:
            return {'data': self.subjects}
        if url.endswith('/subjects'):
            return self.related.get(int(url.split('/')[-2]), [])
        self.fail('unexpected URL: ' + url)

    def add(self, tag, work='', ja=None, zh=None):
        # 原手工边界是旧库输入；布置完毕后走真实启动切换，之后的名字写入只走核心。
        self.store = terms_seed.base(self.root, db=self.root / 'library.sqlite')
        self.source = self.store.source
        self.updater.store = self.store
        with sqlite3.connect(self.source) as c:
            c.execute('INSERT INTO characters VALUES (?,?,?,?,?,?)', (tag, zh, ja, None, work, tag))
        with self.store.connect() as c:
            c.execute('INSERT INTO entities(id,name,tag,work,external_key) VALUES (?,?,?,?,?)', (tag, tag, tag, work, 'index:' + tag))
            Store._links_from_columns(c)
            from web.works import migrate
            migrate(c)
            if work:
                Store.link(c, c.execute("SELECT entity_id FROM links WHERE system='qualifier' AND key=?", (work,)).fetchone()[0], 'danbooru', work)
            c.execute("INSERT INTO candidates VALUES ('c',?,'WD',0.9)", (tag,))
            Store._names_from_tables(c)
        terms_seed.switch(self.store)
        return tag

    def row(self, tag):
        return self.store.characters(ident=tag)[0]

    @staticmethod
    def result(ident, name, zh=None):
        return {'id': ident, 'name': name, 'infobox': [{'key': '简体中文名', 'value': zh}] if zh else []}

    def test_hosts_overlap_without_parallel_same_host_workers(self):
        tag = self.add('mika_(blue_archive)', 'example_work', '聖園ミカ')
        bg_entered, db_entered = threading.Event(), threading.Event()
        def synchronized(url, body=None, key=None):
            if '/search/subjects' in url:
                bg_entered.set()
                if not db_entered.wait(2):
                    raise AssertionError('Danbooru stalled behind Bangumi')
            if url.startswith(DB) and 'mika_' in url:
                db_entered.set()
                if not bg_entered.wait(2):
                    raise AssertionError('Bangumi stalled behind Danbooru')
            return self.transport(url, body, key)
        self.updater.transport = synchronized
        with patch.dict(WIKIS, {'example_work': ['作品甲']}):
            self.updater.run(entity=tag)
        self.assertTrue(bg_entered.is_set() and db_entered.is_set())
        self.assertEqual(self.updater.info()['state']['status'], 'done')

    def test_rate_limits_are_per_host(self):
        self.updater.interval = 1.0
        with patch('web.names.time.monotonic', side_effect=[100, 100, 100.4, 100.4, 100.4, 100.4]), patch('web.names.time.sleep') as sleep:
            self.updater.fetch(BG + '/search/subjects', {'keyword': '原神'})
            self.updater.fetch(BG + '/search/subjects', {'keyword': '原神'})
            self.updater.fetch(DB + '/wiki_pages.json?search%5Btitle%5D=genshin_impact')
        delays = [call.args[0] for call in sleep.call_args_list]
        self.assertEqual(delays[0], 0)
        self.assertAlmostEqual(delays[1], 0.6)
        self.assertEqual(delays[2], 0)

    def test_3000_entity_search_latency_and_prefix_ranking(self):
        self.store = terms_seed.base(self.root, db=self.root / 'library.sqlite')
        self.source = self.store.source
        with self.store.connect() as c:
            c.executemany("""INSERT INTO entities(id,name,tag,work,external_key)
                VALUES (?,?,?,?,?)""", [(f'extra{i}', f'extra{i}', f'extra{i}', 'blue_archive', f'wd:extra{i}')
                                 for i in range(3000)])
            c.executemany("INSERT INTO names(entity_id,value,source,role) VALUES (?,?,'index','alias')",
                          [(f'extra{i}', f'中文别名{i}') for i in range(3000)])
            c.execute("INSERT INTO entities(id,name,tag,work,external_key) VALUES ('mika','mika','mika_(blue_archive)','blue_archive','wd:mika')")
            c.execute("INSERT INTO entities(id,name,tag,work,external_key) VALUES ('mikasa','mikasa','mikasa_ackerman','other','wd:mikasa')")
            Store._links_from_columns(c)
            c.executemany("INSERT INTO candidates VALUES ('a',?,'WD',0.9)", [('mika',), ('mikasa',)])
            Store._names_from_tables(c)
            c.execute("UPDATE entities SET kind='character' WHERE kind=''")
        terms_seed.switch(self.store)
        # Warm SQLite's page cache; assert the query path rather than filesystem startup.
        self.store.characters('蔚蓝', minimum=0)
        for call in (lambda: self.store.characters('蔚蓝', minimum=0),
                     lambda: self.store.entities('mika'),
                     lambda: self.store.assets(query='蔚蓝', limit=10)):
            start = time.perf_counter()
            call()
            self.assertLess((time.perf_counter() - start) * 1000, 300)
        self.assertEqual(self.store.entities('mika')[0]['tag'], 'mika_(blue_archive)')
        self.assertEqual(self.store.characters('mika')[0]['tag'], 'mika_(blue_archive)')
