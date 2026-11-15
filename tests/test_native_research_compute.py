"""#151 计算程序等价测试，用显式给定的固定/合成特征，从不用真实模型或图库。

旧 Python/NumPy 结果已冻结；测试通过当前原生命令准备合成状态。
本套件从不构建或查找二进制。开发窗口期把 ILLUSTASH_RESEARCH_COMPUTE_BIN 和 ILLUSTASH_TERMS_BIN
设为候选的 example 和宿主。
"""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest

import numpy as np

from tests import native_commands as terms_core
from tests.research_fixture import Features, decode
from tests import learn_fixture as reference_learning


class NativeResearchComputeTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = os.environ.get('ILLUSTASH_RESEARCH_COMPUTE_BIN')
        core = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not path or not core:
            raise unittest.SkipTest('explicit candidate research calculator/core required')
        cls.binary, cls.core = Path(path).resolve(), Path(core).resolve()
        if not cls.binary.is_file() or not cls.core.is_file():
            raise AssertionError('configured candidate calculator/core missing')

    def setUp(self):
        reference_learning.Learning.setUp(self)
        self.addCleanup(self.temp.cleanup)
        # 把现有的固定八维输入补零到生产环境的 CCIP 宽度。
        self.raw = np.pad(self.features.X, ((0, 0), (0, 760)))
        self.features = Features(self.store.source, self.raw, self.features.uuids)

    def request(self, kind='regroup', *, regroup=True, args=None, mutate=None):
        root = Path(self.temp.name)
        snapshot = Path(tempfile.mkdtemp(prefix='snapshot-', dir=root))
        for original, name in ((self.store.path, 'library.sqlite'), (self.store.source, 'source.sqlite')):
            with sqlite3.connect(original) as source, sqlite3.connect(snapshot / name) as dest:
                source.backup(dest)
        with sqlite3.connect(snapshot / 'source.sqlite') as c:
            c.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB,PRIMARY KEY(uuid,model))')
            c.executemany('INSERT OR REPLACE INTO embeddings VALUES (?,\'ccip\',?)',
                          [(u, row.astype('<f4').tobytes()) for u, row in zip(self.features.uuids, self.raw)])
        anchors = snapshot / 'anchors'
        anchors.mkdir()
        features = snapshot / 'features'
        features.mkdir()
        token = terms_core.run('learning-snapshot', '--db', self.store.path)['token']
        request = dict(format='illustash-research/1', identity=dict(synthetic=True), version='fixed-input-v1',
                       kind=kind, args=args or {}, regroup=regroup, snapshot=str(snapshot),
                       features=str(features), anchors=str(anchors), core=str(self.core), learning_token=token)
        if mutate:
            mutate(request)
        input_path, output = snapshot / 'request.json', snapshot / 'response.json'
        input_path.write_text(json.dumps(request), encoding='utf-8')
        # 空 PATH 证明这个计算程序自己从不把学习交给 Python 或子进程。
        # 固定答案来自只用原型的算法；判别式认领（#156）在这里保持关闭，由它们的 Rust 测试和
        # tests/test_native_research.py 覆盖。
        env = os.environ | dict(PATH='', ILLUSTASH_SHARED=str(anchors), ILLUSTASH_RESEARCH_PROGRESS=str(snapshot / 'progress.json'),
                                ILLUSTASH_TEST_PROTOTYPES_ONLY='1')
        env.pop('ILLUSTASH_WORKER_PYTHON', None)
        result = subprocess.run([str(self.binary), str(input_path), str(output)], env=env,
                                capture_output=True, timeout=30)
        return result, output, snapshot

    @staticmethod
    def rows(path):
        with sqlite3.connect(path) as c:
            # `actions` 按录制时的列取：core v8 新增了两列（#201），通过历史记录的回复比较。
            columns = {'actions': 'id,label,before_json,ts,undone'}
            return {table: c.execute(f"SELECT {columns.get(table, '*')} FROM {table} ORDER BY 1,2").fetchall()
                    for table in ('prototypes', 'learn_state', 'proto_claims', 'weak_claims', 'assertions', 'actions', 'memberships')}

    def compare_learning(self, *, mutate=None, patched_anchors=False):
        result, output, snapshot = self.request(mutate=mutate)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        response = json.loads(output.read_text())
        fixed = self.fixed['steps'][self.fixed_position]
        self.fixed_position += 1
        expected = fixed['response']
        self.assertEqual(response['result'], expected['result'])
        actual_rows = self.rows(snapshot / 'library.sqlite')
        actual_rows['learn_state'] = [r[:2] for r in actual_rows['learn_state']]
        from tests.fixed_expectations import unclocked
        def normalized(value):
            if isinstance(value, (tuple, list)):
                return [normalized(v) for v in value]
            if isinstance(value, dict):
                return {k:normalized(v) for k,v in value.items()}
            return unclocked(value)
        self.assertEqual(normalized(actual_rows), normalized(fixed['rows']))
        self.assertEqual(response['tables']['clusters'], expected['tables']['clusters'])
        self.assertEqual([r[:4] + r[5:] for r in response['tables']['cluster_info']],
                         [r[:4] + r[5:] for r in expected['tables']['cluster_info']])
        # 把检查过的原生快照当作下一条命令的输入；固定答案保持只读。
        with sqlite3.connect(snapshot / 'library.sqlite') as source, sqlite3.connect(self.store.path) as dest:
            source.backup(dest)
        with self.store.connect() as c:
            for table in ('clusters','cluster_info'):
                c.execute('DELETE FROM '+table)
                rows = response['tables'][table]
                if rows:
                    c.executemany('INSERT INTO '+table+' VALUES ('+','.join('?' for _ in rows[0])+')', rows)
        return response

    def fixed_oc(self):
        value = self.fixed['oc'][self.oc_position]
        self.oc_position += 1
        return value

    def test_prototypes_claims_weak_and_unmatched_cache_exact(self):
        self.compare_learning()

    def test_valid_anchors_skip_bad_encoding_and_json_but_preserve_io_errors(self):
        reference_root = Path(self.temp.name).resolve() / 'shared'
        reference_root.mkdir()
        self.assertTrue(reference_root.is_relative_to(Path(self.temp.name).resolve()))
        with sqlite3.connect(self.store.source) as c:
            for column, kind in [('anchor_p90', 'REAL'), ('anchor_p75', 'REAL'), ('anchor_p50', 'REAL'),
                                 ('base_tag', 'TEXT'), ('skin', 'TEXT'), ('post_count', 'INTEGER'), ('gender_conf', 'REAL')]:
                c.execute(f'ALTER TABLE characters ADD COLUMN {column} {kind}')
            c.execute("UPDATE characters SET anchor_p90=.1,anchor_p75=.03 WHERE char_id='alice'")

        def make_anchors(request, *, io_failure=False):
            vector = np.zeros((1, 768), np.float32)
            vector[0, 7] = .9
            vector[0, 6] = np.sqrt(1 - float(vector[0, 7]) ** 2)
            # 旧的裸 source 布局在 source.sqlite 旁边解析，而原生计算程序收到的是显式的快照资源路径。
            # 给两边真实的解析器提供相同的合成文件；不 mock 计算。
            for root in (Path(request['anchors']), reference_root):
                np.save(root / 'char-anchors.npy', vector)
                (root / 'char-anchors.txt').write_text('alice\n', encoding='utf-8')
                index = root / 'char-index'
                index.mkdir(exist_ok=True)
                (index / 'alice.json').write_text('{"source":"danbooru-patch"}', encoding='utf-8')
                (index / 'unrelated-bad-encoding.json').write_bytes(b'\xff\xfe')
                (index / 'unrelated-bad-json.json').write_text('{', encoding='utf-8')
                if io_failure:
                    (index / 'directory.json').mkdir()

        self.compare_learning(mutate=make_anchors, patched_anchors=True)
        with self.store.connect() as c:
            self.assertEqual([r[0] for r in c.execute("SELECT entity FROM weak_claims WHERE uuid='ring' AND source='CCIP'")], [self.alice])
        before = self.rows(self.store.path)
        result, output, snapshot = self.request(mutate=lambda r: make_anchors(r, io_failure=True))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(output.exists())
        self.assertEqual(self.rows(snapshot / 'library.sqlite'), before)

    def test_negative_seed_and_undo_preserve_human_history(self):
        action = self.store.change(['s0'], 'rejected', self.alice)
        self.compare_learning()
        self.store.undo(action['id'])
        self.compare_learning()

    def test_removed_assets_and_reembedded_seeds_invalidate(self):
        self.compare_learning()
        self.store.import_changes(removed=['s0', 's1'])
        self.raw[3] = 0
        self.raw[3, 7] = 1
        self.features = Features(self.store.source, self.raw, self.features.uuids)
        self.compare_learning()

    def test_too_few_seeds_and_mascot_never_learn(self):
        self.store.change(['s0', 's1', 's2'], 'rejected', self.alice)
        self.compare_learning()
        self.store.set_mascot(self.alice, True)
        self.compare_learning()

    def test_shared_oc_groups_use_same_visibility_and_features(self):
        oc = self.store.shared_oc()
        self.store.change([f'u{i}' for i in range(4)] + ['far'], 'confirmed', oc['id'])
        result, output, _ = self.request('oc', regroup=False, args=dict(entity=oc['id']))
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(json.loads(output.read_text())['result'], self.fixed_oc())
        self.compare_learning()

    def test_invalid_vector_and_stale_token_leave_snapshot_untouched(self):
        original = self.raw.copy()
        self.raw[0, 0] = np.nan
        result, output, snapshot = self.request()
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(output.exists())
        before = self.rows(self.store.path)
        self.assertEqual(self.rows(snapshot / 'library.sqlite'), before)
        self.raw = original
        result, output, snapshot = self.request(mutate=lambda request: request.update(learning_token='obsolete'))
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(output.exists())
        self.assertEqual(self.rows(snapshot / 'library.sqlite'), before)

    def test_unchanged_second_learning_has_no_batch_and_regroup_reclaims(self):
        self.compare_learning()
        result, output, _ = self.request('events', regroup=False)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        response = json.loads(output.read_text())
        self.assertEqual(response['result'], dict(relearned=0, changed=0))
        self.assertNotIn('learning', response)
        self.compare_learning()

    def test_trait_near_pairs_final_groups_match_original_float64_path(self):
        # 距离严格介于 .08 和 .12 之间的两组，需要原来的 float64 加权特征检验，而不是先前的外观捷径。
        # 第三组不带标签、未匹配，让稀有度权重不为零。前两组共有 1、2/3、1/3：范数不平凡，描述得分各不相同，
        # 与未验收的 NumPy 2.x 同分排序路径无关。
        ids = self.features.uuids
        self.raw = np.zeros((len(ids), 768), np.float32)
        for i, ident in enumerate(ids):
            if ident.startswith('s'):
                self.raw[i, 0] = 1
            elif ident in ('u0', 'u1', 'u2'):
                self.raw[i, 1] = 1
            elif ident in ('far', 'ring', 'hintb'):
                self.raw[i, 1:3] = (.8, .6)  # 与第一个未匹配组的距离为 .1
            else:
                self.raw[i, 3] = 1
        self.features = Features(self.store.source, self.raw, ids)
        tags = ('blue_eyes', 'ahoge', 'very_long_hair')
        # 当前的 general_hits 是归 Core 管的投影。这些合成 WD 标签经现有的证据导入器送进去，绝不直接写视图。
        import sqlite3
        with sqlite3.connect(self.store.source) as c:
            c.execute("DELETE FROM labels WHERE dim='general'")
            for group in (('u0', 'u1', 'u2'), ('far', 'ring', 'hintb')):
                for i, ident in enumerate(group):
                    for j, tag in enumerate(tags):
                        if i < (3 - j):
                            c.execute("INSERT INTO labels VALUES (?,'general',?,1)", (ident, tag))
        self.store.refresh_general_hits()
        response = self.compare_learning()
        self.assertEqual(sorted(row[1] for row in response['tables']['cluster_info']), [3, 3, 4])
        near = [row for row in response['tables']['cluster_info'] if row[5] == 'trait']
        self.assertEqual(len(near), 2, response['tables']['cluster_info'])
        for row in near:
            self.assertGreater(row[3], .08)
            self.assertLess(row[3], .12)

    def test_rejected_machine_claim_and_named_unmatched_members_leave_no_stale_group(self):
        self.compare_learning()
        self.assertEqual(reference_learning.Learning.owners(self, 'close'), [self.alice])
        action = self.store.change(['close'], 'rejected', self.alice)
        self.compare_learning()
        self.assertEqual(reference_learning.Learning.owners(self, 'close'), [])
        self.assertEqual(reference_learning.Learning.owners(self, 'edge'), [])
        self.store.undo(action['id'])
        self.compare_learning()
        self.assertEqual(reference_learning.Learning.owners(self, 'close'), [self.alice])
        self.store.change(['u0', 'u1', 'u2'], 'confirmed', self.bob)
        result = self.compare_learning()
        self.assertEqual(result['result']['clusters'], 0)
        self.assertFalse(any(row[0] in ('u0', 'u1', 'u2') for row in result['tables']['clusters']))

    def test_outer_ring_is_only_a_lead_and_human_rejection_removes_it(self):
        self.compare_learning()
        with self.store.connect() as c:
            vector, threshold, hint = c.execute('SELECT vec,threshold,hint FROM prototypes WHERE entity=?', (self.alice,)).fetchone()
        centre = np.frombuffer(vector, np.float32)
        side = reference_learning.unit(np.eye(768)[7] - centre * centre[7])
        distance = (threshold + hint) / 2
        self.raw[self.features.row['ring']] = reference_learning.unit(
            centre * (1 - 2 * distance) + side * np.sqrt(1 - (1 - 2 * distance) ** 2))
        self.features = Features(self.store.source, self.raw, self.features.uuids)
        self.compare_learning()
        self.assertNotIn(self.alice, reference_learning.Learning.owners(self, 'ring'))
        with self.store.connect() as c:
            self.assertEqual([r[0] for r in c.execute("SELECT entity FROM weak_claims WHERE uuid='ring' AND source='PROTO'")], [self.alice])
        self.store.change(['ring'], 'rejected', self.alice)
        self.compare_learning()
        with self.store.connect() as c:
            self.assertEqual(c.execute("SELECT entity FROM weak_claims WHERE uuid='ring' AND entity=?", (self.alice,)).fetchall(), [])

    def test_oc_total_includes_missing_features_but_excludes_hidden_collection_members(self):
        oc = self.store.shared_oc()['id']
        self.store.change(['u0', 'u1', 'u2', 'u3', 'far'], 'confirmed', oc)
        ids = [u for u in self.features.uuids if u != 'u3']
        self.raw = self.raw[[self.features.row[u] for u in ids]]
        self.features = Features(self.store.source, self.raw, ids)
        with self.store.connect() as c:
            history = c.execute('SELECT * FROM actions ORDER BY id').fetchall()
            curation = c.execute('SELECT * FROM curation ORDER BY uuid').fetchall()
            excluded = c.execute('SELECT * FROM excluded ORDER BY uuid').fetchall()
        action = self.store.curate(['far'], 'exclude')['undo']['id']
        current_before = '[{"curation":null,"uuid":"far"}]'
        legacy_before = '[{"uuid": "far", "curation": null}]'
        self.assertEqual(json.loads(current_before), json.loads(legacy_before))
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT before_json FROM actions WHERE id=?', (action,)).fetchone()[0],
                             current_before)
            self.assertEqual(c.execute('SELECT * FROM actions WHERE id<>? ORDER BY id', (action,)).fetchall(), history)
        result, output, _ = self.request('oc', regroup=False, args=dict(entity=oc))
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        actual = json.loads(output.read_text())['result']
        self.assertEqual(actual, self.fixed_oc())
        self.assertEqual(actual['total'], 4)
        self.assertEqual([sorted(g) for g in actual['groups']], [['u0', 'u1', 'u2']])
        # 只在独立快照里替换这条新动作的编码，保持原图库和旧历史不动。
        tokens = {}
        def legacy_snapshot(request):
            database = Path(request['snapshot']) / 'library.sqlite'
            with sqlite3.connect(database) as c:
                c.execute('UPDATE actions SET before_json=? WHERE id=?', (legacy_before, action))
            tokens['current'] = request['learning_token']
            tokens['legacy'] = terms_core.run('learning-snapshot', '--db', database)['token']
            self.assertNotEqual(tokens['legacy'], tokens['current'])
            request['learning_token'] = tokens['legacy']
        result, output, _ = self.request(mutate=legacy_snapshot)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        legacy = json.loads(output.read_text())
        current = self.compare_learning()
        self.assertEqual(legacy['learning']['token'], tokens['legacy'])
        self.assertEqual(current['learning']['token'], tokens['current'])
        # 戳记差异已单独验证；其余完整响应只沿用固定契约已有的时钟遮罩，所有计算值逐字比较。
        legacy['learning']['token'] = tokens['current']
        from tests.fixed_expectations import unclocked
        self.assertEqual(unclocked(json.dumps(current, sort_keys=True)), unclocked(json.dumps(legacy, sort_keys=True)))
        self.store.application_command('/api/undo', {'id': action})
        with self.store.connect() as c:
            self.assertEqual(c.execute('SELECT * FROM curation ORDER BY uuid').fetchall(), curation)
            self.assertEqual(c.execute('SELECT * FROM excluded ORDER BY uuid').fetchall(), excluded)
            self.assertEqual(c.execute('SELECT * FROM actions WHERE id<>? ORDER BY id', (action,)).fetchall(), history)
            self.assertEqual(tuple(c.execute('SELECT before_json,undone FROM actions WHERE id=?', (action,)).fetchone()),
                             (current_before, 1))

    def test_rejected_skin_is_not_a_parent_negative(self):
        self.compare_learning()
        expected = reference_learning.Learning.learned_snapshot(self)
        self.store.change(['s0'], 'remove_variant', 'summer')
        self.compare_learning()
        self.assertEqual(reference_learning.Learning.learned_snapshot(self), expected)
        self.assertEqual(reference_learning.Learning.owners(self, 'close'), [self.alice])

    def test_hair_colour_splits_a_group_and_traits_describe_it(self):
        ids = self.features.uuids
        together = ('u0','u1','u2','u3','far','ring','hintb')
        for ident in together:
            self.raw[self.features.row[ident]] = np.eye(1,768,1,dtype=np.float32)[0]
        self.features = Features(self.store.source,self.raw,ids)
        hits = [(u,'blonde_hair' if i<4 else 'black_hair') for i,u in enumerate(together)]
        hits += [(u,'halo') for u in together]
        with sqlite3.connect(self.store.source) as c:
            c.executemany("INSERT INTO labels VALUES (?,'general',?,1)",hits)
        self.store.refresh_general_hits()
        response = self.compare_learning()
        parts = response['tables']['cluster_info']
        self.assertEqual(sorted(row[1] for row in parts),[3,4])
        self.assertTrue(all(row[2] is None for row in parts),parts)
        self.assertTrue(any('blonde hair' in row[6] for row in parts),parts)
        self.assertTrue(any('black hair' in row[6] for row in parts),parts)
        self.assertTrue(all('halo' in row[6] for row in parts),parts)
