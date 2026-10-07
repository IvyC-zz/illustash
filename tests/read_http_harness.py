"""Read-route byte contract on synthetic databases, against fixed expectations (#9 stage 3).

The core read routes (`http_read::read` with the application's response encoding) are served by
the test-only `read-http` example for whatever library context the test publishes to a state
file. Their bytes are compared with the former Python reference server's responses to the same
requests on the same databases, recorded once through ``tests/fixed_expectations.py``
(#9; the reference is gone, so they cannot be recorded again). The databases themselves are recorded as
packages of the temporary root after every reference-side change and restored before the next
request, so no Python reference code runs in the default mode.
"""
import json
import os
from pathlib import Path
import secrets
import shutil
import sqlite3
import subprocess
import tempfile
import threading
import unittest
from contextlib import closing
from urllib.parse import quote

from tests.fixed_expectations import Expectations
from tests.test_read_http import free_port, stop_process, wait_until
from tools import verify_read_http as verify

ROOT = Path(__file__).resolve().parents[1]
HEADERS = ('Content-Type', 'Content-Length', 'Cache-Control', 'Vary', 'X-Content-Type-Options',
           'Referrer-Policy', 'Content-Security-Policy')


def binary():
    configured = os.environ.get('ILLUSTASH_READ_HTTP_BIN')
    if configured:
        path = Path(configured)
    else:
        terms = os.environ.get('ILLUSTASH_TERMS_BIN')
        if not terms:
            raise unittest.SkipTest('set ILLUSTASH_TERMS_BIN or ILLUSTASH_READ_HTTP_BIN explicitly; skip is not acceptance')
        path = Path(terms).parent / 'examples' / ('read-http.exe' if os.name == 'nt' else 'read-http')
    if not path.is_file():
        raise RuntimeError('Build the test read host: cargo build --release --manifest-path native/terms/Cargo.toml '
                           '--features test-read-host --example read-http')
    return path


def publish(rec, server, state):
    """Atomically publish the request context (formerly the reference server's readiness file)."""
    # The session token is recorded as is: response lengths depend on it.
    context = rec.value('context')
    temp = state.with_suffix('.new')
    temp.write_text(json.dumps({**context, 'generation': secrets.token_hex(16),
                                'tags': str(ROOT / 'tags' / 'tags_v3.csv')}), encoding='utf-8')
    os.replace(temp, state)
    return context


class HTTPHarness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = binary()
        cls.temp = tempfile.TemporaryDirectory(prefix='illustash-r12-')
        cls.root = Path(cls.temp.name).resolve()
        cls.state_dir = tempfile.TemporaryDirectory(prefix='illustash-r12-state-')
        cls.state = Path(cls.state_dir.name) / 'state.json'
        cls.origin = f'http://127.0.0.1:{free_port()}'
        cls.rec = rec = Expectations(cls)
        rec.alias(cls.root, '@ROOT@')
        rec.alias(cls.origin, '@ORIGIN@')
        cls.server = cls.store = None
        rec.package('root', cls.root, library=False)
        cls.context = publish(rec, cls.server, cls.state)
        cls.log = tempfile.TemporaryFile()
        cls.host = subprocess.Popen([str(cls.binary), '--port', cls.origin.rsplit(':', 1)[1], '--state', str(cls.state)],
                                    stdout=cls.log, stderr=cls.log)
        wait_until(lambda: verify.fetch(cls.origin, {'url': '/api/fold'}, cls.origin)['status'] == 200)

    @classmethod
    def tearDownClass(cls):
        if os.name == 'nt' and cls.host.poll() is None:
            cls.host.terminate()   # no console signal handling in the test host
            cls.host.wait(12)
        stop_process(cls.host)
        cls.log.close()
        cls.temp.cleanup()
        cls.state_dir.cleanup()
        if not False and cls.rec.position != len(cls.rec.entries):
            raise AssertionError(f'{cls.rec.test}: recorded reference values not reached')

    def setUp(self):
        self.rec = Expectations(self)
        self.addCleanup(self.rec.finish, self)
        for actual, placeholder in type(self).rec.aliases:
            self.rec.alias(actual, placeholder)
        # Each test starts from the recorded root and context, independent of earlier tests.
        self.sync('start')
        self.publish()

    @property
    def db(self):
        return Path(self.context['db'])

    def connect(self, path=None):
        return closing(sqlite3.connect(path or self.db))

    def publish(self):
        type(self).context = publish(self.rec, self.server, self.state)

    def sync(self, label='changed'):
        """After a reference-side change: record the temporary root, or restore it."""
        self.rec.package(label, self.root, library=False)

    def reference(self, item):
        return self.rec.value('response')

    def pair(self, path, route='core', headers=None):
        item = {'url': quote(path, safe='/?&=;%+#'), 'headers': headers or {}}
        a, b = self.reference(item), verify.fetch(self.origin, item, self.origin)
        self.assertIsNone(verify.difference(a, b), (path, verify.difference(a, b)))
        self.assertEqual(b['route'], route, path)
        for h in HEADERS:
            self.assertEqual(a['headers'].get(h), b['headers'].get(h), (path, h))
        return a

    def corpus(self, *, sample, query_limit, keep=lambda item: True):
        return self.rec.value('corpus')

    def compare(self, requests, require_core=()):
        """The verify_read_http report shape, with the reference side recorded."""
        expected = self.rec.value('responses')
        report = {'compared': 0, 'differ': 0, 'core_equal': 0, 'stages': set(), 'differences': []}
        for item, left in zip(requests, expected, strict=True):
            report['compared'] += 1
            report['stages'].add(item.get('stage'))
            right = verify.fetch(self.origin, item, self.origin)
            diff = verify.difference(left, right)
            if diff is None and right['route'] == 'core':
                report['core_equal'] += 1
            elif diff is None:
                diff = {'routing': right['route']}
            if diff is not None:
                report['differ'] += 1
                report['differences'].append({'request': item, **diff})
        for stage in require_core:
            if stage not in report['stages']:
                report['differ'] += 1
                report['differences'].append({'stage': stage, 'error': '核心验收阶段没有任何请求'})
        return report
