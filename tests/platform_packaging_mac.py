"""Explicit headless macOS App-layout bootstrap acceptance; never launches Tauri GUI.

Supply this round's ILLUSTASH_DESKTOP_BIN, ILLUSTASH_TERMS_BIN,
ILLUSTASH_RESEARCH_ADAPTER_BIN and ILLUSTASH_IQDB_BINARY. Missing artifacts fail.
Model/ORT resources are markers: only accepted native startup learning runs.
This does not replace signed App, Finder/GUI or real model acceptance.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

from tests import test_native_lifecycle as lifecycle
from tests import test_native_research_production as production
from tests import test_packaging as packaging_tests
from tests.test_read_http import SPAWN, free_port, wait_until
from tools.verify_read_http import fetch


class MacAppBootstrapTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if sys.platform != 'darwin':
            raise RuntimeError('explicit macOS platform acceptance only')
        for key in ('ILLUSTASH_DESKTOP_BIN', 'ILLUSTASH_TERMS_BIN',
                    'ILLUSTASH_RESEARCH_ADAPTER_BIN', 'ILLUSTASH_IQDB_BINARY'):
            configured = os.environ.get(key)
            if not configured or not Path(configured).is_absolute() or not Path(configured).is_file():
                raise RuntimeError(f'explicit current built artifact required: {key}')
        production.InstalledResearchComputeTest.setUpClass.__func__(cls)
        cls.desktop = Path(os.environ['ILLUSTASH_DESKTOP_BIN'])

    setUp = production.NativeResearchPublicationTest.setUp
    package = lifecycle.NativeLifecycleTest.package
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    state = production.NativeResearchPublicationTest.state
    finished = production.NativeResearchPublicationTest.finished
    rows = production.NativeResearchPublicationTest.rows

    def start(self, path, extra=(), command=None):
        if not hasattr(self, 'app_executable'):
            return lifecycle.NativeLifecycleTest.start(self, path, extra, command)
        port = free_port()
        log = tempfile.TemporaryFile()
        # No explicit --root or inherited worker env may mask App bundle defaults.
        args = [str(self.app_executable), 'http', '--native', '--no-auto-sync',
                '--library', str(path), '--port', str(port), '--no-open',
                '--no-names', '--no-index-check', *extra]
        process = subprocess.Popen(args, env=self.env, stdout=log, stderr=log, **SPAWN)
        self.addCleanup(log.close)
        self.addCleanup(self.stop, process)
        origin = f'http://127.0.0.1:{port}'
        try:
            wait_until(lambda: fetch(origin, {'url': '/api/bootstrap'}, origin)['status'] == 200, timeout=5)
        except AssertionError:
            log.seek(0)
            self.fail(f'App bootstrap failed ({process.poll()}): {log.read().decode(errors="replace")}')
        return process, log

    def restart(self):
        if not hasattr(self, 'app_executable'):
            inputs = self.root / 'marker inputs'
            inputs.mkdir()
            models = inputs / 'models'
            for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx',
                         'ccip/model_metrics.onnx', 'ccip/metrics.json'):
                path = models / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'not a runnable model; startup learning only')
            runtime = inputs / 'libonnxruntime.dylib'
            runtime.write_bytes(b'not a runnable runtime; startup learning only')
            notices = inputs / 'ort notices'
            notices.mkdir()
            for name in ('LICENSE', 'ThirdPartyNotices.txt'):
                (notices / name).write_bytes(b'synthetic notice')
            portable = self.root / 'portable'
            packaging_tests.build.stage(portable, self.binary.parent, research=True,
                iqdb=Path(os.environ['ILLUSTASH_IQDB_BINARY']), models=models,
                ort_dylib=runtime, ort_licenses=notices)
            app = self.root / 'Original.app'
            packaging_tests.build.macos_desktop_compute(portable, app / 'Contents/Resources/compute')
            executable = app / 'Contents/MacOS/illustash-desktop'
            executable.parent.mkdir(parents=True)
            shutil.copy2(self.desktop, executable)
            moved = self.root / 'Relocated App with spaces.app'
            app.rename(moved)
            self.app_executable = moved / 'Contents/MacOS/illustash-desktop'
            self.compute = moved / 'Contents/Resources/compute'
            self.resources = self.compute / 'resources'
            self.startup_human = self.rows(['actions', 'assertions', 'asset_tags', 'term_verdicts'])
            for key in list(self.env):
                if key.endswith('_WORKER') or key in ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_WORKER_PYTHON'):
                    self.env.pop(key)
        self.process, self.log = self.start(self.lib.path)
        self.port = int(self.process.args[self.process.args.index('--port') + 1])
        self.origin = f'http://127.0.0.1:{self.port}'
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    test_app_startup_learning_recovery_keeps_human_history = (
        packaging_tests.InstalledLauncherResearchTest.test_existing_startup_learning_and_recovery_keep_human_history)

    def test_missing_component_and_bad_manifest_fail_before_app_work(self):
        self.stop(self.process)
        before = self.lib.db.read_bytes()
        manifest = self.compute / 'manifest.json'
        saved_manifest = manifest.read_bytes()
        runtime = self.resources / 'onnxruntime/libonnxruntime.dylib'
        saved_runtime = runtime.read_bytes()
        runtime.unlink()
        for expected, repair in (('libonnxruntime', lambda: runtime.write_bytes(saved_runtime)),
                                 ('manifest', lambda: manifest.write_bytes(saved_manifest))):
            if expected == 'manifest':
                manifest.write_text('invalid')
            result = subprocess.run([str(self.app_executable), 'http', '--native',
                '--library', str(self.lib.path), '--no-open'], env=self.env,
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertIn(expected, result.stderr)
            self.assertEqual(self.lib.db.read_bytes(), before)
            self.assertFalse(self.sentinel.exists())
            repair()
