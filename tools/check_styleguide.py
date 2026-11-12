#!/usr/bin/env python3
"""样式清单页（tools/styleguide/）与实现的同步检查（#352，属 #316）。

清单页的样例 HTML 是照着 web/static/ui/*.js 的模板手写的，AGENTS.md 要求写界面前先看它、照它复用类名，
所以它一过期就会把代理带偏。本脚本只做便宜的机械检查，接在 tools/check_all.sh 和 ci.yml 里：

  1 census     census.js 必须等于 census.py 对当前 app.css 的输出（app.css 改了没重跑、手改 census.js 都会报）
  2 class      index.html 里元素用到的类，必须在 web/static/app.css 里有样式；
               页面外壳的类（sg- 前缀，以及 styleguide.css 里挂在 .sg-* 选择器上的修饰类）必须在 styleguide.css 里有定义；
               只给 JS 当钩子、本来就没有样式的类登记在 HOOK_CLASSES，并且必须仍出现在 web/static 的代码里
  3 listed     条目标题里 <span class="sg-cls"> 列出的 `.类名`，同样必须在 app.css 里有样式

不依赖 Node，也不启动浏览器。用法：python3 tools/check_styleguide.py [--root 目录]
"""
from html.parser import HTMLParser
from pathlib import Path
import argparse
import importlib.util
import re
import sys

ROOT = Path(__file__).resolve().parents[1]
APP_CSS = Path('web/static/app.css')
STATIC = Path('web/static')
GUIDE = Path('tools/styleguide')
CENSUS_FIX = '运行 `python3 tools/styleguide/census.py`，把改写后的 tools/styleguide/census.js 一起提交'
# 样例照抄了应用模板里的这些类：它们只给 JS 当选择器或钩子，app.css 里本来没有样式。
# 每项写明出处；应用代码里不再用它时检查会报，届时从这里和样例里一起删掉。
HOOK_CLASSES = {
    'c1': 'web/static/index.html 任务入口的图标',
    'diff': 'ui/palette.js 对比框（match / diff）',
    'more-control': 'ui/shell.js 更多菜单的点外关闭判断',
    'tagged-dialog': 'ui/catalog.js 打过标签的图对话框',
    'task-group': 'ui/tasks.js 任务列表分组',
    'tour-text': 'ui/guide.js 引导气泡正文',
    'work-dialog': 'ui/dialog.js、ui/catalog.js 作品对话框',
}
_CLASS_IN_SELECTOR = re.compile(r'\.(-?[_a-zA-Z][\w-]*)')


def _census_module():
    """按路径加载 tools/styleguide/census.py（tools/styleguide 不是包）。导入不会写文件。"""
    spec = importlib.util.spec_from_file_location('styleguide_census', ROOT / GUIDE / 'census.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


CENSUS = _census_module()


def selector_classes(css_text):
    """CSS 里所有选择器出现过的类名（含 @media 里的规则；声明体里的值不算）。"""
    found = set()
    for selector, _, _ in CENSUS.parse_rules(CENSUS.strip_comments(css_text)):
        found.update(_CLASS_IN_SELECTOR.findall(re.sub(r'\[[^\]]*\]', '', selector)))
    return found


def shell_modifiers(css_text):
    """styleguide.css 里和 .sg-* 写在同一条选择器里的非 sg- 类（如 `.sg-stage.col`、`.sg-table td.n`）。"""
    found = set()
    for selector, _, _ in CENSUS.parse_rules(CENSUS.strip_comments(css_text)):
        for part in selector.split(','):
            names = _CLASS_IN_SELECTOR.findall(re.sub(r'\[[^\]]*\]', '', part))
            if any(n.startswith('sg-') for n in names):
                found.update(n for n in names if not n.startswith('sg-'))
    return found


class _Page(HTMLParser):
    """收集 index.html 里每个元素的类名（带行号），以及 sg-cls 标题里 <code> 列出的类。"""

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.used = []      # [(类名, 行号)]
        self.listed = []    # [(类名, 行号)]
        self._depth = 0     # 位于 sg-cls 的 <span> 里时，未闭合的 <span> 层数；只数 span，不受省略结束标签影响
        self._code = None   # 正在读的 sg-cls <code> 的文字与行号

    def handle_starttag(self, tag, attrs):
        classes = (dict(attrs).get('class') or '').split()
        line = self.getpos()[0]
        self.used.extend((c, line) for c in classes)
        if tag == 'span' and (self._depth or 'sg-cls' in classes):
            self._depth += 1
        elif tag == 'code' and self._depth:
            self._code = ['', line]

    def handle_endtag(self, tag):
        if tag == 'span' and self._depth:
            self._depth -= 1
        elif tag == 'code' and self._code is not None:
            text, line = self._code
            # 只看类选择器；`#id`、元素名、`::backdrop` 之类不查。去掉属性选择器里的内容免得误读。
            self.listed.extend((c, line) for c in _CLASS_IN_SELECTOR.findall(re.sub(r'\[[^\]]*\]', '', text)))
            self._code = None

    def handle_data(self, data):
        if self._code is not None:
            self._code[0] += data


def _lines(lines):
    unique = sorted(set(lines))
    shown = '、'.join(str(n) for n in unique[:6])
    return shown + (f' 等 {len(unique)} 处' if len(unique) > 6 else '')


def check_census(root):
    path = root / GUIDE / 'census.js'
    raw = (root / APP_CSS).read_bytes().decode('utf-8')
    expected = CENSUS.render(raw)
    if not path.is_file():
        return [f'[样式清单 · census] 缺少 {GUIDE}/census.js。改法：{CENSUS_FIX}。']
    actual = path.read_bytes().decode('utf-8')
    if actual == expected:
        return []
    now = CENSUS.build(raw)
    recorded = re.search(r'"sha256":\s*"([0-9a-f]{64})"', actual)
    lines = re.search(r'"lines":\s*(\d+)', actual)
    if not recorded or recorded.group(1) != now['sha256']:
        was = f'{lines.group(1) if lines else "?"} 行、sha256 {recorded.group(1)[:12] if recorded else "（未记录）"}'
        return [f'[样式清单 · census] census.js 过期：统计时的 app.css 是 {was}，现在是 {now["lines"]} 行、'
                f'sha256 {now["sha256"][:12]}。app.css 改过之后没有重跑统计，清单页的令牌表会是旧的。改法：{CENSUS_FIX}。']
    return [f'[样式清单 · census] census.js 与 census.py 现在的输出不一致，但 app.css 没变：census.js 被手改过，'
            f'或者改了 census.py 的口径没有重跑。改法：{CENSUS_FIX}，不要手改 census.js。']


def check_classes(root):
    app = selector_classes((root / APP_CSS).read_text(encoding='utf-8'))
    guide_css = (root / GUIDE / 'styleguide.css').read_text(encoding='utf-8')
    shell = selector_classes(guide_css)
    modifiers = shell_modifiers(guide_css)
    page = _Page()
    page.feed((root / GUIDE / 'index.html').read_text(encoding='utf-8'))
    page.close()
    missing, missing_shell, missing_listed, hooks_used = {}, {}, {}, set()
    for name, line in page.used:
        if name.startswith('sg-'):
            if name not in shell:
                missing_shell.setdefault(name, []).append(line)
        elif name in app or name in modifiers:
            continue
        elif name in HOOK_CLASSES:
            hooks_used.add(name)
        else:
            missing.setdefault(name, []).append(line)
    for name, line in page.listed:
        if name not in app and name not in HOOK_CLASSES:
            missing_listed.setdefault(name, []).append(line)
    errors = []
    for name, lines in sorted(missing.items()):
        errors.append(f'[样式清单 · class] {GUIDE}/index.html 第 {_lines(lines)} 行：样例用了类 .{name}，web/static/app.css 里没有它的样式。'
                      '约定：样例是给人和代理照抄的，只能用 app.css 现有的类；类改名或删掉时要同步改这页。'
                      '改法：照 app.css 现在的类名改样例；类已删就删掉这个样例或换成替代写法；'
                      '如果它只是 JS 钩子、本来就没有样式，把它加进 tools/check_styleguide.py 的 HOOK_CLASSES 并写明出处。')
    for name, lines in sorted(missing_shell.items()):
        errors.append(f'[样式清单 · class] {GUIDE}/index.html 第 {_lines(lines)} 行：页面外壳类 .{name} 在 {GUIDE}/styleguide.css 里没有定义。'
                      '改法：在 styleguide.css 里补上，或改用已有的 sg- 类；sg- 前缀只给清单页自己用，不要拿来画应用组件。')
    for name, lines in sorted(missing_listed.items()):
        errors.append(f'[样式清单 · listed] {GUIDE}/index.html 第 {_lines(lines)} 行：条目标题列出了 .{name}，web/static/app.css 里没有它的样式。'
                      '改法：照 app.css 现在的类名改标题里的 <code>，类已删就从标题里去掉。')
    code = '\n'.join(p.read_text(encoding='utf-8') for pattern in ('*.html', '*.js', 'core/*.js', 'ui/*.js')
                     for p in sorted((root / STATIC).glob(pattern)))
    for name, where in sorted(HOOK_CLASSES.items()):
        if not re.search(rf'(?<![\w-]){re.escape(name)}(?![\w-])', code):
            errors.append(f'[样式清单 · class] HOOK_CLASSES 里的 .{name}（{where}）在 web/static 的代码里已经找不到。'
                          '改法：从 tools/check_styleguide.py 的 HOOK_CLASSES 删掉这一项，并删改用到它的样例。')
        elif name not in hooks_used and not any(n == name for n, _ in page.listed):
            errors.append(f'[样式清单 · class] HOOK_CLASSES 里的 .{name} 已不在 {GUIDE}/index.html 里用到。'
                          '改法：从 tools/check_styleguide.py 的 HOOK_CLASSES 删掉这一项。')
    return errors


def check(root=ROOT):
    return check_census(root) + check_classes(root)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args(argv)
    errors = check(args.root)
    for error in errors:
        print(error)
    if errors:
        print(f'\n样式清单与实现不同步：{len(errors)} 项。清单页说明见 tools/styleguide/index.html 开头。')
        return 1
    print('样式清单与 app.css 同步：census.js 是最新的，样例用到的类都有定义。')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
