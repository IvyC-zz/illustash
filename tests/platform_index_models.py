"""显式运行的 macOS/Windows CCIP/索引/识别验收；不参与 discover。

需要候选二进制、经授权的模型/运行库，以及 PR #172 里独立的八张生成 PNG 的参考结果。
缺少输入就失败，从不跳过。这里只安装索引所需的闭包，不是完整的签名应用。
以 ILLUSTASH_CPU_ONLY=1 运行：固定期望是 CPU 上的值，逐位一致。识别用的是加速的 CCIP 会话（#270）；
tests/platform_recognition_models.py 在舍入误差范围内比较它。
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import unittest

import numpy as np

from tests import test_native_index_compute as compute
from tests import test_native_index_production as installed
from tools.wait_limit import WAIT


class IndexModelAcceptance(installed.InstalledIndexAppTest):
    # 普通夹具在 macOS 上的跳过不能把显式的平台运行也藏掉。
    __unittest_skip__ = False

    @classmethod
    def setUpClass(cls):
        if sys.platform not in ('darwin', 'win32'):
            raise RuntimeError('macOS/Windows acceptance only; Linux native routing remains unaccepted')
        required = ('ILLUSTASH_TERMS_BIN', 'ILLUSTASH_INDEX_ADAPTER_BIN',
                    'ILLUSTASH_NATIVE_ROOT', 'ILLUSTASH_RECOGNITION_COMPUTE_BIN',
                    'ILLUSTASH_INDEX_INSTALLED_MODEL', 'ILLUSTASH_INDEX_INSTALLED_RUNTIME',
                    'ILLUSTASH_INDEX_REFERENCE_RAW')
        if sys.platform == 'win32':
            required += ('ILLUSTASH_RESEARCH_OPENBLAS_DLL',)  # 复制到安装后的目录布局里
        for key in required:
            if not os.environ.get(key):
                raise RuntimeError(f'explicit authorised input required: {key}')
            Path(os.environ[key]).resolve(strict=True)
        super().setUpClass()
        cls.recognition = Path(os.environ['ILLUSTASH_RECOGNITION_COMPUTE_BIN']).resolve(strict=True)

    def setUp(self):
        # 平台专项复用安装版用例的合成包；本模块从未单独录制 setup。
        method = self._testMethodName
        self._testMethodName = 'test_cached_installed_compute_publish_drop_preserves_history'
        try:
            super().setUp()
        finally:
            self._testMethodName = method
        self.env['ILLUSTASH_CPU_ONLY'] = '1'

    def expected(self):
        # 仅此生成图专项的原算法期望；不复算、不刷新通用固定录制。
        from tests.fixed_expectations import fixed_values
        values = fixed_values('platform_index_values.json.xz')['index']
        cursor = getattr(self, '_platform_expected_cursor', 0)
        self._platform_expected_cursor = cursor + 1
        return values[cursor]

    def install(self, root):
        adapter, resources = super().install(root)
        model = Path(os.environ['ILLUSTASH_INDEX_INSTALLED_MODEL'])
        runtime = Path(os.environ['ILLUSTASH_INDEX_INSTALLED_RUNTIME'])
        (resources/'models/ccip').mkdir(parents=True)
        shutil.copy2(model, resources/'models/ccip/model_feat.onnx')
        (resources/'onnxruntime').mkdir(exist_ok=True)
        shutil.copy2(runtime, resources/('onnxruntime/onnxruntime.dll' if os.name == 'nt' else 'onnxruntime/libonnxruntime.dylib'))
        host = adapter.with_name('illustash-terms.exe' if os.name == 'nt' else 'illustash-terms')
        shutil.copy2(self.binary, host)
        self.binary = host
        self.resources = resources
        return adapter, resources

    def restart(self, extra=()):
        if hasattr(self, 'adapter_resources'):
            # 宿主和适配器都必须使用安装好的资源。基类的真实模型用例设置了外部路径，
            # 这里显式去掉这些覆盖。
            for key in ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_ORT_DYLIB'):
                self.env.pop(key, None)
            self.resources = self.adapter_resources
        return super().restart(extra)

    def assert_recognition(self):
        # 原生 index 已由固定旧算法值验收；这里检查索引输出可供识别消费、
        # 认领只指向已建角色，距离与数据库阈值保持一致，并拒绝篡改。
        anchors = np.load(self.shared/'char-anchors.npy')
        anchor_ids = (self.shared/'char-anchors.txt').read_text().splitlines()
        features = self.reference_rows[:3]
        with sqlite3.connect(self.lib.source) as connection:
            metadata = {row[0]: row[1:] for row in connection.execute(
                'SELECT char_id,anchor_p90,anchor_p75 FROM characters')}
        entities = set(metadata)
        request = dict(mode='claims', shared=str(self.shared), source=str(self.lib.source),
                       assets=['a','b','c'], features=features.tolist(),
                       identity=dict(package=str(self.lib.path), library_id=self.lib.config['id'],
                                     generation=1, session='0'*64, task_id='1'*32),
                       tamper_asset='a', tamper_character='not-an-index-character')
        input_path, output = self.root/'claims.json', self.root/'claims-output.json'
        input_path.write_text(json.dumps(request))
        env = self.env.copy()
        if os.name == 'nt':  # 开发用的 example 没有安装；指明安装好的 DLL。
            env['ILLUSTASH_RESEARCH_OPENBLAS_DLL'] = str(self.adapter_resources/'openblas/openblas.dll')
        result = subprocess.run([str(self.recognition), str(input_path), str(output)],
                                env=env, capture_output=True, timeout=WAIT)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        actual = json.loads(output.read_text())
        self.assertTrue(actual['claims'])
        self.assertEqual({row['asset'] for row in actual['claims']}, {'a','b','c'})
        self.assertTrue(set(actual['entities']) <= entities)
        for claim in actual['claims']:
            for key in ('distance', 'threshold'):
                claim[key] = float(np.float32(claim[key]))
        patched = {path.stem for path in (self.shared/'char-index').glob('*.json')
                   if json.loads(path.read_text()).get('source') == 'danbooru-patch'}
        for claim in actual['claims']:
            self.assertIn(claim['asset'], ('a','b','c'))
            self.assertIn(claim['character'], entities)
            feature = features[('a','b','c').index(claim['asset'])]
            # 独立的余弦距离公式检查实际匹配锚点，别名/排序由固定契约覆盖。
            distances = (1 - anchors @ feature / np.linalg.norm(anchors, axis=1)
                         / np.linalg.norm(feature)) / 2
            candidates = [index for index, distance in enumerate(distances)
                          if abs(float(distance)-claim['distance']) < 1e-6]
            self.assertTrue(candidates)
            thresholds = [float(np.float32(metadata[anchor_ids[index]][anchor_ids[index] in patched]))
                          for index in candidates]
            self.assertIn(claim['threshold'], thresholds)
            self.assertLessEqual(claim['distance'], claim['threshold'])
            self.assertAlmostEqual(claim['confidence'], 1-claim['distance'], places=7)
        self.assertTrue(actual['tampered_rejected'])

    def test_real_index_then_recognition_cached_rebuild_and_drop(self):
        # 就绪检查加载安装好的 CPU 会话，在 Windows 上还加载固定的 OpenBLAS。
        ready = subprocess.run([str(self.adapter), '--check'], env=self.env, capture_output=True, timeout=WAIT)
        self.assertEqual(ready.returncode, 0, ready.stderr.decode(errors='replace'))
        self.assertNotIn('ILLUSTASH_RESEARCH_OPENBLAS_DLL', self.env)
        # 恰好八个新的模型特征，由测试自己持有的回环服务器提供。
        super().test_actual_ccip_download_to_installed_adapter_without_python()
        self.assertNotIn('ILLUSTASH_MODELS', self.env)
        self.assertNotIn('ILLUSTASH_ORT_DYLIB', self.env)
        self.assertFalse((self.adapter_resources/'models/wd_swinv2_v3_emb.onnx').exists())
        self.assertFalse((self.adapter.parent.parent/'workers').exists())
        self.assert_recognition()

        # 用不带模型/运行库的真实适配器，从缓存的 raw 重新计算一份准备好的隔离 review。
        # 公开的 apply 只提供缺少的角色；不对已安装的角色提供强制重算的操作。
        self.stop(self.process)
        raw_path = self.shared/'char-patch/raw/carol_example.npy'
        raw_digest = hashlib.sha256(raw_path.read_bytes()).hexdigest()
        self.expected_result = self.expected()
        for relative in ('models/ccip/model_feat.onnx', 'onnxruntime/onnxruntime.dll' if os.name == 'nt' else 'onnxruntime/libonnxruntime.dylib'):
            (self.adapter_resources/relative).unlink()
        review = self.root/'cached review with spaces'
        review.mkdir()
        for name in ('char-index', 'char-patch'):
            shutil.copytree(self.shared/name, review/name)
        (review/'game-chars').mkdir()
        (review/'album-char-map.json').write_text('{"map":{}}')
        for name in ('full-features.npy', 'full-manifest.csv'):
            shutil.copy2(self.features/name, review/name)
        with sqlite3.connect(self.lib.source) as source, sqlite3.connect(review/'pi.db') as target:
            source.backup(target)
        # raw 特征数据有意不含元数据；保留已接受角色的元数据，丢弃由它导出的清洗后矩阵。
        (review/'char-index/carol_example.npy').unlink()
        before = len(self.calls)
        request = dict(format='illustash-index/1', identity=dict(package=str(self.lib.path),
                       library_id=self.lib.config['id'], generation=1, session='0'*64, task_id='2'*32),
                       before='synthetic-cached', action='apply', rows=self.remote_rows,
                       review=str(review), reason=None)
        input_path, output = review/'request.json', review/'response.json'
        input_path.write_text(json.dumps(request))
        result = subprocess.run([str(self.adapter), str(input_path), str(output)],
                                env=self.env, capture_output=True, timeout=WAIT)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        actual = json.loads(output.read_text())
        for key in ('characters', 'aliases', 'labels'):
            self.assertEqual(sorted(actual[key], key=lambda row:json.dumps(row, sort_keys=True)),
                             sorted(self.expected_result[key], key=lambda row:json.dumps(row, sort_keys=True)), key)
        np.testing.assert_array_equal(np.load(review/'char-anchors.npy'), np.load(self.shared/'char-anchors.npy'))
        self.assertEqual(len(self.calls), before)
        self.assertEqual(hashlib.sha256(raw_path.read_bytes()).hexdigest(), raw_digest)
        self.restart()
        entity = self.assert_projection(self.expected_result)
        self.assert_recognition()
        self.index({'action':'drop','char':'carol_example','reason':'合成验收完成'})
        result = self.finish()
        self.assertEqual(result['state']['status'], 'done', result)
        self.assertEqual(result['history'][0]['characters'][0]['status'], 'dropped')
        with sqlite3.connect(self.lib.db) as db:
            self.assertEqual(db.execute("SELECT count(*) FROM candidates WHERE entity=? AND source='CCIP'", [entity]).fetchone()[0], 0)
            self.assertEqual(db.execute("SELECT count(*) FROM memberships WHERE entity=? AND status='machine'", [entity]).fetchone()[0], 0)
        self.assertEqual(installed.snapshot(self.lib.db)['actions'], self.human)
        self.assertEqual(installed.snapshot(self.lib.db)['assertions'], self.human_assertions)
        self.assertEqual(len(self.calls), before)
        self.assertEqual(hashlib.sha256(raw_path.read_bytes()).hexdigest(), raw_digest)
        self.assertFalse(self.sentinel.exists())


def load_tests(loader, tests, pattern):
    # 只选这个用例，不选继承来的普通/故障/模型测试。
    return unittest.TestSuite([IndexModelAcceptance('test_real_index_then_recognition_cached_rebuild_and_drop')])


if __name__ == '__main__':
    unittest.main()
