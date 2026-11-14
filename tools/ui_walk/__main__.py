"""浏览器界面走查（#453）：一条命令在隔离宿主 + 合成图库上跑完跨页面流程，产出截图和报告；两次运行可以逐步比较。

  python3 -m tools.ui_walk run [--ref 提交 | --static 目录] [--out 目录] [--flow 名字…] [--lang zh,en]
  python3 -m tools.ui_walk compare <运行目录 A> <运行目录 B> [--out 目录]
  python3 -m tools.ui_walk list

run：每条流程、每种界面语言各用一份新复制的合成图库（tools/ui_walk/fixture.py）和一个新宿主，所以流程之间互不影响，
单独跑一条和整套跑结果相同。宿主的 HOME、应用数据、共享资源都在运行目录里（与 tools/dev_preview.py 相同的隔离），
不碰真实图库。`--ref` 用 `git archive` 取出那个提交的 web/ 和 tags/ 当宿主的 --root；`--static` 直接用一份
web/static 目录；都不给时用当前检出。宿主程序总是当前检出构建的那个（ILLUSTASH_TERMS_BIN 可另指），所以新旧前端
是在同一个后端上比。

结果：<out>/report.json（全部步骤的快照和检查）、report.md（检查结果和控制台错误）、<lang>/<流程>-NN.png 截图。
有检查没通过、控制台报错或流程崩溃时退出 1。做法见 docs/LOCAL_CHECKS.md「界面走查」。
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import io
from pathlib import Path

from tools import dev_preview
from tools.ui_walk import compare as compare_mod
from tools.ui_walk import fixture
from tools.ui_walk.browser import Browser, find_chrome
from tools.ui_walk.host import Host

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
LANGS = ('zh', 'en')
SLOW_BOOT_MS = 10000


def node() -> str:
    exe = os.environ.get('ILLUSTASH_WALK_NODE') or shutil.which('node')
    if not exe:
        sys.exit('找不到 node（需要 Node 22 或更新，用它自带的 WebSocket）。')
    return exe


def list_flows() -> list[dict]:
    out = subprocess.run([node(), str(HERE / 'run.mjs'), '--list'], capture_output=True, text=True, check=True)
    return json.loads(out.stdout)


def git(*args: str) -> str:
    return subprocess.run(['git', *args], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()


def static_root(args: argparse.Namespace, scratch: Path) -> tuple[Path, dict]:
    """宿主的 --root 和它的来历。宿主从 <root>/web/static 读前端、从 <root>/tags 读标签表。"""
    if args.ref:
        commit = git('rev-parse', '--verify', args.ref + '^{commit}')
        root = scratch / 'root'
        root.mkdir()
        data = subprocess.run(['git', 'archive', commit, 'web', 'tags'], cwd=ROOT, capture_output=True, check=True).stdout
        with tarfile.open(fileobj=io.BytesIO(data)) as tar:
            # 3.12 起 extractall 要写明过滤方式；更早的版本没有这个参数。
            if hasattr(tarfile, 'data_filter'):
                tar.extractall(root, filter='data')
            else:
                tar.extractall(root)
        return root, {'kind': 'ref', 'ref': args.ref, 'commit': commit}
    if args.static:
        static = args.static.resolve()
        if not (static / 'index.html').is_file():
            sys.exit(f'{static} 不是一份 web/static（没有 index.html）。')
        root = scratch / 'root'
        (root / 'web').mkdir(parents=True)
        (root / 'web' / 'static').symlink_to(static, target_is_directory=True)
        (root / 'tags').symlink_to(ROOT / 'tags', target_is_directory=True)
        return root, {'kind': 'static', 'path': static.name}
    dirty = bool(git('status', '--porcelain', '--', 'web'))
    return ROOT, {'kind': 'worktree', 'commit': git('rev-parse', 'HEAD'), 'dirty_web': dirty}


def run(args: argparse.Namespace) -> int:
    flows = list_flows()
    names = [f['name'] for f in flows]
    wanted = args.flow or names
    unknown = [f for f in wanted if f not in names]
    if unknown:
        sys.exit('没有这些流程：' + '、'.join(unknown) + '；可用：' + '、'.join(names))
    langs = [x.strip() for x in args.lang.split(',') if x.strip()]
    stamp = datetime.datetime.now().strftime('%Y%m%d-%H%M%S')
    out = (args.out or ROOT / '.walk' / stamp).resolve()
    if out.exists() and any(out.iterdir()):
        sys.exit(f'{out} 已经有内容，换一个 --out。')
    out.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix='illustash-walk-'))
    core = dev_preview.binary()
    browser = None
    try:
        root, source = static_root(args, scratch)
        print(f'前端：{describe(source)}；宿主程序：{core.name}；结果：{out}', flush=True)
        pristine = fixture.cached(ROOT / '.walk' / 'fixture', core)
        browser = Browser(find_chrome())
        results = []
        for lang in langs:
            for name in wanted:
                results.append(run_flow(name, lang, pristine, root, core, browser, out, scratch, args))
        report = {
            'format': 'illustash-ui-walk/1',
            'started': stamp,
            'source': source,
            'machine': {'system': platform.system(), 'release': platform.release(), 'arch': platform.machine(),
                        'browser': browser.version, 'node': subprocess.run([node(), '--version'], capture_output=True,
                                                                           text=True).stdout.strip()},
            'fixture': pristine.summary,
            'flows': results,
        }
        (out / 'report.json').write_text(json.dumps(report, ensure_ascii=False, indent=1), encoding='utf-8')
        (out / 'report.md').write_text(markdown(report), encoding='utf-8')
        failed = sum(not c['ok'] for r in results for c in r['checks'])
        errors = sum(len(s['console']) for r in results for s in r['steps']) + sum(bool(r.get('crashed')) for r in results)
        print(f'完成：{len(results)} 条流程，检查未通过 {failed} 项，控制台错误或崩溃 {errors} 处。报告：{out / "report.md"}')
        return 1 if failed or errors else 0
    finally:
        if browser:
            browser.close()
        if not args.keep:
            shutil.rmtree(scratch, ignore_errors=True)
        else:
            print(f'保留了临时目录：{scratch}')


def run_flow(name, lang, pristine, root, core, browser, out, scratch, args) -> dict:
    """一条流程：复制合成图库，起宿主，跑 node 驱动；驱动挂住（退出码 3）时整条重来一次。"""
    for attempt in (1, 2):
        library = pristine.reset()
        work = scratch / 'flows' / f'{lang}-{name}-{attempt}'
        work.mkdir(parents=True)
        research = fixture.worker()
        with Host(core, root, library, pristine.live,
                  env={'ILLUSTASH_RESEARCH_WORKER': str(research)} if research else None) as host:
            config = {'flow': name, 'lang': lang, 'url': host.url, 'port': browser.port, 'out': str(out),
                      'names': pristine.names, 'fixture': pristine.contract(), 'shots': not args.no_shots, 'timeout': args.timeout}
            path = work / 'config.json'
            path.write_text(json.dumps(config, ensure_ascii=False), encoding='utf-8')
            try:
                code = subprocess.run([node(), str(HERE / 'run.mjs'), '--config', str(path)],
                                      timeout=args.timeout + 30).returncode
            except subprocess.TimeoutExpired:
                code = 3
            log = host.log_tail()
        result_file = out / lang / f'{name}.json'
        if code == 0 and result_file.exists():
            result = json.loads(result_file.read_text(encoding='utf-8'))
            if result.get('crashed'):
                result['host_log'] = log
            mark = '✗' if result.get('crashed') or any(not c['ok'] for c in result['checks']) else '✓'
            print(f'  {mark} [{lang}] {name}', flush=True)
            return result
        print(f'  … [{lang}] {name} 第 {attempt} 次没有跑完（退出码 {code}）', flush=True)
    return {'flow': name, 'lang': lang, 'steps': [], 'checks': [], 'crashed': f'驱动两次都没有跑完（退出码 {code}）',
            'host_log': log}


def describe(source: dict) -> str:
    if source['kind'] == 'ref':
        return f"提交 {source['ref']}（{source['commit'][:9]}）"
    if source['kind'] == 'static':
        return f"目录 {source['path']}"
    return f"当前检出 {source['commit'][:9]}" + ('（web/ 有未提交改动）' if source['dirty_web'] else '')


def markdown(report: dict) -> str:
    lines = [f"# 界面走查 {report['started']}", '',
             f"- 前端：{describe(report['source'])}",
             f"- 机器：{report['machine']['system']} {report['machine']['release']} {report['machine']['arch']}；"
             f"{report['machine']['browser']}；Node {report['machine']['node']}",
             f"- 合成图库：{report['fixture']}", '']
    for r in report['flows']:
        bad = [c for c in r['checks'] if not c['ok']]
        errors = [(s['index'], e) for s in r['steps'] for e in s['console']]
        mark = '✗' if bad or errors or r.get('crashed') else '✓'
        covers = f"（{'、'.join(r.get('covers') or [])}）" if r.get('covers') else ''
        lines.append(f"## {mark} [{r['lang']}] {r['flow']}：{r.get('title', '')}{covers}")
        lines.append('')
        if r.get('crashed'):
            lines += ['流程没有跑完：', '', '```', r['crashed'][:3000], '```', '']
        for c in r['checks']:
            lines.append(f"- {'✓' if c['ok'] else '✗'} 第 {c['step']} 步 {c['name']}" + (f"：{c['detail']}" if not c['ok'] and c['detail'] else ''))
        if (r.get('bootMs') or 0) > SLOW_BOOT_MS:
            lines.append(f"- ⚠ 页面启动用了 {r['bootMs'] / 1000:.1f} 秒（正常约 4 秒；宿主连接线程池的问题，见 #477），不算检查失败")
        for index, e in errors:
            lines.append(f"- ✗ 第 {index} 步控制台 {e['kind']}：{e['text'][:300]}")
        shots = [s['shot'] for s in r['steps'] if s.get('shot')]
        if shots:
            lines.append('')
            lines.append('截图：' + '、'.join(f"[{s}]({r['lang']}/{s})" for s in shots))
        lines.append('')
    return '\n'.join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(prog='python3 -m tools.ui_walk', description='浏览器界面走查（#453）')
    sub = parser.add_subparsers(dest='command', required=True)
    r = sub.add_parser('run', help='跑走查')
    r.add_argument('--ref', help='用这个提交的 web/static（git archive）')
    r.add_argument('--static', type=Path, help='用这份 web/static 目录')
    r.add_argument('--out', type=Path, help='结果目录（默认 .walk/<时间>，已被 git 忽略）')
    r.add_argument('--flow', action='append', help='只跑这条流程，可重复；默认全部')
    r.add_argument('--lang', default=','.join(LANGS), help='界面语言，逗号分隔（zh、en），默认两种都跑')
    r.add_argument('--no-shots', action='store_true', help='不截图')
    r.add_argument('--timeout', type=int, default=150, help='每条流程的时限（秒）')
    r.add_argument('--keep', action='store_true', help='保留临时目录（合成图库、宿主日志），用于排查')
    c = sub.add_parser('compare', help='逐步比较两次运行')
    c.add_argument('a', type=Path)
    c.add_argument('b', type=Path)
    c.add_argument('--out', type=Path, help='比较结果目录（默认写在 B 里的 compare/）')
    sub.add_parser('list', help='列出流程')
    args = parser.parse_args()
    if args.ref and args.static if args.command == 'run' else False:
        parser.error('--ref 和 --static 只能给一个')
    if args.command == 'list':
        for f in list_flows():
            print(f"{f['name']:<24} {f['title']}" + (f"（{'、'.join(f['covers'])}）" if f['covers'] else ''))
        return 0
    if args.command == 'compare':
        return compare_mod.main(args.a, args.b, args.out)
    return run(args)


if __name__ == '__main__':
    sys.exit(main())
