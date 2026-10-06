"""Display-only enrichment, with text provider fixtures and no real network."""
import contextlib
import copy
import io
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.parse
from pathlib import Path
from unittest.mock import patch

from tests.test_web import fixture
from tests import terms_seed
from web import names, terms_core
from web.names import (BG, MODEL_BASE, FetchError, NameUpdater, View, danbooru_names,
                       normalize, put, put_name, readable, request_json, scope, skin_zh, wiki_zh)
from web.store import SCHEMA_VERSION, Store
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

    def test_fallback_and_source_priority(self):
        self.assertEqual(self.char()['name'], 'Alice')
        self.assertEqual(self.char()['name_source'], 'readable')
        self.add('character', 'alice', 'ja', 'index', 'アリス')
        self.assertEqual(self.char()['name'], 'アリス')
        for source, name in [('model', '机翻'), ('danbooru', '站点中文'), ('index', '索引中文'), ('bangumi', '中文')]:
            self.add('character', 'alice', 'zh', source, name)
        self.assertEqual(self.char()['name'], '中文')
        self.assertEqual(self.char()['name_source'], 'bangumi')
        self.add('character', 'alice', 'zh', 'user', '用户中文')
        self.assertEqual(self.char()['name'], '用户中文')
        self.assertEqual(self.char()['tag'], 'alice')
        self.assertEqual(readable('mika_(blue_archive)', True), 'Mika')
        self.assertEqual(readable('hatsune_miku', True), 'Hatsune Miku')

    def test_han_only_danbooru_never_chinese(self):
        rows = danbooru_names({'other_names': ['博麗霊夢', '聖園ミカ', 'Mika', '미카']})
        self.assertEqual(rows, ['博麗霊夢', '聖園ミカ', 'Mika', '미카'])

    def test_wiki_chinese_alias_selection(self):
        self.assertEqual(wiki_zh(['ホタル', '流螢', '流萤(崩坏:星穹铁道)', '流萤']), '流萤')
        self.assertEqual(wiki_zh(['モルガン(Fate)', 'モルガン・ル・フェ', '摩根·勒·菲', '摩根(fate)']), '摩根·勒·菲')
        self.assertEqual(wiki_zh(['露娜', '银冕']), '露娜')  # wiki order, not the most "Chinese-looking" one
        self.assertEqual(wiki_zh(['阿尔托莉雅·潘德拉贡〔Alter〕']), '阿尔托莉雅·潘德拉贡〔Alter〕')
        for names in (['博麗霊夢'], ['神里綾華'], ['Ado誕生日'], ['AK-12(少女前線)'], ['寒色兄妹'], ['カイト', 'Kaito'], ['미카']):
            self.assertIsNone(wiki_zh(names), names)
        self.assertIsNone(wiki_zh(['重返未来6'], ['重返未来：1999', '重返未来']))

    def test_wiki_chinese_alias_is_below_bangumi_and_skips_model(self):
        self.providers.wikis['bob'] = ['ボブ', '鲍勃']
        self.updater.configure('sk-test-secret-1234')
        self.updater.run()
        self.assertEqual(self.char('bob')['name'], '鲍勃')
        self.assertEqual(self.char('bob')['name_source'], 'wiki')
        self.assertFalse(any(call[0] == DS for call in self.providers.calls))
        self.add('character', 'bob', 'ja', 'index', 'ボブ')
        self.assertEqual(self.char('bob')['name'], '鲍勃')  # Chinese first, even over a sourced Japanese name
        self.providers.wikis['alice'] = ['ア_リス', '爱丽丝酱']
        self.updater.run(refresh=True)
        self.assertEqual(self.char()['name'], '爱丽丝')
        self.assertEqual(self.char()['name_source'], 'bangumi')

    def test_bangumi_search_names(self):
        self.updater.run()
        self.assertEqual(self.updater.info()['state']['status'], 'done')
        self.assertEqual(self.char()['name'], '爱丽丝')
        self.assertEqual(self.char()['names']['ja'], 'アリス')
        self.assertEqual(self.char()['work'], 'Example')
        self.assertEqual(self.char()['work_name'], '示例作品')
        self.assertEqual(self.char('bob')['name'], 'Bob')
        self.assertEqual(self.char('bob')['name_source'], 'readable')
        self.assertFalse(any('/characters/11' in call[0] for call in self.providers.calls))
        self.assertEqual(normalize('Ａ ・ リ ス'), normalize('A･リス'))

    def test_nonexact_character_search_result_is_not_adopted(self):
        self.providers.character_fields[10] = {'name': '違う人', 'name_cn': '不可采用'}
        self.updater.run()
        self.assertNotEqual(self.char()['name_source'], 'bangumi')
        self.assertIsNone(self.char()['names']['zh'])

    def test_ambiguous_or_nonexact_subject_not_adopted(self):
        self.providers.subjects = self.providers.subjects[:1]
        self.updater.run()
        self.assertEqual(self.char()['work_name'], 'Example')
        self.assertEqual(self.char()['name_source'], 'bangumi')
        self.providers.subjects += [{'id': 1, 'name': 'エグザンプル'}, {'id': 2, 'name': 'エグザンプル'}]
        self.updater.run(refresh=True)
        self.assertEqual(self.char()['name_source'], 'bangumi')

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

    def test_skin_compound_fallback_and_full_display(self):
        self.assertEqual(skin_zh('swimsuit_maid'), '泳装 · 女仆')
        self.assertEqual(skin_zh('first_ascension'), '第一再临')
        self.assertIsNone(skin_zh('swimsuit_foreigner'))
        with self.startup() as old:
            with old.connect() as c:
                c.execute("INSERT INTO entities(id,name,tag,external_key,parent_id,variant,kind) VALUES ('skin','skin','alice_swim','index:skin',?,'swimsuit','skin')", (self.ids['alice'],))
                Store._links_from_columns(c)
                Store._names_from_tables(c)
                c.execute("INSERT INTO candidates VALUES ('a','skin','WD',0.9)")
        self.updater.run()
        v = self.char()['variants'][0]
        self.assertEqual((v['name'], v['tag'], v['name_source']), ('泳装', 'swimsuit', 'builtin'))
        candidate = next(x for x in self.store.detail('a')['candidates'] if x['id'] == 'skin')
        self.assertEqual((candidate['name'], candidate['tag']), ('爱丽丝 · 泳装', 'alice_swim'))
        with self.store.connect() as c:
            self.assertEqual(View(c).choose('skin', 'swimsuit_foreigner')['name'], 'Swimsuit Foreigner')
        with self.startup() as old:
            with old.connect() as c:
                c.execute("DELETE FROM names WHERE entity_id=? AND role='display'", (self.ids['alice'],))
        self.add('character', 'alice', 'zh', 'model', '机翻角色')
        candidate = next(x for x in self.store.detail('a')['candidates'] if x['id'] == 'skin')
        self.assertEqual(candidate['name_source'], 'model')
        self.assertEqual(self.char()['variants'][0]['name_source'], 'builtin')

    def test_all_search_surfaces_and_all_name_sources(self):
        self.updater.run()
        self.add('character', 'bob', 'en', 'index', 'English Alias')
        for query in ('示例作品', 'エグザンプル', 'Example'):
            self.assertEqual({r['id'] for r in self.store.characters(query)}, set(self.ids.values()))
            self.assertEqual({r['id'] for r in self.store.entities(query)}, set(self.ids.values()))
            self.assertEqual(self.store.assets(query=query)['total'], 2)
        for query in ('爱丽丝', 'アリス', 'Alice', 'alice', 'a_li_ce'):
            self.assertEqual(self.store.characters(query)[0]['id'], self.ids['alice'])
        self.assertEqual(self.store.characters('English Alias')[0]['id'], self.ids['bob'])

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

    def test_negative_cache_survives_restart_and_refresh_bypasses(self):
        self.providers.wikis = {}
        self.updater.run()
        count = len(self.providers.calls)
        NameUpdater(self.store, self.providers, interval=0).run()
        self.assertEqual(count, len(self.providers.calls))
        self.updater.run(entity=self.ids['alice'], refresh=True)
        self.assertEqual(len(self.providers.calls), count + 3)  # two wikis and indexed Japanese character search

    def test_network_errors_retry_and_never_log(self):
        def broken(*args):
            raise OSError('secret must not leak')
        self.updater.transport = broken
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            self.updater.run()
        self.assertEqual(output.getvalue(), '')
        self.assertNotIn('secret', json.dumps(self.updater.info()))
        self.updater.transport = self.providers
        self.updater.run()
        self.assertEqual(self.char()['name'], '爱丽丝')

    def test_model_api_key_redaction_clear_and_test(self):
        key = 'sk-secret-only-local-1234'
        self.updater.configure(key, 'deepseek-chat')
        info = self.updater.info()
        self.assertEqual(info['model_api'], {'configured': True, 'hint': '1234', 'model': 'deepseek-chat', 'base': MODEL_BASE})
        self.assertNotIn(key, json.dumps(info))
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            self.assertEqual(self.updater.test(), {'ok': True})
        self.assertNotIn(key, output.getvalue())
        self.assertEqual(self.providers.calls[-1][1]['messages'], [{'role': 'user', 'content': 'Reply with OK.'}])
        self.updater.configure(None)
        self.assertFalse(self.updater.settings()['configured'])
        with self.store.connect() as c:
            self.assertIsNone(c.execute("SELECT value FROM name_settings WHERE key IN ('model_key','deepseek_key')").fetchone())
        self.assertFalse(self.updater.test()['ok'])

    def test_model_api_last_resort_and_negative_cache(self):
        self.updater.configure('sk-test-secret-1234')
        self.updater.run()
        self.assertEqual(self.char()['name_source'], 'bangumi')
        self.assertEqual(self.char('bob')['name_source'], 'model')
        ds = [r for r in self.providers.calls if r[0] == DS]
        self.assertEqual(len(ds), 1)
        self.assertEqual(set(json.loads(ds[0][1]['messages'][1]['content'])), {'tag', 'japanese_name', 'work'})
        self.updater.run()
        self.assertEqual(len([r for r in self.providers.calls if r[0] == DS]), 1)
        self.providers.translation = {'zh': None}
        self.updater.run(refresh=True)
        count = len(self.providers.calls)
        self.updater.run()
        self.assertEqual(len(self.providers.calls), count)

    def test_launch_only_looks_up_unfinished_names(self):
        # 2026-09-26: every launch and sync walked all 3,894 names again, rewriting each one.
        self.updater.run()
        self.assertEqual(self.char()['name'], '爱丽丝')
        count = len(self.providers.calls)
        with patch.object(self.updater, 'seed', side_effect=AssertionError('warm launch must not seed')), \
                patch('web.names.threading.Thread', side_effect=AssertionError('no jobs need no threads')):
            self.updater.run()
        self.assertEqual(self.updater.state['total'], 0)
        self.assertEqual(self.updater.state['status'], 'done')
        self.assertEqual(len(self.providers.calls), count)
        self.updater.run(refresh=True)   # 全部重新获取 still redoes everything
        self.assertGreater(self.updater.state['total'], 0)

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

    def test_failed_lookup_is_retried_next_launch(self):
        def broken(*args):
            raise OSError('offline')
        self.updater.transport = broken
        self.updater.run()
        self.updater.transport = self.providers
        self.updater.run()
        self.assertEqual(self.char()['name'], '爱丽丝')

    def test_unusable_model_answer_is_asked_once(self):
        self.updater.configure('sk-test-secret-1234')
        self.providers.translation = {'zh': 'ボブ'}   # kana: not a Chinese name
        self.updater.run()
        self.assertNotEqual(self.char('bob')['name_source'], 'model')
        asked = len([r for r in self.providers.calls if r[0] == DS])
        self.assertEqual(asked, 1)
        self.updater.run()
        self.assertEqual(len([r for r in self.providers.calls if r[0] == DS]), asked)

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

    def test_user_name_survives_provider_refresh(self):
        self.add('character', 'alice', 'zh', 'user', '人工名')
        self.updater.run(refresh=True)
        self.assertEqual(self.char()['name'], '人工名')
        self.assertEqual(self.char()['name_source'], 'user')
        # Display names never become identity aliases or change create semantics.
        created = self.store.create_entity('人工名')
        self.assertNotEqual(created['id'], self.ids['alice'])

    def test_traffic_budget_stops_before_request(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO name_settings VALUES ('traffic_reserved','500000000')")
        with self.assertRaises(FetchError):
            self.updater.cached(BG + '/subjects/1')
        self.assertFalse(self.providers.calls)

    def test_provider_error_body_never_exposed(self):
        error = urllib.error.HTTPError(DS, 401, 'sk-secret', {}, io.BytesIO(b'sk-secret'))
        with patch('urllib.request.urlopen', side_effect=error):
            with self.assertRaises(FetchError) as caught:
                request_json(DS, {}, 'sk-secret')
        self.assertEqual(str(caught.exception), 'HTTP 401: 认证失败')

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

    def test_only_appeared_entities_are_requested(self):
        with self.startup() as old:
            with old.connect() as c:
                c.execute("INSERT INTO entities(id,name,tag,external_key,work,kind) VALUES ('unused','unused','unused','index:unused','unused_work','character')")
                Store._links_from_columns(c)
                Store._names_from_tables(c)
        self.updater.run()
        self.assertNotIn('unused', str(self.providers.calls))
        self.assertEqual(self.updater.info()['counts']['characters']['total'], 2)

    def test_v18_migration_preserves_identity_and_memberships(self):
        self.store = terms_seed.startup_input(self.store)
        with self.store.connect() as c:
            before = [tuple(r) for r in c.execute('SELECT * FROM memberships')]
            c.execute("UPDATE meta SET value='18' WHERE key='schema_version'")
            for table in ('display_names', 'name_settings', 'name_cache'):
                c.execute('DROP TABLE ' + table)
        self.store = Store(self.source, self.store.path, legacy=True)
        terms_seed.switch(self.store)
        self.updater.store = self.store
        with self.store.connect() as c:
            self.assertEqual(before, [tuple(r) for r in c.execute('SELECT * FROM memberships')])
            self.assertEqual(c.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()[0], SCHEMA_VERSION)

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


class ModelApiSettingsTest(unittest.TestCase):
    """Any OpenAI-compatible endpoint; older deepseek_* settings keep working."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); root = Path(self.temp.name)
        self.store = Store(fixture(root), root / 'app.sqlite'); self.store.refresh()
        self.updater = NameUpdater(self.store, transport=lambda *a: {'choices': [{'message': {'content': 'OK'}}]}, interval=0)

    def tearDown(self):
        self.temp.cleanup()

    def test_custom_endpoint_validation_and_legacy_keys(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO name_settings VALUES ('deepseek_key','sk-legacy-9999'),('deepseek_model','old-model')")
        self.assertEqual(self.updater.settings(), {'configured': True, 'hint': '9999', 'model': 'old-model', 'base': MODEL_BASE})
        self.updater.configure(base='https://api.example.com/v1/')          # key and model are kept
        info = self.updater.settings(secret=True)
        self.assertEqual((info['key'], info['model'], info['base']), ('sk-legacy-9999', 'old-model', 'https://api.example.com/v1'))
        self.assertEqual(NameUpdater.endpoint(info['base']), 'https://api.example.com/v1/chat/completions')
        self.assertEqual(NameUpdater.endpoint('http://localhost:11434/v1/chat/completions'), 'http://localhost:11434/v1/chat/completions')
        calls = []
        self.updater.transport = lambda url, body=None, key=None: calls.append(url) or {'choices': [{'message': {'content': 'OK'}}]}
        self.assertEqual(self.updater.test(), {'ok': True})
        self.assertEqual(calls, ['https://api.example.com/v1/chat/completions'])  # an unknown host gets its own throttle
        self.updater.configure(base='http://localhost:11434/v1')
        for bad in ('http://api.example.com', 'ftp://x', 'https://x.com/?a=1', 'not a url', 12):
            with self.assertRaises(ValueError, msg=bad):
                self.updater.configure(base=bad)
        with self.assertRaises(ValueError):
            self.updater.configure(model='bad model')
        self.updater.configure(None)
        self.assertFalse(self.updater.settings()['configured'])
        self.assertEqual(self.updater.settings()['model'], 'old-model')             # clearing the key keeps the rest
