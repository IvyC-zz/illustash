"""Explicit macOS/Windows CCIP/index/recognition acceptance; excluded from discover.

Requires candidate binaries and authorised model/runtime plus the independent
eight-generated-PNG reference from PR #172. Missing inputs fail, never skip.
This installs only the index closure, not a complete signed application.
"""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import unittest
from unittest.mock import patch

import numpy as np

from tests import test_native_index_compute as compute
from tests import test_native_index_production as installed


class IndexModelAcceptance(installed.InstalledIndexAppTest):
    # The ordinary fixture's macOS skip must not hide an explicit platform run.
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
            required += ('ILLUSTASH_RESEARCH_OPENBLAS_DLL',)  # copied into the installed layout
        for key in required:
            if not os.environ.get(key):
                raise RuntimeError(f'explicit authorised input required: {key}')
            Path(os.environ[key]).resolve(strict=True)
        super().setUpClass()
        cls.recognition = Path(os.environ['ILLUSTASH_RECOGNITION_COMPUTE_BIN']).resolve(strict=True)

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
            # Both host and adapter must use installed resources. The base real
            # model case sets external paths; explicitly remove those overrides.
            for key in ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_ORT_DYLIB'):
                self.env.pop(key, None)
            self.resources = self.adapter_resources
        return super().restart(extra)

    def assert_recognition(self):
        anchors = np.load(self.shared/'char-anchors.npy')
        ids = (self.shared/'char-anchors.txt').read_text().splitlines()
        with patch.object(compute.char_scan, 'REVIEW', str(self.shared)), patch.object(compute.char_scan, 'cross', side_effect=lambda a,b,check=True:compute.ccip.cross(a,b,check=False)), contextlib.redirect_stdout(io.StringIO()):
            _, p90, _, p75, entities = compute.char_scan.anchor_index(anchors, ids, self.lib.source)
            limits = compute.char_scan.claim_thresholds(ids, p90, p75)
        features = self.reference_rows[:3]
        expected = []
        for asset, feature in zip(('a','b','c'), features):
            distances = compute.ccip.cross(anchors, feature[None], check=False)[:,0]
            seen = set()
            for index in np.argsort(distances)[:9]:
                entity = entities[index]
                if entity in seen:
                    continue
                seen.add(entity)
                if limits[index] > 0 and distances[index] <= limits[index]:
                    expected.append(dict(asset=asset, character=entity,
                                         distance=float(distances[index]), threshold=float(limits[index]),
                                         confidence=1-float(distances[index])))
        self.assertTrue(expected)
        request = dict(mode='claims', shared=str(self.shared), source=str(self.lib.source),
                       assets=['a','b','c'], features=features.tolist(),
                       identity=dict(package=str(self.lib.path), library_id=self.lib.config['id'],
                                     generation=1, session='0'*64, task_id='1'*32),
                       tamper_asset=expected[0]['asset'], tamper_character='not-an-index-character')
        input_path, output = self.root/'claims.json', self.root/'claims-output.json'
        input_path.write_text(json.dumps(request))
        env = self.env.copy()
        if os.name == 'nt':  # The development example is not installed; name the installed DLL.
            env['ILLUSTASH_RESEARCH_OPENBLAS_DLL'] = str(self.adapter_resources/'openblas/openblas.dll')
        result = subprocess.run([str(self.recognition), str(input_path), str(output)],
                                env=env, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        actual = json.loads(output.read_text())
        self.assertEqual(actual['entities'], entities)
        for claim in actual['claims']:
            for key in ('distance', 'threshold'):
                claim[key] = float(np.float32(claim[key]))
        self.assertEqual(actual['claims'], expected)
        self.assertTrue(actual['tampered_rejected'])

    def test_real_index_then_recognition_cached_rebuild_and_drop(self):
        # Readiness loads the installed CPU session and, on Windows, the fixed OpenBLAS.
        ready = subprocess.run([str(self.adapter), '--check'], env=self.env, capture_output=True, timeout=20)
        self.assertEqual(ready.returncode, 0, ready.stderr.decode(errors='replace'))
        self.assertNotIn('ILLUSTASH_RESEARCH_OPENBLAS_DLL', self.env)
        # Exactly eight new model features, served by the owned loopback server.
        super().test_actual_ccip_download_to_installed_adapter_without_python()
        self.assertNotIn('ILLUSTASH_MODELS', self.env)
        self.assertNotIn('ILLUSTASH_ORT_DYLIB', self.env)
        self.assertFalse((self.adapter_resources/'models/wd_swinv2_v3_emb.onnx').exists())
        self.assertFalse((self.adapter.parent.parent/'workers').exists())
        self.assert_recognition()

        # Recompute a prepared isolated review from cached raw using the real
        # adapter without model/runtime. Public apply offers only missing roles;
        # it does not expose a force-recompute action for an installed role.
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
        # Raw feature data deliberately does not contain metadata; retain the
        # accepted role's metadata while discarding its derived cleaned matrix.
        (review/'char-index/carol_example.npy').unlink()
        before = len(self.calls)
        request = dict(format='illustash-index/1', identity=dict(package=str(self.lib.path),
                       library_id=self.lib.config['id'], generation=1, session='0'*64, task_id='2'*32),
                       before='synthetic-cached', action='apply', rows=self.remote_rows,
                       review=str(review), reason=None)
        input_path, output = review/'request.json', review/'response.json'
        input_path.write_text(json.dumps(request))
        result = subprocess.run([str(self.adapter), str(input_path), str(output)],
                                env=self.env, capture_output=True, timeout=20)
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
    # Select this case only, not the inherited ordinary/fault/model tests.
    return unittest.TestSuite([IndexModelAcceptance('test_real_index_then_recognition_cached_rebuild_and_drop')])


if __name__ == '__main__':
    unittest.main()
