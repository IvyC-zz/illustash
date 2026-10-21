"""#93: actual base artifact, isolated user, no review/shared/reference and no Python PATH."""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile
import unittest

from PIL import Image
from tests.test_packaging import build
from tests import test_native_lifecycle as lifecycle
from tests import test_native_folder as folder_tests
from tests.test_read_http import free_port, wait_until, SPAWN
from tools.verify_read_http import enter, fetch


class NativeFirstInstallTest(unittest.TestCase):
    setUpClass = classmethod(lifecycle.NativeLifecycleTest.setUpClass.__func__)
    stop = lifecycle.NativeLifecycleTest.stop
    request = lifecycle.NativeLifecycleTest.request
    finish = folder_tests.NativeFolderHTTPTest.finish
    sync = folder_tests.NativeFolderHTTPTest.sync

    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-first-install-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.install = self.root / 'base install'
        build.stage(self.install, self.binary.parent)
        self.binary = self.install / 'bin' / self.binary.name
        self.folder = self.root / '合成 图片'; self.folder.mkdir()
        Image.new('RGB', (31, 23), 'red').save(self.folder / 'red.png')
        self.before = hashlib.sha256((self.folder / 'red.png').read_bytes()).hexdigest()
        self.home = self.root / 'user'
        empty_path = self.root / 'empty-path'; empty_path.mkdir()
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith('ILLUSTASH_') and k not in ('HOME', 'USERPROFILE', 'PATH')}
        self.env.update(HOME=str(self.home), USERPROFILE=str(self.home), PATH=str(empty_path),
                        ILLUSTASH_APP_SUPPORT=str(self.root / 'support'))
        self.assertFalse((self.install / 'resources/review').exists())
        self.assertNotIn('ILLUSTASH_SHARED', self.env)
        self.assertFalse(list(self.root.rglob('reference.sqlite')))

    def create(self):
        dest = self.root / 'first.pilib'
        result = subprocess.run([str(self.binary), 'library-create', '--dest', str(dest),
                                 '--folder', str(self.folder)], cwd=self.home.parent,
                                env=self.env, capture_output=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((dest / 'library.json').is_file())
        with closing(sqlite3.connect(dest / 'source.sqlite')) as c:
            for table in ('assets', 'labels', 'decisions', 'characters', 'character_aliases', 'runs', 'embeddings'):
                self.assertEqual(c.execute(f'SELECT count(*) FROM {table}').fetchone()[0], 0)
        return dest

    def start(self, dest):
        port = free_port(); self.origin = f'http://127.0.0.1:{port}'
        log = tempfile.TemporaryFile(); self.addCleanup(log.close)
        process = subprocess.Popen([str(self.binary), 'http', '--library', str(dest), '--port', str(port),
                                    '--no-auto-sync', '--no-open', '--no-names', '--no-provenance',
                                    '--no-index-check'], cwd=self.install, env=self.env,
                                   stdout=log, stderr=log, **SPAWN)
        self.addCleanup(self.stop, process)
        enter(self.origin, log, process, timeout=8)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']

    def check_sync(self, dest):
        result = self.sync()
        # Recognition is optional and absent from the base install. Inventory/import must finish.
        self.assertEqual(result['state']['status'], 'needs_recognition', result)
        with closing(sqlite3.connect(dest / 'source.sqlite')) as c:
            self.assertEqual(c.execute('SELECT width,height,hidden,orig_size FROM assets').fetchall(),
                             [(31, 23, 0, (self.folder / 'red.png').stat().st_size)])
            self.assertEqual(c.execute('SELECT count(*) FROM files').fetchone()[0], 1)
        with closing(sqlite3.connect(dest / 'library.sqlite')) as c:
            self.assertEqual(c.execute('SELECT filename,width,height FROM assets').fetchall(), [('red.png', 31, 23)])
        self.assertEqual(hashlib.sha256((self.folder / 'red.png').read_bytes()).hexdigest(), self.before)
        self.assertFalse(list(self.root.rglob('reference.sqlite')))
        # Exercise the embeddings invalidation branch after the first successful import.
        Image.new('RGB', (19, 17), 'blue').save(self.folder / 'red.png')
        self.assertEqual(self.sync()['state']['status'], 'needs_recognition')
        with closing(sqlite3.connect(dest / 'library.sqlite')) as c:
            self.assertEqual(c.execute('SELECT width,height FROM assets').fetchall(), [(19, 17)])

    def test_base_install_cli_create_then_first_sync(self):
        dest = self.create()
        self.start(dest)
        self.check_sync(dest)

    def test_base_install_http_create_open_then_first_sync(self):
        first = self.create()  # Current host requires an explicit initial library; create it with installed CLI.
        self.start(first)
        response = self.request('/api/libraries/new', {'kind': 'folder', 'folder': str(self.folder), 'name': '合成 HTTP'})
        self.assertEqual(response['status'], 200, response['body'])
        dest = Path(json.loads(response['body'])['path'])
        response = self.request('/api/libraries/open', {'path': str(dest)})
        self.assertEqual(response['status'], 200, response['body'])
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.check_sync(dest)

    def test_base_install_photos_configuration_without_reference_or_photos_access(self):
        self.start(self.create())
        photos = self.home / 'Pictures' / 'Photos Library.photoslibrary'
        self.assertFalse(photos.exists())
        response = self.request('/api/libraries/new', {'kind': 'photos', 'name': '合成 Photos'})
        self.assertEqual(response['status'], 200, response['body'])
        dest = Path(json.loads(response['body'])['path'])
        with closing(sqlite3.connect(dest / 'source.sqlite')) as c:
            self.assertEqual(c.execute('SELECT z_pk FROM assets').fetchall(), [])
        self.assertFalse(photos.exists())
        self.assertFalse(list(self.root.rglob('reference.sqlite')))

    def test_http_present_incomplete_reference_rejected_without_registry_or_input_mutation(self):
        self.start(self.create())
        reference = self.install / 'resources/review/reference.sqlite'
        reference.parent.mkdir()
        # Build only owned synthetic input, then remove exactly one synchronization dependency.
        from tests.synthetic_source import synthetic_reference
        synthetic_reference(reference.parent)
        with closing(sqlite3.connect(reference)) as c:
            c.execute('ALTER TABLE assets DROP COLUMN hidden'); c.commit()
        before = reference.read_bytes()
        registry = self.root / 'support/libraries.json'
        registry_before = registry.read_bytes()
        response = self.request('/api/libraries/new', {'kind': 'folder', 'folder': str(self.folder), 'name': 'rejected'})
        self.assertEqual(response['status'], 400)
        self.assertEqual(before, reference.read_bytes())
        self.assertEqual(registry_before, registry.read_bytes())
        self.assertFalse((self.home / 'Pictures/Illustash/rejected.pilib').exists())

    def test_http_create_with_blocked_registry_reports_the_published_package(self):
        # #97: only listing it failed. The answer names the package so the page offers it instead of a second one.
        self.start(self.create())
        registry = self.root / 'support/libraries.json'
        before = registry.read_bytes()
        (self.root / 'support/libraries.tmp').mkdir()
        response = self.request('/api/libraries/new', {'kind': 'folder', 'folder': str(self.folder), 'name': '未登记'})
        self.assertEqual(response['status'], 200, response['body'])
        made = json.loads(response['body'])
        self.assertIs(made['listed'], False)
        self.assertTrue(made['error'])
        self.assertTrue((Path(made['path']) / 'library.json').is_file())
        self.assertEqual(registry.read_bytes(), before)
        (self.root / 'support/libraries.tmp').rmdir()
        response = self.request('/api/libraries/open', {'path': made['path']})
        self.assertEqual(response['status'], 200, response['body'])
        # Compare parsed paths: on Windows the registry's JSON escapes every backslash.
        self.assertIn(made['path'], [entry['path'] for entry in json.loads(registry.read_text(encoding='utf-8'))['recent']])

    def test_opening_with_blocked_registry_still_opens_and_says_why(self):
        # #176: listing only spares picking the library again next time; it opens even when the registry cannot be
        # written, and the libraries answer says why once. Without a failure the answer carries no such field.
        self.start(self.create())
        self.assertNotIn('unlisted', json.loads(self.request('/api/libraries')['body']))
        response = self.request('/api/libraries/new', {'kind': 'folder', 'folder': str(self.folder), 'name': '另一个'})
        self.assertEqual(response['status'], 200, response['body'])
        made = json.loads(response['body'])
        registry = self.root / 'support/libraries.json'
        before = registry.read_bytes()
        (self.root / 'support/libraries.tmp').mkdir()
        response = self.request('/api/libraries/open', {'path': made['path']})
        self.assertEqual(response['status'], 200, response['body'])
        info = json.loads(self.request('/api/libraries')['body'])
        self.assertEqual(Path(info['current']).resolve(), Path(made['path']).resolve())
        self.assertTrue(info['unlisted'])
        self.assertEqual(registry.read_bytes(), before)
        (self.root / 'support/libraries.tmp').rmdir()

    def test_existing_user_creating_another_library_keeps_the_first_and_its_guide(self):
        # #97: a second library starts its own guide; the first library, its sources and its guide stay as they were.
        first = self.create()
        self.start(first)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(self.request('/api/onboarding', {'action': 'done'})['status'], 200)
        guide = json.loads(self.request('/api/onboarding')['body'])['guide']
        state = lambda: ((first / 'library.json').read_bytes(), lifecycle.dump(first / 'library.sqlite'),
                         lifecycle.dump(first / 'source.sqlite'))
        kept = state()
        pictures = self.root / '第二批'; pictures.mkdir()
        Image.new('RGB', (13, 11), 'green').save(pictures / 'green.png')
        response = self.request('/api/libraries/new', {'kind': 'folder', 'folder': str(pictures), 'name': '第二个'})
        self.assertEqual(response['status'], 200, response['body'])
        second = json.loads(response['body'])['path']
        self.assertNotIn('listed', json.loads(response['body']))
        self.assertEqual(state(), kept)
        self.assertEqual(json.loads(self.request('/api/onboarding')['body'])['guide'], guide)
        self.assertEqual(self.request('/api/libraries/open', {'path': second})['status'], 200)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/onboarding')['body'])['guide']['status'], 'new')
        self.assertEqual(self.request('/api/libraries/open', {'path': str(first)})['status'], 200)
        self.token = json.loads(self.request('/api/bootstrap')['body'])['token']
        self.assertEqual(json.loads(self.request('/api/onboarding')['body'])['guide'], guide)
