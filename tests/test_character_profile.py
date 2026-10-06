"""自建角色的资料：从 Danbooru 新建、编辑资料、存量关联。Danbooru 查询用桩函数，不联网。"""
import tempfile
import unittest
from pathlib import Path

from web.store import Invalid, Store
from tests.test_web import fixture


class ProfileTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        self.store = Store(fixture(root), root / 'app.sqlite')
        self.store.refresh()
        self.alice = self.store.entities('Alice')[0]['id']
        self.asked = []

    def tearDown(self):
        self.temp.cleanup()

    def copyright(self, tag):
        self.asked.append(tag)
        return {'karen_(blue_archive)': 'blue_archive', 'hatsune_miku': 'vocaloid'}.get(tag, '')

    def row(self, ident):
        with self.store.connect() as c:
            e = dict(c.execute('SELECT work,work_id,original FROM entities WHERE id=?', (ident,)).fetchone())
            e['links'] = {r[0]: r[1] for r in c.execute('SELECT system,key FROM links WHERE entity_id=? AND current=1', (ident,))}
            e['work_links'] = {r[0]: r[1] for r in c.execute('SELECT system,key FROM links WHERE entity_id=?', (e['work_id'],))} if e['work_id'] else {}
            return e

    def test_create_from_danbooru_links_tag_work_and_keeps_typed_alias(self):
        made = self.store.create_linked('karen_(blue_archive)', 'カレン', self.copyright)
        e = self.row(made['id'])
        self.assertEqual(e['links'], {'danbooru': 'karen_(blue_archive)'})
        self.assertEqual(e['work'], 'blue_archive')
        self.assertEqual(e['work_links'], {'danbooru': 'blue_archive', 'qualifier': 'blue_archive'})
        self.assertEqual(self.store.profile(made['id'])['aliases'], ['カレン'])
        self.assertIn(made['id'], [x['id'] for x in self.store.entities('カレン')])
        # No user display name: the tag's own (fetched) names show, readable until then.
        self.assertEqual(made['name'], 'Karen')
        # Same tag again → the same character; a second character of the work joins the same work without a lookup.
        self.assertEqual(self.store.create_linked('karen_(blue_archive)', '', self.copyright)['id'], made['id'])
        self.asked.clear()
        other = self.store.create_linked('hina_(blue_archive)', '', self.copyright)
        self.assertEqual(self.asked, [])
        self.assertEqual(self.row(other['id'])['work_id'], e['work_id'])
        # A tag already in the library is never duplicated.
        self.assertEqual(self.store.create_linked('alice', '', self.copyright)['id'], self.alice)
        with self.assertRaises(Invalid): self.store.create_linked('', '', self.copyright)

    def test_profile_edit_is_one_undoable_step(self):
        mine = self.store.create_entity('小凯伦')
        karen = self.store.create_linked('karen_(blue_archive)', '', self.copyright)
        work = self.row(karen['id'])['work_id']
        before = self.store.profile(mine['id'])
        r = self.store.set_profile(mine['id'], name='凯伦', aliases=['Karen', 'カレン', 'Karen'], work=work)
        p = self.store.profile(mine['id'])
        self.assertEqual((p['name'], p['aliases'], p['work_id']), ('凯伦', ['Karen', 'カレン'], work))
        self.assertEqual(p['work_name'], self.store.profile(karen['id'])['work_name'])
        self.assertTrue(p['work_name'])
        self.assertEqual(self.row(mine['id'])['work'], 'blue_archive')
        self.store.undo(r['undo']['id'])
        self.assertEqual(self.store.profile(mine['id']), before)
        # One tag, one character: linking a tag another character has points to merging instead.
        with self.assertRaises(Invalid) as err: self.store.set_profile(mine['id'], tag='karen_(blue_archive)')
        self.assertIn('合并', str(err.exception))
        # Linking fills a missing work from the tag; unlinking needs a name to fall back to.
        r = self.store.set_profile(mine['id'], tag='hatsune_miku', copyright_of=self.copyright)
        self.assertEqual(self.row(mine['id'])['work_links'].get('danbooru'), 'vocaloid')
        with self.assertRaises(Invalid): self.store.set_profile(mine['id'], name='', tag='')
        self.store.undo(r['undo']['id'])
        self.assertEqual(self.row(mine['id'])['links'], {})
        self.assertIsNone(self.row(mine['id'])['work_id'])

    def test_oc_and_shared_mark_rules(self):
        oc = self.store.create_entity('', kind='oc')
        work = self.row(self.store.create_linked('karen_(blue_archive)', '', self.copyright)['id'])['work_id']
        with self.assertRaises(Invalid): self.store.set_profile(oc['id'], work=work)
        mark = self.store.shared_oc()
        with self.assertRaises(Invalid): self.store.set_profile(mark['id'], tag='hatsune_miku')
        self.store.set_profile(oc['id'], name='我的 OC', aliases=['oc'])
        self.assertEqual(self.store.profile(oc['id'])['name'], '我的 OC')

    def test_local_characters_link_or_merge_suggestions(self):
        typed = self.store.create_entity('Karen_(blue_archive)')
        dupe = self.store.create_entity('Alice_x')
        unknown = self.store.create_entity('エレノア')
        self.store.set_profile(dupe['id'], name='alice')
        for ident in (typed['id'], dupe['id'], unknown['id']):
            self.store.change(['a'], 'confirmed', ident)
        lookup = lambda tag: {'tag': tag, 'posts': 183} if tag == 'karen_(blue_archive)' else None
        found = {x['id']: x for x in self.store.local_link_suggestions(lookup)}
        self.assertEqual(found[typed['id']]['kind'], 'link')
        self.assertEqual(found[typed['id']]['tag'], 'karen_(blue_archive)')
        self.assertEqual((found[dupe['id']]['kind'], found[dupe['id']]['target']['id']), ('merge', self.alice))
        self.assertNotIn(unknown['id'], found)
        r = self.store.link_local([{'id': typed['id'], 'tag': 'karen_(blue_archive)'}], self.copyright)
        p = self.store.profile(typed['id'])
        # The typed tag-like name steps aside as an alias so the tag's proper names can show.
        self.assertEqual((p['user_name'], p['aliases'], p['tag'], p['name']), ('', ['Karen_(blue_archive)'], 'karen_(blue_archive)', 'Karen'))
        self.assertEqual(self.row(typed['id'])['work'], 'blue_archive')
        self.assertNotIn(typed['id'], [x['id'] for x in self.store.local_link_suggestions(lookup)])
        self.store.undo(r['undo']['id'])
        p = self.store.profile(typed['id'])
        self.assertEqual((p['user_name'], p['aliases'], p['tag'], p['work_id']), ('Karen_(blue_archive)', [], '', None))
        with self.assertRaises(Invalid): self.store.link_local([], self.copyright)

    def test_loose_names_get_unticked_guesses(self):
        near = self.store.create_entity("Vepley_(girls'_frontline)")
        dupe = self.store.create_entity('Alice (Summer)')
        for ident in (near['id'], dupe['id']):
            self.store.change(['a'], 'confirmed', ident)
        results = {'Vepley': [{'tag': "vepley_(summer)_(girls'_frontline_2)", 'posts': 90}, {'tag': "vepley_(girls'_frontline_2)", 'posts': 40},
                              {'tag': 'vepley_(other_game)', 'posts': 500}],
                   'Alice': [{'tag': 'alice', 'posts': 10}]}
        found = {x['id']: x for x in self.store.local_link_suggestions(lambda tag: None, None, lambda q: results.get(q, []))}
        # The qualifier narrows to the right game; the character itself beats its outfit.
        self.assertEqual((found[near['id']]['kind'], found[near['id']]['tag'], found[near['id']]['guess']), ('link', "vepley_(girls'_frontline_2)", True))
        # A loose match already in the library is a possible duplicate.
        self.assertEqual((found[dupe['id']]['kind'], found[dupe['id']]['target']['id']), ('merge', self.alice))

    def test_work_from_danbooru_copyright_reuses_or_creates(self):
        made = self.store.edit_work('from_tag', tag='blue_archive')
        self.assertEqual(self.store.edit_work('from_tag', tag='blue_archive')['id'], made['id'])
        self.assertEqual(made['tag'], 'blue_archive')


if __name__ == '__main__':
    unittest.main()
