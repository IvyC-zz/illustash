"""#69: relocated, allowlisted artifacts on synthetic packages; never real Photos/accounts."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import unittest
from contextlib import closing
import sqlite3

from tests import test_native_lifecycle as lifecycle
from tests import test_native_research as research
from tests import test_native_research_production as production
from tests.test_read_http import free_port
from tools.verify_read_http import enter, fetch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('package_build', ROOT / 'packaging/build.py')
build = importlib.util.module_from_spec(spec)
spec.loader.exec_module(build)


class PocketFFTLicenseTest(unittest.TestCase):
    def test_portable_and_desktop_resource_stage_retains_official_license(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory)
            build.resources(destination)
            digest = hashlib.sha256((destination / 'licenses/pocketfft/LICENSE.md').read_bytes()).hexdigest()
            self.assertEqual(digest, 'a85ca13fdf90160b64a0698215868c13b74d835ad0a4e2ba44713b8c5058a056')


class PackagingTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    setUp = lifecycle.NativeLifecycleTest.setUp
    package = lifecycle.NativeLifecycleTest.package
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request

    def test_relocated_base_no_python_static_and_core(self):
        self.stop(self.process)
        install = self.root / 'installed base'
        build.stage(install, self.binary.parent)
        manifest = json.loads((install / 'manifest.json').read_text())
        self.assertEqual(manifest['variant'], 'base')
        for name, digest in manifest['files'].items():
            self.assertFalse(name.endswith('.py') or '.venv' in name or 'target/' in name)
            self.assertEqual(hashlib.sha256((install / name).read_bytes()).hexdigest(), digest)
        env = {k: v for k, v in self.env.items() if k not in ('ILLUSTASH_ROOT', 'ILLUSTASH_TERMS_BIN') and not k.endswith('_WORKER')}
        port = free_port()
        origin = f'http://127.0.0.1:{port}'
        command = ([str(install / 'bin' / self.binary.name), 'http', '--native'] if os.name == 'nt'
                   else [str(install / 'start.sh')])
        before = lifecycle.dump(self.lib.db)
        with tempfile.TemporaryFile() as log:
            process = subprocess.Popen([*command,
                                        '--library', str(self.lib.path), '--port', str(port),
                                        '--no-sync', '--no-open', '--no-names', '--no-index-check'],
                                       env=env, cwd=install, stdout=log, stderr=log, **lifecycle.SPAWN)
            try:
                enter(origin, log, process, timeout=8)
                for name in ('index.html', 'app.js', 'app.css', 'theme.js', 'splash.js', 'favicon.svg'):
                    url = '/' if name == 'index.html' else '/' + name
                    response = fetch(origin, {'url': url}, origin)
                    self.assertEqual(response['status'], 200)
                    self.assertEqual(response['body'], (install / 'resources/web/static' / name).read_bytes())
                self.assertEqual(fetch(origin, {'url': '/api/assets'}, origin)['status'], 200)
                self.assertEqual(fetch(origin, {'url': '/image?id=missing'}, origin)['status'], 404)
                self.assertFalse(self.sentinel.exists())
                self.assertEqual(lifecycle.dump(self.lib.db), before)
            finally:
                self.stop(process)
            self.assertEqual(process.returncode, 0)

    def test_research_adapter_diagnostics_and_failure(self):
        install = self.root / 'installed research'
        build.stage(install, self.binary.parent, research=True, iqdb=Path(os.environ['ILLUSTASH_IQDB_BINARY']) if os.environ.get('ILLUSTASH_IQDB_BINARY') else None)
        suffix = '.exe' if os.name == 'nt' else ''
        adapter = install / 'bin' / ('illustash-worker' + suffix)
        # No interpreter ships: readiness and requests stay native.
        self.assertFalse((install / 'workers').exists())
        self.assertFalse((install / 'runtime').exists())
        # #37: one worker program; the kind is explicit per launch.
        self.assertEqual(sorted(p.name for p in (install / 'bin').iterdir()),
                         sorted(f'illustash-{name}{suffix}' for name in ('terms', 'iqdb', 'worker')))
        env = {k: v for k, v in self.env.items() if k != 'ILLUSTASH_WORKER_KIND'}
        for kind in ('research', 'provenance'):
            result = subprocess.run([str(adapter), '--check'], env={**env, 'ILLUSTASH_WORKER_KIND': kind}, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stderr)
        for extra, message in (({}, 'ILLUSTASH_WORKER_KIND'), ({'ILLUSTASH_WORKER_KIND': 'ocr'}, '未知')):
            result = subprocess.run([str(adapter), '--check'], env={**env, **extra}, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn(message, result.stderr.decode(errors='replace'))
        # Older layouts and explicit overrides naming illustash-<kind> still dispatch by name.
        legacy = self.root / 'legacy layout' / ('illustash-provenance' + suffix)
        legacy.parent.mkdir()
        shutil.copy2(adapter, legacy)
        result = subprocess.run([str(legacy), '--check'], env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        result = subprocess.run([str(legacy), '--check'], env={**env, 'ILLUSTASH_WORKER_KIND': 'research'}, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        invalid = self.root / 'invalid.json'; invalid.write_text('{}')
        result = subprocess.run([str(adapter), str(invalid), str(self.root / 'response.json')],
                                env={**env, 'ILLUSTASH_WORKER_KIND': 'research'}, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / 'response.json').exists())
        self.assertEqual(self.request('/api/assets')['status'], 200)


class InstalledResearchTest(unittest.TestCase):
    """Use existing algorithm acceptance with only the installed optional adapter."""
    def setUp(self):
        research.NativeResearchTest.setUp(self)
        self.stop(self.process)
        install = self.root / 'research artifact'
        build.stage(install, self.binary.parent, research=True, iqdb=Path(os.environ['ILLUSTASH_IQDB_BINARY']) if os.environ.get('ILLUSTASH_IQDB_BINARY') else None)
        # The host names the kind; the installed worker carries no kind in its file name.
        self.env['ILLUSTASH_RESEARCH_WORKER'] = str(install / 'bin' / ('illustash-worker.exe' if os.name == 'nt' else 'illustash-worker'))
        self.env.pop('ILLUSTASH_WORKER_KIND', None)
        self.restart()
        self.finished()
    test_real_similar_relate_cluster_and_oc_routes = research.NativeResearchTest.test_real_similar_relate_cluster_and_oc_routes


for name, value in research.NativeResearchTest.__dict__.items():
    if not name.startswith("test_") and not name.startswith("__") and name != "setUp":
        setattr(InstalledResearchTest, name, value)


@unittest.skipUnless(sys.platform == 'darwin', 'macOS/Linux only; this startup fixture uses accepted macOS native learning')
class InstalledLauncherResearchTest(unittest.TestCase):
    """Prepared actual-host test: startup intent is existing behavior, not a lazy claim.

    Requires this round's explicitly built host/worker/IQDB. No model or system
    OCR is used; similarity is off so #151's accepted learning path is exercised.
    """
    setUpClass = production.InstalledResearchComputeTest.__dict__['setUpClass']
    setUp = production.NativeResearchPublicationTest.setUp
    package = lifecycle.NativeLifecycleTest.package
    start = lifecycle.NativeLifecycleTest.start
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    state = production.NativeResearchPublicationTest.state
    finished = production.NativeResearchPublicationTest.finished
    rows = production.NativeResearchPublicationTest.rows

    def restart(self):
        if not hasattr(self, 'launcher'):
            configured = os.environ.get('ILLUSTASH_IQDB_BINARY')
            if not configured or not Path(configured).is_file():
                raise AssertionError('explicit current candidate IQDB required; no automatic build')
            original = self.root / 'staged full compute'
            build.stage(original, self.binary.parent, research=True, iqdb=Path(configured))
            moved = self.root / 'relocated compute with spaces'
            original.rename(moved)
            self.launcher = moved / 'start.sh'
            self.resources = moved / 'resources'
            self.startup_human = self.rows(['actions', 'assertions', 'asset_tags', 'term_verdicts'])
            for key in list(self.env):
                if key.endswith('_WORKER') or key in ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_WORKER_PYTHON'):
                    self.env.pop(key)
            # Keep the fixture's explicit HOME/support/shared. The launcher must
            # configure its installed adapters; no inherited worker can mask it.
        self.process, self.log = self.start(self.lib.path, extra=('--no-open',), command=[str(self.launcher)])
        self.port = int(self.process.args[self.process.args.index('--port') + 1])
        self.origin = f'http://127.0.0.1:{self.port}'
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def test_existing_startup_learning_and_recovery_keep_human_history(self):
        human = self.startup_human
        self.assertEqual(self.rows(list(human)), human)
        self.assertGreater(len(self.rows(['prototypes'])['prototypes']), 0)
        self.assertGreater(len(self.rows(['clusters'])['clusters']), 0)
        with closing(sqlite3.connect(self.lib.db)) as c:
            self.assertEqual(c.execute("SELECT status FROM research_runs WHERE id='startup'").fetchall(), [('done',)])
        self.stop(self.process)
        # A persisted interrupted intent must be recovered by the same ordinary
        # launcher without a user HTTP request creating a replacement task.
        with closing(sqlite3.connect(self.lib.db)) as c:
            with c:
                c.execute("UPDATE research_runs SET status='running' WHERE id='startup'")
        self.restart()
        self.finished()
        with closing(sqlite3.connect(self.lib.db)) as c:
            self.assertEqual(c.execute("SELECT status FROM research_runs WHERE id='startup'").fetchall(), [('done',)])
            self.assertEqual(c.execute("SELECT count(*) FROM research_runs WHERE status IN ('running','pending')").fetchone()[0], 0)
        self.assertEqual(self.rows(list(human)), human)
        self.assertFalse(self.sentinel.exists())


class DesktopComputeStagingTest(unittest.TestCase):
    def test_full_installer_allowlist_and_hashes(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp); source = root / 'artifact'; dest = root / 'compute'
            names = ['resources/onnxruntime/onnxruntime.dll', 'resources/licenses/onnxruntime/LICENSE',
                     'resources/licenses/onnxruntime/ThirdPartyNotices.txt', 'resources/models/wd_swinv2_v3_emb.onnx',
                     'resources/models/ccip/model_feat.onnx', 'resources/models/ccip/model_metrics.onnx',
                     'resources/models/ccip/metrics.json', 'resources/openblas/openblas.dll',
                     'resources/licenses/openblas/LICENSE.txt', 'resources/web/static/index.html',
                     'resources/tags/tags_v3.csv', 'bin/illustash-worker.exe',
                     *(f'resources/{name}' for name in build.NOTICES),
                     'dependencies/core-Cargo.lock',
                     'bin/illustash-desktop.exe', 'private/library.sqlite',
                     *(installed for installed, _ in build.model_licenses())]
            for name in names:
                path = source / name; path.parent.mkdir(parents=True, exist_ok=True); path.write_bytes(b'fixture')
            with patch.object(build.sys, 'platform', 'win32'), patch.object(build, 'check_windows_crt'), patch.dict(build.RESEARCH_OPENBLAS_IDENTITIES, {hashlib.sha256(b'fixture').hexdigest(): dict(version='synthetic', numpy='synthetic')}, clear=True):
                with patch.object(build, 'check_windows_crt', side_effect=ValueError('dynamic VC++ runtime')):
                    with self.assertRaisesRegex(ValueError, 'dynamic VC'):
                        build.desktop_compute(source, dest)
                    self.assertFalse(dest.exists())
                build.desktop_compute(source, dest)
                with self.assertRaises(ValueError):
                    build.desktop_compute(source, dest)
            self.assertFalse((dest / 'private').exists())
            self.assertFalse((dest / 'bin/illustash-desktop.exe').exists())
            manifest = json.loads((dest / 'manifest.json').read_text())
            self.assertEqual(manifest['variant'], 'research')
            self.assertIn('resources/models/ccip/model_feat.onnx', manifest['files'])
            for name, digest in manifest['files'].items():
                self.assertEqual(hashlib.sha256((dest / name).read_bytes()).hexdigest(), digest)
            # #8: a full installer without the bundled models' license texts is never staged.
            (source / 'resources/licenses/models/ccip/BigScience-OpenRAIL-M.pdf').unlink()
            with patch.object(build.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'OpenRAIL-M.pdf'):
                build.desktop_compute(source, root / 'no model license')
            self.assertFalse((root / 'no model license').exists())
            # #161: nor one without the Rust crates' license texts.
            rust = source / 'resources/licenses/rust-dependencies.txt'
            rust.rename(root / 'rust-dependencies.txt')
            with patch.object(build.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'rust-dependencies.txt'):
                build.desktop_compute(source, root / 'no rust licenses')
            self.assertFalse((root / 'no rust licenses').exists())
            (root / 'rust-dependencies.txt').rename(rust)
            (source / 'resources/onnxruntime/onnxruntime.dll').unlink()
            with patch.object(build.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'onnxruntime.dll'):
                build.desktop_compute(source, root / 'incomplete')
            self.assertFalse((root / 'incomplete').exists())
            with patch.object(build.sys, 'platform', 'darwin'), self.assertRaisesRegex(ValueError, 'Windows'):
                build.desktop_compute(source, root / 'mac')
            (root / 'onnxruntime.dll').write_bytes(b'fixture')
            (root / 'onnxruntime.dll').rename(source / 'resources/onnxruntime/onnxruntime.dll')
            (source / 'resources/licenses/models/ccip/BigScience-OpenRAIL-M.pdf').write_bytes(
                (build.MODEL_LICENSES / 'ccip/BigScience-OpenRAIL-M.pdf').read_bytes())
            # #168: the release installer has no models; only some of them is refused.
            (source / 'resources/models/ccip/metrics.json').unlink()
            with patch.object(build.sys, 'platform', 'win32'), patch.object(build, 'check_windows_crt'), self.assertRaisesRegex(ValueError, 'all present or all absent; missing resources/models/ccip/metrics.json'):
                build.desktop_compute(source, root / 'partial models')
            self.assertFalse((root / 'partial models').exists())
            shutil.rmtree(source / 'resources/models')
            release = root / 'release'
            with patch.object(build.sys, 'platform', 'win32'), patch.object(build, 'check_windows_crt'), patch.dict(build.RESEARCH_OPENBLAS_IDENTITIES, {hashlib.sha256(b'fixture').hexdigest(): dict(version='synthetic', numpy='synthetic')}, clear=True):
                build.desktop_compute(source, release)
            files = json.loads((release / 'manifest.json').read_text())['files']
            self.assertFalse(any(name.startswith('resources/models/') for name in files))
            self.assertIn('resources/licenses/models/ccip/BigScience-OpenRAIL-M.pdf', files)


class WindowsCRTTest(unittest.TestCase):
    def test_checks_exes_dlls_and_delay_imports(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp)
            for name in ('bin/worker.exe', 'resources/ort/onnxruntime.dll', 'resources/openblas/openblas.dll'):
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'synthetic')
            system = subprocess.CompletedProcess([], 0, '    KERNEL32.dll\n    api-ms-win-crt-runtime-l1-1-0.dll\n')
            with patch.object(build.subprocess, 'run', return_value=system) as run:
                build.check_windows_crt(source)
                self.assertEqual(run.call_count, 3)
                self.assertEqual({Path(call.args[0][-1]).name for call in run.call_args_list},
                                 {'worker.exe', 'onnxruntime.dll', 'openblas.dll'})
            for name in ('VCRUNTIME140.dll', 'MSVCP140_1.dll', 'VCRUNTIME140_1.dll', 'concrt140.dll'):
                delayed = subprocess.CompletedProcess([], 0, f'  Image has the following delay load dependencies:\n\n    {name}\n')
                with self.subTest(name=name), patch.object(build.subprocess, 'run', return_value=delayed):
                    with self.assertRaisesRegex(ValueError, 'dynamic VC'):
                        build.check_windows_crt(source)
            with patch.object(build.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'dumpbin')):
                with self.assertRaises(subprocess.CalledProcessError):
                    build.check_windows_crt(source)
