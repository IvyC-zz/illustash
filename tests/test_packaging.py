"""#69: relocated, allowlisted artifacts on synthetic packages; never real Photos/accounts."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
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
from tests.test_read_http import free_port, wait_until
from tools.verify_read_http import fetch

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
                wait_until(lambda: fetch(origin, {'url': '/api/bootstrap'}, origin)['status'] == 200, timeout=8)
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

    @unittest.skipIf(sys.platform in ('darwin', 'win32'), 'macOS/Windows use the native production dispatcher')
    def test_relocated_worker_shared_paths_and_legacy_refusal(self):
        install = self.root / 'relocated worker artifact'
        build.stage(install, self.binary.parent, research=True,
                    iqdb=Path(os.environ['ILLUSTASH_IQDB_BINARY']) if os.environ.get('ILLUSTASH_IQDB_BINARY') else None)
        adapter = install / 'bin' / ('illustash-provenance.exe' if os.name == 'nt' else 'illustash-provenance')
        # Replace only this test's staged script with an environment probe; no model or real image is read.
        # Steps the native adapter does not handle still reach the staged Python script
        # (local provenance steps and recognition's accepted macOS route never read it).
        (install / 'workers/provenance/native_worker.py').write_text(
            'import json, os, sys\nfrom pathlib import Path\n'
            'sys.path.insert(0, str(Path(__file__).resolve().parents[1]))\n'
            'from web.library import SHARED\n'
            'Path(sys.argv[2]).write_text(json.dumps({"shared": str(SHARED), "env": os.environ["ILLUSTASH_SHARED"]}))\n')
        cwd = self.root / 'different cwd'
        cwd.mkdir()
        env = {k: v for k, v in self.env.items() if k not in ('ILLUSTASH_ROOT', 'ILLUSTASH_SHARED')}
        env['ILLUSTASH_WORKER_PYTHON'] = sys.executable
        response = self.root / 'probe.json'
        request = self.root / 'fallback-request.json'
        request.write_text(json.dumps({'step': 'not-a-local-step'}))
        def probe():
            return subprocess.run([str(adapter), str(request), str(response)],
                                  cwd=cwd, env=env, capture_output=True)
        result = probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(response.read_text())['shared'], str((install / 'resources/review').resolve()))
        env['ILLUSTASH_SHARED'] = 'shared'
        result = probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = str((install / 'resources/shared').resolve())
        self.assertEqual(json.loads(response.read_text()), {'shared': expected, 'env': expected})
        old = cwd / 'shared'
        (old / 'char-index').mkdir(parents=True)
        response.unlink()
        result = probe()
        self.assertEqual(result.returncode, 1)
        self.assertIn('绝对路径', result.stderr.decode())
        self.assertFalse(response.exists())
        self.assertFalse((install / 'resources/shared').exists())
        env['ILLUSTASH_SHARED'] = str(old)
        result = probe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(response.read_text())['shared'], str(old.resolve()))

    def test_research_adapter_diagnostics_and_failure(self):
        install = self.root / 'installed research'
        build.stage(install, self.binary.parent, research=True, iqdb=Path(os.environ['ILLUSTASH_IQDB_BINARY']) if os.environ.get('ILLUSTASH_IQDB_BINARY') else None)
        if sys.platform not in ('darwin', 'win32'):
            for name in ('store', 'terms_core', 'server', 'results'):
                self.assertFalse((install / 'workers/web' / (name + '.py')).exists())
            for name in ('research_snapshot', 'research_constants'):
                self.assertTrue((install / 'workers/web' / (name + '.py')).is_file())
        adapter = install / 'bin' / ('illustash-research.exe' if os.name == 'nt' else 'illustash-research')
        env = {k: v for k, v in self.env.items() if k != 'ILLUSTASH_WORKER_PYTHON'}
        if sys.platform in ('darwin', 'win32'):
            # No interpreter ships or is configured: readiness and requests stay native.
            self.assertFalse((install / 'workers').exists())
            self.assertFalse((install / 'runtime').exists())
            for kind in ('research', 'provenance'):
                checked = install / 'bin' / (f'illustash-{kind}.exe' if os.name == 'nt' else f'illustash-{kind}')
                result = subprocess.run([str(checked), '--check'], env=env, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stderr)
            invalid = self.root / 'invalid.json'; invalid.write_text('{}')
            result = subprocess.run([str(adapter), str(invalid), str(self.root / 'response.json')], env=env, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((self.root / 'response.json').exists())
            self.assertEqual(self.request('/api/assets')['status'], 200)
            return
        result = subprocess.run([str(adapter), '--check'], env=env, capture_output=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('ILLUSTASH_WORKER_PYTHON', result.stderr.decode())
        if os.name != 'nt':
            env['ILLUSTASH_WORKER_PYTHON'] = str(self.bin / 'python')
            result = subprocess.run([str(adapter), '--check'], env=env, capture_output=True)
            self.assertEqual(result.returncode, 97)
        else:
            env['ILLUSTASH_WORKER_PYTHON'] = str(self.root / 'missing.exe')
            result = subprocess.run([str(adapter), '--check'], env=env, capture_output=True)
            self.assertEqual(result.returncode, 1)
        env['ILLUSTASH_WORKER_PYTHON'] = sys.executable
        result = subprocess.run([str(adapter), '--check'], env=env, capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        invalid = self.root / 'invalid.json'; invalid.write_text('{}')
        result = subprocess.run([str(adapter), str(invalid), str(self.root / 'response.json')], env=env, capture_output=True)
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
        self.env['ILLUSTASH_RESEARCH_WORKER'] = str(install / 'bin' / ('illustash-research.exe' if os.name == 'nt' else 'illustash-research'))
        self.env['ILLUSTASH_WORKER_PYTHON'] = sys.executable
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
                     'resources/tags/tags_v3.csv', 'bin/illustash-recognition.exe',
                     'dependencies/core-Cargo.lock',
                     'bin/illustash-desktop.exe', 'private/library.sqlite']
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
            (source / 'resources/onnxruntime/onnxruntime.dll').unlink()
            with patch.object(build.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'onnxruntime.dll'):
                build.desktop_compute(source, root / 'incomplete')
            self.assertFalse((root / 'incomplete').exists())
            with patch.object(build.sys, 'platform', 'darwin'), self.assertRaisesRegex(ValueError, 'Windows'):
                build.desktop_compute(source, root / 'mac')


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
