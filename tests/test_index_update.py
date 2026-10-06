"""定期检查新角色：检查结果、补角色后只重导认领变化的图、放弃角色；不联网，用假的 char_patch。"""
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from web.index_update import IndexUpdater
from web.store import Store
from tests.test_web import fixture
from tests.test_web_v2 import assert_memberships_current


class IndexUpdateTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        self.store = Store(self.source, self.root / 'app.sqlite')
        self.store.refresh()
        (self.root / 'char-index').mkdir()
        self.calls, self.changed = [], 0
        self.busy = False
        rows = [{'tag': 'carol_(example)', 'short_tag': 'carol_example', 'copyright': 'example', 'post_count': 300, 'overlap': 0.9},
                {'tag': 'dani_(example)', 'short_tag': 'dani_example', 'copyright': 'example', 'post_count': 120, 'overlap': 0.8}]

        def finder(works, min_posts, per_work):
            per_work('example', 2)
            per_work('no_such_work', None)
            return rows

        def runner(args):
            self.calls.append(args)
            with sqlite3.connect(self.source) as c:
                if args[0] == 'apply':
                    yield {'step': 'fetch', 'done': 0, 'total': 2}
                    (self.root / 'char-index' / 'carol_example.json').write_text(json.dumps({'short_tag': 'carol_example', 'source': 'danbooru-patch'}))
                    (self.root / 'char-patch' / 'rejected').mkdir(parents=True, exist_ok=True)
                    (self.root / 'char-patch' / 'rejected' / 'dani_example.json').write_text(json.dumps({'rejected': '参考图没有一致的主体'}))
                    yield {'step': 'scan'}
                    c.execute("INSERT INTO characters VALUES ('carol_example','Carol',NULL,NULL,'example',NULL)")
                    c.execute("DELETE FROM labels WHERE dim='char_ext' AND uuid='b'")
                    c.executemany("INSERT INTO labels VALUES (?,'char_ext','carol_example',?)", [('b', 0.97), ('c', 0.96)])
                else:
                    (self.root / 'char-index' / 'carol_example.json').unlink()
                    c.execute("DELETE FROM characters WHERE char_id='carol_example'")
                    c.execute("DELETE FROM labels WHERE value='carol_example'")
                    c.execute("INSERT INTO labels VALUES ('b','char_ext','alice',0.95)")
            yield {'step': 'done'}

        self.updater = IndexUpdater(self.store, runner=runner, finder=finder, on_change=self.on_change, busy=lambda: self.busy)

    def on_change(self):
        self.changed += 1

    def tearDown(self):
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def wait(self):
        self.updater.thread.join(10)
        return self.updater.info()

    def test_check_add_and_drop(self):
        self.assertTrue(self.updater.due())
        self.updater.maybe_check()
        info = self.wait()
        self.assertEqual(info['state']['status'], 'checked')
        self.assertEqual(info['check']['works'], {'example': 2, 'no_such_work': None})
        self.assertFalse(self.updater.due())
        with self.assertRaises(ValueError):
            self.updater.start_apply(['not_offered'])
        self.busy = True
        with self.assertRaises(ValueError):                            # a Photos sync is running
            self.updater.start_apply(['carol_example'])
        self.busy = False
        self.updater.start_apply(['carol_example', 'dani_example'])
        info = self.wait()
        self.assertEqual(self.calls[-1], ['apply', '--only', 'carol_example,dani_example'])
        self.assertEqual((info['state']['status'], info['state']['added'], info['state']['changed']), ('done', 1, 2))
        added, rejected = info['history'][0]['characters']
        self.assertEqual((added['status'], added['claims'], added['entity']['count']), ('added', 2, 2))
        self.assertEqual((rejected['status'], rejected['reason']), ('rejected', '参考图没有一致的主体'))
        self.assertEqual(info['check']['missing'], [])                  # applied rows leave the offer list
        self.assertEqual([r['short_tag'] for r in json.loads((self.root / 'char-patch' / 'plan.json').read_text())],
                         ['carol_example', 'dani_example'])
        carol = added['entity']['id']
        self.assertEqual(sorted(r['uuid'] for r in self.store.assets(entity=carol)['items']), ['b', 'c'])
        self.assertEqual(self.changed, 1)
        # Give it up again: its claims go, b is alice's again.
        with self.assertRaises(ValueError):
            self.updater.start_drop('alice', '不是补充角色')
        self.updater.start_drop('carol_example', '认领的图不对')
        info = self.wait()
        self.assertEqual(self.calls[-1][:2], ['drop', 'carol_example'])
        self.assertEqual(info['history'][0]['characters'][0]['status'], 'dropped')
        self.assertEqual(self.store.assets(entity=carol)['total'], 0)
        self.assertIn('b', [r['uuid'] for r in self.store.assets(entity=self.store.entities('Alice')[0]['id'])['items']])

    def test_watch_settings_are_validated(self):
        ident = self.store.edit_work('create', name='蔚蓝档案', tag='blue_archive')['id']
        self.assertEqual(self.updater.set_watch([ident, ident], 150)['works'], [ident])
        self.assertEqual(self.updater.watch()['version'], 2)
        self.assertEqual(self.updater.watch()['min_posts'], 150)
        for works, posts in ((['x'], 100), (['x'], 5), ('blue_archive', 100)):
            with self.assertRaises(ValueError):
                self.updater.set_watch(works, posts)


if __name__ == '__main__':
    unittest.main()
