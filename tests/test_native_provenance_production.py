"""安装后的溯源适配器：本地步骤以原生方式运行，不用 Python。

复用显式探针的测试套件，把候选 illustash-worker 以安装后的名字复制过去。需要
ILLUSTASH_TERMS_BIN（取它旁边的 worker）或 ILLUSTASH_PROVENANCE_ADAPTER_BIN；只有未知步骤才会交给
Python worker。
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import unittest
from unittest.mock import patch

from tests import test_native_provenance_local as local
from tests import test_native_provenance_match as match
from tests import test_native_provenance_verify as verify


def candidate_worker():
    configured = os.environ.get('ILLUSTASH_PROVENANCE_ADAPTER_BIN')
    host = os.environ.get('ILLUSTASH_TERMS_BIN')
    if not configured and not host:
        raise unittest.SkipTest('explicit candidate host and its built worker required')
    worker = Path(configured or Path(host).with_name('illustash-worker.exe' if os.name == 'nt' else 'illustash-worker'))
    if not worker.is_file():
        raise AssertionError('candidate worker missing; do not substitute an older binary')
    return worker.resolve()


class Installed:
    @classmethod
    def setUpClass(cls):
        cls.candidate_worker = candidate_worker()

    def setUp(self):
        super().setUp()
        # 不配置解释器：原生步骤绝不能落到 Python 兜底。
        environment = patch.dict(os.environ)
        environment.start()
        self.addCleanup(environment.stop)
        os.environ.pop('ILLUSTASH_WORKER_PYTHON', None)
        folder = self.root / 'installed-bin'
        folder.mkdir()
        self.binary = folder / ('illustash-provenance.exe' if os.name == 'nt' else 'illustash-provenance')
        shutil.copy2(self.candidate_worker, self.binary)


skip_linux = unittest.skipUnless(sys.platform in ('darwin', 'win32'), 'Linux is frozen: no local computation')


@skip_linux
class InstalledLocalProvenanceTest(Installed, local.NativeLocalProvenanceTest):
    setUpClass = classmethod(Installed.setUpClass.__func__)


@skip_linux
class InstalledProvenanceMatchTest(Installed, match.NativeProvenanceMatchTest):
    @classmethod
    def setUpClass(cls):
        cls._missing_numeric_candidate = None
        try:
            Installed.setUpClass.__func__(cls)
        except unittest.SkipTest as error:
            if not match.numeric_reference_mode.defer_missing(cls, error):
                raise


@skip_linux
class InstalledProvenanceVerifyTest(Installed, verify.NativeProvenanceVerifyTest):
    setUpClass = classmethod(Installed.setUpClass.__func__)


class _FixtureOnly(unittest.TestCase):
    setUp = local.NativeLocalProvenanceTest.setUp
    fixture_local = local.NativeLocalProvenanceTest.fixture_local


@skip_linux
class InstalledProvenanceRoutingTest(Installed, _FixtureOnly):
    setUpClass = classmethod(Installed.setUpClass.__func__)

    def test_unknown_step_is_refused(self):
        request, _ = self.fixture_local('groups')
        for step in ('not-a-local-step',):
            with self.subTest(step=step):
                incoming, output = self.root / f'{step}-request.json', self.root / f'{step}-output.json'
                incoming.write_text(json.dumps(dict(request, step=step)), encoding='utf-8')
                run = subprocess.run([str(self.binary), str(incoming), str(output)], env=os.environ | {'PATH': ''},
                                     capture_output=True, timeout=30, start_new_session=os.name != 'nt')
                self.assertNotEqual(run.returncode, 0)
                self.assertIn('不支持的计算请求', run.stderr.decode(errors='replace'))
                self.assertFalse(output.exists())


def load_tests(loader, tests, pattern):
    # 只跑安装后的这几个类；导入的基础套件由各自的模块运行。
    suite = unittest.TestSuite()
    for case in (InstalledLocalProvenanceTest, InstalledProvenanceMatchTest,
                 InstalledProvenanceVerifyTest, InstalledProvenanceRoutingTest):
        suite.addTests(loader.loadTestsFromTestCase(case))
    return suite


if __name__ == '__main__':
    unittest.main()
