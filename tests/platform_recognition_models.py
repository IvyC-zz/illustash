"""显式、需主动选择运行的模型验收；只用生成的图片/锚点。

除 AcceleratedRecognitionTest 外，每个用例都以 ILLUSTASH_CPU_ONLY=1 运行，与 Python ONNX Runtime 的 CPU
参考结果逐位比较。AcceleratedRecognitionTest 在本平台已有的加速器上运行（Apple 芯片上 WD 与 CCIP 用 CoreML，#270；
Windows x64 上 WD 和 CCIP 用 DirectML，#271），在 #213 测得的界限内比较。
不随 discover 运行，单独运行。必需的输入：ILLUSTASH_TERMS_BIN、
ILLUSTASH_RECOGNITION_ADAPTER_BIN、ILLUSTASH_RECOGNITION_COMPUTE_BIN、
ILLUSTASH_RECOGNITION_MODELS 和 ILLUSTASH_ORT_DYLIB。缺少输入就失败。
下面重新安置的资源是只够识别用的目录布局夹具，不是签名的完整安装包。Windows 还要求显式给出与参考
NumPy wheel 匹配的 ILLUSTASH_RESEARCH_OPENBLAS_DLL；Linux 已冻结。
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

import numpy as np
import onnxruntime as ort
from PIL import Image
from tests.native_claims_fixture import claims as native_claims

from tests import test_native_lifecycle as lifecycle
from tests.test_native_human_labels import snapshot
from tests.test_read_http import free_port
from tools.wait_limit import WAIT

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
        variant = json.loads((ROOT / 'native/terms/models.json').read_text())['wd_coreml']['file']['path']
        if sys.platform == 'darwin' and (models / variant).is_file():
            copies.append((models / variant, cls.resources / 'models' / variant))
        # Windows 上的运行库从它自己的文件夹加载 DirectML.dll（#271），与安装后一致。
        copies += [(runtime.parent / name, cls.resources / 'onnxruntime' / name)
                   for name in ('DirectML.dll',) if sys.platform == 'win32' and (runtime.parent / name).is_file()]
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
        options = ort.SessionOptions(); options.log_severity_level = 3
        if sys.platform != 'win32':  # WD 线程数与 session::WD_THREADS 相同：Windows 上用 ORT 默认值，其他平台用 4
            options.intra_op_num_threads = 4
        wd = ort.InferenceSession(str(models / 'wd_swinv2_v3_emb.onnx'), options, providers=['CPUExecutionProvider'])
        options = ort.SessionOptions(); options.log_severity_level = 3
        ccip = ort.InferenceSession(str(models / 'ccip/model_feat.onnx'), options, providers=['CPUExecutionProvider'])
        import gzip, io
        frozen = ROOT / 'tests/fixtures/model_inputs'
        for i, path in enumerate(cls.images):
            wd_input = np.load(io.BytesIO(gzip.decompress((frozen/f'wd-{i}.npy.gz').read_bytes())),allow_pickle=False)
            ccip_input = np.load(io.BytesIO(gzip.decompress((frozen/f'ccip-{i}.npy.gz').read_bytes())),allow_pickle=False)
            probabilities, embedding = wd.run(None, {wd.get_inputs()[0].name: wd_input})
            feature = ccip.run(None, {ccip.get_inputs()[0].name: ccip_input})[0]
            cls.reference.append(dict(probs=probabilities.reshape(-1), wd=embedding.reshape(-1), ccip=feature.reshape(-1)))
        # 验收运行期间不同时开着 Python 和原生的模型会话。
        del wd, ccip
        cls.runtime = cls.resources / 'onnxruntime' / runtime_name

    def setUp(self):
        temp = tempfile.TemporaryDirectory(dir=self.layout, prefix='case-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.shared = self.root / 'shared'; self.shared.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith('ILLUSTASH_')}
        self.env.update(PATH='', HOME=str(self.root / 'home'), USERPROFILE=str(self.root / 'home'),
                        ILLUSTASH_APP_SUPPORT=str(self.root / 'support'), ILLUSTASH_SHARED=str(self.shared),
                        ILLUSTASH_CPU_ONLY='1')
        if sys.platform == 'win32':
            # 安装版的 Windows 启动器提供 ROOT；后端只解析 ROOT/openblas/openblas.dll，
            # 不用 PATH，也不接受 Python/DLL 覆盖。
            self.env['ILLUSTASH_ROOT'] = str(self.resources)

    def assert_ccip(self, actual, expected):
        """CCIP 向量及由它算出的距离和置信度：在 CPU 上逐位一致。"""
        np.testing.assert_array_equal(actual, expected)

    def assert_wd(self, actual, expected):
        """WD 概率：在 CPU 上逐位一致。"""
        np.testing.assert_array_equal(actual, expected)

    def assert_wd_vector(self, actual, expected):
        """WD 向量：在 CPU 上逐位一致。"""
        np.testing.assert_array_equal(actual, expected)

    def assert_labels(self, actual, expected):
        """存储的标签（概率 >= 0.001，四位小数）：在 CPU 上完全一致。"""
        self.assertEqual(actual, expected)

    def invoke(self, binary, request, *, env=None):
        input_path = self.root / 'request.json'
        output_path = self.root / 'response.json'
        input_path.write_text(json.dumps(request), encoding='utf-8')
        output_path.unlink(missing_ok=True)
        result = subprocess.run([str(binary), str(input_path), str(output_path)],
                                env=env or self.env, cwd=self.root, capture_output=True, timeout=WAIT * 2)
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
        for a in assets: a['input_sha256'] = hashlib.sha256(Path(a['path']).read_bytes()).hexdigest()
        output = self.root / 'output'; output.mkdir()
        # 生产方还在运行时就能看到进度：一直采样到进程退出。
        progress = self.root / 'progress.json'
        request = dict(mode='produce', resources=str(self.resources), models=str(self.resources / 'models'),
                       runtime=str(self.runtime), shared=str(self.shared),
                       request=dict(format='illustash-sync/2', identity=identity, assets=assets, output=str(output)))
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
            self.assertEqual(process.wait(timeout=WAIT), 0, process.stderr.read().decode(errors='replace'))
        response = json.loads((self.root / 'response.json').read_text(encoding='utf-8'))
        self.assertEqual({v['stage'] for v in seen} & {'preparing', 'recognising'}, {'preparing', 'recognising'})
        self.assertTrue(any(0 < v['processed'] < 26 for v in seen), seen)
        self.assertTrue(all(v['task_id'] == 'synthetic' and v['total'] == 26 for v in seen))
        self.assertEqual([v['processed'] for v in seen], sorted(v['processed'] for v in seen))
        final = json.loads(progress.read_text(encoding='utf-8'))
        self.assertEqual((final['stage'], final['processed'], final['failed']), ('recognising', 26, 1))
        self.assertEqual(response['identity'], identity)
        self.assertEqual(response['failed'], [{'uuid': 'asset-24', 'reason': 'undecodable'}])
        self.assertEqual(len(response['packages']), 2)
        self.assertEqual(len(response['character_claims']), 2)
        for batch, sidecar, size in zip(response['packages'], response['character_claims'], (24, 1)):
            path = Path(batch); manifest = json.loads((path / 'manifest.json').read_text(encoding='utf-8'))
            self.assertEqual(manifest['assets'], [f'asset-{i}' for i in (range(24) if size == 24 else (25,))])
            claim = json.loads(Path(sidecar).read_text(encoding='utf-8'))
            # 最后写入的完成标记，精确锁定已完成的 manifest 和 sidecar。
            complete = json.loads((path / 'complete.json').read_text(encoding='utf-8'))
            self.assertEqual(complete, dict(format='illustash-recognition-batch/1', task_id='synthetic',
                                            assets=manifest['assets'],
                                            manifest_sha256=hashlib.sha256((path / 'manifest.json').read_bytes()).hexdigest(),
                                            sidecar_sha256=hashlib.sha256(Path(sidecar).read_bytes()).hexdigest()))
            self.assertEqual(claim['identity'], identity)
            self.assertEqual(claim['assets'], manifest['assets'])
            self.assertEqual(claim['models'], manifest['models'])
            # 执行变体不能进入证据包身份；旧 CPU 结果和加速结果必须属于同一原始模型。
            configured = json.loads((ROOT / 'native/terms/models.json').read_text(encoding='utf-8'))
            originals = {entry['path']: entry['sha256'] for entry in configured['files']}
            self.assertEqual(manifest['models']['wd']['sha256'], originals['wd_swinv2_v3_emb.onnx'])
            self.assertEqual(manifest['models']['ccip']['sha256'], originals['ccip/model_feat.onnx'])
            expected_labels = []
            reference_features = np.asarray([self.reference[int(asset.split('-')[1]) % 8]['ccip']
                                             for asset in manifest['assets']], np.float32)
            fixture = native_claims(self.compute, self.root, self.shared, source, identity,
                                    manifest['assets'], reference_features, self.env)
            expected_claims = fixture['claims']
            for asset in manifest['assets']:
                row = self.reference[int(asset.split('-')[1]) % 8]
                expected_labels.extend(labels(asset, row['probs'], self.tags))
            self.assert_labels([json.loads(line) for line in (path / 'labels.jsonl').read_text(encoding='utf-8').splitlines()], expected_labels)
            self.assert_ccip(np.asarray([r['distance'] for r in claim['claims']], np.float32),
                             np.asarray([r['distance'] for r in expected_claims], np.float32))
            np.testing.assert_array_equal(np.asarray([r['threshold'] for r in claim['claims']], np.float32),
                                          np.asarray([r['threshold'] for r in expected_claims], np.float32))
            self.assertEqual([(r['asset'], r['character']) for r in claim['claims']],
                             [(r['asset'], r['character']) for r in expected_claims])
            # 置信度是 1 - 距离，所以随距离而定。
            self.assert_ccip(np.asarray([r['confidence'] for r in claim['claims']]),
                             np.asarray([r['confidence'] for r in expected_claims]))
            for model in ('wd', 'ccip'):
                expected = np.asarray([self.reference[int(a.split('-')[1]) % 8][model] for a in manifest['assets']], np.float32)
                (self.assert_ccip if model == 'ccip' else self.assert_wd_vector)(np.load(path / (model+'.npy')), expected)
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
        request = dict(format='illustash-sync/2', identity=identity, output=str(output),
                       assets=[dict(id='good', path=str(self.images[0])), dict(id='bad', path=str(bad))])
        for a in request['assets']: a['input_sha256'] = hashlib.sha256(Path(a['path']).read_bytes()).hexdigest()
        response = self.invoke(self.adapter, request)
        self.assertEqual(response['failed'], [{'uuid': 'bad', 'reason': 'undecodable'}])
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
                                        cwd=self.root, env=env, capture_output=True, timeout=WAIT)
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
                                            cwd=self.root, env=env, capture_output=True, timeout=WAIT)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('OpenBLAS', result.stderr.decode(errors='replace'))
                    self.assertNotIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode(errors='replace'))
                    self.assertFalse(reply.exists())
                    self.assertEqual(source.read_bytes(), before)
        # 模型合法但 tag 表不兼容时，必须在发布任何批次之前失败。
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
                                cwd=self.root, env=env, capture_output=True, timeout=WAIT)
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
                              env=self.env, capture_output=True, timeout=WAIT)
        self.assertEqual(made.returncode, 0, made.stderr.decode(errors='replace'))
        self.env['ILLUSTASH_RECOGNITION_WORKER'] = str(self.adapter)
        # 默认的 ROOT/models/runtime 必须从重新安置的安装版 exe 找到；
        # 宿主的 --root 是文档里写明的资源输入，没有 worker 覆盖，也没有 Python。
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
        fixture_identity = dict(package=str(source.parent), library_id='synthetic',
                                generation=1, session='0'*64, task_id='synthetic-claims')
        fixture = native_claims(self.compute, self.root, self.shared, source, fixture_identity,
                                ['synthetic-asset'], np.asarray([self.reference[2]['ccip']], np.float32), self.env)
        expected_claims = [(row['character'], row['confidence']) for row in fixture['claims']]
        self.assertTrue(expected_claims, 'generated anchors must exercise formal character publication')
        shutil.copy2(self.images[2], folder / 'generated-2.png')
        (folder / 'bad.png').write_bytes(b'not an image')
        incremental = sync()
        self.assertEqual(incremental['state']['status'], 'done', incremental)
        # #112：完整读到了但不是图片，所以算作损坏并跳过，直到它改变；不重试。
        self.assertEqual((incremental['state']['failed'], incremental['state']['damaged']), (0, 1), incremental)
        self.assertEqual([Path(p).name for p in incremental['damaged']['paths']], ['bad.png'])
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
        again = sync()
        self.assertEqual((again['state']['status'], again['state']['failed'], again['state']['damaged']), ('done', 0, 1), again)
        # 换成一张缺了后半截的图片：仍然算损坏，还是同一个资源。
        whole = self.images[3].read_bytes()
        (folder / 'bad.png').write_bytes(whole[:len(whole) // 2])
        truncated = sync()
        self.assertEqual((truncated['state']['failed'], truncated['state']['damaged']), (0, 1), truncated)
        # 修好了：下次同步时识别，还是同一个资源。
        shutil.copy2(self.images[3], folder / 'bad.png')
        fixed = sync()
        self.assertEqual((fixed['state']['failed'], fixed['state'].get('damaged')), (0, None), fixed)
        self.assertNotIn('damaged', fixed)
        with closing(sqlite3.connect(source)) as c:
            self.assertEqual(c.execute("SELECT count(*) FROM recognition_assets r JOIN assets a ON a.uuid=r.asset WHERE a.filename='bad.png'").fetchone()[0], 1)
            self.assertEqual(c.execute("SELECT count(*) FROM assets WHERE filename='bad.png'").fetchone()[0], 1)
        self.stop(process)
        self.assertEqual(process.returncode, 0)


def accelerator():
    """本平台可用的加速后端（recognition_compute/backend.rs），没有则为 None。"""
    import platform
    if sys.platform == 'darwin' and platform.machine() == 'arm64':
        return 'coreml'
    if sys.platform == 'win32' and platform.machine() == 'AMD64':
        return 'directml'
    return None


# 加速器运行哪些模型；其余的留在 CPU 上，逐位一致。
ACCELERATED = {'coreml': ('wd', 'ccip'), 'directml': ('wd', 'ccip')}.get(accelerator(), ())


@unittest.skipUnless(accelerator(), 'no accelerated recognition backend on this platform')
class AcceleratedRecognitionTest(RecognitionModelTest):
    """设置的默认值：ACCELERATED 中的模型跑在加速器上。#213 测得的只有 fp32 舍入差异（CCIP 余弦相似度
    CoreML 上 >= 0.99999999999958，DirectML 上 >= 0.9999999999995）；这里的界限留有余地，每个判定都必须不变。"""
    COSINE = 0.9999999999

    def setUp(self):
        super().setUp()
        del self.env['ILLUSTASH_CPU_ONLY']

    def status(self):
        return json.loads((self.shared / 'acceleration.json').read_text(encoding='utf-8'))

    def infer(self):
        return self.invoke(self.compute, dict(mode='infer', models=str(self.resources / 'models'),
                           runtime=str(self.runtime), tags=str(self.resources / 'tags/tags_v3.csv'),
                           shared=str(self.shared), images=list(map(str, self.images))))

    def test_raw_probabilities_vectors_and_labels(self):
        got = self.infer()
        for model in ACCELERATED:
            status = self.status()[model]
            self.assertEqual(status, dict(status, requested=accelerator(), used=accelerator(), reason=None), model)
        if accelerator() == 'coreml':
            root = self.shared / 'coreml-cache'
            self.assertEqual({p.name for p in root.iterdir() if p.is_dir()}, set(ACCELERATED))
            for model in ACCELERATED:
                cache = [p for p in (root / model).iterdir() if p.is_dir()]
                self.assertEqual(len(cache), 1, '每个模型独立保留当前内容的缓存')
                self.assertTrue(any(cache[0].rglob('*')), '编译模型已缓存')
        reference = np.asarray([r['ccip'] for r in self.reference], np.float64)
        for i, (expected, actual) in enumerate(zip(self.reference, got)):
            # 两种加速后端都以原始 CPU 输出为参考，WD 概率在 WD_ATOL 以内。
            self.assert_wd(np.asarray(actual['probs'], np.float32), expected['probs'])
            self.assert_wd_vector(np.asarray(actual['wd'], np.float32), expected['wd'])
            self.assert_labels(actual['labels'], labels('synthetic', expected['probs'], self.tags))
            feature = np.asarray(actual['ccip'], np.float64)
            cosine = feature @ reference[i] / np.linalg.norm(feature) / np.linalg.norm(reference[i])
            self.assertGreaterEqual(cosine, self.COSINE, i)
            # 其余图片里最近的那张与 CPU 上相同。
            others = [j for j in range(len(reference)) if j != i]
            nearest = lambda v: max(others, key=lambda j: v @ reference[j] / np.linalg.norm(v) / np.linalg.norm(reference[j]))
            self.assertEqual(nearest(feature), nearest(reference[i]), i)
        # 再跑一次（CoreML 上用编译缓存）：输出相同，逐位一致。
        cache_state = lambda: {str(p.relative_to(self.shared)): (p.stat().st_ino, p.stat().st_mtime_ns, p.stat().st_size)
                               for p in (self.shared / 'coreml-cache').rglob('*') if p.is_file() and p.name != 'compile.lock'}
        before = cache_state() if accelerator() == 'coreml' else None
        again = self.infer()
        if before is not None:
            self.assertEqual(cache_state(), before, 'WD 与 CCIP 均复用已有编译文件')
        self.assertEqual([r['ccip'] for r in again], [r['ccip'] for r in got])
        self.assertEqual([r['probs'] for r in again], [r['probs'] for r in got])

    def assert_ccip(self, actual, expected):
        actual, expected = np.asarray(actual, np.float64), np.asarray(expected, np.float64)
        self.assertEqual(actual.shape, expected.shape)
        if actual.ndim == 2:
            # 原始向量（未归一化，值最大约为 5）：按方向比较，与距离计算的用法一致。
            cosine = (actual * expected).sum(1) / np.linalg.norm(actual, axis=1) / np.linalg.norm(expected, axis=1)
            self.assertGreaterEqual(cosine.min(), self.COSINE)
        else:
            # 起决定作用的是 0.01–0.15 的距离，#213 观察到它们最多变动 1.3e-7。
            np.testing.assert_allclose(actual, expected, rtol=0, atol=1e-6)

    # DirectML（#213，92 张公开图片）：WD 概率与 CPU 的相差最多 4.2e-6，平均 2.7e-8。
    WD_ATOL = 2e-5

    def assert_wd(self, actual, expected):
        if 'wd' not in ACCELERATED:
            return super().assert_wd(actual, expected)
        np.testing.assert_allclose(actual, expected, rtol=0, atol=self.WD_ATOL)

    def assert_wd_vector(self, actual, expected):
        if 'wd' not in ACCELERATED:
            return super().assert_wd_vector(actual, expected)
        actual, expected = np.atleast_2d(np.asarray(actual, np.float64)), np.atleast_2d(np.asarray(expected, np.float64))
        cosine = (actual * expected).sum(1) / np.linalg.norm(actual, axis=1) / np.linalg.norm(expected, axis=1)
        self.assertGreaterEqual(cosine.min(), self.COSINE)

    def assert_labels(self, actual, expected):
        """Core ML / DirectML 上，距 0.001 截断线在 WD_ATOL 以内的标签可能出现或消失，存储的置信度第四位小数可能差 1；
        其他都不变，处在判定阈值上的 tag 保持不变。"""
        if 'wd' not in ACCELERATED:
            return super().assert_labels(actual, expected)
        key = lambda r: (r['asset'], r['dim'], r['value'])
        got, want = {key(r): r['confidence'] for r in actual}, {key(r): r['confidence'] for r in expected}
        for name in got.keys() ^ want.keys():
            self.assertLess(abs(got.get(name, want.get(name)) - .001), self.WD_ATOL + 5e-5, name)
        for name in got.keys() & want.keys():
            self.assertLessEqual(abs(got[name] - want[name]), 1e-4 + 1e-9, name)
        for cut in (.2, .35, .5, .85):
            self.assertEqual({k for k, v in got.items() if v >= cut}, {k for k, v in want.items() if v >= cut}, cut)

    # 继承的批次测试以原始模型身份导入；WD 输出与保存标签按数值边界比较，
    # CCIP 向量、距离和置信度经 assert_ccip 比较，认领的角色完全一致。

    def test_installed_no_anchor_bad_image_and_explicit_dependency_failures(self):
        self.skipTest('the CPU case covers dependency failures; nothing accelerated is involved')

    def test_relocated_native_sync_first_incremental_bad_image_and_human_preservation(self):
        self.skipTest('the CPU case covers sync; its stored vectors are compared bit for bit')

    @unittest.skipUnless(accelerator() == 'coreml', 'only CoreML keeps a compiled cache')
    def test_damaged_cache_is_dropped_and_the_cpu_takes_over(self):
        self.infer()
        cache = next(p for p in (self.shared / 'coreml-cache' / 'ccip').iterdir() if p.is_dir())
        for path in sorted(cache.rglob('*'), reverse=True):
            if path.is_file():
                path.write_bytes(b'damaged')
        got = self.infer()
        status = self.status()['ccip']
        if status['used'] == 'cpu':
            # 损坏的副本加载不了：这个 worker 改用 CPU，副本已删除。
            self.assertTrue(status['reason'])
            self.assertFalse(cache.exists())
            for expected, actual in zip(self.reference, got):
                np.testing.assert_array_equal(np.asarray(actual['ccip'], np.float32), expected['ccip'])
        else:
            # 或者 CoreML 发现了问题并重新编译；不论哪种，输出都是可靠的。
            self.assertEqual(status['reason'], None)
        self.infer()
        self.assertEqual(self.status()['ccip']['used'], accelerator(), 'the next worker compiles again')


    @unittest.skipUnless(accelerator() == 'coreml', 'WD Core ML execution variant')
    def test_missing_or_corrupt_wd_variant_falls_back_to_original_cpu(self):
        entry = json.loads((ROOT / 'native/terms/models.json').read_text())['wd_coreml']['file']
        variant = self.resources / 'models' / entry['path']
        saved = variant.with_suffix('.saved')
        variant.rename(saved)
        try:
            for corrupt in (False, True):
                if corrupt:
                    shutil.copy2(saved, variant)
                    with variant.open('r+b') as stream:
                        first = stream.read(1)
                        stream.seek(0)
                        stream.write(bytes([first[0] ^ 1]))
                got = self.infer()
                self.assertEqual(self.status()['wd']['used'], 'cpu')
                self.assertIn('加速文件', self.status()['wd']['reason'])
                self.assertEqual(self.status()['ccip']['used'], 'coreml')
                for expected, actual in zip(self.reference, got):
                    np.testing.assert_array_equal(np.asarray(actual['probs'], np.float32), expected['probs'])
                    np.testing.assert_array_equal(np.asarray(actual['wd'], np.float32), expected['wd'])
        finally:
            variant.unlink(missing_ok=True)
            saved.rename(variant)
