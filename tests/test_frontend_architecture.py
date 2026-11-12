"""Synthetic negative cases for tools/check_frontend.py: deliberately break each rule on a copy of web/static."""
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
    """A scratch copy of the files the check reads, so a case can break one rule at a time."""

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
        # every report names the convention and how to fix it
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
            # ui/dom.js is imported by the cycle but imports none of it; importing back closes a new loop.
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
            tree.append('ui/tasks.js', '\nif (state.total) go();\n')
            tree.append('core/state.js', '')
            self.assertEqual(tree.errors(), [])

    def test_declaring_a_field_in_state_js_fixes_it(self):
        with Tree() as tree:
            tree.append('ui/tasks.js', '\nstate.brandNewField = 1;\n')
            self.assertTrue(tree.errors())
            path = tree.root / STATIC / 'core/state.js'
            # 声明在一个写明 ui/tasks.js 是写入者的组里，规则 7 也不报
            path.write_text(path.read_text(encoding='utf-8').replace('  // ---- 路由', '  // ---- 任务面板：ui/tasks.js ----\n  brandNewField: 0,\n  // ---- 路由', 1),
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

    def test_the_overlay_manager_is_the_one_allowed_entry(self):
        # 用户 2026-11-12 在 #343 批准：ui/overlay.js 是全局 keydown / click 监听的唯一入口，别的文件照旧报错。
        with Tree() as tree:
            overlay = (tree.root / STATIC / 'ui/overlay.js').read_text(encoding='utf-8')
            self.assertIn("document.addEventListener('keydown'", overlay)
            self.assertIn("document.addEventListener('click'", overlay)
            tree.append('ui/overlay.js', "\ndocument.addEventListener('click', e => {});\nwindow.addEventListener('keydown', e => {});\n")
            self.assertEqual([e for e in tree.errors() if 'document-listener' in e], [])
        with Tree() as tree:
            tree.append('ui/dialog.js', "\ndocument.addEventListener('keydown', e => {});\n")
            self.assertViolation(tree, 'document-listener', 'ui/dialog.js::keydown')

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

    def test_other_events_and_element_listeners_are_fine(self):
        with Tree() as tree:
            tree.append('ui/artists.js', "\ndocument.addEventListener('change', e => {});\n"
                                         "document.body.addEventListener('click', e => {});\n"
                                         "button.addEventListener('keydown', e => {});\n"
                                         "// document.addEventListener('keydown', e => {});\n")
            self.assertEqual(tree.errors(), [])

    def test_new_runtime_dialog(self):
        with Tree() as tree:
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")
            message = self.assertViolation(tree, 'dialog-element', 'ui/home.js')
            self.assertIn('ui/dialog.js', message)
        with Tree() as tree:  # 基线里已经登记了存量的文件（ui/dialog.js 登记 1 处，openDialog 内的那一处）
            tree.append('ui/dialog.js', '\nconst d = document.createElement("dialog");\n')
            self.assertViolation(tree, 'dialog-element', 'ui/dialog.js')

    def test_new_page_name_comparison(self):
        # 页面名判断只能写在 core/pages.js 的注册表里；新增比较要报错，改写成读注册表字段
        with Tree() as tree:
            tree.append('ui/palette.js', "\nif (state.route.page === 'library') go();\n")
            message = self.assertViolation(tree, 'page-name', 'ui/palette.js')
            self.assertIn('core/pages.js', message)
        with Tree() as tree:
            tree.append('ui/palette.js', "\nif (['a', 'b'].includes(state.route.page)) go();\n")
            self.assertViolation(tree, 'page-name', 'ui/palette.js')
        with Tree() as tree:  # 注册表自己不受限，读注册表字段不算
            tree.append('core/pages.js', "\nconst x = r.page === 'home';\n")
            tree.append('ui/palette.js', "\nif (viewOf(state.route).click === 'cleanup' && onPage(state.route, 'home')) go();\n")
            self.assertEqual(tree.errors(), [])

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
            # shifting every line of a file by prepending unrelated lines changes nothing
            path = tree.root / STATIC / 'ui/settings.js'
            path.write_text('// one\n// two\n// three\n' + path.read_text(encoding='utf-8'), encoding='utf-8')
            self.assertEqual(tree.errors(), [])

    def test_fixed_violation_is_reported_as_stale_not_failure(self):
        with Tree() as tree:
            # 改掉基线里仍登记着的违规（ui/dialog.js 的那一处），应报「过期」而不是失败
            path = tree.root / STATIC / 'ui/dialog.js'
            path.write_text(path.read_text(encoding='utf-8').replace("document.createElement('dialog')", "document.createElement('div')"),
                            encoding='utf-8')
            errors, stale = check(tree.root)
            self.assertEqual(errors, [])
            self.assertTrue(any('dialog-element' in line and 'ui/dialog.js' in line for line in stale))
            code, output = tree.run_main('--strict-stale')
            self.assertEqual(code, 1)
            self.assertIn('--shrink', output)

    def test_shrink_lowers_but_never_adds(self):
        with Tree() as tree:
            fixed = tree.root / STATIC / 'ui/dialog.js'
            fixed.write_text(fixed.read_text(encoding='utf-8').replace("document.createElement('dialog')", "document.createElement('div')"),
                             encoding='utf-8')
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")  # a new violation
            before = tree.baseline()
            code, _ = tree.run_main('--shrink')
            after = tree.baseline()
            self.assertEqual(code, 1, 'the new violation still fails after shrinking')
            self.assertNotIn('ui/dialog.js', after['dialog-element'])
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
            # "make the new violation pass" by writing it into the baseline
            tree.append('ui/home.js', "\nconst d = document.createElement('dialog');\n")
            grown = tree.baseline()
            grown['dialog-element']['ui/home.js'] = 1
            (tree.root / BASELINE).write_text(json.dumps({'rules': grown}), encoding='utf-8')
            self.assertEqual(tree.errors(), [])
            code, output = tree.run_main('--base-ref', 'HEAD')
            self.assertEqual(code, 1)
            self.assertIn('基线变大', output)

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


    # ---------- 规则 7：state 字段的写入者（#353） ----------
    def test_state_written_outside_its_writer(self):
        # cursor 的写入者是 ui/grid.js，focused 的是 ui/detail.js，route 的是 core/route.js
        cases = [('ui/home.js', 'state.cursor = 3;', 'state.cursor', 'ui/grid.js'),
                 ('ui/home.js', 'state.focused ??= null;', 'state.focused', 'ui/detail.js'),
                 ('ui/palette.js', 'state.dataVersion++;', 'state.dataVersion', 'ui/grid.js'),
                 ('ui/palette.js', '--state.generation;', 'state.generation', 'ui/grid.js'),
                 ('ui/shell.js', 'state.route.sort = "x";', 'state.route', 'core/route.js'),
                 ('ui/shell.js', 'state.sel.ids = new Set();', 'state.sel', 'ui/grid.js'),
                 ('ui/catalog.js', 'state.items[0] = null;', 'state.items', 'ui/grid.js'),
                 ('ui/catalog.js', 'Object.assign(state.route, {q: ""});', 'state.route', 'core/route.js'),
                 ('app.js', 'delete state.char;', 'state.char', 'core/meta.js')]
        for path, text, field, writer in cases:
            with self.subTest(text=text), Tree() as tree:
                tree.append(path, '\n' + text + '\n')
                message = self.assertViolation(tree, 'state-writer', f'{path}: {field}（声明的写入者是 {writer}）')
                self.assertIn(text, message)
                self.assertIn('core/state.js', message)

    def test_state_reads_and_the_writer_itself_are_fine(self):
        with Tree() as tree:
            tree.append('ui/home.js', '\nif (state.cursor === 3 || state.cursor >= 2 || state.cursor <= 1 || state.cursor !== 0) go(state.cursor == 1);\n'
                                      'const f = state.items.filter(x => x).length; state.pending.add(1); const s = "state.cursor = 3";\n'
                                      '// state.cursor = 3;\n')
            tree.append('ui/grid.js', '\nstate.cursor = 3; state.items[0] = null; state.sel.ids = new Set();\n')
            tree.append('core/route.js', '\nObject.assign(state.route, {q: ""});\n')
            self.assertEqual(tree.errors(), [])

    def test_state_groups_need_exactly_one_writer(self):
        state = 'core/state.js'
        with Tree() as tree:
            path = tree.root / STATIC / state
            text = path.read_text(encoding='utf-8')
            path.write_text(text.replace('// ---- 详情栏与大图：ui/detail.js ----', '// ---- 详情栏与大图：ui/detail.js、ui/home.js ----'), encoding='utf-8')
            self.assertViolation(tree, 'state-writer', '写了 2 个')
        with Tree() as tree:
            path = tree.root / STATIC / state
            text = path.read_text(encoding='utf-8')
            path.write_text(text.replace('// ---- 详情栏与大图：ui/detail.js ----', '// ---- 详情栏与大图 ----'), encoding='utf-8')
            self.assertViolation(tree, 'state-writer', '写了 0 个')
        with Tree() as tree:
            # 换了写入者：原来的写入者再赋值就报错，新写入者不报
            path = tree.root / STATIC / state
            text = path.read_text(encoding='utf-8')
            path.write_text(text.replace('// ---- 详情栏与大图：ui/detail.js ----', '// ---- 详情栏与大图：ui/home.js ----'), encoding='utf-8')
            message = self.assertViolation(tree, 'state-writer', 'ui/detail.js: state.focused（声明的写入者是 ui/home.js）')
            self.assertIn('setCursor', message)


if __name__ == '__main__':
    unittest.main()
