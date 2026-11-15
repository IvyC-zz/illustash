"""不稳定测试的隔离名单（#501）：读取、校验 `tests/quarantine.json`，并给 Rust 测试提供「照跑不挡」的入口。

名单里的测试照常运行；只在条目列出的平台上，它们的失败不计入结果，单列在「隔离名单中的失败」一节。
到期的条目（今天晚于 `expires`）、格式不对的条目、找不到对应测试的条目都让检查失败。
Python 测试由 `tests.run_fixed_suite` 处理；Rust 测试用本脚本的 `cargo-test` 子命令代替 `cargo test`：

    python tools/quarantine.py check
    python tools/quarantine.py cargo-test --manifest-path native/terms/Cargo.toml [其余 cargo test 参数]

条目字段：`kind`（`python` 或 `rust`）、`id`（Python 为 discover 下的测试 ID，如
`test_native_research.NativeResearchTest.test_x`；Rust 为 libtest 名称，如 `app_sync::tests::test_x`）、
`crate`（仅 Rust，Cargo 项目相对仓库根的目录）、`platforms`（`macos`、`windows` 中的一个或两个）、
`issue`（跟踪的 Issue 编号）、`added` 与 `expires`（ISO 日期，默认两周）。
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
LIST = ROOT / 'tests' / 'quarantine.json'
PLATFORMS = {'darwin': 'macos', 'win32': 'windows'}
FIELDS = {'python': {'kind', 'id', 'platforms', 'issue', 'added', 'expires'},
          'rust': {'kind', 'crate', 'id', 'platforms', 'issue', 'added', 'expires'}}
HEADING = '隔离名单中的失败（不计入结果）'
# 测试替换成合成的 cargo。
CARGO = ['cargo']


class QuarantineError(Exception):
    """名单本身不合格：格式错误、条目到期或找不到对应测试。"""


def platform():
    return PLATFORMS.get(sys.platform, sys.platform)


def today():
    # 测试可以固定日期；其余情况用运行机器的本地日期。
    value = os.environ.get('ILLUSTASH_QUARANTINE_TODAY')
    return datetime.date.fromisoformat(value) if value else datetime.date.today()


def load(path=None):
    """读取并校验名单，返回条目列表；不合格时抛出 QuarantineError，列出全部问题。"""
    path = Path(path or os.environ.get('ILLUSTASH_QUARANTINE') or LIST)
    try:
        data = json.loads(path.read_text(encoding='utf-8'))
    except (OSError, ValueError) as error:
        raise QuarantineError(f'读不出隔离名单 {path.name}：{error}') from error
    entries = data.get('entries') if isinstance(data, dict) else None
    if not isinstance(entries, list):
        raise QuarantineError(f'{path.name} 须是带 entries 列表的对象')
    problems = []
    seen = set()
    for index, entry in enumerate(entries):
        where = f'第 {index + 1} 条'
        if not isinstance(entry, dict) or entry.get('kind') not in FIELDS:
            problems.append(f'{where}：kind 须为 python 或 rust')
            continue
        where += f'（{entry.get("id")}）'
        if set(entry) != FIELDS[entry['kind']]:
            problems.append(f'{where}：字段须为 {sorted(FIELDS[entry["kind"]])}')
            continue
        if not isinstance(entry['id'], str) or not entry['id'] or any(c.isspace() for c in entry['id']):
            problems.append(f'{where}：id 须是不含空白的测试名')
        if entry['kind'] == 'rust' and not (ROOT / str(entry['crate']) / 'Cargo.toml').is_file():
            problems.append(f'{where}：crate 不是仓库里的 Cargo 项目')
        platforms = entry['platforms']
        if not isinstance(platforms, list) or not platforms or not set(platforms) <= set(PLATFORMS.values()) \
                or len(set(platforms)) != len(platforms):
            problems.append(f'{where}：platforms 须是 macos、windows 中不重复的一个或两个')
        if not isinstance(entry['issue'], int) or isinstance(entry['issue'], bool) or entry['issue'] <= 0:
            problems.append(f'{where}：issue 须是正整数')
        try:
            added = datetime.date.fromisoformat(entry['added'])
            expires = datetime.date.fromisoformat(entry['expires'])
        except (TypeError, ValueError):
            problems.append(f'{where}：added、expires 须是 YYYY-MM-DD')
        else:
            if expires < added:
                problems.append(f'{where}：expires 早于 added')
            elif today() > expires:
                problems.append(f'{where}：已于 {expires} 到期（#{entry["issue"]}），修好后删掉这一条，或在 Issue 里写明理由再续期')
        key = (entry['kind'], entry.get('crate'), entry['id'])
        if key in seen:
            problems.append(f'{where}：重复的条目')
        seen.add(key)
    if problems:
        raise QuarantineError('隔离名单不合格：\n' + '\n'.join('  ' + line for line in problems))
    return entries


def describe(entry):
    return f'{entry["id"]}（#{entry["issue"]}，{"/".join(entry["platforms"])}，{entry["expires"]} 到期）'


def report(lines):
    """输出「隔离名单中的失败」一节；在 GitHub Actions 里同时写进 job 摘要。"""
    if not lines:
        return
    text = '\n'.join([f'### {HEADING}', '', *(f'- {line}' for line in lines), ''])
    print('\n' + text, flush=True)
    if summary := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(summary, 'a', encoding='utf-8') as output:
            output.write(text + '\n')


def cargo_test(manifest, extra):
    """`cargo test` 的替代：名单里本平台的 Rust 测试单独运行、只汇报；其余测试照常决定退出码。"""
    crate = Path(manifest).resolve().parent.relative_to(ROOT).as_posix()
    entries = [entry for entry in load() if entry['kind'] == 'rust' and entry['crate'] == crate]
    command = [*CARGO, 'test', '--locked', '--manifest-path', manifest, *extra]
    if not entries:
        return subprocess.call(command)
    listed = subprocess.run([*command, '--', '--list'], stdout=subprocess.PIPE, text=True, encoding='utf-8',
                            errors='replace')
    if listed.returncode:
        sys.stdout.write(listed.stdout)
        return listed.returncode
    names = {line[:-len(': test')] for line in listed.stdout.splitlines() if line.endswith(': test')}
    missing = [entry for entry in entries if entry['id'] not in names]
    if missing:
        print('隔离名单里的 Rust 测试在 ' + crate + ' 中找不到，删掉或改正这些条目：', file=sys.stderr)
        for entry in missing:
            print('  ' + describe(entry), file=sys.stderr)
        return 1
    held = [entry for entry in entries if platform() in entry['platforms']]
    skips = [arg for entry in held for arg in ('--skip', entry['id'])]
    # --exact 让 --skip 只跳过同名测试，不跳过名字里含有它的其他测试。
    status = subprocess.call([*command, '--', '--exact', *skips])
    failures = []
    for entry in held:
        print(f'\n== 隔离名单：单独运行 {describe(entry)} ==', flush=True)
        run = subprocess.run([*command, '--', '--exact', entry['id']], stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, text=True, encoding='utf-8', errors='replace')
        sys.stdout.write(run.stdout)
        ran = sum(int(a) + int(b) for a, b in re.findall(r'test result: \w+\. (\d+) passed; (\d+) failed', run.stdout))
        if ran == 0:
            print(f'隔离名单里的 {entry["id"]} 一项也没有运行', file=sys.stderr)
            return 1
        if run.returncode:
            failures.append(f'{describe(entry)}：失败（退出码 {run.returncode}）')
    report(failures)
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest='command', required=True)
    sub.add_parser('check', help='只校验名单的格式和到期日')
    cargo = sub.add_parser('cargo-test', help='代替 cargo test，按名单单独运行并汇报隔离的测试')
    cargo.add_argument('--manifest-path', required=True)
    args, extra = parser.parse_known_args()
    try:
        if args.command == 'check':
            entries = load()
            print(f'隔离名单：{len(entries)} 条，本平台 {platform()} 适用 '
                  f'{sum(platform() in entry["platforms"] for entry in entries)} 条')
            return 0
        return cargo_test(args.manifest_path, extra)
    except QuarantineError as error:
        print(error, file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
