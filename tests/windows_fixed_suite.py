"""运行 Windows 的 Python 全量或定向选择，记录输出并拦截白名单外的跳过。

不带 `--select` 时是全量；带 `--select`（可重复，原样交给 `tests.run_fixed_suite`）时只跑选中的测试，
用于 Windows 快检和定向复跑（#501）。`--scope` 是写进日志和 job 摘要的范围名，定向结果不能当作全量。
"""
import argparse
import ast
from collections import Counter
import json
import os
from pathlib import Path
import subprocess
import sys


ALLOWED_SKIP_REASONS = frozenset({
    'a FIFO holds the import open; POSIX only',
    'macOS shell launcher acceptance',
    'macOS/Linux only: the Photos permission is a macOS privacy setting, refused here through sandbox-exec',
    'macOS/Linux only; this startup fixture uses accepted macOS native learning',
    'POSIX CI shell with git and jq only',
    'POSIX local acceptance controller only',
    'POSIX shell launcher execution only',
    'POSIX 权限位制造复制失败',
    'POSIX 权限位制造读取失败；Windows ACL 另行验证',
    'Windows 上原生 worker 按设计拒绝启动（app_tasks），不适用',
    '固定期望录自 macOS：资产键序与文件元数据随平台不同',
    '固定期望录自 POSIX，包名含 Windows 不允许的 ?',
    '固定期望录自 POSIX 上的 Python 参考实现；Windows 原生行为见 tools/windows_acceptance.py',
    '目录符号链接在 Windows 上需要额外权限',
    'Apple 照片图库只在 macOS 上提供',
})
SKIP_MARK = ' ... skipped '
REFERENCE_MARK = 'NUMERIC_REFERENCE '


def reference_record(line):
    """verbosity=2 的方法进度未换行时，首条记录会接在其后。"""
    prefix, marker, payload = line.partition(REFERENCE_MARK)
    if not marker or (prefix and not prefix[-1].isspace()):
        return None
    return json.loads(payload)


def reference_summary(records):
    """按真实进入的比较汇总；失败不会删掉，也不会用后来的参考结果覆盖。"""
    totals = {}
    for record in records:
        key = (record['test'], record['reference'])
        totals.setdefault(key, Counter())[record['status']] += 1
    lines = ['### Windows 数值参考比较', '', '| 方法 | 参考 | 通过的比较 | 失败的比较 |', '|---|---|---:|---:|']
    for (test, reference), count in sorted(totals.items()):
        lines.append(f"| {test} | {reference} | {count['passed']} | {count['failed']} |")
    if not totals:
        lines.append('| 尚未执行到数值参考比较 | — | 0 | 0 |')
    return lines


def skip_allowed(line):
    """unittest 用 repr 输出跳过理由，按完整理由匹配。"""
    _, _, reason = line.partition(SKIP_MARK)
    try:
        return ast.literal_eval(reason) in ALLOWED_SKIP_REASONS
    except (SyntaxError, ValueError, TypeError):
        return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--select', action='append', default=[], help='测试 ID 前缀，如 test_native_research.NativeResearchTest')
    parser.add_argument('--scope', default='', help='写进日志和摘要的范围名；带 --select 时必填')
    args = parser.parse_args()
    if args.select and not args.scope:
        parser.error('带 --select 时须用 --scope 写明范围')
    scope = args.scope or '全量'

    skips = []
    references = []
    tail = []
    command = [sys.executable, '-X', 'utf8', '-m', 'tests.run_fixed_suite', '-v', '-b',
               *(f'--select={prefix}' for prefix in args.select)]
    heading = f'Windows Python 范围：{scope}' + (f'（只跑 {", ".join(args.select)}）' if args.select else '')
    print(heading, flush=True)
    args.log.write_text(heading + '\n', encoding='utf-8', newline='\n')
    with args.log.open('a', encoding='utf-8', newline='\n') as log, subprocess.Popen(
        command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, encoding='utf-8', errors='replace',
    ) as suite:
        for line in suite.stdout:
            log.write(line)
            print(line, end='', flush=True)
            if SKIP_MARK in line:
                skips.append(line.rstrip('\n'))
            if (record := reference_record(line)) is not None:
                references.append(record)
            tail = [*tail[-2:], line.rstrip('\n')]
        status = suite.wait()

    summary = [f'### {heading}', '', *reference_summary(references), '',
               f'### Windows Python 跳过：{len(skips)} 项', '```', *skips, *tail, '```']
    print('\n'.join(summary))
    if path := os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(path, 'a', encoding='utf-8') as output:
            output.write('\n'.join(summary) + '\n')

    unexpected = [line for line in skips if not skip_allowed(line)]
    if unexpected:
        print('以下跳过不在 Windows 白名单内，视为未验收：', *unexpected, sep='\n', file=sys.stderr)
        return 1
    return status


if __name__ == '__main__':
    sys.exit(main())
