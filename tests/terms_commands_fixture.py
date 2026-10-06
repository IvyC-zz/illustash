"""C3 核心命令夹具：固定旧库输入经真实启动切换，公开入口与原生命令分别执行。

每一步先对照删除旧路径前录下的固定答案，再保持公开入口与原生命令的逐行逐列精确比较（含时间）；
连续撤销、全量重建与投影核对全部保留。两份库均为完整核心形态，没有旧 Python 写入分支。
"""
import copy
import json
import sqlite3
import tempfile
import time
import unittest
from pathlib import Path

from tests import terms_seed
from tests.terms_c3_snapshots import Checkpoints
from tests.test_terms_core import core
from tests.test_terms_character_learning import rebuilt_snapshot, snapshot
from tests.test_terms_characters_contract import COMPAT, PROJECTIONS, refreshed_projections
from web import names, terms_core
from web.store import Store

PRIVATE = ('identity_canonical', 'identity_scopes', 'identity_suspended', 'identity_uncertain',
           'identity_anchor_decisions', 'identity_anchor_history', 'learn_state', 'asked', 'reviews', 'deferred',
           'backend_effects', 'names', 'prefs', 'audit_sample', 'duplicates', 'assets', 'auto_rules', 'excluded',
           'imported', 'sync_added', 'curation')


class CoreCommands(unittest.TestCase):
    policy = None
    labels = False   # 先把标签切到核心（角色切换的前提；同步、刷新走核心时 general_hits 是视图）

    @classmethod
    def setUpClass(cls):
        if core() is None:
            raise unittest.SkipTest('no terms core binary and no cargo')

    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        name = terms_seed.key(self)
        self.api, attrs = terms_seed.load(name, self.root, db=self.root / 'api.sqlite')
        for key, value in attrs.items():
            setattr(self, key, value)
        terms_seed.switch(self.api)
        terms_seed.resume_ids(self, name)
        self.checkpoints = Checkpoints(self)
        self.new = copy.copy(self.api)
        self.new.path = self.root / 'core.sqlite'
        with sqlite3.connect(self.api.path) as src, sqlite3.connect(self.new.path) as dst:
            src.backup(dst)
        self.same()

    @staticmethod
    def aligned():
        """两边的 CURRENT_TIMESTAMP 要落在同一秒。"""
        fraction = time.time() % 1
        if fraction > .5:
            time.sleep(1 - fraction + .01)

    def last(self, store):
        with store.connect() as c:
            row = c.execute('SELECT id,label FROM actions ORDER BY id DESC LIMIT 1').fetchone()
            return (row[0], row[1]) if row else (None, None)

    def both(self, old_call, op, *, expect_action=True, effect='verdict', rebuild=True, late=None, **args):
        """公开核心入口调用 old_call()，原生命令执行同一操作；固定旧答案核对历史文案。late() 提供公开入口执行后才能确定的参数。"""
        self.aligned()
        before, _ = self.last(self.api)
        result = old_call()
        if late:
            args.update(late())
        step, label = self.last(self.api)
        if expect_action:
            self.assertNotEqual(step, before, '公开入口应写一步历史')
            args['label'] = label
        command = {'op': op, **args, **({'effect': effect} if effect else {})}
        out = terms_core.character_command(self.new.path, command)
        if expect_action:
            self.assertEqual(out.get('action'), step)
        self.same(rebuild=rebuild)
        return result, out

    def refused(self, old_call, op, **args):
        """两边都拒绝，核心什么都不留。"""
        before = self.dump()
        with self.assertRaises(ValueError):
            old_call()
        with self.assertRaises(terms_core.Failed):
            terms_core.character_command(self.new.path, {'op': op, 'label': 'x', **args})
        self.assertEqual(self.dump(), before)
        self.same()

    def dump(self):
        with sqlite3.connect(self.new.path) as c:
            return '\n'.join(c.iterdump())

    def undo(self, rebuild=True):
        """两种核心调用形式各撤最近一步；外围步骤继续由 Python 撤。"""
        self.aligned()
        step, _ = self.last_live(self.api)
        self.api.undo(step)
        with self.new.connect() as c:
            before = json.loads(c.execute('SELECT before_json FROM actions WHERE id=?', (step,)).fetchone()[0])
        if terms_core.character_owns(before):
            terms_core.character_command(self.new.path, {'op': 'undo', 'action': step, 'effect': 'verdict'})
        else:
            self.new.undo(step)
        self.same(rebuild=rebuild)
        return step

    def undo_all(self, rebuild=True):
        while self.last_live(self.api)[0] is not None:
            self.undo(rebuild=rebuild)

    @staticmethod
    def last_live(store):
        with store.connect() as c:
            row = c.execute('SELECT id,label FROM actions WHERE undone=0 ORDER BY id DESC LIMIT 1').fetchone()
            return (row[0], row[1]) if row else (None, None)

    @staticmethod
    def actions(store):
        """历史：拆开信封后的快照，以及按当前名字重新渲染的文案（names.action_label）。"""
        with store.connect() as c:
            return [(r['id'], r['label'], names.action_label(c, r), terms_core.legacy_before(json.loads(r['before_json'])),
                     r['undone'], r['ts']) for r in c.execute('SELECT * FROM actions ORDER BY id')]

    def same(self, rebuild=True):
        tables = COMPAT + PROJECTIONS + PRIVATE
        self.checkpoints.capture(self.api, tables + ('actions',))
        self.assertEqual(snapshot(self.api, tables), snapshot(self.new, tables))
        with self.api.connect() as a, self.new.connect() as b:
            query = "SELECT key,value FROM meta WHERE key LIKE 'identity_%' ORDER BY key"
            self.assertEqual(list(map(tuple, a.execute(query))), list(map(tuple, b.execute(query))))
        self.assertEqual(self.actions(self.api), self.actions(self.new))
        try:
            terms_core.run('verify-characters', '--db', self.new.path)
        except terms_core.Failed as exc:
            self.fail(str(exc))
        if rebuild:
            full = refreshed_projections(self.new.path, True)
            self.assertEqual({t: sorted(full[t], key=repr) for t in PROJECTIONS}, snapshot(self.new, PROJECTIONS))
            # 核心的投影镜像（人物、皮肤那一片）：增量维护的等于整表重算的。
            self.assertEqual(snapshot(self.new, ('term_projection',)), rebuilt_snapshot(self.new, ('term_projection',)))
