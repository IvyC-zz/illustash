"""从缓存摘出的名字回归输入；仅使用临时库与固定网络回答。"""
import sqlite3
import tempfile
import threading
import time
import unittest
import urllib.parse
from pathlib import Path
from unittest.mock import patch

from tests.test_web import fixture
from tests import terms_seed
from web.names import BG, DB, NameUpdater, View, WORK_NAMES, group_alias, keywords, put
from web.store import SCHEMA_VERSION, Store

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

    def test_mika_chinese_subject_subtitle_and_cache(self):
        tag = self.add('mika_(blue_archive)', 'blue_archive', '聖園ミカ', '未花')
        self.results['聖園ミカ'] = [self.result(114601, '聖園ミカ', '圣园未花')]
        self.subjects = [{'id': 1, 'name': 'ブルーアーカイブ -Blue Archive-', 'name_cn': '蔚蓝档案'},
                         {'id': 2, 'name': 'ブルーアーカイブ The Animation', 'name_cn': '蔚蓝档案'}]
        self.updater.run(entity=tag)
        row = self.row(tag)
        self.assertEqual((row['name'], row['name_source'], row['work_name']), ('圣园未花', 'bangumi', '蔚蓝档案'))
        queries = [body['keyword'] for url, body in self.calls if '/search/characters' in url]
        self.assertEqual(queries, ['聖園ミカ'])
        self.assertFalse(any(url.endswith('/114601') for url, _ in self.calls))
        count = len(self.calls)
        NameUpdater(self.store, self.transport, interval=0).run(entity=tag)
        self.assertEqual(count, len(self.calls))
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM display_names WHERE source='danbooru'").fetchone()[0], 0)

    def test_reimu_never_displays_pairing_alias_and_alias_remains_searchable(self):
        tag = self.add('hakurei_reimu', 'touhou')
        self.updater.run(entity=tag)
        # Bangumi misses: the first Chinese wiki alias is shown, never the pairing alias.
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('博丽灵梦', 'wiki'))
        self.assertEqual(self.store.characters('レイマリ')[0]['id'], tag)
        self.results['博丽灵梦'] = [self.result(12, '博麗霊夢', '博丽灵梦')]
        self.updater.run(entity=tag, refresh=True)
        self.assertEqual(self.row(tag)['name'], '博丽灵梦')
        self.assertEqual(self.row(tag)['work_name'], '东方Project')  # failed work does not gate character

    def test_character_ambiguity_uses_related_work_or_abstains(self):
        tag = self.add('mika_(blue_archive)', 'blue_archive', '聖園ミカ')
        self.results['聖園ミカ'] = [self.result(1, '聖園ミカ', '正确名'), self.result(2, '聖園ミカ', '错误名')]
        self.related = {1: [{'name': '带副标题的原名', 'name_cn': '蔚蓝档案'}], 2: [{'name': '其他作品'}]}
        self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['name'], '正确名')
        self.related[2] = self.related[1]
        self.updater.run(entity=tag, refresh=True)
        # Bangumi abstains; the wiki's Chinese name comes before the index Japanese.
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('圣园未花', 'wiki'))
        self.related = {}
        self.updater.run(entity=tag, refresh=True)
        self.assertEqual(self.row(tag)['name_source'], 'wiki')

    def test_work_ambiguity_chinese_candidate_tiebreak(self):
        tag = self.add('raiden_shogun', 'genshin_impact')
        self.subjects = [{'id': 1, 'name': '原神', 'name_cn': '无关中文名'},
                         {'id': 2, 'name': '原神 -Genshin Impact-', 'name_cn': '原神'}]
        self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['work_name'], '原神')
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('雷电将军', 'wiki'))
        self.subjects[1] = {'id': 2, 'name': '原神', 'name_cn': '另一中文名'}
        self.updater.run(entity=tag, refresh=True)
        self.assertEqual(self.row(tag)['work_name'], '原神')

    def test_candidates_bounded_and_parentheses_filtered(self):
        tag = self.add('amiya_(arknights)', 'arknights')
        self.updater.run(entity=tag)
        self.assertEqual([body['keyword'] for url, body in self.calls if '/search/characters' in url], ['阿米娅', 'アーミヤ'])
        self.assertEqual(keywords(['名（作品）', '名(作品)', '名' * 41, '一', '二', '三', '四']), ['一', '二', '三'])

    def test_work_network_failure_does_not_block_character(self):
        tag = self.add('mika_(blue_archive)', 'example_work', '聖園ミカ')
        self.results['聖園ミカ'] = [self.result(114601, '聖園ミカ', '圣园未花')]
        def failing_work(url, body=None, key=None):
            if '/search/subjects' in url:
                raise OSError('offline')
            return self.transport(url, body, key)
        self.updater.transport = failing_work
        with patch.dict(WIKIS, {'example_work': ['作品甲']}):
            self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['name'], '圣园未花')
        self.assertEqual(self.updater.info()['state']['status'], 'error')

    def test_index_japanese_precedes_model(self):
        tag = self.add('hakurei_reimu', 'touhou', '博麗霊夢')
        with patch.dict(WIKIS, {tag: ['博麗霊夢', '霊夢']}):  # no Chinese wiki alias
            self.updater.run(entity=tag)
        self.updater.replace('character', tag, 'model', [('zh', '机翻名')])
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('博麗霊夢', 'index'))
        self.updater.configure('sk-fixture-only-1234')
        count = len(self.calls)
        self.updater.translate('character', tag, 'touhou')
        self.assertEqual(len(self.calls), count)

    def test_v19_migration_removes_bad_display_rows_only(self):
        self.store = terms_seed.base(self.root, db=self.root / 'library.sqlite')
        self.source = self.store.source
        with self.store.connect() as c:
            c.execute("UPDATE meta SET value='19' WHERE key='schema_version'")
            c.execute("INSERT INTO display_names VALUES ('character','alice','ja','danbooru','レイマリ')")
            c.execute("INSERT INTO display_names VALUES ('character','alice','zh','deepseek','机翻名')")
            c.execute("INSERT INTO name_cache VALUES ('legacy-search','{}')")
            before = [tuple(r) for r in c.execute('SELECT * FROM memberships')]
        Store(self.source, self.store.path)
        with self.store.connect() as c:
            self.assertFalse(c.execute("SELECT 1 FROM display_names WHERE source='danbooru'").fetchone())
            self.assertTrue(c.execute("SELECT 1 FROM name_cache WHERE key='legacy-search'").fetchone())
            # Character names moved onto their identities (schema 24).
            self.assertFalse(c.execute("SELECT 1 FROM names WHERE source='danbooru' AND role='display'").fetchone())
            self.assertEqual(c.execute("SELECT source FROM names WHERE value='机翻名'").fetchone()[0], 'model')
            self.assertEqual(c.execute("SELECT value FROM display_names WHERE kind='work' AND key='touhou' AND source='builtin'").fetchone()[0], '东方Project')
            self.assertEqual(c.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()[0], SCHEMA_VERSION)
            self.assertEqual(before, [tuple(r) for r in c.execute('SELECT * FROM memberships')])

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

    def test_chinese_original_name_beats_secret_name_in_infobox(self):
        tag = self.add('raiden_shogun', 'genshin_impact')
        self.results['雷电将军'] = [self.result(98862, '雷电将军', '巴尔泽布')]
        self.subjects = [{'id': 1, 'name': '原神', 'name_cn': '原神'}]
        self.updater.run(entity=tag)
        row = self.row(tag)
        self.assertEqual((row['name'], row['names']['zh'], row['names']['ja'], row['name_source']),
                         ('雷电将军', '雷电将军', None, 'bangumi'))
        self.assertEqual(self.store.characters('巴尔泽布')[0]['id'], tag)
        with self.store.connect() as c:
            self.assertFalse(c.execute("SELECT 1 FROM display_names WHERE kind='character' AND key=? AND value='巴尔泽布'", (tag,)).fetchone())

    def test_japanese_han_name_is_not_chinese(self):
        tag = self.add('hakurei_reimu', 'touhou')
        self.results['博丽灵梦'] = [self.result(12, '博麗霊夢', '博丽灵梦')]
        self.updater.run(entity=tag)
        row = self.row(tag)
        self.assertEqual((row['names']['ja'], row['names']['zh']), ('博麗霊夢', '博丽灵梦'))
        self.assertEqual(row['name'], '博丽灵梦')

    def test_japanese_han_name_without_matched_work_is_not_chinese(self):
        tag = self.add('yuuki_makoto_(persona_3)', 'persona_3')
        with patch.dict(WIKIS, {tag: ['結城理']}):
            self.results['結城理'] = [self.result(546, '結城理', '结城理')]
            self.updater.run(entity=tag)
        row = self.row(tag)
        self.assertEqual((row['names']['zh'], row['names']['ja']), ('结城理', '結城理'))

    def test_chinese_name_without_unique_work_subject_uses_wiki_exact(self):
        tag = self.add('nahida_(genshin_impact)', 'genshin_impact', ja='纳西妲')
        self.results['纳西妲'] = [self.result(112644, '纳西妲', '布耶尔')]
        self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['name'], '纳西妲')

    def test_unknown_chinese_work_uses_exact_wiki_name_without_subject(self):
        tag = self.add('角色甲_(unknown)', 'unknown')
        with patch.dict(WIKIS, {tag: ['角色甲']}):
            self.results['角色甲'] = [self.result(101, '角色甲', '人物别名')]
            self.updater.run(entity=tag)
        self.assertEqual((self.row(tag)['name'], self.row(tag)['names']['ja']), ('角色甲', None))

    def test_hangul_wiki_alias_never_becomes_chinese_or_japanese(self):
        tag = self.add('modernia_(nikke)', 'nikke')
        with patch.dict(WIKIS, {tag: ['모더니아']}):
            self.results['모더니아'] = [self.result(102, '모더니아')]
            self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['name_source'], 'readable')
        self.assertEqual((self.row(tag)['names']['zh'], self.row(tag)['names']['ja']), (None, None))

    def test_individual_does_not_adopt_team_wiki_alias(self):
        self.assertFalse(group_alias('Steam'))
        self.assertTrue(group_alias('カバさんチーム'))
        tag = self.add('saemonza_(girls_und_panzer)', 'girls_und_panzer')
        with patch.dict(WIKIS, {tag: ['左衛門佐', '杉山清美', 'カバさんチーム']}):
            self.results['左衛門佐'] = [self.result(17426, '左衛門佐 / 杉山清美', '左卫门佐')]
            self.results['カバさんチーム'] = [self.result(166515, 'カバさんチーム', '河马队')]
            self.updater.run(entity=tag)
        self.assertEqual(self.row(tag)['name_source'], 'readable')
        self.assertFalse(any(body and body.get('keyword') == 'カバさんチーム' for _, body in self.calls))

    def test_japanese_han_name_with_unmatched_work_stays_japanese(self):
        tag = self.add('shiina_mahiru_(otonari_no_tenshi-sama)', 'otonari_no_tenshi-sama')
        with patch.dict(WIKIS, {tag: ['椎名真昼']}):
            self.results['椎名真昼'] = [self.result(76270, '椎名真昼', '椎名真昼')]
            self.updater.run(entity=tag)
        self.assertEqual((self.row(tag)['names']['zh'], self.row(tag)['names']['ja']), ('椎名真昼', '椎名真昼'))

    def test_ambiguous_entries_agree_or_abstain_on_chinese_name(self):
        tag = self.add('hatsune_miku', 'vocaloid', '初音ミク')
        self.results['初音ミク'] = [self.result(10452, '初音ミク', '初音未来'),
                                self.result(184752, '初音ミク', '初音未来')]
        self.updater.run(entity=tag)
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('初音未来', 'bangumi'))
        self.results['初音ミク'][1]['infobox'][0]['value'] = '另一名字'
        self.updater.run(entity=tag, refresh=True)
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('初音ミク', 'index'))

    def test_exact_query_result_beats_broad_wiki_aliases_in_same_page(self):
        tag = self.add('hatsune_miku', 'vocaloid', '初音ミク')
        with patch.dict(WIKIS, {tag: ['初音ミク', '初音', '初音未來']}):
            self.results['初音ミク'] = [self.result(10452, '初音ミク', '初音未来'),
                                   self.result(184752, '初音未來', '初音未来'),
                                   self.result(1501, '初音')]
            self.updater.run(entity=tag)
        self.assertEqual((self.row(tag)['name'], self.row(tag)['name_source']), ('初音未来', 'bangumi'))

    def test_series_builtin_overrides_specific_subject_but_not_user(self):
        for key, title in WORK_NAMES.items():
            self.updater.replace('work', key, 'bangumi', [('zh', '某个具体作品')])
            with self.store.connect() as c:
                chosen = View(c).choose('work', key)
                self.assertEqual((chosen['name'], chosen['name_source']), (title, 'builtin'))
        self.updater.replace('work', 'fate', 'user', [('zh', '用户命名')])
        with self.store.connect() as c:
            self.assertEqual(View(c).choose('work', 'fate')['name'], '用户命名')
            self.assertEqual(View(c).choose('work', 'touhou_koumakyou')['name_source'], 'readable')

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
