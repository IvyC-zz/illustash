"""C3 启动切换中的临时只读：读照常，所有公开写入口拒绝，四项完成后自动恢复。"""
import contextlib
import io
import json
import sqlite3
import tempfile
import threading
import unittest
import urllib.error
import urllib.request
from pathlib import Path

from tests import terms_seed
from web import learn, names, terms_core
from web.server import Server, adopt_characters, adopt_labels, adopt_names
from web.store import Invalid

MESSAGE = '图库正在切换到词条核心，完成前只读'


class Switching(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.store, _ = terms_seed.load('refusals_none', self.root)

    def dump(self):
        with sqlite3.connect(self.store.path) as c:
            return '\n'.join(c.iterdump())

    def test_pending_refuses_every_public_write_without_side_effects(self):
        calls = [
            lambda: self.store.change(['a'], 'unknown'),
            lambda: self.store.answer('missing'),
            lambda: self.store.answer_audit(),
            lambda: self.store.reassign(['a'], 'a', 'b'),
            lambda: self.store.sweep('missing'),
            lambda: self.store.uncertain_identity(['a'], ['a', 'b']),
            lambda: self.store.decide_identity(a='a', b='b', kind='duplicate_of'),
            lambda: self.store.merge('a', 'b'),
            lambda: self.store.create_entity('新人物'),
            lambda: self.store.create_linked('new_tag'),
            lambda: self.store.set_profile('a', name='新名'),
            lambda: self.store.link_local([]),
            lambda: self.store.shared_oc(),
            lambda: self.store.set_kind('a', ''),
            lambda: self.store.set_mascot('a', True),
            lambda: self.store.set_work('a', None),
            lambda: self.store.tag(['a'], '标签'),
            lambda: self.store.judge_artist(['a'], 'artist', 'yes'),
            lambda: self.store.undo(1),
            lambda: self.store.edit_work('create', name='作品'),
            lambda: self.store.apply_work_evidence({}),
            lambda: self.store.refresh(),
            lambda: self.store.refresh_general_hits(),
            lambda: self.store.refresh_ratings(),
            lambda: self.store.reindex_characters(['a']),
            lambda: self.store.import_changes(removed=['a']),
            lambda: self.store.curate(['a'], 'exclude'),
            lambda: self.store.refresh_paths(),
            lambda: self.store.sync_begin(),
            lambda: self.store.make_audit(),
            lambda: self.store.refresh_similar(),
            lambda: self.store.skip_identity_pair('a', 'b'),
            lambda: self.store.set_pins([]),
            lambda: self.store.set_startup({'sync': False}),
            lambda: learn.learn(self.store, None),
        ]
        updater = names.NameUpdater(self.store, enabled=False)
        calls += [lambda: updater.replace('character', 'a', 'bangumi', []),
                  lambda: updater.aliases('character', 'a', fetch=True)]
        before = self.dump()
        self.assertEqual(self.store.write_refusal(), MESSAGE)
        self.assertTrue(self.store.entities())
        self.assertTrue(self.store.detail('a'))
        for call in calls:
            with self.subTest(入口=call.__code__.co_firstlineno), self.assertRaisesRegex(Invalid, '^' + MESSAGE + '$'):
                call()
            self.assertEqual(self.dump(), before)

    def test_partial_switch_keeps_read_only_and_names_completion_restores_writes(self):
        with contextlib.redirect_stdout(io.StringIO()):
            terms_core.run('migrate', '--db', self.store.path)
            terms_core.cutover_artists(self.store.path)
            self.assertTrue(adopt_labels(self.store) and adopt_characters(self.store))
            self.assertEqual(self.store.write_refusal(), MESSAGE)
            with self.assertRaisesRegex(Invalid, MESSAGE):
                self.store.create_entity('尚未完成')
            self.assertTrue(adopt_names(self.store))
        self.assertIsNone(self.store.write_refusal())
        made = self.store.create_entity('切换完成后人物')
        self.assertEqual(self.store.entities('切换完成后人物')[0]['id'], made['id'])
        self.assertTrue(terms_core.run('verify-characters', '--db', self.store.path)['committed'])
        self.assertTrue(terms_core.run('verify-names', '--db', self.store.path)['committed'])

    def test_http_reads_and_cache_work_and_write_status_recovers(self):
        server = Server(('127.0.0.1', 0), self.store, self.root / 'cache')
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(lambda: (server.shutdown(), thread.join(), server.server_close()))
        def call(path, body=None):
            headers = {'Content-Type': 'application/json', 'Origin': server.origin, 'X-CSRF-Token': server.token} if body is not None else {}
            request = urllib.request.Request(server.origin + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
            try:
                with urllib.request.urlopen(request, timeout=5) as r:
                    return r.status, json.loads(r.read())
            except urllib.error.HTTPError as exc:
                return exc.code, json.loads(exc.read())
        status, boot = call('/api/bootstrap')
        self.assertEqual((status, boot['read_only']), (200, MESSAGE))
        self.assertEqual(call('/api/entities', {'name': '临时人物'}), (409, {'error': MESSAGE}))
        self.assertEqual(call('/api/cache', {'action': 'clear'})[0], 200)
        terms_seed.switch(self.store)
        status, boot = call('/api/bootstrap')
        self.assertEqual((status, boot['read_only']), (200, None))
        self.assertEqual(call('/api/entities', {'name': '恢复后人物'})[0], 200)
