"""分组推荐：同一张图直接认定，原创角色强推荐，聚类和保存段要两张一致才推荐，画师不一致不推荐；每次重算。不联网。"""
import collections
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from provenance import db
from provenance.sources import groups
from web import library


class GroupsTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.lib = library.create(Path(self.temp.name).resolve() / 'A.pilib')
        with db.connect(self.lib) as c:
            self.A = db.artist_for(c, [('danbooru', 'a', '')], 'test')
            self.B = db.artist_for(c, [('danbooru', 'b', '')], 'test')
            for asset, artist in (('d1', self.A), ('o1', self.A), ('c1', self.A), ('c2', self.A), ('k1', self.A), ('k2', self.B), ('r1', self.B)):
                c.execute("INSERT INTO image_artists(asset,artist_id,method,score,evidence,status) VALUES (?,?,'iqdb',1,'{}','auto')", (asset, artist))

    def run_with(self, found):
        full = {kind: collections.defaultdict(list) for kind in groups.RULES}
        for kind, members in found.items():
            for key, pics in members.items():
                full[kind][key] = pics
        with mock.patch.object(groups, 'library_groups', return_value=full):
            return groups.run(self.lib, log=lambda m: None)

    def rows(self):
        with db.connect(self.lib) as c:
            return {(r[0], r[1], r[2], r[3]) for r in c.execute("SELECT asset, artist_id, method, status FROM image_artists WHERE method <> 'iqdb'")}

    def test_rules_by_kind(self):
        result = self.run_with({
            'duplicate': {'g': ['d1', 'd2']},                  # the same drawing: attributed
            'oc': {'e': ['o1', 'o2', 'o3']},                   # one picture is enough for an original character
            'cluster': {'one': ['c1', 'c3'], 'two': ['c1', 'c2', 'c4'], 'split': ['k1', 'k2', 'k3']},
            'run': {'r': ['r1', 'r2']},                        # a save run needs two agreeing pictures
        })
        self.assertEqual(self.rows(), {('d2', self.A, 'same_picture', 'auto'), ('o2', self.A, 'group_oc', 'candidate'),
                                       ('o3', self.A, 'group_oc', 'candidate'), ('c4', self.A, 'group_cluster', 'candidate')})
        self.assertEqual((result['auto'], result['candidates']), (1, 3))
        with db.connect(self.lib) as c:
            proof = json.loads(c.execute("SELECT evidence FROM image_artists WHERE asset='o2'").fetchone()[0])
        self.assertEqual((proof['group'], proof['count']), ('oc', 1))

    def test_recomputed_every_run(self):
        self.run_with({'oc': {'e': ['o1', 'o2']}})
        self.run_with({})                                      # the character was split up since: its candidate goes
        self.assertEqual(self.rows(), set())

    def test_originals_without_any_artist_go_first(self):
        full = {kind: collections.defaultdict(list) for kind in groups.RULES}
        full['oc'].update({'known': ['o1', 'x1'], 'open': ['n1', 'n2', 'n3', 'n4']})
        with mock.patch.object(groups, 'library_groups', return_value=full), db.connect(self.lib) as c:
            self.assertEqual(groups.unsolved_originals(self.lib, c), ['n1', 'n2', 'n3'])


if __name__ == '__main__':
    unittest.main()
