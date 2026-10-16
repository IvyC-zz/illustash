"""Resource staging only: marker binaries/models, no install, inference or compiler."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'packaging' / f'{name}.py')
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


pack = module('build')


def desktop_required(name):
    """The desktop host's required-component list (desktop/src/bundled_workers.rs)."""
    source = (ROOT / 'desktop/src/bundled_workers.rs').read_text(encoding='utf-8')
    body = re.search(rf'const {name}: \[&str; \d+\] = \[(.*?)\];', source, re.S).group(1)
    return re.findall(r'"([^"]+)"', body)


def assert_desktop_contract(test, manifest, platform, name):
    """The host accepts only format 1 / research / its platform and manifested components."""
    test.assertEqual((manifest['format'], manifest['variant'], manifest['platform']), (1, 'research', platform))
    required = desktop_required(name)
    test.assertTrue(required)
    test.assertEqual([path for path in required if path not in manifest['files']], [])
    assert_single_worker_and_notices(test, manifest, '.exe' if platform == 'win32' else '')


def assert_single_worker_and_notices(test, manifest, suffix, research=True):
    """#37: one worker program, no duplicate executables; #8: project and third-party notices ship."""
    files = manifest['files']
    binaries = {name: digest for name, digest in files.items() if name.startswith('bin/')}
    test.assertEqual(len(set(binaries.values())), len(binaries), binaries)
    workers = [name for name in binaries if 'worker' in name or any(
        name == f'bin/illustash-{kind}{suffix}' for kind in pack.WORKER_KINDS)]
    test.assertEqual(workers, [f'bin/illustash-worker{suffix}'] if research else [])
    for installed, source in pack.NOTICES.items():
        test.assertEqual(files['resources/' + installed], hashlib.sha256((ROOT / source).read_bytes()).hexdigest())


class PackagingResourcesTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-package-markers-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.binaries = self.root / 'inputs/bin'
        self.binaries.mkdir(parents=True)
        suffix = '.exe' if os.name == 'nt' else ''
        for name in ('illustash-terms', 'illustash-worker', 'illustash-iqdb', 'illustash-desktop'):
            (self.binaries / (name + suffix)).write_bytes(('marker:' + name).encode())
        self.iqdb = self.binaries / ('illustash-iqdb' + suffix)
        self.models = self.root / 'inputs/models'
        self.models.mkdir()
        for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx',
                     'ccip/model_metrics.onnx', 'ccip/metrics.json'):
            target = self.models / name
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(('synthetic model marker:' + name).encode())
        (self.models / 'unlisted-private-marker').write_bytes(b'must not ship')
        (self.binaries / 'unlisted-private-marker').write_bytes(b'must not ship')

    def manifest(self, output):
        manifest = json.loads((output / 'manifest.json').read_text())
        files = {p.relative_to(output).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in output.rglob('*') if p.is_file() and p.name != 'manifest.json'}
        # The manifest must cover exactly all delivered files, not just selected names.
        self.assertEqual(manifest['files'], files)
        self.assertTrue(all(not Path(name).is_absolute() and '..' not in Path(name).parts for name in files))
        self.assertFalse(any('unlisted-private-marker' in name for name in files))
        return manifest

    def test_base_full_difference_aliases_and_model_allowlist(self):
        base, full = self.root / 'base', self.root / 'full'
        pack.stage(base, self.binaries)
        pack.stage(full, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        plain, complete = self.manifest(base), self.manifest(full)
        self.assertEqual((plain['variant'], complete['variant']), ('base', 'research'))
        self.assertTrue(plain['files'].keys() <= complete['files'].keys())
        for name, digest in plain['files'].items():
            if name in ('start.sh', 'start.cmd'):
                self.assertNotEqual(complete['files'][name], digest)
            else:
                self.assertEqual(complete['files'][name], digest)
        delta = complete['files'].keys() - plain['files'].keys()
        self.assertEqual({name for name in delta if name.startswith('resources/')}, {
            'resources/models/wd_swinv2_v3_emb.onnx', 'resources/models/ccip/model_feat.onnx',
            'resources/models/ccip/model_metrics.onnx', 'resources/models/ccip/metrics.json',
            *(installed for installed, _ in pack.model_licenses())})
        suffix = '.exe' if os.name == 'nt' else ''
        self.assertEqual({name for name in delta if name.startswith('bin/')}, {
            f'bin/illustash-{name}{suffix}' for name in ('worker', 'iqdb')})
        worker = hashlib.sha256((self.binaries / ('illustash-worker' + suffix)).read_bytes()).hexdigest()
        self.assertEqual(complete['files'][f'bin/illustash-worker{suffix}'], worker)
        assert_single_worker_and_notices(self, plain, suffix, research=False)
        assert_single_worker_and_notices(self, complete, suffix)
        self.assertFalse((base / 'resources/licenses/models').exists())
        # Every installed request is native: no Python calculators ship.
        self.assertFalse(any(name.startswith(('workers/', 'runtime/')) for name in delta))
        self.assertFalse((base / 'workers').exists())
        self.assertFalse((base / 'runtime').exists())
        self.assertFalse((base / 'resources/models').exists())

    def test_static_notices_preserve_fixed_dependency_bytes(self):
        for relative, digest in {
            'native/terms/vendor/pocketfft/LICENSE.md': 'a85ca13fdf90160b64a0698215868c13b74d835ad0a4e2ba44713b8c5058a056',
            'native/terms/licenses/LCMS.txt': '9ed584604d11ed0ab26211e1775687283008d35e164baa8294757fbd4d829f90',
        }.items():
            self.assertEqual(hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(), digest)

    def test_relocation_hashes_zip_paths_and_exclusive_output(self):
        original = self.root / 'artifact with spaces'
        pack.stage(original, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        before = self.manifest(original)
        relocated = self.root / '新位置' / original.name
        relocated.parent.mkdir()
        original.rename(relocated)
        self.assertEqual(self.manifest(relocated), before)
        first, second = self.root / 'one.zip', self.root / 'two.zip'
        pack.archive(relocated, first)
        pack.archive(relocated, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            self.assertEqual(set(archive.namelist()), {
                f'{relocated.name}/{name}' for name in before['files']} | {f'{relocated.name}/manifest.json'})
            for item in archive.infolist():
                self.assertEqual(item.date_time, (1980, 1, 1, 0, 0, 0))
        with self.assertRaisesRegex(ValueError, 'already exists'):
            pack.stage(relocated, self.binaries)
        with self.assertRaisesRegex(ValueError, 'already exists'):
            pack.archive(relocated, first)
        self.assertEqual(self.manifest(relocated), before)

    def test_missing_model_is_reported_and_symlink_resource_rejected(self):
        (self.models / 'ccip/model_feat.onnx').unlink()
        with self.assertRaisesRegex(FileNotFoundError, 'model_feat.onnx'):
            pack.stage(self.root / 'incomplete', self.binaries, research=True,
                       iqdb=self.iqdb, models=self.models)
        self.assertFalse((self.root / 'incomplete/manifest.json').exists())
        if os.name != 'nt':
            source = self.root / 'link'
            source.symlink_to(self.iqdb)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                pack.copy(source, self.root / 'copied')
            self.assertFalse((self.root / 'copied').exists())

    def test_explicit_cpu_runtime_and_notices_are_manifested(self):
        runtime = self.root / 'existing-cpu-runtime'
        runtime.write_bytes(b'synthetic CPU runtime marker')
        notices = self.root / 'notices'
        notices.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (notices / name).write_bytes(('synthetic notice:' + name).encode())
        (notices / 'unlisted-private-marker').write_bytes(b'not allowed')
        # Cross-platform source staging only, not an installed macOS runtime check.
        with patch.object(pack.sys, 'platform', 'darwin'):
            if os.name == 'nt':
                for name in ('illustash-terms', 'illustash-worker'):
                    (self.binaries / name).write_bytes(b'synthetic marker:' + name.encode())
            output = self.root / 'explicit-native-resources'
            with patch.object(pack.subprocess, 'run') as compiler:
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb,
                           ort_dylib=runtime, ort_licenses=notices)
                compiler.assert_not_called()
            manifest = self.manifest(output)
            for name, source in {
                'resources/onnxruntime/libonnxruntime.dylib': runtime,
                'resources/licenses/onnxruntime/LICENSE': notices / 'LICENSE',
                'resources/licenses/onnxruntime/ThirdPartyNotices.txt': notices / 'ThirdPartyNotices.txt',
                'resources/licenses/pocketfft/LICENSE.md': ROOT / 'native/terms/vendor/pocketfft/LICENSE.md',
                'resources/licenses/LCMS.txt': ROOT / 'native/terms/licenses/LCMS.txt',
            }.items():
                self.assertEqual(manifest['files'][name], hashlib.sha256(source.read_bytes()).hexdigest())
        for values in ({'ort_dylib': runtime}, {'ort_licenses': notices}):
            with self.assertRaisesRegex(ValueError, 'ORT runtime'):
                pack.stage(self.root / 'unpaired', self.binaries, research=True, **values)
            self.assertFalse((self.root / 'unpaired').exists())
        with self.assertRaisesRegex(ValueError, '--research'):
            pack.stage(self.root / 'wrong-variant', self.binaries, ort_dylib=runtime, ort_licenses=notices)
        self.assertFalse((self.root / 'wrong-variant').exists())
        (notices / 'ThirdPartyNotices.txt').unlink()
        with self.assertRaisesRegex(FileNotFoundError, 'ThirdPartyNotices'):
            pack.stage(self.root / 'incomplete-notices', self.binaries, research=True,
                       iqdb=self.iqdb, ort_dylib=runtime, ort_licenses=notices)
        self.assertFalse((self.root / 'incomplete-notices/manifest.json').exists())

    def test_model_licenses_ship_verbatim_with_models(self):
        """#8: packages with models carry the committed license originals; nothing else is a source."""
        registry = json.loads((pack.MODEL_LICENSES / 'sources.json').read_text())['files']
        self.assertEqual(sorted(registry), ['ccip/BigScience-OpenRAIL-M.pdf', 'ccip/deepghs-ccip-README.md',
                                            'ccip/deepghs-ccip_onnx-README.md', 'wd-swinv2-tagger-v3/LICENSE-2.0.txt',
                                            'wd-swinv2-tagger-v3/SmilingWolf-wd-swinv2-tagger-v3-README.md',
                                            'wd-swinv2-tagger-v3/deepghs-wd14_tagger_with_embeddings-README.md'])
        notice = (pack.MODEL_LICENSES / 'NOTICE.txt').read_text()
        # The notice names every bundled model file and the license text that covers it.
        for name in ('wd_swinv2_v3_emb.onnx', 'model_feat.onnx', 'model_metrics.onnx', 'metrics.json',
                     'wd-swinv2-tagger-v3/LICENSE-2.0.txt', 'ccip/BigScience-OpenRAIL-M.pdf'):
            self.assertIn(name, notice)
        output = self.root / 'with models'
        pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        manifest = self.manifest(output)
        shipped = sorted(name for name in manifest['files'] if name.startswith('resources/licenses/models/'))
        self.assertEqual(shipped, sorted(f'resources/licenses/models/{name}'
                                         for name in ('NOTICE.txt', 'sources.json', *registry)))
        for name in shipped:
            source = pack.MODEL_LICENSES / name.removeprefix('resources/licenses/models/')
            self.assertEqual((output / name).read_bytes(), source.read_bytes())
        without = self.root / 'without models'
        pack.stage(without, self.binaries, research=True, iqdb=self.iqdb)
        self.assertFalse((without / 'resources/licenses/models').exists())

    def test_packages_with_models_refuse_incomplete_model_licenses(self):
        def broken(name, change):
            source = self.root / name
            shutil.copytree(pack.MODEL_LICENSES, source)
            change(source)
            return source
        cases = {
            'missing': (lambda s: (s / 'ccip/BigScience-OpenRAIL-M.pdf').unlink(), 'missing: ccip/BigScience'),
            'altered': (lambda s: (s / 'wd-swinv2-tagger-v3/LICENSE-2.0.txt').write_bytes(b'edited'), 'differs'),
            'no notice': (lambda s: (s / 'NOTICE.txt').unlink(), "missing \\['NOTICE.txt'\\]"),
            'unregistered': (lambda s: (s / 'ccip/extra.txt').write_bytes(b'x'), 'unregistered'),
            'no registry': (lambda s: (s / 'sources.json').unlink(), 'registry missing'),
        }
        for name, (change, message) in cases.items():
            with self.subTest(name), patch.object(pack, 'MODEL_LICENSES', broken(name, change)):
                output = self.root / f'out {name}'
                with self.assertRaisesRegex(ValueError, message):
                    pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
                self.assertFalse(output.exists())

    def test_full_installers_ship_resources_once(self):
        """#37: full installers use compute/resources only; the base installer keeps resources/."""
        def mapping(name):
            return json.loads((ROOT / 'packaging' / name).read_text())['bundle']['resources']
        self.assertEqual(mapping('tauri-bundle.json'), {'../packaging/desktop-resources/': 'resources/'})
        for name in ('tauri-full.json', 'tauri-full-macos.json'):
            # The host roots full installs at compute/resources (bundled_workers tests), so a
            # second top-level resources/ copy would have no consumer.
            self.assertEqual(mapping(name), {'../packaging/desktop-compute/': 'compute/'}, name)
        # Upgrading an older full install must not leave the per-kind copies or resources/ behind.
        config = json.loads((ROOT / 'packaging/tauri-full.json').read_text())
        hooks = (ROOT / 'desktop' / config['bundle']['windows']['nsis']['installerHooks']).resolve()
        text = hooks.read_text()
        self.assertIn('!macro NSIS_HOOK_PREINSTALL', text)
        for kind in pack.WORKER_KINDS:
            self.assertIn(f'Delete "$INSTDIR\\compute\\bin\\illustash-{kind}.exe"', text)
        self.assertIn('RMDir /r "$INSTDIR\\resources"', text)
        self.assertNotIn('worker', ''.join(line for line in text.splitlines() if line.lstrip().startswith(('Delete', 'RMDir'))))

    @unittest.skipIf(os.name == 'nt', 'POSIX shell launcher execution only')
    def test_relocated_ordinary_launch_preserves_overrides_and_remains_lazy(self):
        keys = ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_SHARED',
                'ILLUSTASH_RESEARCH_WORKER', 'ILLUSTASH_INDEX_WORKER',
                'ILLUSTASH_PROVENANCE_WORKER', 'ILLUSTASH_RECOGNITION_WORKER')
        probe = '#!/bin/sh\n' + ''.join(f'printf "%s\\n" "${key}"\n' for key in keys) + 'printf "%s\\n" "$@"\n'
        for name in ('illustash-terms', 'illustash-desktop'):
            binary = self.binaries / name
            binary.write_text(probe)
            binary.chmod(0o755)
        worker = self.binaries / 'illustash-worker'
        worker.write_text('#!/bin/sh\n/bin/touch "$HOME/worker-was-started"\nexit 97\n')
        worker.chmod(0o755)
        output = self.root / 'before move'
        pack.stage(output, self.binaries, research=True, iqdb=self.iqdb,
                   desktop=self.binaries / 'illustash-desktop')
        moved = self.root / '已移动 with spaces'
        output.rename(moved)
        home, cwd = self.root / 'synthetic home', self.root / 'unrelated cwd'
        home.mkdir(); cwd.mkdir()
        env = {'HOME': str(home), 'PATH': ''}
        shared = (home / 'Library/Application Support/Illustash/Shared' if pack.sys.platform == 'darwin'
                  else home / '.local/share/Illustash/Shared')
        expected = [str(moved / 'resources'), str(moved / 'resources/models'), str(shared)] + [
            str(moved / 'bin/illustash-worker')] * 4
        for script, args in (('start.sh', ['http', '--native']), ('desktop.sh', [])):
            with self.subTest(script=script):
                result = subprocess.run(['/bin/sh', str(moved / script), '--library', 'synthetic.pilib'],
                                        cwd=cwd, env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), expected + args + [
                    '--root', str(moved / 'resources'), '--library', 'synthetic.pilib'])
                overrides = {key: str(self.root / ('override ' + key)) for key in keys}
                result = subprocess.run(['/bin/sh', str(moved / script)], cwd=cwd,
                                        env={**env, **overrides}, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), list(overrides.values()) + args + [
                    '--root', overrides['ILLUSTASH_ROOT']])
        self.assertFalse((home / 'worker-was-started').exists())
        self.assertFalse(shared.exists())
        self.assertFalse((cwd / 'review').exists())


class CandidateExampleWiringTest(unittest.TestCase):
    def test_full_runners_build_and_export_each_current_candidate_before_discovery(self):
        shell = (ROOT / 'tools/check_all.sh').read_text()
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        probes = {'READ_HTTP': 'read-http', 'RECOGNITION_COMPUTE': 'recognition-compute',
                  'INDEX_COMPUTE': 'index-compute', 'RESEARCH_COMPUTE': 'research-compute',
                  'RELATION_PROBE': 'relation-probe', 'PROVENANCE_COMPUTE': 'provenance-compute'}
        for key, example in probes.items():
            for source in (shell, ci):
                build_at = source.index('--example ' + example)
                export_at = source.index('ILLUSTASH_' + key + '_BIN')
                discover_at = source.index('-m tests.run_fixed_suite -v -b')
                self.assertLess(build_at, export_at)
                self.assertLess(export_at, discover_at)
                self.assertIn('native/terms/target/release/examples/' + example, source)
        self.assertIn("'refs/heads/main'", ci)
        self.assertNotIn('\n  push:', ci)
        self.assertNotIn('\n  pull_request:', ci)


    def test_full_runners_accept_research_platform_skips_but_not_missing_candidates_or_models(self):
        import re
        patterns = []
        for name in ('tools/check_all.sh', '.github/workflows/ci.yml'):
            source = (ROOT / name).read_text()
            pattern = re.search(r"allowed=['\"]([^'\"]+)['\"]", source).group(1)
            patterns.append(pattern)
            for reason in ('Windows pinned OpenBLAS backend only',
                           'explicit Windows native development calculator only',
                           'explicit Windows development App publication only',
                           'Linux is frozen: no local computation'):
                self.assertRegex(reason, pattern)
            for reason in ('explicit candidate research calculator/core required',
                           'explicit candidate host and its built worker required',
                           'explicit candidate math example and matching reference DLL required',
                           'explicit authorised CCIP model/runtime required'):
                self.assertNotRegex(reason, pattern)
        self.assertEqual(*patterns)


class MacDesktopResourcesTest(unittest.TestCase):
    setUp = PackagingResourcesTest.setUp
    manifest = PackagingResourcesTest.manifest

    def test_complete_app_layout_relocation_and_incomplete_input_refusal(self):
        runtime = self.root / 'runtime-marker'
        runtime.write_bytes(b'synthetic runtime')
        notices = self.root / 'notices'
        notices.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (notices / name).write_text('synthetic notice')
        with patch.object(pack.sys, 'platform', 'darwin'):
            if os.name == 'nt':
                for name in ('illustash-terms', 'illustash-worker'):
                    (self.binaries / name).write_bytes(b'synthetic marker:' + name.encode())
            source = self.root / 'portable'
            pack.stage(source, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       desktop=self.binaries / ('illustash-desktop.exe' if os.name == 'nt' else 'illustash-desktop'),
                       ort_dylib=runtime, ort_licenses=notices)
            (source / 'private').mkdir()
            (source / 'private/library.sqlite').write_text('not a shipped resource')
            output = self.root / 'App.app/Contents/Resources/compute'
            pack.macos_desktop_compute(source, output)
            installed = self.manifest(output)
            original = json.loads((source / 'manifest.json').read_text())
            self.assertEqual((installed['variant'], installed['platform']), ('research', 'darwin'))
            assert_desktop_contract(self, installed, 'darwin', 'MACOS_FILES')
            self.assertEqual(installed['files'], {name: digest for name, digest in original['files'].items()
                             if name.startswith(('bin/', 'resources/', 'dependencies/'))
                             and Path(name).name != 'illustash-desktop'})
            self.assertFalse((output / 'bin/illustash-desktop').exists())
            self.assertFalse((output / 'private').exists())
            self.assertFalse((output / 'start.sh').exists())
            moved = self.root / 'relocated app with spaces'
            (self.root / 'App.app').rename(moved)
            self.assertEqual(self.manifest(moved / 'Contents/Resources/compute'), installed)
            with self.assertRaisesRegex(ValueError, 'already exists'):
                pack.macos_desktop_compute(source, moved / 'Contents/Resources/compute')
            lost = source / 'resources/web/static/app.js'
            saved = lost.read_bytes()
            lost.unlink()
            with self.assertRaisesRegex(ValueError, 'source manifest file set'):
                pack.macos_desktop_compute(source, self.root / 'lost-resource')
            self.assertFalse((self.root / 'lost-resource').exists())
            lost.write_bytes(saved)
            runtime = source / 'resources/onnxruntime/libonnxruntime.dylib'
            runtime.write_bytes(b'changed after staging')
            with self.assertRaisesRegex(ValueError, 'does not match manifest'):
                pack.macos_desktop_compute(source, self.root / 'bad-hash')
            self.assertFalse((self.root / 'bad-hash').exists())
            runtime.unlink()
            with self.assertRaisesRegex(ValueError, 'component missing'):
                pack.macos_desktop_compute(source, self.root / 'incomplete')
            self.assertFalse((self.root / 'incomplete').exists())


class WindowsOpenBLASResourcesTest(unittest.TestCase):
    setUp = PackagingResourcesTest.setUp
    manifest = PackagingResourcesTest.manifest
    """Only source staging on synthetic identities, never a Windows load/ABI test."""
    def make_runtime(self):
        """An explicit site-packages holding the fixed NumPy wheel, plus a CPU ORT marker."""
        site = self.root / 'explicit wheel/site-packages'
        libs = site / 'numpy.libs'
        libs.mkdir(parents=True)
        self.ort = self.root / 'ort/onnxruntime.dll'
        self.ort.parent.mkdir()
        self.ort.write_bytes(b'synthetic CPU runtime marker')
        self.ort_licenses = self.root / 'ort-licenses'
        self.ort_licenses.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (self.ort_licenses / name).write_bytes(b'synthetic ORT notice')
        self.dll = libs / 'fixed.dll'
        self.dll.write_bytes(b'synthetic registered DLL, never executable')
        self.digest = hashlib.sha256(self.dll.read_bytes()).hexdigest()
        self.notices = site / 'numpy-synthetic.dist-info/licenses'
        self.notices.mkdir(parents=True)
        (self.notices / 'LICENSE.txt').write_bytes(b'synthetic full NumPy/OpenBLAS bundled notice')
        (self.notices / 'nested').mkdir()
        (self.notices / 'nested/third-party.txt').write_bytes(b'synthetic transitive dependency notice')
        registry = patch.dict(pack.RESEARCH_OPENBLAS_IDENTITIES,
                              {self.digest: dict(version='synthetic', numpy='synthetic')}, clear=True)
        registry.start()
        self.addCleanup(registry.stop)
        # Marker binaries cannot be inspected by dumpbin; real CRT imports are
        # covered independently by WindowsCRTTest and installed Windows acceptance.
        crt = patch.object(pack, 'check_windows_crt')
        crt.start()
        self.addCleanup(crt.stop)
        # Only marker binaries get Windows names. This is cross-platform source testing.
        for name in ('illustash-terms', 'illustash-worker'):
            (self.binaries / (name + '.exe')).write_bytes(b'synthetic marker:' + name.encode())
        return site

    def test_fixed_resource_and_complete_notices_manifest_relocation(self):
        runtime = self.make_runtime()
        output = self.root / 'portable'
        with patch.object(pack.sys, 'platform', 'win32'):
            pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       openblas_site=runtime, ort_dylib=self.ort, ort_licenses=self.ort_licenses)
            manifest = self.manifest(output)
            self.assertEqual(manifest['files']['resources/openblas/openblas.dll'], self.digest)
            self.assertFalse(any(name.startswith(('workers/', 'runtime/')) for name in manifest['files']))
            self.assertNotIn('ILLUSTASH_WORKER_PYTHON', (output / 'start.cmd').read_text())
            for source in self.notices.rglob('*'):
                if source.is_file():
                    target = output / 'resources/licenses/openblas' / source.relative_to(self.notices)
                    self.assertEqual(target.read_bytes(), source.read_bytes())
            compute = self.root / 'relocated with spaces/compute'
            pack.desktop_compute(output, compute)
            assert_desktop_contract(self, self.manifest(compute), 'win32', 'WINDOWS_FILES')
            self.assertEqual((compute / 'resources/openblas/openblas.dll').read_bytes(), self.dll.read_bytes())
            self.assertIn('ILLUSTASH_RESEARCH_OPENBLAS_DLL=%ILLUSTASH_ROOT%\\openblas\\openblas.dll', (output / 'start.cmd').read_text())
            self.assertIn('if not defined ILLUSTASH_RESEARCH_OPENBLAS_DLL', (output / 'research.cmd').read_text())
            (output / 'resources/openblas/openblas.dll').write_bytes(b'changed after staging')
            with self.assertRaisesRegex(ValueError, 'fixed reference identity'):
                pack.desktop_compute(output, self.root / 'bad compute')
            self.assertFalse((self.root / 'bad compute').exists())

    def test_unregistered_missing_ambiguous_and_symlink_resources_fail_before_staging(self):
        runtime = self.make_runtime()
        output = self.root / 'bad'
        def stage():
            with patch.object(pack.sys, 'platform', 'win32'):
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, openblas_site=runtime)
        self.dll.write_bytes(b'unregistered')
        with self.assertRaisesRegex(ValueError, 'exactly one fixed'):
            stage()
        self.assertFalse(output.exists())
        self.dll.write_bytes(b'synthetic registered DLL, never executable')
        second = self.dll.with_name('second.dll')
        second.write_bytes(self.dll.read_bytes())
        with self.assertRaisesRegex(ValueError, 'exactly one fixed'):
            stage()
        second.unlink()
        (self.notices / 'LICENSE.txt').unlink()
        with self.assertRaisesRegex(ValueError, 'licenses/LICENSE.txt'):
            stage()
        (self.notices / 'LICENSE.txt').write_bytes(b'fixture')
        second.symlink_to(self.dll)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            stage()
        self.assertFalse(output.exists())

    def test_copy_race_fails_without_publishing_manifest(self):
        runtime = self.make_runtime()
        output = self.root / 'race'
        original_copy = pack.copy
        def corrupt(source, destination):
            original_copy(source, destination)
            if destination.as_posix().endswith('/resources/openblas/openblas.dll'):
                destination.write_bytes(b'changed during copy')
        with patch.object(pack.sys, 'platform', 'win32'), patch.object(pack, 'copy', side_effect=corrupt):
            with self.assertRaisesRegex(ValueError, 'changed while staging'):
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, openblas_site=runtime)
        self.assertFalse((output / 'manifest.json').exists())
        portable = self.root / 'good portable'
        desktop = self.root / 'race desktop'
        with patch.object(pack.sys, 'platform', 'win32'):
            pack.stage(portable, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       openblas_site=runtime, ort_dylib=self.ort, ort_licenses=self.ort_licenses)
            with patch.object(pack, 'copy', side_effect=corrupt), self.assertRaisesRegex(ValueError, 'changed while staging desktop'):
                pack.desktop_compute(portable, desktop)
        self.assertFalse((desktop / 'manifest.json').exists())
