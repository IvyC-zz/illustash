"""Display-only enrichment, with text provider fixtures and no real network."""
import contextlib
import copy
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
from pathlib import Path
from unittest.mock import patch

from tests.synthetic_source import source_fixture as fixture
from tests import terms_seed
from web import names
from web.names import MODEL_BASE, NameUpdater, View, put_name
from web.store import Store
DS = MODEL_BASE + '/chat/completions'


class Providers:
    def __init__(self):
        self.calls = []
        self.wikis = {'Example': ['エグザンプル'], 'alice': ['ア_リス', '愛麗絲'], 'bob': ['ボブ']}
        self.subjects = [{'id': 99, 'name': 'エグザンプル外伝', 'name_cn': '错误作品'},
                         {'id': 1, 'name': 'エグザンプル', 'name_cn': '示例作品'}]
        self.characters = [{'id': 10, 'name': 'アリス'}, {'id': 11, 'name': '別人'}]
        self.character_fields = {10: {'name': 'アリス', 'infobox': [{'key': '简体中文名', 'value': '爱丽丝'}]}}
        self.translation = {'zh': '译名'}

    def __call__(self, url, body=None, key=None):
        self.calls.append((url, copy.deepcopy(body), key))
        if 'wiki_pages.json' in url:
            tag = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)['search[title]'][0]
            return [{'title': tag, 'other_names': self.wikis[tag]}] if tag in self.wikis else []
        if '/search/subjects' in url:
            return {'data': self.subjects, 'total': len(self.subjects)}
        if '/search/characters' in url:
            return {'data': [r | self.character_fields.get(r['id'], {}) for r in self.characters]}
        if '/characters/' in url and url.endswith('/subjects'):
            return [{'name': 'エグザンプル', 'name_cn': '示例作品'}]
        if url == DS:
            return {'choices': [{'message': {'content': json.dumps(self.translation, ensure_ascii=False)}}]}
        raise AssertionError('Unexpected endpoint')


class NamesTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        # Ensure Chinese names here come from providers, not the old fixture.
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE characters SET name_zh=NULL,name_ja=CASE WHEN char_id='alice' THEN 'アリス' ELSE NULL END")
        self.store = Store(self.source, self.root / 'library.sqlite')
        work = self.store.edit_work('create', name='Example')['id']
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                Store.link(c, work, 'qualifier', 'Example')
                Store.link(c, work, 'danbooru', 'Example')
                c.execute("UPDATE names SET source='index',preferred=0 WHERE entity_id=?", (work,))
        self.store = old
        self.store.refresh()
        with self.store.connect() as c:
            self.ids = dict(c.execute("SELECT key,entity_id FROM links WHERE system='danbooru' AND entity_id IN (SELECT id FROM entities WHERE kind<>'work')"))
        self.providers = Providers()
        self.updater = NameUpdater(self.store, self.providers, interval=0)

    def tearDown(self):
        self.temp.cleanup()

    def char(self, tag='alice'):
        return next(r for r in self.store.characters() if r['id'] == self.ids[tag])

    def add(self, kind, key, lang, source, value):
        self.updater._core({'op':'put','kind':kind,'key':key,'lang':lang,'source':source,'value':value})

    @contextlib.contextmanager
    def startup(self):
        with terms_seed.staged(self.store) as old:
            yield old
        self.store = old
        self.updater.store = old

    def clear_display(self, *, entities=False):
        # 缺失显示数据是旧启动边界；补回空表后收编，核心仍承担全部后续读写。
        with self.startup() as old:
            with old.connect() as c:
                c.execute('DROP TABLE display_names')
                c.executescript(names.SCHEMA)
                if entities:
                    c.execute("DELETE FROM names WHERE role='display'")

    def test_start_validates_without_loading_the_whole_scope(self):
        with patch('web.names.scope', side_effect=AssertionError('前台不应装载全库')), patch('threading.Thread.start') as start:
            with self.assertRaisesRegex(ValueError, '角色不存在'):
                self.updater.start('missing')
            start.assert_not_called()
            self.updater.start(self.ids['alice'])
            start.assert_called_once()

    def test_local_oc_and_local_child_names_untouched(self):
        for kind in ('', 'oc'):
            local = self.store.create_entity('我的名字' + kind, kind=kind)
            self.add('character', local['name'], 'zh', 'bangumi', '不可替换')
            row = self.store.entities(local['name'])[0]
            self.assertEqual(row['name'], local['name'])
            self.assertEqual(row['tag'], '')
            self.assertEqual(row['name_source'], 'user')
        self.store.set_kind(self.ids['alice'], 'oc')
        before = self.char()['name']
        self.updater.run()
        self.assertEqual(self.char()['name'], before)
        self.assertEqual(self.char()['name_source'], 'user')
        self.assertEqual(self.char()['tag'], 'alice')

    def test_no_identity_effect_when_names_removed(self):
        self.updater.run()
        action = self.store.change(['c'], 'confirmed', self.ids['alice'])
        self.assertIn('爱丽丝', action['label'])
        with self.store.connect() as c:
            snapshot = {t: [tuple(r) for r in c.execute('SELECT * FROM ' + t)] for t in ('entities', 'memberships', 'assertions', 'candidates', 'links')}
        before = self.store.detail('a')
        self.clear_display(entities=True)
        after = self.store.detail('a')
        def identity_only(obj):
            if isinstance(obj, list):
                return [identity_only(x) for x in obj]
            if isinstance(obj, dict):
                return {k: identity_only(v) for k, v in obj.items() if k not in ('name', 'names', 'name_source', 'work_name')}
            return obj
        self.assertEqual(identity_only(before), identity_only(after))
        with self.store.connect() as c:
            for t, rows in snapshot.items():
                self.assertEqual(rows, [tuple(r) for r in c.execute('SELECT * FROM ' + t)])
        self.store.undo(action['id'])
        self.assertEqual(self.store.assets(entity=self.ids['alice'])['total'], 2)
        # Identity deduplication cannot start treating translations as aliases.
        self.assertEqual(self.store.create_entity('alice')['id'], self.ids['alice'])

    def test_single_work_and_skin_choices_do_not_load_all_identities(self):
        self.updater.run()
        with self.store.connect() as c:
            full = View(c)
            full.load()
            keys = [('work', 'Example'), ('work', 'touhou'), ('work', 'missing'),
                    ('skin', 'swimsuit'), ('skin', 'missing')]
            expected = {pair: full.choose(*pair) for pair in keys}
            with patch('web.names.entity_rows', side_effect=AssertionError('full library scan')):
                for pair in keys:
                    self.assertEqual(View(c).choose(*pair), expected[pair])
                view = View(c)
                view.choose('work', 'Example')
                self.add('work', 'Example', 'zh', 'user', '人工作品名')
                self.assertEqual(view.choose('work', 'Example')['name'], '人工作品名')
        self.clear_display()
        with self.store.connect() as c:
            self.assertEqual(View(c).choose('work', 'touhou')['name'], '东方Project')

    def test_single_character_lookup_preserves_canonical_links_and_priority(self):
        self.updater.run()
        extra = [self.store.create_entity('lookup-case-' + str(i))['id'] for i in range(5)]
        with self.startup() as old:
            with old.connect() as c:
                Store.link(c, extra[0], 'wd', 'alice')  # same canonical tag on a second identity
                Store.link(c, extra[1], 'ccip_base', json.dumps(['alice', 'Other']))
                Store.link(c, extra[2], 'danbooru', 'skin-only')
                c.execute('UPDATE entities SET parent_id=? WHERE id=?', (self.ids['alice'], extra[2]))
                Store.link(c, extra[3], 'danbooru', 'retired')
                c.execute("UPDATE links SET current=0 WHERE key='retired'")
                Store.link(c, extra[3], 'ccip_base', json.dumps(['retired-base', 'Other']))
                c.execute("UPDATE links SET current=0 WHERE entity_id=?", (extra[3],))
                Store.link(c, extra[4], 'danbooru', 'z-current')
                Store.link(c, extra[4], 'danbooru', 'a-current')
                c.execute("UPDATE links SET added_at='2000-01-01' WHERE entity_id=?", (extra[4],))
                for i, ident in enumerate(extra):
                    put_name(c, ident, 'zh', 'user', '额外名字' + str(i), preferred=1)
        keys = ['alice', 'bob', 'missing', 'skin-only', 'retired', 'retired-base', 'a-current', 'z-current']
        def compare():
            with self.store.connect() as c:
                full = View(c)
                full.load()
                expected = {key: full.choose('character', key) for key in keys}
                with patch('web.names.entity_rows', side_effect=AssertionError('full library scan')):
                    for key in keys:
                        self.assertEqual(View(c).choose('character', key), expected[key], key)
        compare()
        # 同一 WD 链接变为次要链接，合并身份仍保留自身规范键。
        with self.startup() as old:
            with old.connect() as c:
                Store.link(c, extra[0], 'danbooru', 'new-canonical')
                c.execute('UPDATE entities SET merged_into=? WHERE id=?', (self.ids['alice'], extra[1]))
        keys.append('new-canonical')
        compare()
        self.clear_display()
        compare()

    def test_translate_with_cached_names_never_loads_full_library(self):
        self.updater.configure('sk-test-secret-1234')
        self.updater.run()
        calls = len(self.providers.calls)
        with patch('web.names.entity_rows', side_effect=AssertionError('full library scan')):
            self.updater.translate('character', 'alice', 'Example')  # sourced name: no model call
            self.updater.translate('character', 'bob', 'Example')  # cached model response
        self.assertEqual(len(self.providers.calls), calls)
        self.assertEqual(self.char()['name'], '爱丽丝')
        self.assertEqual(self.char('bob')['name_source'], 'model')

    def test_progress_is_a_database_free_state_copy(self):
        self.updater.state = {'status': 'running', 'done': 3, 'total': 10}
        with patch.object(self.store, 'connect', side_effect=AssertionError('progress must not read DB')):
            result = self.updater.progress()
        self.assertEqual(result, {'state': self.updater.state})
        result['state']['done'] = 9
        self.assertEqual(self.updater.state['done'], 3)

    def test_model_api_cache_restores_deleted_name_data(self):
        self.updater.configure('sk-test-secret-1234')
        self.updater.run()
        count = len(self.providers.calls)
        self.clear_display()
        self.updater.run()
        self.assertEqual(len(self.providers.calls), count)
        self.assertEqual(self.char('bob')['name_source'], 'model')

    def test_legacy_undo_label_renders_without_rewriting_history(self):
        action = self.store.change(['c'], 'confirmed', self.ids['alice'])
        with self.store.connect() as c:
            c.execute("UPDATE actions SET label='已将 1 张设为 alice' WHERE id=?", (action['id'],))
        self.updater.run()
        self.assertEqual(self.store.overview()['undo']['label'], '已将 1 张设为 爱丽丝')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT label FROM actions WHERE id=?', (action['id'],)).fetchone()[0], '已将 1 张设为 alice')
        self.store.undo(action['id'])
        self.assertEqual(self.store.assets(entity=self.ids['alice'])['total'], 2)

    def test_new_external_rows_return_original_tag_without_identity_write(self):
        with self.startup() as old:
            with old.connect() as c:
                c.execute("INSERT INTO entities(id,name,external_key,kind) VALUES ('new','new_tag_(work)','wd:new_tag_(work)','character')")
                Store._links_from_columns(c)
                Store._names_from_tables(c)
        row = self.store.entities('new_tag')[0]
        self.assertEqual((row['name'], row['tag']), ('New Tag', 'new_tag_(work)'))
        with self.store.connect() as c:
            self.assertIsNone(c.execute("SELECT tag FROM entities WHERE id='new'").fetchone()[0])

    def test_background_does_not_block_browsing(self):
        entered, release = threading.Event(), threading.Event()
        def slow(*args):
            entered.set()
            release.wait(3)
            return self.providers(*args)
        self.updater.transport = slow
        self.updater.start()
        self.assertTrue(entered.wait(2))
        try:
            self.assertEqual(self.store.overview()['total'], 3)
            self.assertEqual(self.updater.info()['state']['status'], 'running')
            # 真实索引输入在后台遍历期间到达；公开重扫交给核心。
            with sqlite3.connect(self.source) as c:
                c.execute("INSERT INTO characters VALUES ('fresh',NULL,NULL,NULL,NULL,'fresh')")
                c.execute("INSERT INTO labels VALUES ('c','character','fresh',0.9)")
            self.store.reindex_characters(['c'])
            self.updater.start()  # an import arriving during a pass queues another pass
        finally:
            release.set()
            self.updater.thread.join(5)
        self.assertEqual(self.updater.info()['state']['status'], 'done')
        self.assertTrue(any('fresh' in call[0] for call in self.providers.calls))
        off = NameUpdater(self.store, enabled=False)
        with self.assertRaises(ValueError):
            off.start()


