"""Run provenance steps on a library package.

    .venv/bin/python -m provenance.run --library ~/Pictures/Illustash/我的图库.pilib --steps pixel_hash --limit 500

The app starts this as a child process with `--json` (tests/reference/web/provenance_job.py): one JSON object per line on stdout,
{"log": text} while a step works, {"step": name, "result": {...}} after it, {"done": status, ...} at the end.
Every run leaves a row in provenance.sqlite `runs`.
"""
from contextlib import ExitStack
import argparse
import os
import threading
import time
from provenance.lifecycle import Lease, recover, finish
import json
import signal
import sys
from pathlib import Path

from web import library as libraries
from provenance import accounts, db, desktop, report, verify
from provenance.net import BudgetExceeded, Net, Stopped
from provenance.sources import groups, iqdb, pixel_hash, pixiv_related, x_archive, pixiv_following, nicknames

STEPS = {'groups': groups.run, 'local_match': verify.local_run, 'pixiv_following': pixiv_following.run, 'nicknames': nicknames.run, 'pixel_hash': pixel_hash.run, 'iqdb': iqdb.run, 'x_following': x_archive.run, 'verify': verify.run, 'pixiv_related': pixiv_related.run, 'report': report.run}
KINDS = ('cli', 'full', 'new', 'pick', 'artist')   # pick / artist: 按需查找 from the library view
LOCAL_STEPS = {'report', 'local_match', 'groups'}   # never ask outside: no authorisation needed
# Steps that ask other hosts than the steps after them run beside those (2026-09-29): IQDB asks danbooru.donmai.us's API
# while verification downloads thumbnails from the CDNs, so neither waits for the other. Each host keeps its own pace.
BESIDE = {'iqdb'}
AFTER_BESIDE = {'groups'}   # wait for the steps beside: this run's IQDB finds spread through the groups too
PIXIV_STEPS = {'pixiv_following', 'nicknames', 'verify', 'pixiv_related'}   # steps that ask www.pixiv.net
LOG = libraries.APP_SUPPORT / 'logs' / 'provenance.log'   # every worker log line, all libraries; rotated at 5 MB


def net_for(budget_mb):
    return accounts.make_net(budget_mb)   # bound accounts (the app's Pixiv window or the Keychain), shared by every library


def _ask_login(kind, log):
    """The login expired on a run the person started: bring up the app's Pixiv window so they can log in again.
    Runs queued after a sync don't pop up a window by themselves; the settings page shows the expiry."""
    if kind == 'new' or not accounts.pixiv_in_app() or not desktop.available():
        log(f'不弹出 Pixiv 登录窗口（运行方式 {kind}，窗口登录 {accounts.pixiv_in_app()}，桌面 app {desktop.available()}）')
        return
    try:
        desktop.login(force=True)
        log('已打开 Pixiv 登录窗口，重新登录后再运行一次')
    except Exception as exc:     # never let the popup take the run down; say why it did not appear
        log(f'没能打开 Pixiv 登录窗口：{type(exc).__name__}: {exc}')


def _attributed(library):
    with db.connect(library) as c:
        return c.execute("SELECT count(DISTINCT asset) FROM image_artists WHERE status='auto'").fetchone()[0]


def _check_pixiv(net, log, kind):
    """An expired session must not quietly turn into logged-out lookups (no R18, cached as misses):
    stop Pixiv for this run instead, so its steps report the reason and retry after the user binds again."""
    try:
        ok = accounts.recheck_pixiv(net)
    except (Stopped, BudgetExceeded, ConnectionError) as exc:
        log(f'没能检查 Pixiv 登录：{exc}')
        return
    if ok is False:
        detail = accounts.status()['pixiv'].get('expired_detail') or ''
        net.stopped['www.pixiv.net'] = accounts.PIXIV_EXPIRED + (f'：{detail}' if detail else '')
        log(net.stopped['www.pixiv.net'])
        _ask_login(kind, log)


def _narrowed(step, args):
    """按需查找 narrows a step to the pictures or artists the person picked."""
    if args.only and step in ('pixel_hash', 'iqdb'):
        return {'only': [x for x in args.only.split(',') if x]}
    if args.artists and step == 'verify':
        return {'artists': [x for x in args.artists.split(',') if x]}
    return {}


def _beside(index, step, lib, net, limit, keep, emit):
    """Start `step` in a thread; its text and numbers go out as `side` events so the app shows them next to the main step."""
    stop, side = threading.Event(), {'index': index, 'step': step, 'result': {}}

    def side_log(text):
        emit(side={'step': step, 'text': text})
        keep(text)
    side_log.progress = lambda **numbers: emit(side={'step': step, 'progress': numbers})

    def body():
        try:
            side['result'] = STEPS[step](lib, net, limit=limit, log=side_log, stop=stop)
        except Exception as exc:   # recorded like a failed step; the main steps go on
            side['result'] = {'failed': 1, 'stopped': f'{type(exc).__name__}: {exc}'}
    emit(side={'step': step, 'text': '开始…'})
    side.update(stop=stop, thread=threading.Thread(target=body, daemon=True))
    side['thread'].start()
    return side


class Interrupted(Exception):
    pass


def _interrupt(*_):
    raise Interrupted('已手动停止')


def _main(argv, stack):
    ap = argparse.ArgumentParser(description='溯源：找回画师')
    ap.add_argument('--library', type=Path, required=True)
    ap.add_argument('--steps', default='pixel_hash,x_following,verify,pixiv_related,verify', help='逗号分隔，可以重复：' + '、'.join(STEPS))
    ap.add_argument('--x-archive', type=Path, help='先导入 X 数据归档（zip 或解压后的文件夹）的关注列表')
    ap.add_argument('--limit', type=int, help='每步最多处理几张（试跑用）')
    ap.add_argument('--budget-mb', type=float, default=300, help='本次运行的外网流量上限')
    ap.add_argument('--kind', choices=KINDS, default='cli', help='谁发起的：命令行、App 里手动、同步后自动')
    ap.add_argument('--allow-network', action='store_true', help='本次明确授权外部请求；默认只允许本地步骤')
    ap.add_argument('--run-id', type=int, help=argparse.SUPPRESS)
    ap.add_argument('--owner-pid', type=int, help=argparse.SUPPRESS)
    ap.add_argument('--only', help='只查这些图（逗号分隔的 uuid；按需查找）')
    ap.add_argument('--artists', help='只比对这些画师（逗号分隔的 id；「找 TA 的其他图」）')
    ap.add_argument('--json', action='store_true', help='每行输出一个 JSON 事件（App 读取进度用）')
    args = ap.parse_args(argv)
    steps = [s for s in args.steps.split(',') if s]
    if args.limit is not None and args.limit < 0:
        ap.error('limit 不可为负数')
    if args.budget_mb < 0:
        ap.error('budget 不可为负数')
    if unknown := [s for s in steps if s not in STEPS]:
        ap.error('未知步骤：' + '、'.join(unknown))
    printing = threading.Lock()   # a step beside the others prints too: one whole line at a time

    def emit(**e):
        with printing:
            print(json.dumps(e, ensure_ascii=False), flush=True)
    emit = emit if args.json else None
    shown = (lambda text: emit(log=text)) if emit else print

    def keep(text):   # the app only shows the latest line: keep every line on disk for diagnosis
        try:
            LOG.parent.mkdir(parents=True, exist_ok=True)
            if LOG.exists() and LOG.stat().st_size > 5 * 2 ** 20:
                os.replace(LOG, LOG.with_suffix('.log.1'))
            with LOG.open('a', encoding='utf-8') as f:
                f.write(f"{time.strftime('%Y-%m-%d %H:%M:%S')} [{args.run_id or 'cli'}] {text}\n")
        except OSError:
            pass

    def log(text):
        shown(text)
        keep(text)
    # Numbers for the settings page's progress bars; steps call it as getattr(log, 'progress', None) (tests pass plain logs).
    log.progress = (lambda **numbers: emit(progress=numbers)) if emit else (lambda **numbers: None)
    lib = libraries.open_library(args.library)
    lease = Lease(lib)
    stack.callback(lease.close)
    if args.run_id is None:
        if not lease.acquire():
            ap.error('这个图库已有溯源任务运行中')
        recover(lib)
    elif not args.owner_pid or os.getppid() != args.owner_pid:
        ap.error('任务所有者不匹配')
    net = Net(0)
    net.authorized = False
    with db.connect(lib) as c:
        run_id = args.run_id or c.execute('INSERT INTO runs(kind, steps) VALUES (?, ?)', (args.kind, ','.join(steps))).lastrowid
    signal.signal(signal.SIGTERM, _interrupt)
    if args.owner_pid:
        def watch_owner():
            while os.getppid() == args.owner_pid:
                time.sleep(0.5)
            if os.name != 'nt':
                os.killpg(os.getpgrp(), signal.SIGKILL)
            else:
                os.kill(os.getpid(), signal.SIGTERM)
        threading.Thread(target=watch_owner, daemon=True).start()
    before, results, reasons, status, coverage, beside = 0, {}, [], 'done', None, []
    try:
        before = _attributed(lib)
        net = net_for(args.budget_mb) if args.allow_network else Net(0)
        net.authorized = args.allow_network
        if args.allow_network and PIXIV_STEPS & set(steps) and accounts.pixiv_bound():
            _check_pixiv(net, log, args.kind)
        if args.x_archive:
            log(f'X 关注：新导入 {x_archive.import_archive(lib, args.x_archive)} 个账号')
        for i, step in enumerate(steps):
            if step not in LOCAL_STEPS and not args.allow_network:   # skip it; local steps after it (groups) still run
                status, reasons = 'stopped', ['外部请求未授权']
                continue
            if step in BESIDE and emit and i + 1 < len(steps):
                beside.append(_beside(i, step, lib, net, args.limit, keep, emit))
                continue
            if step in AFTER_BESIDE:
                for side in beside:
                    if side['thread'].is_alive():
                        log('等待 IQDB 查相似图完成，再进行分组推荐…')
                    side['thread'].join()
            if emit:
                emit(step_start=step, index=i, total=len(steps))
            result = STEPS[step](lib, net, limit=args.limit, log=log, **_narrowed(step, args))
            results[f'{i + 1}.{step}'] = result
            if result.get('failed'):
                status = 'failed'
                reasons.append(f"{step}：{result['failed']} 项失败")
            if step == 'report':
                coverage = result
            elif result.get('stopped') and result['stopped'] not in reasons:
                reasons.append(result['stopped'])
                if status != 'failed':
                    status = 'stopped'
            if emit:
                emit(step=step, result=result)
            else:
                print(step, json.dumps(result, ensure_ascii=False))
    except (Interrupted, KeyboardInterrupt) as exc:
        status, reasons = 'stopped', [str(exc) or '已手动停止']
    except Exception as exc:     # recorded for the app, then raised for the log
        status, reasons = 'failed', [f'{type(exc).__name__}: {exc}']
        raise
    finally:
        for side in beside:   # wait for the steps that ran beside (asked to stop if the run itself stopped)
            if status != 'done':
                side['stop'].set()
            side['thread'].join()
            step, result = side['step'], side['result']
            results[f"{side['index'] + 1}.{step}"] = result
            if result.get('failed') and status == 'done':
                status = 'failed'
                reasons.append(f"{step}：{result['failed']} 项失败")
            elif result.get('stopped') and result['stopped'] not in reasons and status == 'done':
                reasons.append(result['stopped'])
                status = 'stopped'
            if emit:
                emit(side_done=step, result=result)
        if coverage is None and args.kind != 'cli' and status != 'failed':
            try:
                coverage = report.run(lib)
            except Exception as exc:
                status, reasons = 'failed', [f'统计失败：{exc}']
        if 'www.pixiv.net' in getattr(net, 'rejected', ()):    # the session expired during the run
            accounts.mark_pixiv(problem=accounts.PIXIV_EXPIRED)
            _ask_login(args.kind, log)
        found = _attributed(lib) - before
        with db.connect(lib) as c:
            c.execute("""UPDATE runs SET finished_at=datetime('now'), status=?, reason=?, results=?, found=?, traffic_mb=?, report=?
                         WHERE id=?""", (status, '；'.join(reasons), json.dumps(results, ensure_ascii=False), found,
                                         round(net.used / 2 ** 20, 2), json.dumps(coverage, ensure_ascii=False) if coverage else None, run_id))
        if emit:
            emit(done=status, reason='；'.join(reasons), found=found, traffic_mb=round(net.used / 2 ** 20, 2))
        else:
            print(f'新找到画师 {found} 张；流量合计 {net.used / 2 ** 20:.1f} MB')
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    lease.close()
    return 0 if status != 'failed' else 1


def main(argv=None):
    previous = signal.getsignal(signal.SIGTERM)
    try:
        with ExitStack() as stack:
            return _main(argv, stack)
    finally:
        signal.signal(signal.SIGTERM, previous)


if __name__ == '__main__':
    sys.exit(main())
