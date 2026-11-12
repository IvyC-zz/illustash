"""tools/check_styleguide.py 的合成负例：在副本上故意弄错样例类名、改 app.css 不重跑 census 等，确认都会报错。"""
from contextlib import redirect_stdout
from pathlib import Path
import importlib.util
import io
import re
import shutil
import tempfile
import unittest
from tools.check_styleguide import APP_CSS, CENSUS, GUIDE, HOOK_CLASSES, ROOT, STATIC, check, main


class Tree(tempfile.TemporaryDirectory):
    """检查要读的文件的临时副本，每个用例只弄坏一处。"""

    def __enter__(self):
        self.root = Path(super().__enter__())
        shutil.copytree(ROOT / STATIC, self.root / STATIC, ignore=shutil.ignore_patterns('vendor', 'locales', '*.png', '*.svg'))
        (self.root / GUIDE).mkdir(parents=True)
        for name in ('index.html', 'styleguide.css', 'census.js'):
            shutil.copyfile(ROOT / GUIDE / name, self.root / GUIDE / name)
        return self

    def read(self, relative):
        return (self.root / relative).read_text(encoding='utf-8')

    def write(self, relative, text):
        (self.root / relative).write_text(text, encoding='utf-8')

    def replace(self, relative, old, new, count=1):
        text = self.read(relative)
        self.assertIn(old, text)
        self.write(relative, text.replace(old, new, count))

    def assertIn(self, needle, text):
        if needle not in text:
            raise AssertionError(f'副本里找不到 {needle!r}，用例需要更新')

    def errors(self):
        return check(self.root)

    def run_main(self):
        out = io.StringIO()
        with redirect_stdout(out):
            code = main(['--root', str(self.root)])
        return code, out.getvalue()


class StyleguideSyncTest(unittest.TestCase):
    def assertOne(self, errors, *needles):
        matching = [e for e in errors if all(n in e for n in needles)]
        self.assertTrue(matching, f'应有一条含 {needles!r} 的报错，实际为 {errors}')
        self.assertIn('改法：', matching[0])
        return matching[0]

    def test_repository_is_in_sync(self):
        self.assertEqual(check(), [])

    def test_clean_copy_has_no_errors(self):
        with Tree() as tree:
            self.assertEqual(tree.errors(), [])
            self.assertEqual(tree.run_main()[0], 0)

    def test_misspelt_sample_class_is_reported_with_line(self):
        with Tree() as tree:
            html = tree.read(GUIDE / 'index.html')
            line = html[:html.index('<button class="quiet">')].count('\n') + 1
            tree.replace(GUIDE / 'index.html', '<button class="quiet">', '<button class="quiett">')
            error = self.assertOne(tree.errors(), '.quiett', f'第 {line}', 'app.css 里没有')
            self.assertIn('[样式清单 · class]', error)
            code, out = tree.run_main()
            self.assertEqual(code, 1)
            self.assertIn('.quiett', out)

    def test_class_removed_from_app_css_is_reported(self):
        # 模拟 #337 一类的改名：app.css 里的选择器改了，样例没跟着改。
        with Tree() as tree:
            css = tree.read(APP_CSS)
            self.assertIsNotNone(re.search(r'\.link-button(?![\w-])', css))
            tree.write(APP_CSS, re.sub(r'\.link-button(?![\w-])', '.text-button', css))
            tree.write(GUIDE / 'census.js', CENSUS.render(tree.read(APP_CSS)))
            errors = tree.errors()
            self.assertOne(errors, '[样式清单 · class]', '.link-button')
            self.assertOne(errors, '[样式清单 · listed]', '.link-button')

    def test_listed_class_in_item_header_is_checked(self):
        with Tree() as tree:
            tree.replace(GUIDE / 'index.html', '<code>.quiet.small</code>', '<code>.quiet.tiny</code>')
            self.assertOne(tree.errors(), '[样式清单 · listed]', '.tiny')

    def test_unknown_shell_class_is_reported(self):
        with Tree() as tree:
            tree.replace(GUIDE / 'index.html', 'class="sg-lead"', 'class="sg-leed"')
            self.assertOne(tree.errors(), '页面外壳类 .sg-leed', 'styleguide.css')

    def test_app_css_change_without_census_rerun_is_reported(self):
        with Tree() as tree:
            tree.write(APP_CSS, tree.read(APP_CSS) + '.sg-test-only{gap:var(--space-4)}\n')
            error = self.assertOne(tree.errors(), '[样式清单 · census]', 'census.js 过期', 'python3 tools/styleguide/census.py')
            self.assertRegex(error, r'现在是 \d+ 行')
            # 按提示重跑之后就通过
            tree.write(GUIDE / 'census.js', CENSUS.render(tree.read(APP_CSS)))
            self.assertEqual(tree.errors(), [])

    def test_hand_edited_census_is_reported(self):
        with Tree() as tree:
            text = tree.read(GUIDE / 'census.js')
            tree.write(GUIDE / 'census.js', re.sub(r'"rules": \d+', '"rules": 1', text, count=1))
            self.assertOne(tree.errors(), '[样式清单 · census]', '手改')

    def test_missing_census_is_reported(self):
        with Tree() as tree:
            (tree.root / GUIDE / 'census.js').unlink()
            self.assertOne(tree.errors(), '缺少', 'census.js')

    def test_hook_class_gone_from_app_code_is_reported(self):
        name = 'tour-text'
        self.assertIn(name, HOOK_CLASSES)
        with Tree() as tree:
            path = STATIC / 'ui/guide.js'
            tree.write(path, tree.read(path).replace(name, 'tour-body'))
            self.assertOne(tree.errors(), f'HOOK_CLASSES 里的 .{name}', '找不到')

    def test_unused_hook_entry_is_reported(self):
        with Tree() as tree:
            tree.replace(GUIDE / 'index.html', 'class="tour-text"', 'class="tour-title"')
            self.assertOne(tree.errors(), 'HOOK_CLASSES 里的 .tour-text', '已不在')

    def test_importing_census_writes_nothing(self):
        out = ROOT / GUIDE / 'census.js'
        before = (out.read_bytes(), out.stat().st_mtime_ns)
        spec = importlib.util.spec_from_file_location('census_import_probe', ROOT / GUIDE / 'census.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual((out.read_bytes(), out.stat().st_mtime_ns), before)
        self.assertTrue(callable(module.render))

    def test_census_records_tokens_and_hash(self):
        data = CENSUS.build((ROOT / APP_CSS).read_text(encoding='utf-8'))
        self.assertRegex(data['sha256'], r'^[0-9a-f]{64}$')
        for family in ('fs', 'fsDisplay', 'radius', 'space', 'h', 'z'):
            self.assertTrue(data['tokens'][family], f'{family} 令牌为空：TOKENS-BEGIN/END 块变了？')
        self.assertTrue(all(t['name'].startswith('--z-') for t in data['tokens']['z']))
        self.assertEqual(data['themeVars']['groups'][0]['title'], '基础')


if __name__ == '__main__':
    unittest.main()
