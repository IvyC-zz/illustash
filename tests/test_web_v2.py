"""V2 归属、迁移和事务契约；全部写入仅在临时库中。"""
import hashlib
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch
from web.store import Invalid, MIXED, SCHEMA, Store
from tests.test_web import fixture
from tests import terms_seed


def assert_memberships_current(test, store):
    """The stored memberships must equal a fresh evaluation of the ownership rules."""
    cols = 'uuid,entity,status,origin,sources,score,source_count,ccip_only'
    with store.connect() as c:
        stored = c.execute(f'SELECT {cols} FROM (SELECT * FROM memberships UNION ALL SELECT * FROM tag_memberships) ORDER BY uuid,entity').fetchall()
        fresh = c.execute(f'SELECT {cols} FROM membership_rules ORDER BY uuid,entity').fetchall()
    test.assertEqual([tuple(r) for r in stored], [tuple(r) for r in fresh])
    with store.connect() as c:
        stale = c.execute(f'SELECT count(*) FROM duplicates AS d WHERE hidden<>(rep=0 AND NOT {MIXED})').fetchone()[0]
    test.assertEqual(stale, 0)
    with store.connect() as c:
        stored = c.execute('SELECT uuid FROM excluded ORDER BY 1').fetchall()
        fresh = c.execute("""SELECT a.uuid FROM assets a LEFT JOIN curation k ON k.uuid=a.uuid
            LEFT JOIN auto_rules r ON r.uuid=a.uuid AND r.kind='exclude'
            WHERE k.status='excluded' OR (r.uuid IS NOT NULL AND k.uuid IS NULL) ORDER BY 1""").fetchall()
    test.assertEqual([tuple(r) for r in stored], [tuple(r) for r in fresh])


class V2Test(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = fixture(self.root)
        with sqlite3.connect(self.source) as c:
            c.executescript("""
                ALTER TABLE characters ADD COLUMN base_tag TEXT;
                ALTER TABLE characters ADD COLUMN skin TEXT;
                UPDATE characters SET base_tag=char_id;
                INSERT INTO characters VALUES ('alice_summer','Alice summer',NULL,NULL,'Example','summer_tag','alice','swimsuit');
                INSERT INTO characters VALUES ('orphan_summer','Orphan summer',NULL,NULL,'Example','orphan_tag','orphan','swimsuit');
                INSERT INTO characters VALUES ('alice_other','Other Alice',NULL,NULL,'Other','other_tag','alice',NULL);
                ALTER TABLE characters ADD COLUMN danbooru_tag TEXT;
                UPDATE characters SET danbooru_tag='alice_(example)' WHERE char_id='alice';
                INSERT INTO labels VALUES ('a','rating','explicit',0.9),('a','rating','general',0.1),('b','rating','sensitive',0.8),('c','rating','questionable',0.6);
                INSERT INTO labels VALUES ('a','char_ext','bob',0.99),('b','char_ext','alice_summer',0.98),('c','char_ext','alice_summer',0.96);
            """)
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.store = Store(self.source, self.root/'app.sqlite')
        self.store.refresh()
        with self.store.connect() as c:
            self.entities = {'index:' + k: i for k, i in c.execute("SELECT key,entity_id FROM links WHERE system='ccip'")}
        self.alice, self.bob, self.variant = [self.entities['index:' + n] for n in ('alice', 'bob', 'alice_summer')]

    def tearDown(self):
        self.assertEqual(hashlib.sha256(self.source.read_bytes()).hexdigest(), self.digest)
        assert_memberships_current(self, self.store)
        self.temp.cleanup()

    def members(self, **kw):
        return [r['uuid'] for r in self.store.assets(**kw)['items']]

    def test_hierarchy_and_orphan_are_stable(self):
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?', (self.variant,)).fetchone()[0], self.alice)
            orphan = c.execute('SELECT parent_id FROM entities WHERE id=?', (self.entities['index:orphan_summer'],)).fetchone()[0]
            self.assertIsNotNone(orphan)
        self.store.refresh()
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?', (self.entities['index:orphan_summer'],)).fetchone()[0], orphan)
        self.assertEqual(self.members(entity=self.alice), ['a', 'b', 'c'])
        self.assertEqual(self.members(entity=self.alice, variant=self.variant), ['b', 'c'])
        chars = {r['id']: r for r in self.store.characters()}
        self.assertEqual(chars[self.alice]['count'], 3)
        self.assertEqual(chars[self.alice]['unchecked'], 2)
        self.assertEqual(chars[self.alice]['variants'][0]['count'], 2)

    def test_skin_without_a_qualified_parent_falls_back_to_the_plain_character(self):
        # dokibird_(vtuber)_(bounty_hunter) once hung under dokibird_(bounty_hunter), itself folded into a skin of dokibird.
        with sqlite3.connect(self.source) as c:
            c.execute("UPDATE characters SET danbooru_tag='bob' WHERE char_id='bob'")
            c.execute("""INSERT INTO characters(char_id,name_zh,copyright,base_tag,skin,danbooru_tag)
                VALUES ('bob_vtuber_retro','Bob retro','retro','bob','vtuber','bob_(vtuber)_(retro)')""")
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.store.refresh()
        with self.store.connect() as c:
            skin = c.execute("SELECT entity_id FROM links WHERE system='ccip' AND key='bob_vtuber_retro'").fetchone()[0]
            self.assertEqual(c.execute('SELECT parent_id FROM entities WHERE id=?', (skin,)).fetchone()[0], self.bob)

    def test_explicit_merge_preserves_best_raw_anchor_score_on_every_import(self):
        self.store.merge(self.alice, self.bob)
        with sqlite3.connect(self.source) as c:
            c.executemany("INSERT INTO labels VALUES ('c','char_ext',?,?)", [('alice', .99), ('bob', .91)])
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        for import_rows in (lambda: self.store.reindex_characters(['c']), self.store.refresh,
                            lambda: self.store.import_changes(['c'])):
            import_rows()
            with self.store.connect() as c:
                score = c.execute("SELECT score FROM candidates WHERE uuid='c' AND entity=? AND source='CCIP'", (self.bob,)).fetchone()[0]
            self.assertEqual(score, .99)

    def test_collision_is_derived_and_reversible(self):
        self.assertEqual(self.members(entity=self.bob), ['b'])
        with self.store.connect() as c:
            self.assertIsNone(c.execute('SELECT 1 FROM assertions WHERE uuid=? AND entity=?', ('a', self.bob)).fetchone())
        action = self.store.change(['a'], 'rejected', self.alice)
        self.assertEqual(self.members(entity=self.bob), ['a', 'b'])
        self.store.undo(action['id'])
        self.assertEqual(self.members(entity=self.bob), ['b'])
        # WD evidence exempts a machine identity from collision hiding.
        self.store.change(['b'], 'confirmed', self.alice)
        self.assertIn('b', self.members(entity=self.bob))
        self.store.change(['a'], 'confirmed', self.bob)
        self.assertIn('a', self.members(entity=self.bob))

    def test_parent_and_variant_rejections(self):
        action = self.store.change(['c'], 'remove_variant', self.variant)
        self.assertIn('c', self.members(entity=self.alice))
        self.assertNotIn('c', self.members(variant=self.variant))
        self.store.undo(action['id'])
        self.store.change(['c'], 'add_variant', self.variant)
        self.assertIn('c', self.members(entity=self.alice, status='confirmed'))
        action = self.store.change(['c'], 'rejected', self.alice)
        self.assertNotIn('c', self.members(entity=self.alice))
        self.assertNotIn('c', self.members(variant=self.variant))
        with self.assertRaises(Invalid): self.store.change(['c'], 'add_variant', self.variant)
        self.store.refresh()
        self.assertNotIn('c', self.members(entity=self.alice))
        self.store.undo(action['id'])
        self.assertIn('c', self.members(variant=self.variant))

    def test_sweep_snapshot_undo_and_refresh(self):
        self.store.change(['b'], 'unknown')
        action = self.store.sweep(self.alice)
        self.assertEqual(self.members(entity=self.alice, status='machine'), [])
        self.assertTrue(self.store.detail('b')['unknown'])
        self.assertEqual(next(r for r in self.store.detail('c')['assertions'] if r['id']==self.alice)['origin'], 'sweep')
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM reviews').fetchone()[0], 1)
        self.store.refresh()
        self.store.undo(action['id'])
        self.assertEqual(self.members(entity=self.alice, status='machine'), ['b', 'c'])
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM reviews').fetchone()[0], 0)

    def test_filter_exclude_and_atomic_reassign(self):
        action = self.store.change(None, 'rejected', self.alice, filters={'entity': self.alice}, exclude=['a','c'])
        self.assertEqual(self.members(entity=self.alice), ['a', 'c'])
        self.store.undo(action['id'])
        action = self.store.reassign(None, self.alice, self.bob, filters={'entity': self.alice, 'variant': self.variant}, exclude=['b'])
        self.assertEqual(self.members(entity=self.alice), ['a', 'b'])
        self.store.undo(action['id'])
        with self.assertRaises(Invalid):
            self.store.reassign(['b', 'c'], self.bob, self.alice)
        self.assertEqual(self.store.detail('b')['assertions'], [])
        self.assertEqual(self.members(entity=self.bob), ['b'])
        with self.assertRaises(Invalid): self.store.change(['a'], 'unknown', filters={})
        with self.assertRaises(Invalid): self.store.change(None, 'unknown', filters={'oops': 'ignored?'})
        self.assertEqual(self.store.ids({'entity':self.alice},1,1), ['b'])

    def test_large_batch_confirmation_and_undo(self):
        with self.store.connect() as c:
            c.executemany('INSERT INTO assets(uuid,filename) VALUES (?,?)', [(f'bulk{i}',f'bulk{i}') for i in range(501)])
        with self.assertRaises(Invalid): self.store.change(None, 'unknown', filters={'q': 'bulk'})
        self.assertEqual(self.members(status='unknown'), [])
        action = self.store.change(None, 'unknown', filters={'q':'bulk'}, confirm=True)
        self.assertEqual(self.store.assets(status='unknown')['total'],501)
        self.store.undo(action['id'])
        self.assertEqual(self.members(status='unknown'), [])

    def test_suspicious_order_and_evidence(self):
        self.assertEqual(self.members(entity=self.alice,order='suspicious'), ['c','b','a'])
        items = self.store.assets(entity=self.alice)['items']
        evidence = next(m['evidence'] for m in items[1]['memberships'] if m['id']==self.alice)
        self.assertEqual(evidence, {'sources':['CCIP','WD'], 'max_score':0.98})
        self.assertEqual(self.members(pool='unmatched'), [])
        self.store.change(['c'], 'rejected', self.alice)
        self.assertEqual(self.members(pool='unmatched'), ['c'])
        self.store.change(['c'], 'unknown')
        self.assertEqual(self.members(pool='unmatched'), [])

    def test_out_of_scope_assertions_remain_without_ghost_memberships(self):
        self.store.change(['c'], 'confirmed', self.alice)
        self.store.import_changes(removed=['c'])
        self.assertEqual(self.store.characters('alice',1)[0]['count'], 2)
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM assertions WHERE uuid='c'").fetchone()[0], 1)
            self.assertEqual(c.execute("SELECT count(*) FROM memberships WHERE uuid='c'").fetchone()[0], 0)

    def test_sort_key_single_source_ccip_score_other_identity(self):
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.executemany('INSERT INTO assets(uuid,filename) VALUES (?,?)', [(i, i) for i in ('ccip','low','high','multi','tie1','tie2')])
                c.executemany('INSERT INTO candidates VALUES (?,?,?,?)', [
                    ('ccip',self.alice,'CCIP',0.99), ('low',self.alice,'WD',0.4),
                    ('high',self.alice,'WD',0.9), ('multi',self.alice,'WD',0.35),
                    ('multi',self.alice,'CCIP',0.99), ('tie1',self.alice,'WD',0.8),
                    ('tie2',self.alice,'WD',0.8)])
                old._refresh_evidence(c)
                old._refresh_memberships(c)
        self.store = old
        self.store.change(['tie2'], 'confirmed', self.bob)
        ids = self.members(entity=self.alice, order='suspicious')
        synthetic = [i for i in ids if i in ('ccip','low','high','multi','tie1','tie2')]
        self.assertEqual(synthetic, ['ccip','low','tie2','tie1','high','multi'])
        self.assertEqual(ids[-1], 'a')

    def test_album_variant_import_confirms_parent_only(self):
        # The migration projection must not reinterpret old album identity as clothing.
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute('DELETE FROM assertions WHERE uuid=?', ('a',))
                c.execute("INSERT INTO assertions VALUES (?,?,'confirmed','album')", ('a',self.variant))
                old._refresh_memberships(c, ['a'])
        self.store = old
        membership = next(m for m in self.store.detail('a')['memberships'] if m['id']==self.alice)
        self.assertEqual(membership['origin'], 'album')
        self.assertEqual(membership['variants'], [])

    def test_migration_preserves_legacy_history_and_backs_up(self):
        migration = self.root / '旧版本启动'
        migration.mkdir()
        legacy = migration / 'legacy.sqlite'
        with sqlite3.connect(legacy) as c:
            c.executescript(SCHEMA)
            c.execute("INSERT INTO entities VALUES ('alice','Alice','Example','index:alice')")
            c.execute("INSERT INTO entities VALUES ('summer','Alice summer','Example','index:alice_summer')")
            c.execute("INSERT INTO assets(uuid) VALUES ('a')")
            c.execute("INSERT INTO assertions VALUES ('a','summer','confirmed','album')")
            c.execute("INSERT INTO deferred VALUES ('b')")
            before = [{'uuid':'b','entity':None,'assertion':None,'deferred':False}]
            c.execute('INSERT INTO actions(label,before_json) VALUES (?,?)', ('旧操作',json.dumps(before)))
        store = Store(self.source, legacy, build_general=False, legacy=True)
        backups = list((legacy.parent/'library-backups').glob('*.sqlite'))
        self.assertEqual(len(backups),1)
        with sqlite3.connect(backups[0]) as old, store.connect() as new:
            self.assertEqual(old.execute('PRAGMA quick_check').fetchone()[0], 'ok')
            for table in ('assertions','deferred','actions'):
                self.assertEqual(old.execute(f'SELECT * FROM {table}').fetchall(), [tuple(r) for r in new.execute(f'SELECT * FROM {table}')])
            self.assertIn(('a','alice'), [tuple(r) for r in new.execute('SELECT * FROM family_confirmed')])
        Store(self.source, legacy, build_general=False, legacy=True)
        self.assertEqual(len(list((legacy.parent/'library-backups').glob('*.sqlite'))), 1)
        terms_seed.switch(store)
        store.undo(1)
        with store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM deferred').fetchone()[0],0)

    def make_single(self, *ids):
        # 显式源夹具准备：题材事实须能在真实启动切换中重新交付。
        with sqlite3.connect(self.source) as c:
            c.executemany("INSERT INTO labels VALUES (?,'general','solo',1)", [(i,) for i in ids])
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.executemany("INSERT OR IGNORE INTO general_hits VALUES (?,'solo')", [(i,) for i in ids])
                old._refresh_single(c)
                old._refresh_memberships(c)
        self.store = old

    def test_single_person_keeps_top_machine_identity(self):
        # b: alice has WD+CCIP (via variant), bob only WD -> bob is only an alternative.
        self.assertEqual(self.members(entity=self.bob), ['b'])
        self.make_single('b')
        self.assertEqual(self.members(entity=self.bob), [])
        detail = self.store.detail('b')
        self.assertTrue(detail['single'])
        self.assertEqual([m['id'] for m in detail['memberships']], [self.alice])
        self.assertEqual([a['id'] for a in detail['alternatives']], [self.bob])
        # A multi-person tag cancels the rule.
        with sqlite3.connect(self.source) as c:
            c.execute("INSERT INTO labels VALUES ('b','general','2girls',1)")
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("INSERT INTO general_hits VALUES ('b','2girls')")
                old._refresh_single(c)
                old._refresh_memberships(c)
        self.store = old
        self.assertEqual(self.members(entity=self.bob), ['b'])

    def test_low_score_ccip_only_claim_is_a_lead_not_an_owner(self):
        # 随机抽查 round 1: only-CCIP claims under 0.925 were wrong 9 times in 10.
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute("INSERT INTO candidates VALUES ('c',?,'CCIP',0.91)", (self.bob,))
                c.execute("INSERT INTO candidates VALUES ('b',?,'CCIP',0.93)", (self.entities['index:orphan_summer'],))
                old._refresh_evidence(c)
                old._refresh_memberships(c)
        self.store = old
        self.assertNotIn('c', self.members(entity=self.bob))
        self.assertIn(self.bob, [a['id'] for a in self.store.detail('c')['alternatives']])   # kept as「也可能是」
        orphan = self.entities['index:orphan_summer']
        with self.store.connect() as c:
            owner = c.execute('SELECT coalesce(parent_id,id) FROM entities WHERE id=?', (orphan,)).fetchone()[0]
        self.assertIn('b', self.members(entity=owner))   # 0.93 still counts

    def test_single_person_human_identity_wins_and_multi_is_explicit(self):
        self.make_single('b')
        action = self.store.change(['b'], 'confirmed', self.bob)
        self.assertEqual([m['id'] for m in self.store.detail('b')['memberships']], [self.bob])
        self.assertEqual([a['id'] for a in self.store.detail('b')['alternatives']], [self.alice])
        self.store.change(['b'], 'confirmed', self.alice)
        self.assertEqual({m['id'] for m in self.store.detail('b')['memberships']}, {self.alice, self.bob})
        self.store.undo(self.store.overview()['undo']['id'])
        self.store.undo(action['id'])
        self.assertEqual([m['id'] for m in self.store.detail('b')['memberships']], [self.alice])

    def test_single_person_removal_does_not_promote_next(self):
        self.make_single('b')
        self.store.change(['b'], 'rejected', self.alice)
        self.assertEqual(self.store.detail('b')['memberships'], [])
        self.assertEqual([a['id'] for a in self.store.detail('b')['alternatives']], [self.bob])
        self.assertIn('b', self.members(pool='unmatched'))
        self.store.change(['b'], 'confirmed', self.bob)
        self.assertEqual([m['id'] for m in self.store.detail('b')['memberships']], [self.bob])

    def test_rank_prefers_agreement_then_strength_then_wd(self):
        def prepare(ccip, wd, bob_wd=None):
            with terms_seed.staged(self.store) as old:
                with old.connect() as c:
                    c.execute("INSERT OR IGNORE INTO assets(uuid,filename) VALUES ('r','r')")
                    c.execute("DELETE FROM candidates WHERE uuid='r'")
                    c.executemany('INSERT INTO candidates VALUES (?,?,?,?)',
                                  [('r', self.bob, 'CCIP', ccip), ('r', self.alice, 'WD', wd)])
                    if bob_wd is not None:
                        c.execute("INSERT INTO candidates VALUES ('r',?,'WD',?)", (self.bob, bob_wd))
                    old._refresh_evidence(c)
                    old._refresh_memberships(c)
            self.store = old
        def top():
            with self.store.connect() as c:
                return c.execute("SELECT entity FROM family_evidence WHERE uuid='r' AND rank=1").fetchone()[0]
        prepare(0.93, 0.4)
        self.assertEqual(top(), self.alice)
        prepare(0.99, 0.4)
        self.assertEqual(top(), self.bob)
        prepare(0.99, 0.95)
        self.assertEqual(top(), self.alice)
        prepare(0.99, 0.4, 0.5)
        self.assertEqual(top(), self.bob)

    def test_display_uses_readable_name_and_aliases_still_search(self):
        names = {r['id']: r['name'] for r in self.store.characters()}
        self.assertEqual(names[self.alice], 'Alice')
        self.assertEqual(names[self.bob], 'Bob')
        self.assertEqual(self.store.entities('爱丽丝')[0]['name'], 'Alice')
        self.assertEqual(self.store.entities('爱丽丝')[0]['matched'], '爱丽丝')
        self.assertEqual(self.store.entities('alice_(ex')[0]['id'], self.alice)
        variant = next(v for v in self.store.characters() if v['id'] == self.alice)['variants'][0]
        self.assertEqual(variant['name'], '泳装')
        self.assertEqual(self.store.create_entity('Alice_(Example)')['id'], self.alice)
        local = self.store.create_entity('我的新角色')
        self.assertEqual(self.store.entities('我的新角色')[0]['name'], '我的新角色')
        self.assertIn('我的新角色', self.store.change(['c'], 'confirmed', local['id'])['label'])

    def test_artist_oc_keeps_its_artist_and_is_found_by_it(self):
        oc = self.store.create_entity('Mina', 'oc', ' someartist ')
        self.assertEqual((oc['kind'], oc['artist']), ('oc', 'someartist'))
        self.store.change(['c'], 'confirmed', oc['id'])
        row = next(r for r in self.store.characters() if r['id'] == oc['id'])
        self.assertEqual((row['kind'], row['artist'], row['count']), ('oc', 'someartist', 1))
        self.assertEqual([e['id'] for e in self.store.entities('someart')], [oc['id']])
        self.assertEqual([r['id'] for r in self.store.characters('someart')], [oc['id']])
        self.assertEqual(self.store.artists(), [{'artist': 'someartist', 'characters': 1}])
        # An existing character can be marked, edited and unmarked; the artist only exists on OCs.
        self.store.set_kind(self.bob, 'oc', 'other')
        self.assertEqual(self.store.entities('other')[0]['id'], self.bob)
        self.store.set_kind(oc['id'], '', '')
        self.assertEqual(self.store.artists(), [{'artist': 'other', 'characters': 1}])
        for bad in (('Nope', 'oc', 'x' * 121), ('Nope', 'npc', ''), ('Nope', '', 'artist')):
            with self.assertRaises(Invalid):
                self.store.create_entity(*bad)
        variant = next(v for v in self.store.characters() if v['id'] == self.alice)['variants'][0]
        with self.assertRaises(Invalid):
            self.store.set_kind(variant['id'], 'oc', '')
        # One click, no name: 原创角色 N, numbered past the names already taken.
        first, second = self.store.create_entity(None, 'oc'), self.store.create_entity('', 'oc')
        self.assertEqual((first['name'], second['name'], first['kind']), ('原创角色 1', '原创角色 2', 'oc'))
        with self.assertRaises(Invalid):
            self.store.create_entity('', '')

    def test_skip_pool_and_resume(self):
        self.store.change(['c'], 'rejected', self.alice)
        self.assertEqual(self.members(pool='unmatched'), ['c'])
        action = self.store.change(['c'], 'unknown')
        self.assertEqual(action['label'], '已跳过 1 张')
        self.assertEqual(self.members(pool='unmatched'), [])
        self.assertEqual(self.members(pool='skipped'), ['c'])
        self.assertEqual({p['kind']: p['count'] for p in self.store.review_pools()}, {'unmatched': 0, 'skipped': 1, 'duplicates': 0, 'excluded': 0, 'pending': 0, 'recent': 0, 'audit': 0})
        self.store.change(['c'], 'resume')
        self.assertEqual(self.members(pool='unmatched'), ['c'])

    def test_duplicates_group_filter_and_mixed(self):
        with sqlite3.connect(self.source) as c:
            c.execute('ALTER TABLE assets ADD COLUMN stable_hash TEXT')
            c.execute("UPDATE assets SET stable_hash='h1' WHERE uuid IN ('a','b')")
            c.execute("UPDATE assets SET stable_hash='h2' WHERE uuid IN ('c','trash')")
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        self.store.refresh()
        # The trashed copy is out of scope, so c has no duplicate left.
        dup = next(p for p in self.store.review_pools() if p['kind'] == 'duplicates')
        self.assertEqual((dup['count'], dup['groups']), (2, 1))
        self.assertEqual(self.members(pool='duplicates'), ['a', 'b'])
        items = {i['uuid']: i['dup'] for i in self.store.assets()['items']}
        self.assertEqual(items['a'], {'grp': 'h1', 'size': 2, 'pos': 1})
        self.assertIsNone(items['c'])
        self.assertEqual([d['uuid'] for d in self.store.detail('a')['duplicates']], ['b'])
        self.assertEqual(self.store.detail('c')['duplicates'], [])
        # A write on one copy applies to the whole group, counted once.
        action = self.store.change(['b'], 'rejected', self.bob)
        self.assertEqual(action['label'], '已将 1 张移出 Bob')
        self.assertEqual(self.members(pool='duplicates', dup='mixed'), [])
        # Consistent group: folded to its representative (same size, so the earliest copy: a).
        self.assertEqual(self.members(), ['a', 'c'])
        self.assertEqual(self.members(entity=self.alice), ['a', 'c'])
        self.assertEqual(self.store.overview()['total'], 2)
        self.assertEqual(next(r for r in self.store.characters() if r['id'] == self.alice)['count'], 2)
        self.assertEqual(self.store.ids({'entity': self.alice}), ['a', 'c'])
        self.store.undo(action['id'])
        # Legacy divergence (written before copies were linked) stays unfolded until resolved.
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.execute('DELETE FROM duplicates')
        self.store = old
        self.store.change(['b'], 'rejected', self.bob)
        self.store.change(['a'], 'confirmed', self.bob)
        self.store.refresh()
        self.assertEqual(self.members(pool='duplicates', dup='mixed'), ['a', 'b'])
        self.assertEqual(self.store.ids({'pool': 'duplicates', 'dup': 'mixed'}), ['a', 'b'])
        self.assertEqual(next(p for p in self.store.review_pools() if p['kind'] == 'duplicates')['mixed'], 1)
        self.assertEqual(self.members(), ['a', 'b', 'c'])
        action = self.store.change(['b'], 'confirmed', self.bob)
        self.assertEqual(self.members(pool='duplicates', dup='mixed'), [])
        self.assertEqual(self.members(), ['a', 'c'])
        self.store.undo(action['id'])
        self.assertEqual(self.members(), ['a', 'b', 'c'])
        # Tags follow the artwork too.
        self.store.tag(['a'], '收藏')
        self.assertEqual(self.store.detail('b')['user_tags'], ['收藏'])
        with self.assertRaises(Invalid): self.store.assets(pool='duplicates', dup='maybe')

    def test_maybe_lists_machine_evidence_counted_elsewhere(self):
        # a: bob's external-index match is hidden behind the album's alice.
        self.assertEqual(self.members(entity=self.bob, status='maybe'), ['a'])
        self.assertEqual(self.store.ids({'entity': self.bob, 'status': 'maybe'}), ['a'])
        action = self.store.change(None, 'confirmed', self.bob, filters={'entity': self.bob, 'status': 'maybe'})
        self.assertEqual(self.members(entity=self.bob, status='maybe'), [])
        self.assertIn('a', self.members(entity=self.bob))
        self.store.undo(action['id'])
        self.store.change(['a'], 'rejected', self.bob)
        self.assertEqual(self.members(entity=self.bob, status='maybe'), [])
        with self.assertRaises(Invalid): self.store.assets(status='maybe')

    def test_hinted_order_puts_unmatched_leads_first(self):
        high, low = 'lead-high', 'lead-low'
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                for ident in ('plain-new', high, low):
                    c.execute("INSERT INTO assets(uuid,filename,src_path,width,height,added_at) VALUES (?,?,'',10,10,'2026-09-26')", (ident, ident))
                c.execute("INSERT INTO weak_claims VALUES (?,?,'test',0.9)", (high, self.alice))
                c.execute("INSERT INTO weak_claims VALUES (?,?,'test',0.4)", (low, self.bob))
                c.execute("INSERT INTO weak_claims VALUES (?,?,'test',0.3)", (low, self.alice))
        self.store = old
        pool = self.members(pool='unmatched')
        items = self.store.assets(pool='unmatched', order='hinted')['items']
        self.assertEqual([r['uuid'] for r in items][:2], [high, low])
        self.assertEqual(sorted(r['uuid'] for r in items), sorted(pool))
        self.assertEqual(items[1]['hint']['id'], self.bob)
        self.assertTrue(items[1]['hint']['name'])
        self.assertTrue(all(r['hint'] is None for r in items[2:]))
        # A rejected lead neither sorts nor shows; the next-best lead takes its place.
        self.store.change([low], 'rejected', self.bob)
        items = self.store.assets(pool='unmatched', order='hinted')['items']
        self.assertEqual(next(r for r in items if r['uuid'] == low)['hint']['id'], self.alice)
        # One-click add confirms the lead and the image leaves the pool.
        self.store.change([high], 'confirmed', self.alice)
        self.assertNotIn(high, self.members(pool='unmatched', order='hinted'))
        with self.assertRaises(Invalid): self.store.assets(order='random')

    def test_change_sets_several_characters_as_one_undo(self):
        with self.store.connect() as c:
            c.execute("INSERT INTO assets(uuid,filename,src_path,width,height,added_at) VALUES ('pair','pair','',10,10,'2026-09-26')")
        self.store.change(['pair'], 'unknown')
        action = self.store.change(['pair'], 'confirmed', entities=[self.alice, self.bob, self.alice])
        self.assertIn('、', action['label'])
        self.assertEqual({m['id'] for m in self.store.detail('pair')['memberships']}, {self.alice, self.bob})
        self.assertNotIn('pair', self.members(pool='skipped'))
        self.store.undo(action['id'])
        self.assertEqual(self.store.detail('pair')['memberships'], [])
        self.assertIn('pair', self.members(pool='skipped'))
        for bad in ([], [self.alice] * 21, 'x', [self.alice, 'missing']):
            with self.assertRaises(Invalid): self.store.change(['pair'], 'confirmed', entities=bad)
        with self.assertRaises(Invalid): self.store.change(['pair'], 'rejected', entities=[self.alice])
        with self.assertRaises(Invalid): self.store.change(['pair'], 'confirmed', self.alice, entities=[self.bob])
        self.assertEqual(self.store.detail('pair')['memberships'], [])

    def test_merge_moves_human_knowledge_and_survives_refresh(self):
        local = self.store.create_entity('Alicia')['id']
        self.store.change(['c'], 'confirmed', local)
        self.store.change(['b'], 'rejected', local)
        self.store.set_pins([local])
        before = {r['id']: r['count'] for r in self.store.characters()}
        action = self.store.merge(local, self.alice)
        self.assertEqual(action['label'], '已将 Alicia 合并到 Alice（2 条人工结论）')
        # c keeps alice (now confirmed); b's removal from Alicia became a removal from alice.
        self.assertIn('c', self.members(entity=self.alice, status='confirmed'))
        self.assertNotIn('b', self.members(entity=self.alice))
        self.assertNotIn(local, {r['id'] for r in self.store.characters()})
        self.assertEqual(self.store.prefs()['pins'], [self.alice])
        self.assertEqual(self.store.entities('Alicia')[0]['id'], self.alice)          # the old name still finds her
        self.assertIn('c', self.members(entity=local))                                  # stale ids resolve
        self.store.change(['a'], 'confirmed', local)                                    # writes land on the target
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT count(*) FROM assertions WHERE entity=?', (local,)).fetchone()[0], 0)
        self.store.undo(self.store.overview()['undo']['id'])
        self.store.refresh()
        self.assertNotIn('b', self.members(entity=self.alice))
        self.store.undo(action['id'])
        self.assertEqual({r['id']: r['count'] for r in self.store.characters()}, before)
        self.assertEqual(self.store.prefs()['pins'], [local])
        self.assertIn('b', self.members(entity=self.alice))
        with self.assertRaises(Invalid): self.store.merge(self.alice, self.alice)
        with self.assertRaises(Invalid): self.store.merge(self.variant, self.bob)

    def test_collection_rules_and_human_override(self):
        extra = ('shot', 'text', 'pend', 'photo')
        topics = [('shot', 'chat_log'), ('text', 'text_focus'), ('pend', 'chat_log'), ('pend', '1girl'), ('c', 'chat_log')]
        with sqlite3.connect(self.source) as c:
            c.executemany("INSERT INTO assets VALUES (?,?,'',1,1,'','orig',1,0)", [(i,i) for i in extra])
            c.executemany("INSERT INTO labels VALUES (?,'general',?,1)", topics + [('photo','realistic'),('a','realistic')])
        self.digest = hashlib.sha256(self.source.read_bytes()).hexdigest()
        with terms_seed.staged(self.store) as old:
            with old.connect() as c:
                c.executemany('INSERT INTO assets(uuid,filename) VALUES (?,?)', [(i, i) for i in extra])
                c.executemany('INSERT INTO general_hits VALUES (?,?)', [('shot', 'chat_log'), ('text', 'text_focus'),
                              ('pend', 'chat_log'), ('pend', '1girl'), ('c', 'chat_log')])
                old._refresh_memberships(c, list(extra))
                # a is realistic: excluded even though a character was recognised on it.
                old._refresh_rules(c, ['photo', 'a'])
                old._refresh_excluded(c)
        self.store = old
        pools = {p['kind']: p for p in self.store.review_pools()}
        # c has a screenshot tag but also a character, so it stays.
        self.assertEqual(pools['excluded']['reasons'], {'photo': 2, 'screenshot': 1, 'text': 1})
        self.assertEqual(pools['pending']['count'], 1)
        self.assertEqual(self.members(pool='pending'), ['pend'])
        self.assertEqual(sorted(self.members()), ['b', 'c', 'pend'])
        self.assertEqual(self.store.overview()['total'], 3)
        self.assertNotIn('a', self.members(entity=self.alice))
        self.assertEqual(self.members(pool='excluded', reason='photo'), ['a', 'photo'])
        self.assertEqual(self.store.detail('a')['excluded'], {'reason': 'photo', 'origin': 'rule'})
        # A human decision wins over the rule and is undoable.
        action = self.store.curate(['a'], 'include')
        self.assertEqual(action['label'], '已收录 1 张')
        self.assertIn('a', self.members(entity=self.alice))
        self.assertEqual(self.store.detail('a')['curation'], 'included')
        self.store.undo(action['id'])
        self.assertNotIn('a', self.members(entity=self.alice))
        self.store.curate(['b'], 'exclude')
        self.assertEqual(self.members(pool='excluded', reason='manual'), ['b'])
        self.assertEqual(self.store.detail('b')['excluded'], {'reason': 'manual', 'origin': 'review'})
        self.store.curate(['pend'], 'include')
        self.assertEqual(self.members(pool='pending'), [])
        self.assertIn('pend', self.members())
        self.store.curate(['pend'], 'reset')
        self.assertEqual(self.members(pool='pending'), ['pend'])
        # Rules are rebuilt from WD data; human decisions survive a refresh.
        self.store.refresh()
        self.assertEqual(self.members(pool='excluded', reason='manual'), ['b'])
        with self.assertRaises(Invalid): self.store.assets(pool='unmatched', reason='photo')
        with self.assertRaises(Invalid): self.store.curate(['b'], 'delete')

    def test_user_tags_are_separate_undoable_and_filterable(self):
        action = self.store.tag(['a', 'b'], '收藏')
        self.assertEqual(self.store.user_tags(), [{'tag': '收藏', 'count': 2}])
        self.assertEqual(self.members(utag='收藏'), ['a', 'b'])
        self.assertEqual(self.store.detail('a')['user_tags'], ['收藏'])
        self.assertEqual(self.store.assets(utag='收藏')['items'][0]['user_tags'], ['收藏'])
        removal = self.store.tag(None, '收藏', add=False, filters={'utag': '收藏'}, exclude=['a'])
        self.assertEqual(self.members(utag='收藏'), ['a'])
        self.store.undo(removal['id'])
        self.assertEqual(self.members(utag='收藏'), ['a', 'b'])
        self.store.refresh()
        self.assertEqual(self.members(utag='收藏'), ['a', 'b'])
        self.store.undo(action['id'])
        self.assertEqual(self.store.user_tags(), [])
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT count(*) FROM general_hits WHERE value='收藏'").fetchone()[0], 0)
        with self.assertRaises(Invalid): self.store.tag(['a'], 'x' * 61)

    def test_nsfw_filter_uses_top_rating(self):
        self.assertEqual(self.members(nsfw='hide'), ['b'])
        self.assertEqual(self.members(nsfw='only'), ['a', 'c'])
        self.assertEqual(self.store.ids({'entity': self.alice, 'nsfw': 'hide'}), ['b'])
        cover = next(c for c in self.store.characters(nsfw='hide') if c['id'] == self.alice)['cover']
        self.assertEqual(cover, 'b')
        with self.assertRaises(Invalid): self.store.assets(nsfw='maybe')
        action = self.store.change(None, 'unknown', filters={'nsfw': 'only'})
        self.assertEqual(self.members(status='unknown'), ['a', 'c'])
        self.store.undo(action['id'])

    def test_failed_migration_rolls_back_schema(self):
        legacy = self.root/'legacy.sqlite'
        with sqlite3.connect(legacy) as c: c.executescript(SCHEMA)
        with patch.object(Store, '_hierarchy', side_effect=RuntimeError('模拟迁移失败')):
            with self.assertRaises(RuntimeError): Store(self.source, legacy, build_general=False, legacy=True)
        with sqlite3.connect(legacy) as c:
            self.assertNotIn('parent_id', {r[1] for r in c.execute('PRAGMA table_info(entities)')})
            self.assertEqual(c.execute("SELECT count(*) FROM sqlite_master WHERE name='reviews'").fetchone()[0],0)
        Store(self.source, legacy, build_general=False, legacy=True)
