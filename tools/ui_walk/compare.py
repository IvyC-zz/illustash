"""比较两次界面走查（#453）：同一条流程、同一种语言、同一步的快照逐项比，检查结果和控制台错误也比。

用途是新旧两份前端各跑一遍（#469 迁一页之前和之后），差异只剩有意改动的那些。输出 compare.md（文字）和
compare.html（有差异的那几步，左右两张截图并排）。两边完全一致时退出 0，否则 1。
"""
from __future__ import annotations

import html
import json
import os
from pathlib import Path


def load(run: Path) -> dict:
    path = run / 'report.json' if run.is_dir() else run
    return json.loads(path.read_text(encoding='utf-8'))


def flat(value, prefix=''):
    """快照摊平成「路径 → 值」，列表整体当一个值。"""
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            out.update(flat(v, f'{prefix}.{k}' if prefix else k))
        return out
    return {prefix: value}


def diff_steps(a: list[dict], b: list[dict]) -> list[dict]:
    rows = []
    for i in range(max(len(a), len(b))):
        sa, sb = (a[i] if i < len(a) else None), (b[i] if i < len(b) else None)
        if sa is None or sb is None or sa['label'] != sb['label']:
            rows.append({'index': i + 1, 'label': (sa or sb)['label'], 'fields': [],
                         'structure': f"步骤不对应：A {sa and sa['label']!r}，B {sb and sb['label']!r}", 'a': sa, 'b': sb})
            continue
        fa, fb = flat(sa['snap']), flat(sb['snap'])
        fields = [(k, fa.get(k), fb.get(k)) for k in sorted(set(fa) | set(fb)) if fa.get(k) != fb.get(k)]
        ca, cb = [e['text'] for e in sa['console']], [e['text'] for e in sb['console']]
        if ca != cb:
            fields.append(('控制台', ca, cb))
        if sa.get('error') != sb.get('error'):
            fields.append(('动作出错', sa.get('error'), sb.get('error')))
        if fields:
            rows.append({'index': i + 1, 'label': sa['label'], 'fields': fields, 'a': sa, 'b': sb})
    return rows


def compare(ra: dict, rb: dict) -> list[dict]:
    fa = {(f['lang'], f['flow']): f for f in ra['flows']}
    fb = {(f['lang'], f['flow']): f for f in rb['flows']}
    out = []
    for key in sorted(set(fa) | set(fb)):
        a, b = fa.get(key), fb.get(key)
        entry = {'lang': key[0], 'flow': key[1], 'only': None, 'checks': [], 'steps': []}
        if a is None or b is None:
            entry['only'] = 'A' if b is None else 'B'
            out.append(entry)
            continue
        ca = {c['name']: c for c in a['checks']}
        cb = {c['name']: c for c in b['checks']}
        for name in list(dict.fromkeys([*ca, *cb])):
            x, y = ca.get(name), cb.get(name)
            if (x and x['ok']) != (y and y['ok']):
                entry['checks'].append((name, x, y))
        if bool(a.get('crashed')) != bool(b.get('crashed')):
            entry['checks'].append(('流程跑完', {'ok': not a.get('crashed')}, {'ok': not b.get('crashed')}))
        entry['steps'] = diff_steps(a['steps'], b['steps'])
        if entry['checks'] or entry['steps']:
            out.append(entry)
    return out


def source_text(report: dict) -> str:
    s = report['source']
    return {'ref': lambda: f"提交 {s['ref']}（{s['commit'][:9]}）", 'static': lambda: f"目录 {s['path']}",
            'worktree': lambda: f"检出 {s['commit'][:9]}"}[s['kind']]()


def show(v) -> str:
    return '—' if v is None else json.dumps(v, ensure_ascii=False) if not isinstance(v, str) else v


def ok_text(c) -> str:
    return '没有这项' if c is None else ('通过' if c['ok'] else '未通过' + (f"（{c.get('detail')}）" if c.get('detail') else ''))


def main(a: Path, b: Path, out: Path | None) -> int:
    ra, rb = load(a), load(b)
    result = compare(ra, rb)
    out = (out or (b / 'compare')).resolve()
    out.mkdir(parents=True, exist_ok=True)
    md = [f'# 走查比较', '', f'- A：{source_text(ra)}（{ra["started"]}）', f'- B：{source_text(rb)}（{rb["started"]}）', '']
    if not result:
        md.append('两次运行的全部步骤、检查和控制台输出一致。')
    for e in result:
        md.append(f"## [{e['lang']}] {e['flow']}")
        md.append('')
        if e['only']:
            md += [f"只在 {e['only']} 里有这条流程。", '']
            continue
        for name, x, y in e['checks']:
            md.append(f'- 检查「{name}」：A {ok_text(x)}，B {ok_text(y)}')
        for s in e['steps']:
            md.append(f"- 第 {s['index']} 步「{s['label']}」" + (f"：{s['structure']}" if s.get('structure') else ''))
            for k, x, y in s['fields']:
                md.append(f'  - `{k}`：A {show(x)} → B {show(y)}')
        md.append('')
    (out / 'compare.md').write_text('\n'.join(md), encoding='utf-8')
    (out / 'compare.html').write_text(page(result, a, b, out, ra, rb), encoding='utf-8')
    changed = len(result)
    print(f'{"没有差异" if not changed else f"{changed} 条流程有差异"}：{out / "compare.md"}')
    return 1 if changed else 0


def page(result, a: Path, b: Path, out: Path, ra, rb) -> str:
    def img(run: Path, lang: str, step: dict | None) -> str:
        if not step or not step.get('shot'):
            return '<div class="none">没有截图</div>'
        rel = os.path.relpath((run if run.is_dir() else run.parent) / lang / step['shot'], out)
        return f'<img src="{html.escape(rel)}" loading="lazy">'

    parts = []
    for e in result:
        for s in e['steps']:
            if not ((s.get('a') or {}).get('shot') or (s.get('b') or {}).get('shot')):
                continue
            fields = ''.join(f'<li><code>{html.escape(k)}</code>：{html.escape(show(x))} → {html.escape(show(y))}</li>'
                             for k, x, y in s['fields'])
            parts.append(f"<section><h2>[{e['lang']}] {html.escape(e['flow'])} · 第 {s['index']} 步 {html.escape(s['label'])}</h2>"
                         f"<ul>{fields}</ul><div class='pair'>{img(a, e['lang'], s.get('a'))}{img(b, e['lang'], s.get('b'))}</div></section>")
    body = ''.join(parts) or '<p>有差异的步骤都没有截图（或没有差异）。文字结果见 compare.md。</p>'
    return ('<!doctype html><meta charset="utf-8"><title>走查比较</title><style>'
            'body{font:14px system-ui;margin:16px;background:#fff;color:#222}.pair{display:grid;grid-template-columns:1fr 1fr;gap:8px}'
            'img{width:100%;border:1px solid #ccc}.none{padding:40px;border:1px dashed #ccc;color:#888}'
            '@media (prefers-color-scheme:dark){body{background:#1b1b1b;color:#ddd}}</style>'
            f'<h1>走查比较</h1><p>左 A：{html.escape(source_text(ra))}　右 B：{html.escape(source_text(rb))}</p>{body}')
