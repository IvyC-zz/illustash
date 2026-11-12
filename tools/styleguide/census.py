#!/usr/bin/env python3
"""Count what web/static/app.css really uses, for the style guide (tools/styleguide/index.html, #318).

Writes tools/styleguide/census.js (`window.CENSUS = {...}`), which the page reads with a plain <script>, so the page
opens from file:// without a server or a build step. Run it again whenever app.css changes (#317 changes it a lot):

    python3 tools/styleguide/census.py          # rewrite census.js
    python3 tools/styleguide/census.py --print  # only print a summary

What is counted, and how:
  * one "use" is one declaration `property:value` in one rule (a selector list counts once, not once per selector);
  * values are normalised (`!important` dropped, `0.5` -> `.5`, spaces collapsed) but never merged by similarity:
    `10.5px` and `11px` are two values, which is exactly what the style guide wants to show;
  * font-size also reads the `font:` shorthand; radius, gap, padding, height and z-index read the properties of that name;
    height counts only fixed px values of `height` and `min-height` (not every element with a height is a control);
  * the colour tables are read from the two `:root` blocks (`:root{...}` and `:root[data-theme=dark]{...}`).
"""
import json
import re
import sys
from collections import Counter, defaultdict
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CSS = ROOT / 'web/static/app.css'
OUT = Path(__file__).with_name('census.js')


def strip_comments(css):
    return re.sub(r'/\*.*?\*/', '', css, flags=re.S)


def parse_rules(css, context=''):
    """[(selector, declarations, at-rule prelude or '')] for every style rule, @media bodies included."""
    rules, pos, head, n = [], 0, 0, len(css)
    while pos < n:
        if css[pos] != '{':
            pos += 1
            continue
        depth, j = 1, pos + 1
        while depth and j < n:
            depth += {'{': 1, '}': -1}.get(css[j], 0)
            j += 1
        prelude, body = css[head:pos].strip(), css[pos + 1:j - 1]
        if prelude.startswith(('@media', '@supports')):
            rules.extend(parse_rules(body, prelude))
        elif not prelude.startswith('@'):  # @keyframes steps are not component styles
            rules.append((prelude, body, context))
        pos = head = j
    return rules


def declarations(body):
    """[(property, value)]; splits on ; outside parentheses and quotes (data: URLs contain ;)."""
    out, depth, quote, cur = [], 0, '', ''
    for ch in body:
        if quote:
            cur += ch
            if ch == quote:
                quote = ''
        elif ch in '"\'':
            quote = ch
            cur += ch
        elif ch == '(':
            depth += 1
            cur += ch
        elif ch == ')':
            depth -= 1
            cur += ch
        elif ch == ';' and depth == 0:
            out.append(cur)
            cur = ''
        else:
            cur += ch
    out.append(cur)
    result = []
    for d in out:
        if ':' in d:
            p, v = d.split(':', 1)
            result.append((p.strip().lower(), v.strip()))
    return result


def norm(v):
    v = re.sub(r'\s*!important', '', v).strip()
    v = re.sub(r'\s+', ' ', v)
    v = re.sub(r'(?<![\d.])0\.(\d)', r'.\1', v)
    return v


def top_selector(sel):
    return re.sub(r'\s+', ' ', sel.strip())


class Tally:
    def __init__(self):
        self.count = Counter()
        self.where = defaultdict(list)

    def add(self, value, selector):
        self.count[value] += 1
        s = top_selector(selector)
        if s not in self.where[value] and len(self.where[value]) < 4:
            self.where[value].append(s if len(s) <= 90 else s[:87] + '…')

    def table(self, key=None):
        items = sorted(self.count.items(), key=key or (lambda kv: (-kv[1], kv[0])))
        return [{'value': v, 'uses': c, 'examples': self.where[v]} for v, c in items]


def num(v):
    m = re.match(r'-?[\d.]+', v)
    return float(m.group()) if m else 1e9


def theme_vars(css, header):
    """{name: value} of the custom properties declared in the :root block starting with `header`."""
    i = css.find(header)
    if i < 0:
        return {}
    j = css.find('{', i)
    depth, k = 1, j + 1
    while depth:
        depth += {'{': 1, '}': -1}.get(css[k], 0)
        k += 1
    return {p: norm(v) for p, v in declarations(css[j + 1:k - 1]) if p.startswith('--')}


def main():
    raw = CSS.read_text()
    css = strip_comments(raw)
    rules = parse_rules(css)
    fs, rad, gap, pad, hei, zi, lit = Tally(), Tally(), Tally(), Tally(), Tally(), Tally(), Tally()
    dark_overrides = 0
    for sel, body, ctx in rules:
        dark = 'data-theme=dark' in sel
        for prop, val in declarations(body):
            v = norm(val)
            if prop == 'font-size':
                fs.add(v, sel)
            elif prop == 'font':
                m = re.search(r'(?<![\w.-])(\d*\.?\d+(?:px|em|rem))(?:/[\d.]+)?\s', v + ' ')
                if m:
                    fs.add(m.group(1) + ' (font 简写)', sel)
            elif prop in ('border-radius', 'border-top-left-radius', 'border-top-right-radius', 'border-bottom-left-radius', 'border-bottom-right-radius'):
                rad.add(v, sel)
            elif prop in ('gap', 'row-gap', 'column-gap'):
                gap.add(v, sel)
            elif prop == 'padding':
                pad.add(v, sel)
            elif prop in ('height', 'min-height') and re.fullmatch(r'[\d.]+px', v):  # fixed sizes only: 100%, auto, 0 are layout
                hei.add(f'{prop}: {v}', sel)
            elif prop == 'z-index':
                zi.add(v, sel)
            if re.search(r'#[0-9a-fA-F]{3,8}\b', v) and not prop.startswith('--'):
                for h in re.findall(r'#[0-9a-fA-F]{3,8}\b', v):
                    lit.add(h.lower(), sel)
        if dark and not sel.startswith(':root[data-theme=dark]{'):
            dark_overrides += 1
    # background-image url(...) holds '#' colours percent-encoded as %23, never as #: not counted above, correctly.
    var_uses = Counter(re.findall(r'var\((--[\w-]+)', css))
    light = theme_vars(css, ':root{')
    dark = theme_vars(css, ':root[data-theme=dark]{')
    rgba_n = len(re.findall(r'rgba?\(', css))
    data = {
        'source': 'web/static/app.css',
        'lines': raw.count('\n') + 1,
        'rules': len(rules),
        'themeVars': {'light': light, 'dark': dark, 'uses': dict(var_uses)},
        'darkOverrideRules': dark_overrides,
        'literalHexColors': {'kinds': len(lit.count), 'uses': sum(lit.count.values()), 'table': lit.table()[:60]},
        'rgbaCalls': rgba_n,
        'fontSize': {'kinds': len(fs.count), 'uses': sum(fs.count.values()), 'table': fs.table(key=lambda kv: (num(kv[0]), kv[0]))},
        'radius': {'kinds': len(rad.count), 'uses': sum(rad.count.values()), 'table': rad.table(key=lambda kv: (num(kv[0]), kv[0]))},
        'gap': {'kinds': len(gap.count), 'uses': sum(gap.count.values()), 'table': gap.table(key=lambda kv: (num(kv[0]), kv[0]))},
        'padding': {'kinds': len(pad.count), 'uses': sum(pad.count.values()), 'table': pad.table()[:24]},
        'height': {'kinds': len(hei.count), 'uses': sum(hei.count.values()), 'table': hei.table(key=lambda kv: (num(kv[0].split(': ')[1]), kv[0]))},
        'zIndex': {'kinds': len(zi.count), 'uses': sum(zi.count.values()), 'table': zi.table(key=lambda kv: (num(kv[0]), kv[0]))},
    }
    if '--print' in sys.argv:
        for k in ('fontSize', 'radius', 'gap', 'padding', 'height', 'zIndex', 'literalHexColors'):
            print(f"{k}: {data[k]['kinds']} 种取值, {data[k]['uses']} 处")
        print('light vars', len(light), 'dark vars', len(dark), 'var() uses', sum(var_uses.values()), 'dark override rules', dark_overrides, 'rgba', rgba_n)
        return
    OUT.write_text('// Generated by tools/styleguide/census.py from web/static/app.css; do not edit by hand.\n'
                   'window.CENSUS = ' + json.dumps(data, ensure_ascii=False, indent=1) + ';\n')
    print(f'wrote {OUT.relative_to(ROOT)} ({OUT.stat().st_size} bytes)')


main()  # a script, never imported (so no `if __name__` entry point for tools/migration_inventory.py to count)
