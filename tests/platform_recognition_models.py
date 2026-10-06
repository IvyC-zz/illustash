"""Explicit, opt-in CPU model acceptance; generated images/anchors only.

Run separately from discover. Required inputs: ILLUSTASH_TERMS_BIN,
ILLUSTASH_RECOGNITION_ADAPTER_BIN, ILLUSTASH_RECOGNITION_COMPUTE_BIN,
ILLUSTASH_RECOGNITION_MODELS and ILLUSTASH_ORT_DYLIB. Missing inputs fail.
The relocated resources below are a narrow recognition layout fixture, not a
signed/full installer. Windows also requires explicit ILLUSTASH_RESEARCH_OPENBLAS_DLL matching the
reference NumPy wheel; Linux production remains gated pending acceptance.
"""
from contextlib import closing
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

import numpy as np
import onnxruntime as ort
from PIL import Image
from tools import char_scan
from tools.ccip import cross, preprocess
from web.infer import wd_input
from tests import test_native_lifecycle as lifecycle
from tests.test_native_human_labels import snapshot
from tests.test_read_http import free_port

ROOT = Path(__file__).resolve().parents[1]


def explicit(name):
    value = os.environ.get(name)
    if not value:
        raise RuntimeError(f'{name} must be explicitly configured')
    return Path(value).resolve(strict=True)


def labels(asset, probabilities, tags):
    dims = {'0': 'general', '4': 'character', '9': 'rating'}
    return [dict(asset=asset, dim=dims[tag['category']], value=tag['name'],
                 confidence=round(float(probability), 4))
            for tag, probability in zip(tags, probabilities)
            if tag['category'] in dims and float(probability) >= .001]


class RecognitionModelTest(unittest.TestCase):
    stop = lifecycle.NativeLifecycleTest.stop
    start = lifecycle.NativeLifecycleTest.start
    request = lifecycle.NativeLifecycleTest.request

    @classmethod
    def setUpClass(cls):
        terms = explicit('ILLUSTASH_TERMS_BIN')
        adapter = explicit('ILLUSTASH_RECOGNITION_ADAPTER_BIN')
        cls.compute = explicit('ILLUSTASH_RECOGNITION_COMPUTE_BIN')
        models = explicit('ILLUSTASH_RECOGNITION_MODELS')
        runtime = explicit('ILLUSTASH_ORT_DYLIB')
        cls.temp = tempfile.TemporaryDirectory(prefix='illustash-recognition-models-')
        cls.addClassCleanup(cls.temp.cleanup)
        cls.layout = Path(cls.temp.name).resolve() / 'relocated recognition with spaces'
        cls.resources = cls.layout / 'resources'
        suffix = '.exe' if os.name == 'nt' else ''
        cls.binary = cls.layout / 'bin' / ('illustash-terms' + suffix)
        cls.adapter = cls.layout / 'bin' / ('illustash-recognition' + suffix)
        runtime_name = {'darwin': 'libonnxruntime.dylib', 'win32': 'onnxruntime.dll'}.get(sys.platform, 'libonnxruntime.so')
        copies = [(terms, cls.binary), (adapter, cls.adapter),
                  (runtime, cls.resources / 'onnxruntime' / runtime_name),
                  (ROOT / 'tags/tags_v3.csv', cls.resources / 'tags/tags_v3.csv')]
        copies += [(models / name, cls.resources / 'models' / name)
                   for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx')]
        for source, dest in copies:
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, dest)
        shutil.copytree(ROOT / 'web/static', cls.resources / 'web/static')
        if sys.platform == 'win32':
            (cls.resources / 'openblas').mkdir()
            shutil.copy2(explicit('ILLUSTASH_RESEARCH_OPENBLAS_DLL'), cls.resources / 'openblas' / 'openblas.dll')
        with (cls.resources / 'tags/tags_v3.csv').open(newline='', encoding='utf-8') as f:
            cls.tags = list(csv.DictReader(f))
        cls.images, cls.reference = [], []
        for i, (w, h, mode) in enumerate(((448, 448, 'RGB'), (67, 39, 'RGB'), (39, 67, 'RGB'),
                                        (384, 384, 'RGB'), (33, 49, 'RGBA'), (35, 21, 'L'),
                                        (19, 23, 'P'), (71, 53, 'JPEG'))):
            y, x = np.indices((h, w))
            image = Image.fromarray(np.stack(((x*71+y*13)%256, (x*5+y*107)%256,
                                             (x*19+y*37)%256), axis=2).astype('uint8'))
            if mode == 'RGBA':
                image = image.convert(mode)
                image.putalpha(Image.fromarray(((x*41+y*59)%256).astype('uint8')))
            elif mode not in ('RGB', 'JPEG'):
                image = image.convert(mode)
            path = cls.layout / f'generated-{i}.{"jpg" if mode == "JPEG" else "png"}'
            image.save(path)
            cls.images.append(path)
        options = ort.SessionOptions(); options.log_severity_level = 3; options.intra_op_num_threads = 4
        wd = ort.InferenceSession(str(models / 'wd_swinv2_v3_emb.onnx'), options, providers=['CPUExecutionProvider'])
        options = ort.SessionOptions(); options.log_severity_level = 3
        ccip = ort.InferenceSession(str(models / 'ccip/model_feat.onnx'), options, providers=['CPUExecutionProvider'])
        for path in cls.images:
            with Image.open(path) as image:
                probabilities, embedding = wd.run(None, {wd.get_inputs()[0].name: wd_input(image)})
                feature = ccip.run(None, {ccip.get_inputs()[0].name: preprocess(image)})[0]
            cls.reference.append(dict(probs=probabilities.reshape(-1), wd=embedding.reshape(-1), ccip=feature.reshape(-1)))
        # No simultaneous Python/native model sessions during the acceptance run.
        del wd, ccip
        cls.runtime = cls.resources / 'onnxruntime' / runtime_name

    def setUp(self):
        temp = tempfile.TemporaryDirectory(dir=self.layout, prefix='case-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.shared = self.root / 'shared'; self.shared.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('ILLUSTASH_')}
        self.env.update(PATH='', HOME=str(self.root / 'home'), USERPROFILE=str(self.root / 'home'),
                        ILLUSTASH_APP_SUPPORT=str(self.root / 'support'), ILLUSTASH_SHARED=str(self.shared))
        if sys.platform == 'win32':
            # Installed Windows launchers provide ROOT; the backend resolves only
            # ROOT/openblas/openblas.dll, without PATH or a Python/DLL override.
            self.env['ILLUSTASH_ROOT'] = str(self.resources)

    def invoke(self, binary, request, *, env=None):
        input_path = self.root / 'request.json'
        output_path = self.root / 'response.json'
        input_path.write_text(json.dumps(request), encoding='utf-8')
        output_path.unlink(missing_ok=True)
        result = subprocess.run([str(binary), str(input_path), str(output_path)],
                                env=env or self.env, cwd=self.root, capture_output=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        return json.loads(output_path.read_text(encoding='utf-8'))

    def test_raw_probabilities_vectors_and_labels(self):
        got = self.invoke(self.compute, dict(mode='infer', models=str(self.resources / 'models'),
                          runtime=str(self.runtime), tags=str(self.resources / 'tags/tags_v3.csv'),
                          images=list(map(str, self.images))))
        self.assertEqual(len(got), len(self.reference))
        for expected, actual in zip(self.reference, got):
            for key in ('probs', 'wd', 'ccip'):
                np.testing.assert_array_equal(np.asarray(actual[key], np.float32), expected[key])
            self.assertEqual(actual['labels'], labels('synthetic', expected['probs'], self.tags))

    def test_candidate_batches_bad_image_and_synthetic_anchor_claims(self):
        package = self.root / 'synthetic.pilib'; package.mkdir()
        source = package / 'source.sqlite'
        ids = [f'generated-character-{i}' for i in range(8)]
        with closing(sqlite3.connect(source)) as c:
            c.execute('CREATE TABLE characters(char_id TEXT PRIMARY KEY,anchor_p90 REAL,copyright TEXT,wd_tag TEXT,base_tag TEXT,skin TEXT,post_count INTEGER,gender_conf REAL,anchor_p50 REAL,anchor_p75 REAL)')
            c.executemany("INSERT INTO characters VALUES (?,.05,'synthetic',NULL,NULL,NULL,?,1,.01,.025)", [(v, 100+i) for i, v in enumerate(ids)])
            c.commit()
        before = source.read_bytes()
        vectors = np.asarray([r['ccip'] for r in self.reference], np.float32)
        np.save(self.shared / 'char-anchors.npy', vectors)
        (self.shared / 'char-anchors.txt').write_text('\n'.join(ids)+'\n', encoding='utf-8')
        (self.shared / 'char-index').mkdir()
        (self.shared / 'char-index' / (ids[0]+'.json')).write_text('{"source":"danbooru-patch"}', encoding='utf-8')
        identity = dict(package=str(package), library_id='synthetic', generation=1,
                        session=hashlib.sha256(b'synthetic').hexdigest(), task_id='synthetic')
        bad = self.root / 'bad.png'; bad.write_bytes(b'not an image')
        assets = [dict(id=f'asset-{i}', path=str(bad if i == 24 else self.images[i % 8])) for i in range(26)]
        output = self.root / 'output'; output.mkdir()
        # Progress is visible while the producer still runs: sample it until the process exits.
        progress = self.root / 'progress.json'
        request = dict(mode='produce', resources=str(self.resources), models=str(self.resources / 'models'),
                       runtime=str(self.runtime), shared=str(self.shared),
                       request=dict(format='illustash-sync/1', identity=identity, assets=assets, output=str(output)))
        (self.root / 'request.json').write_text(json.dumps(request), encoding='utf-8')
        (self.root / 'response.json').unlink(missing_ok=True)
        process = subprocess.Popen([str(self.compute), str(self.root / 'request.json'), str(self.root / 'response.json')],
                                   env=dict(self.env, ILLUSTASH_RECOGNITION_PROGRESS=str(progress)), cwd=self.root,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        seen, deadline = [], time.monotonic() + 120
        while process.poll() is None and time.monotonic() < deadline:
            try:
                value = json.loads(progress.read_text(encoding='utf-8'))
            except (OSError, ValueError):
                value = None
            if value and (not seen or seen[-1] != value):
                seen.append(value)
            time.sleep(.01)
        with process:
            self.assertEqual(process.wait(timeout=5), 0, process.stderr.read().decode(errors='replace'))
        response = json.loads((self.root / 'response.json').read_text(encoding='utf-8'))
        self.assertEqual({v['stage'] for v in seen} & {'preparing', 'recognising'}, {'preparing', 'recognising'})
        self.assertTrue(any(0 < v['processed'] < 26 for v in seen), seen)
        self.assertTrue(all(v['task_id'] == 'synthetic' and v['total'] == 26 for v in seen))
        self.assertEqual([v['processed'] for v in seen], sorted(v['processed'] for v in seen))
        final = json.loads(progress.read_text(encoding='utf-8'))
        self.assertEqual((final['stage'], final['processed'], final['failed']), ('recognising', 26, 1))
        self.assertEqual(response['identity'], identity)
        self.assertEqual(response['failed'], [{'uuid': 'asset-24'}])
        self.assertEqual(len(response['packages']), 2)
        self.assertEqual(len(response['character_claims']), 2)
        _, p90, _, p75, aliases = char_scan.anchor_index(vectors, ids, str(source))
        with patch.object(char_scan, 'REVIEW', str(self.shared)):
            limits = char_scan.claim_thresholds(ids, p90, p75)
        for batch, sidecar, size in zip(response['packages'], response['character_claims'], (24, 1)):
            path = Path(batch); manifest = json.loads((path / 'manifest.json').read_text(encoding='utf-8'))
            self.assertEqual(manifest['assets'], [f'asset-{i}' for i in (range(24) if size == 24 else (25,))])
            claim = json.loads(Path(sidecar).read_text(encoding='utf-8'))
            # The completion marker, written last, pins exactly the finished manifest and sidecar.
            complete = json.loads((path / 'complete.json').read_text(encoding='utf-8'))
            self.assertEqual(complete, dict(format='illustash-recognition-batch/1', task_id='synthetic',
                                            assets=manifest['assets'],
                                            manifest_sha256=hashlib.sha256((path / 'manifest.json').read_bytes()).hexdigest(),
                                            sidecar_sha256=hashlib.sha256(Path(sidecar).read_bytes()).hexdigest()))
            self.assertEqual(claim['identity'], identity)
            self.assertEqual(claim['assets'], manifest['assets'])
            self.assertEqual(claim['models'], manifest['models'])
            expected_labels, expected_claims = [], []
            for asset in manifest['assets']:
                row = self.reference[int(asset.split('-')[1]) % 8]
                expected_labels.extend(labels(asset, row['probs'], self.tags))
                distances = cross(vectors, np.asarray([row['ccip']], np.float32), check=False)[:, 0]
                seen = set()
                for j in np.argsort(distances)[:9]:
                    char = aliases[j]
                    if char in seen:
                        continue
                    seen.add(char)
                    if limits[j] > 0 and distances[j] <= limits[j]:
                        expected_claims.append(dict(asset=asset, character=char, distance=float(distances[j]),
                                                    threshold=float(limits[j]), confidence=1-float(distances[j])))
            self.assertEqual([json.loads(line) for line in (path / 'labels.jsonl').read_text(encoding='utf-8').splitlines()], expected_labels)
            for field in ('distance', 'threshold'):
                np.testing.assert_array_equal(np.asarray([r[field] for r in claim['claims']], np.float32),
                                              np.asarray([r[field] for r in expected_claims], np.float32))
            self.assertEqual([(r['asset'], r['character'], r['confidence']) for r in claim['claims']],
                             [(r['asset'], r['character'], r['confidence']) for r in expected_claims])
            for model in ('wd', 'ccip'):
                expected = np.asarray([self.reference[int(a.split('-')[1]) % 8][model] for a in manifest['assets']], np.float32)
                np.testing.assert_array_equal(np.load(path / (model+'.npy')), expected)
            for name, fingerprint in manifest['files'].items():
                data = (path / name).read_bytes()
                self.assertEqual(fingerprint, dict(bytes=len(data), sha256=hashlib.sha256(data).hexdigest()))
        self.assertEqual(source.read_bytes(), before)

    @unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux native production gate awaits platform acceptance')
    def test_installed_no_anchor_bad_image_and_explicit_dependency_failures(self):
        package = self.root / 'synthetic.pilib'; package.mkdir()
        source = package / 'source.sqlite'; source.write_bytes(b'no-anchor source must never be read')
        before = source.read_bytes()
        bad = self.root / 'bad.png'; bad.write_bytes(b'not an image')
        output = self.root / 'output'; output.mkdir()
        identity = dict(package=str(package), library_id='synthetic', generation=1,
                        session=hashlib.sha256(b'synthetic').hexdigest(), task_id='synthetic')
        request = dict(format='illustash-sync/1', identity=identity, output=str(output),
                       assets=[dict(id='good', path=str(self.images[0])), dict(id='bad', path=str(bad))])
        response = self.invoke(self.adapter, request)
        self.assertEqual(response['failed'], [{'uuid': 'bad'}])
        self.assertEqual(len(response['packages']), 1)
        sidecar = json.loads(Path(response['character_claims'][0]).read_text(encoding='utf-8'))
        self.assertEqual(sidecar['claims'], [])
        self.assertEqual(sidecar['anchors'], dict(vectors=None, ids=None, metadata=None, patches=None))
        self.assertEqual(source.read_bytes(), before)
        for corrupt in (False, True):
            with self.subTest(corrupt_runtime=corrupt):
                runtime = self.root / 'invalid-runtime'
                if corrupt:
                    runtime.write_bytes(b'not a runtime')
                env = dict(self.env, ILLUSTASH_ORT_DYLIB=str(runtime))
                reply = self.root / 'failed-response.json'
                result = subprocess.run([str(self.adapter), str(self.root / 'request.json'), str(reply)],
                                        cwd=self.root, env=env, capture_output=True, timeout=30)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(runtime.name, result.stderr.decode(errors='replace'))
                self.assertFalse(reply.exists())
                self.assertEqual(source.read_bytes(), before)
        if sys.platform == 'win32':
            for corrupt in (False, True):
                with self.subTest(corrupt_openblas=corrupt):
                    backend = self.root / 'invalid-openblas.dll'
                    if corrupt:
                        backend.write_bytes(b'not an accepted backend')
                    env = dict(self.env, ILLUSTASH_RESEARCH_OPENBLAS_DLL=str(backend))
                    reply = self.root / 'failed-response.json'
                    result = subprocess.run([str(self.adapter), str(self.root / 'request.json'), str(reply)],
                                            cwd=self.root, env=env, capture_output=True, timeout=30)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('OpenBLAS', result.stderr.decode(errors='replace'))
                    self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
                    self.assertFalse(reply.exists())
                    self.assertEqual(source.read_bytes(), before)
        # A valid model with an incompatible tag table must fail before publishing any batch.
        resources = self.root / 'wrong-tags'; (resources / 'tags').mkdir(parents=True)
        (resources / 'tags/tags_v3.csv').write_text('name,category\ngeneral,9\n', encoding='utf-8')
        env = dict(self.env, ILLUSTASH_ROOT=str(resources),
                   ILLUSTASH_MODELS=str(self.resources / 'models'), ILLUSTASH_ORT_DYLIB=str(self.runtime))
        if sys.platform == 'win32':
            env['ILLUSTASH_RESEARCH_OPENBLAS_DLL'] = str(self.resources / 'openblas/openblas.dll')
        request['output'] = str(self.root / 'wrong-tags-output')
        (self.root / 'request.json').write_text(json.dumps(request), encoding='utf-8')
        reply = self.root / 'failed-response.json'
        result = subprocess.run([str(self.adapter), str(self.root / 'request.json'), str(reply)],
                                cwd=self.root, env=env, capture_output=True, timeout=60)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('WD 概率', result.stderr.decode(errors='replace'))
        self.assertFalse(reply.exists())
        self.assertFalse(Path(request['output']).exists())
        self.assertEqual(source.read_bytes(), before)

    @unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux native production gate awaits platform acceptance')
    def test_relocated_native_sync_first_incremental_bad_image_and_human_preservation(self):
        folder = self.root / 'images'; folder.mkdir()
        for i in range(2):
            shutil.copy2(self.images[i], folder / f'generated-{i}.png')
        package = self.root / 'fresh.pilib'
        made = subprocess.run([str(self.binary), 'library-create', '--dest', str(package), '--folder', str(folder),
                               '--name', 'synthetic'],
                              env=self.env, capture_output=True, timeout=30)
        self.assertEqual(made.returncode, 0, made.stderr.decode(errors='replace'))
        self.env['ILLUSTASH_RECOGNITION_WORKER'] = str(self.adapter)
        # Default ROOT/models/runtime must be found from the relocated installed exe;
        # --root on the host is its documented resource input, no worker overrides/Python.
        self.port = free_port(); self.origin = f'http://127.0.0.1:{self.port}'
        process, _ = self.start(package, extra=('--no-open',))
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        source, app = package / 'source.sqlite', package / 'library.sqlite'
        def sync():
            result = self.request('/api/sync', {})
            self.assertEqual(result['status'], 200, result['body'])
            deadline = time.monotonic()+120
            while time.monotonic() < deadline:
                state = json.loads(self.request('/api/sync')['body'])
                if state['state']['status'] not in ('scanning', 'importing', 'recognising'):
                    return state
                time.sleep(.05)
            self.fail('real model sync timed out')
        first = sync()
        self.assertEqual(first['state']['status'], 'done', first)
        with closing(sqlite3.connect(source)) as c:
            rows = c.execute('SELECT uuid,filename FROM assets ORDER BY rowid').fetchall()
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0], 2)
            for asset, name in rows:
                embeddings = dict(c.execute('SELECT model,vec FROM embeddings WHERE uuid=?', (asset,)))
                self.assertEqual(set(embeddings), {'wd', 'ccip'})
                for model, raw in embeddings.items():
                    np.testing.assert_array_equal(np.frombuffer(raw, '<f4'), self.reference[int(name.split('-')[1].split('.')[0])][model])
        asset = rows[0][0]
        tagged = self.request('/api/tag', dict(ids=[asset], tag='合成人工'))
        self.assertEqual(tagged['status'], 200, tagged['body'])
        before = snapshot(app)
        human = {table: before[table] for table in ('actions', 'assertions', 'asset_tags', 'term_verdicts')}
        self.assertTrue(human['term_verdicts'][1], 'manual verdict must actually exist')
        unchanged = sync()
        self.assertEqual(unchanged['state']['status'], 'done', unchanged)
        with closing(sqlite3.connect(source)) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_imports').fetchone()[0], 1)
            ids = [f'generated-character-{i}' for i in range(8)]
            c.executemany("INSERT INTO characters(char_id,anchor_p90,copyright,post_count,gender_conf,anchor_p50,anchor_p75) VALUES (?,.05,'synthetic',?,1,.01,.025)", [(v, 100+i) for i, v in enumerate(ids)])
            c.commit()
        vectors = np.asarray([r['ccip'] for r in self.reference], np.float32)
        np.save(self.shared / 'char-anchors.npy', vectors)
        (self.shared / 'char-anchors.txt').write_text('\n'.join(ids)+'\n', encoding='utf-8')
        (self.shared / 'char-index').mkdir()
        (self.shared / 'char-index' / (ids[0]+'.json')).write_text('{"source":"danbooru-patch"}', encoding='utf-8')
        _, p90, _, p75, aliases = char_scan.anchor_index(vectors, ids, str(source))
        with patch.object(char_scan, 'REVIEW', str(self.shared)):
            limits = char_scan.claim_thresholds(ids, p90, p75)
        distances = cross(vectors, np.asarray([self.reference[2]['ccip']], np.float32), check=False)[:, 0]
        expected_claims, seen = [], set()
        for j in np.argsort(distances)[:9]:
            if aliases[j] in seen:
                continue
            seen.add(aliases[j])
            if limits[j] > 0 and distances[j] <= limits[j]:
                expected_claims.append((aliases[j], 1-float(distances[j])))
        self.assertTrue(expected_claims, 'generated anchors must exercise formal character publication')
        shutil.copy2(self.images[2], folder / 'generated-2.png')
        (folder / 'bad.png').write_bytes(b'not an image')
        incremental = sync()
        self.assertEqual(incremental['state']['status'], 'done', incremental)
        self.assertEqual(incremental['state']['failed'], 1)
        with closing(sqlite3.connect(source)) as c:
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0], 3)
            self.assertEqual(c.execute('SELECT count(*) FROM recognition_imports').fetchone()[0], 2)
            actual = c.execute("SELECT l.value,l.conf FROM labels l JOIN assets a ON a.uuid=l.uuid WHERE a.filename='generated-2.png' AND l.source='char_index' AND l.dim='char_ext'").fetchall()
            self.assertEqual(sorted(actual), sorted(expected_claims))
            self.assertEqual(c.execute("SELECT count(*) FROM embeddings e JOIN assets a ON a.uuid=e.uuid WHERE a.filename='bad.png'").fetchone()[0], 0)
        with closing(sqlite3.connect(app)) as c:
            count = c.execute("SELECT count(*) FROM term_current_evidence WHERE source='ccip_index' AND method='char_ext' AND metric='ccip_score'").fetchone()[0]
            self.assertEqual(count, len(expected_claims))
        after = snapshot(app)
        for table, expected in human.items():
            self.assertEqual(after[table], expected, table)
        self.stop(process)
        self.assertEqual(process.returncode, 0)
