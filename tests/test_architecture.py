"""Synthetic negative manifests verify architecture enforcement, including aliases/cfg/dev/build."""
from pathlib import Path
import shutil
import tempfile
import unittest
from tools.check_architecture import ROOT, MANIFESTS, check


class ArchitectureTest(unittest.TestCase):
    def test_repository(self):
        self.assertEqual(check(), [])

    def test_forbidden_reverse_dependency_in_all_tables(self):
        cases = [('core', 'dependencies', 'illustash-platform', '../../crates/platform'),
                 ('platform', 'dev-dependencies', 'illustash-terms', '../../native/terms'),
                 ('core', 'target.\'cfg(windows)\'.build-dependencies', 'tiny_http', None),
                 ('platform', 'dependencies', 'tauri', None),
                 ('desktop', 'dependencies', 'illustash-core', '../../crates/core')]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for relative in MANIFESTS.values():
                dest = root / relative
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / relative, dest)
            for owner, table, package, path in cases:
                with self.subTest(owner=owner, table=table, package=package):
                    dest = root / MANIFESTS[owner]
                    original = dest.read_text()
                    suffix = f'\n[{table}]\n' if table not in ('dependencies', 'dev-dependencies') or f'[{table}]' not in original else None
                    declaration = f'forbidden_alias = {{ package = "{package}", ' + (f'path = "{path}"' if path else 'version = "0.1"') + ' }\n'
                    if suffix:
                        mutated = original + suffix + declaration
                    else:
                        mutated = original.replace(f'[{table}]', f'[{table}]\n{declaration}', 1)
                    dest.write_text(mutated)
                    self.assertTrue(check(root))
                    dest.write_text(original)
            self.assertEqual(check(root), [])
