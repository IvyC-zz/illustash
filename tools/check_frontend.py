"""Front-end structure check for web/static (#320, part of #316).

Plain Python, no Node tooling and no build step. Freezes today's bad patterns in a baseline file and only blocks new
ones, the way tools/check_architecture.py guards the crate direction. Rules (docs: AGENTS.md「前端结构约定」):

  1 import-core-ui       core/ must not import ui/ (hard rule, no baseline)
  1 import-cycle         import edges inside a ui/ import cycle; the existing ones are baselined, no new edge or cycle
  2 undeclared-state     `state.<field>` must be declared in core/state.js
  3 document-listener    no new document/window `keydown`/`click` listeners
  4 dialog-element       no new `document.createElement('dialog')`
  5 long-line            no new line over 200 characters (app.js, core/, ui/)
  6 page-name            no new page-name comparison outside core/pages.js (the page registry, #342)

Baseline (tools/frontend_baseline.json) is keyed by file + content feature (edge, field name, per-file count), never by
line number, so edits elsewhere in a file do not invalidate it. It can only shrink: `--shrink` lowers entries and never
adds one; `--base-ref REF` fails when the baseline is larger than the one at REF (use it in review / the merge queue).
A new rule (e.g. rule 6, CSS literals after #317) is one more `Rule` in RULES.
"""
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
import argparse
import json
import posixpath
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
STATIC = Path('web/static')
BASELINE = Path('tools/frontend_baseline.json')
MAX_LINE = 200
# Module files the rules look at. i18n.js / splash.js / theme.js are classic scripts, locales/ is data.
MODULE_GLOBS = ('app.js', 'core/*.js', 'ui/*.js')


# ---------------------------------------------------------------------------------------------------------- scanning
_BLANK = re.compile(r'[^\n]')
_TEMPLATE_RUN = re.compile(r'[^\\`$]+|\$(?!\{)')
_REGEX_KEYWORDS = {'return', 'typeof', 'case', 'in', 'of', 'void', 'delete', 'throw', 'new', 'else', 'do', 'yield', 'await'}


def strip_comments(src, strings=True):
    """Blank out comments (keeping newlines, so line numbers stay); keep strings, template text and regex literals.
    With strings=False the text of quoted strings and template literals is blanked too (`${…}` code stays)."""
    keep = (lambda text: text) if strings else (lambda text: _BLANK.sub(' ', text))
    out, i, n = [], 0, len(src)
    stack = []  # 'b' = open brace in code, 't' = `${` of a template literal
    template = False
    tail = ''  # the last few emitted characters, to tell a regex literal from a division

    def emit(piece):
        nonlocal tail
        out.append(piece)
        tail = (tail + piece)[-64:]

    while i < n:
        c = src[i]
        if template:
            if c == '\\':
                emit(keep(src[i:i + 2])); i += 2
            elif c == '`':
                emit(c); i += 1; template = False
            elif src.startswith('${', i):
                emit('${'); i += 2; stack.append('t'); template = False
            else:
                m = _TEMPLATE_RUN.match(src, i)
                emit(keep(m.group(0))); i = m.end()
        elif c in '\'"':
            j = i + 1
            while j < n and src[j] != c and src[j] != '\n':
                j += 2 if src[j] == '\\' else 1
            emit(c + keep(src[i + 1:j]) + src[j:j + 1]); i = j + 1
        elif c == '`':
            emit(c); i += 1; template = True
        elif src.startswith('//', i):
            j = src.find('\n', i)
            j = n if j < 0 else j
            emit(' ' * (j - i)); i = j
        elif src.startswith('/*', i):
            j = src.find('*/', i + 2)
            j = n if j < 0 else j + 2
            emit(_BLANK.sub(' ', src[i:j])); i = j
        elif c == '/' and _regex_allowed(tail):
            j, in_class = i + 1, False
            while j < n and src[j] != '\n':
                if src[j] == '\\':
                    j += 2; continue
                if src[j] == '[':
                    in_class = True
                elif src[j] == ']':
                    in_class = False
                elif src[j] == '/' and not in_class:
                    break
                j += 1
            emit(src[i:j + 1]); i = j + 1
        else:
            if c == '{':
                stack.append('b')
            elif c == '}' and stack and stack.pop() == 't':
                template = True
            emit(c); i += 1
    if template or stack:
        raise ValueError('unbalanced string/template/brace while scanning')
    return ''.join(out)


def _regex_allowed(before):
    """A `/` starts a regex literal after an operator or `(`; after a value (name, number, `)`, `]`) it divides."""
    tail = before.rstrip()
    if not tail:
        return True
    if tail[-1] in '(,=:[!&|?{};+-*%<>~^':
        return True
    m = re.search(r'([A-Za-z_$][\w$]*)$', tail)
    return bool(m and m.group(1) in _REGEX_KEYWORDS)


@dataclass
class Source:
    path: str  # relative to web/static, posix
    raw: str
    code: str  # comments blanked
    bare: str = ''  # comments and string/template text blanked (for matching identifiers)

    def line_of(self, offset):
        return self.code.count('\n', 0, offset) + 1

    def raw_line(self, number):
        return self.raw.splitlines()[number - 1].strip()


def load_sources(root):
    base = root / STATIC
    found = {}
    for pattern in MODULE_GLOBS:
        for path in sorted(base.glob(pattern)):
            raw = path.read_text(encoding='utf-8')
            rel = path.relative_to(base).as_posix()
            try:
                found[rel] = Source(rel, raw, strip_comments(raw), strip_comments(raw, strings=False))
            except ValueError as error:
                raise SystemExit(f'check_frontend: cannot scan {rel}: {error}')
    return found


IMPORT_RE = re.compile(r'''\b(?:import|export)\b[^'"`;]*?\bfrom\s*(['"])(.+?)\1|\bimport\s*\(?\s*(['"])(.+?)\3''', re.S)


def imports(source, known):
    """Modules (relative to web/static) that `source` imports statically or dynamically; non-module paths are ignored."""
    result = []
    for m in IMPORT_RE.finditer(source.code):
        spec = m.group(2) or m.group(4)
        if spec.startswith('/'):
            target = posixpath.normpath(spec.lstrip('/'))
        elif spec.startswith('.'):
            target = posixpath.normpath(posixpath.join(posixpath.dirname(source.path), spec))
        else:
            continue
        if target in known:
            result.append((target, source.line_of(m.start())))
    return result


def strongly_connected(graph):
    """Tarjan; returns the components with more than one node (or a self-loop)."""
    index, low, on, stack, out, counter = {}, {}, set(), [], [], [0]

    def visit(v):
        index[v] = low[v] = counter[0]; counter[0] += 1
        stack.append(v); on.add(v)
        for w in graph.get(v, ()):
            if w not in index:
                visit(w); low[v] = min(low[v], low[w])
            elif w in on:
                low[v] = min(low[v], index[w])
        if low[v] == index[v]:
            comp = []
            while True:
                w = stack.pop(); on.discard(w); comp.append(w)
                if w == v:
                    break
            if len(comp) > 1 or v in graph.get(v, ()):
                out.append(comp)

    for v in sorted(graph):
        if v not in index:
            visit(v)
    return out


# -------------------------------------------------------------------------------------------------------------- rules
@dataclass
class Rule:
    id: str
    number: str
    title: str
    convention: str
    fix: str
    baselined: bool = True
    found: Counter = field(default_factory=Counter)
    where: dict = field(default_factory=dict)  # key -> ["file:line: text", ...]

    def add(self, key, place):
        self.found[key] += 1
        self.where.setdefault(key, []).append(place)


def new_rules():
    return {r.id: r for r in [
        Rule('import-core-ui', '1', 'core/ 导入了 ui/',
             'core/ 不碰 DOM、不依赖 ui/（任何布局都能复用，Node 里能直接导入）。',
             '把 core/ 需要的东西留在 core/；要通知界面就用 core/events.js 的监听（onPost / onJob 那一类），由 app.js 把界面接上。',
             baselined=False),
        Rule('import-cycle', '1', '新增了 ui/ 模块循环依赖里的导入边',
             'ui/ 现有的循环导入已冻结为基线；不得新增新的环，也不得往已有的环里再加边。',
             '把两边共用的函数/常量下沉到更底层的模块（core/ 或没有回头导入的 ui/ 模块），或改成由调用方传入回调；'
             '不要为了“顺手”再互相 import。拆掉一条环内边后，运行 `python tools/check_frontend.py --shrink` 把它从基线里删掉。'),
        Rule('undeclared-state', '2', 'state 上使用了未在 core/state.js 声明的字段',
             'state 上的字段必须先在 core/state.js 的 state 对象里声明（并写初始值），现有未声明字段已冻结为基线。',
             '在 core/state.js 里加上这个字段和初始值；如果它只是某个模块自己的局部状态，改成该模块里的 let/const，不要挂在 state 上。'),
        Rule('document-listener', '3', '新增了 document/window 级的 keydown / click 监听',
             '全局键盘、点击监听让 Esc 的先后、点外关闭各写一套（#316）。现有的已冻结为基线，不得新增。',
             '把监听挂在具体元素或 <dialog> 上（dialog 用 cancel/close 事件，点外关闭在 dialog 自己的 click 里判断）；'
             '确需全局监听时，在 Issue 里提出请用户决定，由用户改本检查的规则，不要往基线里添加。'),
        Rule('dialog-element', '4', '新增了 document.createElement(\'dialog\')',
             '运行时拼出的 <dialog> 各自复制同一份外壳（#316）。现有的已冻结为基线，不得新增。',
             '复用 ui/dialog.js 里的 confirmAction / chooseWork；不够用就在 ui/dialog.js 里扩展参数，不要再在别处拼一份 dialog 外壳。'),
        Rule('long-line', '5', f'新增了超过 {MAX_LINE} 字符的行',
             f'一行不超过 {MAX_LINE} 个字符（代理和评审都读不动一行 1000 字符）。现有的超长行已冻结为基线，不得新增。',
             '把这行拆成多行：长模板字符串按块换行，长条件/链式调用拆成中间变量；顺手拆开的话，用 `--shrink` 更新基线。'),
        Rule('page-name', '6', '在页面注册表以外的位置新增了页面名判断',
             '页面是哪一类（点击语义、主区、侧栏高亮、工具栏、pool 参数、重画方式……）统一声明在 core/pages.js（#342）。'
             '现有的判断已冻结为基线（主要是异步回应回来时「还在不在这一页」的守卫），不得新增。',
             '把页面的性质写进 core/pages.js 的声明，用 `viewOf(route).<字段>` 或 `pageOf(route).<字段>` 读；'
             '要登记渲染入口、标题、重画方式用 `implement()`。见 docs/WEB_UI_MAP.md「新增一个页面要做什么」。'),
    ]}


LISTENER_RE = re.compile(r'''\b(document|window)\s*\.\s*(?:addEventListener\s*\(\s*(['"`])(keydown|click)\2|on(keydown|click)\s*=(?!=))''')
DIALOG_RE = re.compile(r'''createElement\s*\(\s*(['"`])dialog\1\s*\)''')
# 页面名判断：`page === '…'`、`route.page !== '…'`，以及 `[…].includes(route.page)`。
PAGE_NAME_RE = re.compile(r'''(?:\b(?:route|r)\.page|\bpage)\s*[!=]==\s*['"`]|\.includes\(\s*(?:state\.)?(?:route|r)\.page\s*\)''')
STATE_RE = re.compile(r'(?<![\w$.])state\s*\.\s*([A-Za-z_$][\w$]*)')


def declared_state(sources):
    """Top-level keys of the `state` object literal in core/state.js."""
    code = sources['core/state.js'].bare
    start = code.index('export const state = {') + len('export const state = ')
    depth, keys = 0, set()
    for m in re.finditer(r'''[{}\[\]()]|'[^'\n]*'|"[^"\n]*"|`[^`]*`|([A-Za-z_$][\w$]*)\s*:(?!:)''', code[start:]):
        s = m.group(0)
        if s in '{[(':
            depth += 1
        elif s in '}])':
            depth -= 1
            if depth == 0:
                break
        elif m.group(1) and depth == 1:
            keys.add(m.group(1))
    return keys


def run_rules(sources):
    rules = new_rules()
    graph = {p: [t for t, _ in imports(s, sources)] for p, s in sources.items()}
    edges = {(p, t): s.raw_line(line) for p, s in sources.items() for t, line in imports(s, sources)}
    # 1: core -> ui, hard
    for (p, t), text in sorted(edges.items()):
        if p.startswith('core/') and (t.startswith('ui/') or t == 'app.js'):
            rules['import-core-ui'].add(f'{p} -> {t}', f'{STATIC}/{p}: {text}')
    # 1: edges inside import cycles
    for comp in strongly_connected({p: [t for t in ts] for p, ts in graph.items()}):
        members = set(comp)
        for (p, t), text in sorted(edges.items()):
            if p in members and t in members:
                rules['import-cycle'].add(f'{p} -> {t}', f'{STATIC}/{p}: {text}  (in a cycle of {len(members)} files: '
                                          + ', '.join(sorted(members)) + ')')
    # 2: state fields
    declared = declared_state(sources)
    for p, s in sources.items():
        for m in STATE_RE.finditer(s.bare):
            if m.group(1) not in declared:
                line = s.line_of(m.start())
                rules['undeclared-state'].add(m.group(1), f'{STATIC}/{p}:{line}: {s.raw_line(line)[:110]}')
    # 3, 4, 5
    for p, s in sources.items():
        for m in LISTENER_RE.finditer(s.code):
            line = s.line_of(m.start())
            rules['document-listener'].add(f'{p}::{m.group(3) or m.group(4)}', f'{STATIC}/{p}:{line}: {s.raw_line(line)[:110]}')
        for m in DIALOG_RE.finditer(s.code):
            line = s.line_of(m.start())
            rules['dialog-element'].add(p, f'{STATIC}/{p}:{line}: {s.raw_line(line)[:110]}')
        if p != 'core/pages.js':
            for m in PAGE_NAME_RE.finditer(s.code):
                line = s.line_of(m.start())
                rules['page-name'].add(p, f'{STATIC}/{p}:{line}: {s.raw_line(line)[:110]}')
        for number, text in enumerate(s.raw.splitlines(), 1):
            if len(text) > MAX_LINE:
                rules['long-line'].add(p, f'{STATIC}/{p}:{number}: ({len(text)} chars) {text.strip()[:70]}…')
    return rules


# ----------------------------------------------------------------------------------------------------------- baseline
def snapshot(rules):
    return {r.id: dict(sorted(r.found.items())) for r in rules.values() if r.baselined}


def load_baseline(root):
    path = root / BASELINE
    if not path.exists():
        return None
    return json.loads(path.read_text(encoding='utf-8')).get('rules', {})


def write_baseline(root, data):
    doc = {'_note': '前端结构检查的基线（tools/check_frontend.py，#320）：现状冻结，只能缩小。改好一处就运行 '
                    '`python tools/check_frontend.py --shrink` 删掉对应项；新增违规不要往这里添加，按报错的“改法”修代码。',
           'rules': data}
    (root / BASELINE).write_text(json.dumps(doc, ensure_ascii=False, indent=1) + '\n', encoding='utf-8')


def compare(rules, baseline):
    """Returns (errors, stale): new violations beyond the baseline and baseline entries that no longer occur."""
    errors, stale = [], []
    for rule in rules.values():
        base = baseline.get(rule.id, {}) if rule.baselined else {}
        for key, count in sorted(rule.found.items()):
            allowed = base.get(key, 0)
            if count > allowed:
                places = rule.where[key]
                shown = places if len(places) <= 8 else places[:8] + [f'… 另有 {len(places) - 8} 处']
                what = f'{key}（现有 {count} 处，基线允许 {allowed} 处）' if rule.baselined else key
                errors.append(f'[规则 {rule.number} · {rule.id}] {rule.title}: {what}\n  '
                              + '\n  '.join(shown)
                              + f'\n  约定：{rule.convention}\n  改法：{rule.fix}')
        for key, allowed in sorted(base.items()):
            if rule.found.get(key, 0) < allowed:
                stale.append(f'[{rule.id}] {key}: 基线 {allowed}，现有 {rule.found.get(key, 0)}')
    return errors, stale


def baseline_growth(current, reference):
    """Entries in `current` that are absent from or larger than `reference` (the baseline at a base ref)."""
    grown = []
    for rule, entries in current.items():
        for key, count in entries.items():
            if count > reference.get(rule, {}).get(key, 0):
                grown.append(f'[{rule}] {key}: {reference.get(rule, {}).get(key, 0)} -> {count}')
    return grown


def baseline_at(root, ref):
    result = subprocess.run(['git', '-C', str(root), 'show', f'{ref}:{BASELINE.as_posix()}'],
                            capture_output=True, text=True)
    if result.returncode != 0:
        return None
    return json.loads(result.stdout).get('rules', {})


def check(root=ROOT, baseline=None):
    """Returns (errors, stale). `baseline` overrides the file (used by tests)."""
    rules = run_rules(load_sources(root))
    if baseline is None:
        baseline = load_baseline(root)
    if baseline is None:
        return ['缺少 tools/frontend_baseline.json；首次建立基线用 `python tools/check_frontend.py --init`。'], []
    return compare(rules, baseline)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--init', action='store_true', help='create the baseline from the current tree (only when none exists)')
    parser.add_argument('--shrink', action='store_true', help='lower/remove baseline entries that no longer occur; never adds')
    parser.add_argument('--base-ref', metavar='REF', help='fail if the baseline is larger than the one at this git ref')
    parser.add_argument('--strict-stale', action='store_true', help='treat baseline entries that no longer occur as errors')
    args = parser.parse_args(argv)
    root = args.root
    rules = run_rules(load_sources(root))
    if args.init:
        if (root / BASELINE).exists():
            print('基线已存在；只能用 --shrink 缩小。', file=sys.stderr)
            return 1
        write_baseline(root, snapshot(rules))
        print(f'已写入 {BASELINE}')
        return 0
    baseline = load_baseline(root)
    if baseline is None:
        print('缺少 tools/frontend_baseline.json；首次建立基线用 `python tools/check_frontend.py --init`。')
        return 1
    if args.shrink:
        shrunk = {rid: {k: min(v, rules[rid].found.get(k, 0)) for k, v in entries.items() if rules[rid].found.get(k, 0)}
                  for rid, entries in baseline.items() if rid in rules}
        write_baseline(root, shrunk)
        print(f'已按现状收紧 {BASELINE}（只删减，不新增；新增违规仍会报错）')
        baseline = shrunk
    errors, stale = compare(rules, baseline)
    if args.base_ref:
        reference = baseline_at(root, args.base_ref)
        if reference is None:
            print(f'注意：{args.base_ref} 上没有基线文件，跳过“基线变大”检查。')
        else:
            for line in baseline_growth(baseline, reference):
                errors.append(f'[基线变大] {line}\n  基线只能缩小：新增违规要改代码，不能往 {BASELINE} 里添加。')
    for error in errors:
        print(error)
    if stale:
        label = '错误' if args.strict_stale else '提示'
        print(f'\n{label}：基线里有 {len(stale)} 项已不再出现，可收紧（`python tools/check_frontend.py --shrink`）：')
        for line in stale[:20]:
            print('  ' + line)
        if len(stale) > 20:
            print(f'  … 另有 {len(stale) - 20} 项')
        if args.strict_stale:
            errors.append('stale baseline')
    if not errors:
        total = {r.id: sum(r.found.values()) for r in rules.values() if r.baselined}
        print('frontend structure valid: no new violations beyond baseline ('
              + ', '.join(f'{k} {v}' for k, v in total.items()) + ')')
    return 1 if errors else 0


if __name__ == '__main__':
    raise SystemExit(main())
