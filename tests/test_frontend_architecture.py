"""tools/check_frontend.py 的合成负例：在 web/static 的副本上故意违反每条规则，确认会报错。"""
from contextlib import redirect_stdout
from pathlib import Path
import io
import json
import shutil
import subprocess
import tempfile
import unittest
from tools.check_frontend import BASELINE, ROOT, STATIC, baseline_growth, check, main, strip_comments


class Tree(tempfile.TemporaryDirectory):
    """检查所读文件的临时副本，每个用例在副本上只违反一条规则。"""

    def __enter__(self):
        self.root = Path(super().__enter__())
        base = self.root / STATIC
        base.mkdir(parents=True)
        shutil.copyfile(ROOT / STATIC / 'app.js', base / 'app.js')
        for folder in ('core', 'ui'):
            shutil.copytree(ROOT / STATIC / folder, base / folder)
        (self.root / BASELINE).parent.mkdir(parents=True)
        shutil.copyfile(ROOT / BASELINE, self.root / BASELINE)
        return self

    def append(self, relative, text):
        path = self.root / STATIC / relative
        path.write_text(path.read_text(encoding='utf-8') + text, encoding='utf-8')

    def errors(self):
        return check(self.root)[0]

    def baseline(self):
        return json.loads((self.root / BASELINE).read_text(encoding='utf-8'))['rules']

    def run_main(self, *args):
        out = io.StringIO()
        with redirect_stdout(out):
            code = main(['--root', str(self.root), *args])
        return code, out.getvalue()


class FrontendArchitectureTest(unittest.TestCase):
    def assertViolation(self, tree, rule, needle=''):
        errors = tree.errors()
        matching = [e for e in errors if f'· {rule}]' in e and needle in e]
        self.assertTrue(matching, f'expected a {rule} error containing {needle!r}, got {errors}')
        # 每条报错都写明违反的约定和改法
        self.assertIn('约定：', matching[0])
        self.assertIn('改法：', matching[0])
        return matching[0]

    def test_repository_matches_baseline(self):
        errors, _ = check()
        self.assertEqual(errors, [])

    def test_clean_copy_has_no_errors(self):
        with Tree() as tree:
            self.assertEqual(tree.errors(), [])

    def test_core_must_not_import_ui(self):
        with Tree() as tree:
            tree.append('core/format.js', "\nimport {toast} from '../ui/dom.js';\n")
            self.assertViolation(tree, 'import-core-ui', 'core/format.js -> ui/dom.js')

    def test_core_must_not_import_the_entry(self):
        with Tree() as tree:
            tree.append('core/format.js', "\nimport '/app.js';\n")
            self.assertViolation(tree, 'import-core-ui', 'core/format.js -> app.js')

    def test_new_cycle_through_a_leaf_module(self):
        with Tree() as tree:
            # ui/dom.js 被环里的模块导入，自己不导入其中任何一个；反过来导入就闭合出新的环。
            tree.append('ui/dom.js', "\nimport {renderGrid} from './grid.js';\n")
            self.assertViolation(tree, 'import-cycle', 'ui/dom.js -> ui/grid.js')

    def test_new_edge_inside_an_existing_cycle(self):
        with Tree() as tree:
            baseline = {e for e in tree.baseline()['import-cycle']}
            self.assertNotIn('ui/artists.js -> ui/tasks.js', baseline)
            tree.append('ui/artists.js', "\nimport {taskTick} from './tasks.js';\n")
            self.assertViolation(tree, 'import-cycle', 'ui/artists.js -> ui/tasks.js')

    def test_dynamic_and_reexport_imports_count(self):
        with Tree() as tree:
            tree.append('core/format.js', "\nexport {toast} from '../ui/dom.js';\n")
            self.assertViolation(tree, 'import-core-ui')
        with Tree() as tree:
            tree.append('core/format.js', "\nconst later = () => import('../ui/dom.js');\n")
            self.assertViolation(tree, 'import-core-ui')

    def test_undeclared_state_field(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', '\nstate.brandNewField = 1;\n')
            message = self.assertViolation(tree, 'undeclared-state', 'brandNewField')
            self.assertIn('core/state.js', message)

    def test_state_field_in_template_expression(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', '\nconst label = `${state.alsoNew} items`;\n')
            self.assertViolation(tree, 'undeclared-state', 'alsoNew')

    def test_declared_state_field_is_fine(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', '\nstate.total = 1;\n')
            tree.append('core/state.js', '')
            self.assertEqual(tree.errors(), [])

    def test_declaring_a_field_in_state_js_fixes_it(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', '\nstate.brandNewField = 1;\n')
            self.assertTrue(tree.errors())
            path = tree.root / STATIC / 'core/state.js'
            path.write_text(path.read_text(encoding='utf-8').replace('route: {page', 'brandNewField: 0, route: {page', 1),
                            encoding='utf-8')
            self.assertEqual(tree.errors(), [])

    def test_state_in_comments_and_strings_is_ignored(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', "\n// state.inComment = 1\n/* state.inBlock */\nconst s = 'state.inString';\n"
                                       "import {state as unused} from './state.js';\n")
            self.assertEqual([e for e in tree.errors() if 'undeclared-state' in e], [])

    def test_new_document_listener_in_a_new_file_position(self):
        for event in ('keydown', 'click'):
            with self.subTest(event=event), Tree() as tree:
                tree.append('ui/artists.js', f"\ndocument.addEventListener('{event}', e => {{}});\n")
                self.assertViolation(tree, 'document-listener', f'ui/artists.js::{event}')

    def test_extra_listener_in_a_file_that_already_has_some(self):
        with Tree() as tree:
            tree.append('ui/settings.js', "\ndocument.addEventListener('click', async e => {});\n")
            self.assertViolation(tree, 'document-listener', 'ui/settings.js::click')

    def test_window_and_property_listeners_count_too(self):
        with Tree() as tree:
            tree.append('ui/artists.js', '\nwindow.addEventListener("keydown", e => {});\n')
            self.assertViolation(tree, 'document-listener', 'ui/artists.js::keydown')
        with Tree() as tree:
            tree.append('ui/artists.js', '\ndocument.onclick = () => {};\n')
            self.assertViolation(tree, 'document-listener', 'ui/artists.js::click')

    def test_body_and_more_events_count_too(self):
        cases = ["document.body.addEventListener('click', e => {});",
                 "document.documentElement.addEventListener('keydown', e => {});",
                 "document.addEventListener('pointerdown', e => {});",
                 "document.addEventListener('mousedown', e => {});",
                 "document.addEventListener('keyup', e => {});"]
        for text in cases:
            with self.subTest(text=text), Tree() as tree:
                tree.append('ui/artists.js', '\n' + text + '\n')
                self.assertViolation(tree, 'document-listener', 'ui/artists.js::')

    def test_dialog_markup_in_a_string_counts(self):
        for text in ("el.innerHTML = '<dialog class=\"x\"></dialog>';", "el.insertAdjacentHTML('beforeend', `<dialog>${x}</dialog>`);"):
            with self.subTest(text=text), Tree() as tree:
                tree.append('ui/home.js', '\n' + text + '\n')
                self.assertViolation(tree, 'dialog-element', 'ui/home.js')

    def test_other_events_and_element_listeners_are_fine(self):
        with Tree() as tree:
            tree.append('ui/artists.js', "\ndocument.addEventListener('change', e => {});\n"
                                         "document.addEventListener('pointermove', e => {});\n"
                                         "button.addEventListener('keydown', e => {});\n"
                                         "// document.addEventListener('keydown', e => {});\n")
            self.assertEqual(tree.errors(), [])

    def test_new_runtime_dialog(self):
        with Tree() as tree:
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")
            message = self.assertViolation(tree, 'dialog-element', 'ui/home.js')
            self.assertIn('ui/dialog.js', message)
        with Tree() as tree:  # 本来就有基线的文件（palette.js 登记了 2 处）
            tree.append('ui/palette.js', '\nconst d = document.createElement("dialog");\n')
            self.assertViolation(tree, 'dialog-element', 'ui/palette.js')

    def test_long_line_boundary(self):
        with Tree() as tree:
            tree.append('ui/home.js', '\nconst ok = "' + 'x' * (200 - len('const ok = "";')) + '";\n')
            self.assertEqual(tree.errors(), [])
            tree.append('ui/home.js', 'const tooLong = "' + 'x' * 190 + '";\n')
            message = self.assertViolation(tree, 'long-line', 'ui/home.js')
            self.assertIn('200', message)

    def test_long_comment_line_counts(self):
        with Tree() as tree:
            tree.append('core/format.js', '\n// ' + 'word ' * 50 + '\n')
            self.assertViolation(tree, 'long-line', 'core/format.js')

    def test_baseline_is_not_keyed_by_line_number(self):
        with Tree() as tree:
            # 在文件开头插入无关行、让所有行号后移，检查结果不变
            path = tree.root / STATIC / 'ui/settings.js'
            path.write_text('// one\n// two\n// three\n' + path.read_text(encoding='utf-8'), encoding='utf-8')
            self.assertEqual(tree.errors(), [])

    def test_fixed_violation_is_reported_as_stale_not_failure(self):
        with Tree() as tree:
            path = tree.root / STATIC / 'ui/palette.js'
            path.write_text(path.read_text(encoding='utf-8').replace("document.createElement('dialog')", "document.createElement('div')"),
                            encoding='utf-8')
            errors, stale = check(tree.root)
            self.assertEqual(errors, [])
            self.assertTrue(any('dialog-element' in line and 'ui/palette.js' in line for line in stale))
            code, output = tree.run_main('--strict-stale')
            self.assertEqual(code, 1)
            self.assertIn('--shrink', output)

    def test_shrink_lowers_but_never_adds(self):
        with Tree() as tree:
            palette = tree.root / STATIC / 'ui/palette.js'
            palette.write_text(palette.read_text(encoding='utf-8').replace("document.createElement('dialog')", "document.createElement('div')"),
                               encoding='utf-8')
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")  # 一处新违规
            before = tree.baseline()
            code, _ = tree.run_main('--shrink')
            after = tree.baseline()
            self.assertEqual(code, 1, 'the new violation still fails after shrinking')
            self.assertNotIn('ui/palette.js', after['dialog-element'])
            self.assertNotIn('ui/home.js', after['dialog-element'])
            self.assertEqual(baseline_growth(after, before), [])

    def test_baseline_growth_is_detected(self):
        reference = {'dialog-element': {'ui/a.js': 2}, 'long-line': {'ui/a.js': 5}}
        self.assertEqual(baseline_growth(reference, reference), [])
        self.assertEqual(baseline_growth({'dialog-element': {'ui/a.js': 1}}, reference), [])
        self.assertTrue(baseline_growth({'dialog-element': {'ui/a.js': 3}}, reference))
        self.assertTrue(baseline_growth({'dialog-element': {'ui/b.js': 1}}, reference))
        self.assertTrue(baseline_growth({'undeclared-state': {'x': 1}}, reference))

    def test_base_ref_rejects_a_grown_baseline(self):
        with Tree() as tree:
            def git(*args):
                subprocess.run(['git', '-c', 'user.name=t', '-c', 'user.email=t@example.invalid', *args],
                               cwd=tree.root, check=True, capture_output=True)
            git('init', '-q')
            git('add', str(BASELINE))
            git('commit', '-q', '-m', 'baseline')
            code, _ = tree.run_main('--base-ref', 'HEAD')
            self.assertEqual(code, 0)
            # 把新违规写进基线来“让它通过”
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")
            grown = tree.baseline()
            grown['dialog-element']['ui/home.js'] = 1
            (tree.root / BASELINE).write_text(json.dumps({'rules': grown}), encoding='utf-8')
            self.assertEqual(tree.errors(), [])
            code, output = tree.run_main('--base-ref', 'HEAD')
            self.assertEqual(code, 1)
            self.assertIn('基线变大', output)

    def test_local_and_ci_runs_are_strict(self):
        # 本地全量与 ci.yml 保持一致：都带 --strict-stale；本地另带 --base-ref。
        script = (ROOT / 'tools/check_all.sh').read_text(encoding='utf-8')
        workflow = (ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        line = next(l for l in script.splitlines() if 'tools/check_frontend.py' in l and l.startswith('run '))
        self.assertIn('--strict-stale', line)
        self.assertIn('--base-ref', line)
        self.assertIn('check_frontend.py --strict-stale', workflow)

    def test_missing_baseline_is_an_error(self):
        with Tree() as tree:
            (tree.root / BASELINE).unlink()
            self.assertIn('frontend_baseline.json', tree.errors()[0])

    def test_scanner_handles_strings_regexes_and_templates(self):
        src = ("const a = '//x'; // tail\n"
               "const b = /\\/\\/[/]/g.test(x) ? 1 : 2 / 3; /* block\nspans */ state.real = `${state.inTemplate}//kept`;\n")
        code = strip_comments(src)
        self.assertEqual(code.count('\n'), src.count('\n'))
        self.assertIn("'//x'", code)
        self.assertNotIn('tail', code)
        self.assertNotIn('block', code)
        self.assertIn('state.inTemplate', code)
        self.assertIn('//kept', code)
        bare = strip_comments(src, strings=False)
        self.assertNotIn('//x', bare)
        self.assertIn('state.inTemplate', bare)
        with self.assertRaises(ValueError):
            strip_comments('const t = `unterminated')


if __name__ == '__main__':
    unittest.main()
