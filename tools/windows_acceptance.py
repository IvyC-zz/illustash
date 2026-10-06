"""Windows 基础版验收（#72）：只用合成数据，只测原生程序。

Python 只负责驱动和生成合成数据；被测进程的 PATH 里没有 Python，进程启动事件全程记录，
产品进程树里出现任何 python/py 进程即判失败。不读写真实图库、账号或用户目录（USERPROFILE、
APPDATA、ILLUSTASH_APP_SUPPORT 都指向 --work 下的合成目录）。

用法（Windows，管理员 PowerShell，仓库根目录）：
  python tools/windows_acceptance.py --bin <基础版 bin\\illustash-terms.exe> --work <空目录>
      [--desktop <已安装的 illustash-desktop.exe>] [--rounds 3] [--requests 100]
--desktop 需要管理员已登录本机控制台：桌面窗口经计划任务在该会话里打开、截图后关闭。
结果写 <work>\\result.json，并打印逐项通过/失败；任一项失败时退出码为 1。
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import statistics
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from tools.verify_read_http import fetch  # noqa: E402

PYTHON_NAMES = {'python.exe', 'python3.exe', 'pythonw.exe', 'py.exe', 'pyw.exe'}
MONITOR = r'''
$ErrorActionPreference = 'Stop'
Register-CimIndicationEvent -ClassName Win32_ProcessStartTrace -SourceIdentifier illustash-accept | Out-Null
Set-Content -Path $args[1] -Value 'ready'
while ($true) {
  $e = Wait-Event -SourceIdentifier illustash-accept
  $n = $e.SourceEventArgs.NewEvent
  Add-Content -Path $args[0] -Value ("{0}`t{1}`t{2}" -f $n.ProcessID, $n.ParentProcessID, $n.ProcessName)
  Remove-Event -EventIdentifier $e.EventIdentifier
}
'''
DESKTOP = r'''
param($exe, $library, $port, $out, $shot)
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System; using System.Runtime.InteropServices;
public static class Win {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern IntPtr GetMenu(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
}
'@
[Win]::SetProcessDPIAware() | Out-Null  # 坐标与截屏都用物理像素
$p = Start-Process -FilePath $exe -ArgumentList @('--library', $library, '--port', $port) -PassThru
$ready = $false
for ($i = 0; $i -lt 120 -and -not $ready; $i++) {
  Start-Sleep -Milliseconds 500
  try { $r = Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:$port/api/bootstrap" -TimeoutSec 2; $ready = $r.StatusCode -eq 200 } catch {}
}
Start-Sleep -Seconds 4
$p.Refresh()
$children = @(Get-CimInstance Win32_Process -Filter "ParentProcessId=$($p.Id)" | ForEach-Object { $_.Name })
# 只截应用窗口自己的区域，不截桌面上的其他内容
[Win]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
Start-Sleep -Milliseconds 800
$r = New-Object Win+RECT
# DWMWA_EXTENDED_FRAME_BOUNDS = 9：可见窗口边界，不含透明的缩放边框
if ([Win]::DwmGetWindowAttribute($p.MainWindowHandle, 9, [ref]$r, 16) -eq 0 -and $r.R -gt $r.L) {
  $bmp = New-Object System.Drawing.Bitmap ($r.R - $r.L), ($r.B - $r.T)
  [System.Drawing.Graphics]::FromImage($bmp).CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
  $bmp.Save($shot, [System.Drawing.Imaging.ImageFormat]::Png)
}
$menuZero = [Win]::GetMenu($p.MainWindowHandle).ToInt64() -eq 0
$title = $p.MainWindowTitle
$closed = $p.CloseMainWindow()
$exited = $p.WaitForExit(15000)
Start-Sleep -Milliseconds 500
$left = @(Get-CimInstance Win32_Process -Filter "Name='illustash-desktop.exe'" | ForEach-Object { $_.ProcessId })
@{ menu_zero = $menuZero; ready = $ready; title = $title; children = $children; close_sent = $closed; exited = $exited;
   exit_code = $(if ($exited) { $p.ExitCode } else { $null }); remaining = $left; pid = $p.Id } |
  ConvertTo-Json | Set-Content -Encoding utf8 $out
'''


class Acceptance:
    def __init__(self, args):
        self.args = args
        self.binary = Path(args.bin).resolve()
        self.work = Path(args.work).resolve()
        self.results, self.roots = [], []
        self.home = self.work / 'home'
        self.env = {
            'SystemRoot': os.environ['SystemRoot'], 'SystemDrive': os.environ.get('SystemDrive', 'C:'),
            'PATH': str(Path(os.environ['SystemRoot']) / 'System32'),  # 被测进程找不到任何 Python
            'TEMP': str(self.work / 'tmp'), 'TMP': str(self.work / 'tmp'),
            'USERPROFILE': str(self.home), 'HOME': str(self.home),
            'APPDATA': str(self.home / 'AppData' / 'Roaming'), 'LOCALAPPDATA': str(self.home / 'AppData' / 'Local'),
            'ILLUSTASH_APP_SUPPORT': str(self.work / 'support'), 'ILLUSTASH_SHARED': str(self.work / 'shared'),
        }

    # ---------- 基础设施 ----------
    def check(self, name, ok, detail=''):
        if isinstance(detail, bytes):
            detail = detail.decode('utf-8', errors='replace')
        self.results.append({'name': name, 'ok': bool(ok), 'detail': detail})
        print(('PASS ' if ok else 'FAIL ') + name + (f' — {detail}' if detail else ''), flush=True)

    def port(self):
        with socket.socket() as s:
            s.bind(('127.0.0.1', 0))
            return s.getsockname()[1]

    def released(self, port):
        with socket.socket() as s:  # Windows 上不设 SO_REUSEADDR，否则仍在监听的端口也能绑上
            try:
                s.bind(('127.0.0.1', port)); return True
            except OSError:
                return False

    def wait(self, predicate, timeout=10):
        until = time.time() + timeout
        while time.time() < until:
            try:
                if predicate():
                    return True
            except OSError:
                pass
            time.sleep(0.1)
        return False

    def start(self, library, port, *extra, env=None, executable=None, stdin=False):
        log = open(self.work / f'host-{port}.log', 'wb')
        args = [str(executable or self.binary), 'http', '--native', '--library', str(library), '--port', str(port),
                '--no-open', '--no-names', '--no-index-check', '--no-auto-sync', *extra]
        p = subprocess.Popen(args, env=env or self.env, stdout=log, stderr=log, cwd=self.work,
                             stdin=subprocess.PIPE if stdin else None,
                             creationflags=subprocess.CREATE_NEW_PROCESS_GROUP)
        if stdin:
            p.stdin.write(b'\x01'); p.stdin.flush()
        self.roots.append(p.pid)
        origin = f'http://127.0.0.1:{port}'
        up = self.wait(lambda: fetch(origin, {'url': '/api/bootstrap'}, origin)['status'] == 200, 15)
        return p, origin, up

    def get(self, origin, url):
        return fetch(origin, {'url': url}, origin)

    def post(self, origin, url, body, token):
        return fetch(origin, {'url': url, 'method': 'POST', 'body': json.dumps(body).encode(),
                              'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': token}}, origin)

    def token(self, origin):
        return json.loads(self.get(origin, '/api/bootstrap')['body'])['token']

    def stop(self, p, how='break'):
        if how == 'break':
            p.send_signal(signal.CTRL_BREAK_EVENT)
        else:
            p.stdin.close()
        try:
            return p.wait(15)
        except subprocess.TimeoutExpired:
            p.kill(); p.wait(5); return 'killed'

    # ---------- 合成数据 ----------
    def synthetic(self):
        from PIL import Image, ImageDraw
        from tests.test_native_library_create import synthetic_reference
        for d in ('tmp', 'support', 'shared', 'home'):
            (self.work / d).mkdir(parents=True, exist_ok=True)
        pics = self.work / 'pictures'
        pics.mkdir()
        colours = ['red', 'green', 'blue', 'orange', 'purple', 'teal']
        for i, c in enumerate(colours):
            im = Image.new('RGB', (640 + 40 * i, 480), c)
            ImageDraw.Draw(im).text((20, 20), f'synthetic {i}', fill='white')
            im.save(pics / f'p{i}.png')
        Image.new('RGBA', (300, 300), (255, 0, 0, 0)).save(pics / 'alpha.png')        # 透明
        tall = Image.new('RGB', (200, 400), 'navy'); ex = tall.getexif(); ex[0x0112] = 6  # EXIF 方向：旋转 90°
        tall.save(pics / 'rotated.jpg', exif=ex)
        Image.new('RGB', (4000, 3000), 'gray').save(pics / 'large.jpg', quality=70)
        (pics / '中文 目录').mkdir()
        Image.new('RGB', (320, 240), 'gold').save(pics / '中文 目录' / '图片.png')
        (pics / 'broken.png').write_bytes(b'not an image')
        self.images = sorted(p for p in pics.rglob('*') if p.is_file())
        reference = synthetic_reference(self.work / 'shared')
        return pics, reference

    def create(self, name, pics, reference):
        dest = self.work / f'{name}.pilib'
        r = subprocess.run([str(self.binary), 'library-create', '--dest', str(dest), '--folder', str(pics),
                            '--reference', str(reference), '--name', name], env=self.env, capture_output=True, timeout=60)
        self.check(f'CLI 建库 {name}', r.returncode == 0 and (dest / 'library.json').is_file(),
                   r.stderr.decode(errors='replace')[-300:])
        return dest

    # ---------- 验收项 ----------
    def run(self):
        if self.work.exists() and any(self.work.iterdir()):
            raise SystemExit(f'--work 必须是空目录：{self.work}')
        self.work.mkdir(parents=True, exist_ok=True)
        (self.work / 'tmp').mkdir()
        monitor_log, monitor_ready = self.work / 'processes.tsv', self.work / 'monitor.ready'
        script = self.work / 'monitor.ps1'; script.write_text(MONITOR, encoding='utf-8')
        monitor = subprocess.Popen(['powershell', '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', str(script),
                                    str(monitor_log), str(monitor_ready)])
        monitored = self.wait(monitor_ready.exists, 30)
        self.check('进程启动监控就绪（需要管理员）', monitored)
        try:
            pics, reference = self.synthetic()
            first = self.create('first', pics, reference)
            second = self.create('second', pics, reference)
            self.browse_and_curate(first, second)
            self.shutdown_paths(first)
            self.worker_absent(first)
            self.performance(first)
            if self.args.desktop:
                self.desktop(first)
        finally:
            time.sleep(1)
            monitor.kill()
        self.python_never_spawned(monitor_log)
        return all(r['ok'] for r in self.results)

    def browse_and_curate(self, first, second):
        port = self.port()
        p, origin, up = self.start(first, port)
        self.check('CLI 原生前门启动并就绪', up)
        if not up:
            return
        token = self.token(origin)
        r = self.post(origin, '/api/sync', {}, token)
        self.check('文件夹同步发起', r['status'] == 200, r['body'][:200])
        # 没有识别 worker 时同步停在 needs_recognition：图已入库可浏览，识别留到之后。
        done = self.wait(lambda: json.loads(self.get(origin, '/api/sync')['body'])['state']['status']
                         not in ('idle', 'scanning', 'importing'), 60)
        state = json.loads(self.get(origin, '/api/sync')['body'])['state']
        count = json.loads(self.get(origin, '/api/assets/count')['body']).get('total', 0)
        # 损坏图也入库（尺寸记 0×0），与 test_native_folder 的约定一致。
        self.check('同步后可浏览资产', done and count == len(self.images),
                   f"状态 {state['status']}，可见 {count}，合成文件 {len(self.images)}（含 1 张损坏图）")
        assets = json.loads(self.get(origin, '/api/assets?limit=100')['body'])
        items = assets.get('items', assets) if isinstance(assets, dict) else assets
        ids = [a['uuid'] if isinstance(a, dict) else a for a in items]
        names = {}
        with sqlite3.connect(f'file:{first / "library.sqlite"}?mode=ro', uri=True) as c:
            for uuid, filename in c.execute('SELECT uuid, filename FROM assets'):
                names[uuid] = filename
        from PIL import Image
        import io
        ok_thumbs, shapes = 0, {}
        for uuid in ids:
            for size in ('thumb', 'preview'):
                r = self.get(origin, f'/image?id={uuid}&size={size}')
                if r['status'] == 200:
                    im = Image.open(io.BytesIO(r['body']))
                    shapes[(names.get(uuid), size)] = (im.size, im.mode)
                    ok_thumbs += 1
        self.check('缩略图与预览可取', ok_thumbs >= 2 * (len(ids) - 1), f'{ok_thumbs} 张，资产 {len(ids)}')
        rotated = shapes.get(('rotated.jpg', 'thumb'))
        self.check('缩略图按 EXIF 方向旋转', rotated and rotated[0][0] > rotated[0][1], str(rotated))
        alpha = shapes.get(('alpha.png', 'thumb'))
        self.check('透明图缩略图', alpha is not None, str(alpha))
        large = shapes.get(('large.jpg', 'thumb'))
        self.check('大图缩略图限制在 400 内', large and max(large[0]) <= 400, str(large))
        cjk = [u for u, n in names.items() if n == '图片.png']
        self.check('中文路径图片可取', bool(cjk) and self.get(origin, f'/image?id={cjk[0]}')['status'] == 200)
        for url in ('/api/detail?id=' + (ids[0] if ids else 'x'), '/api/characters', '/api/character/profile',
                    '/api/home', '/api/tagged', '/api/settings', '/api/libraries'):
            r = self.get(origin, url)
            self.check(f'读取 {url.split("?")[0]}', r['status'] in (200, 400) and r['status'] != 500, str(r['status']))
        # 人工整理与连续撤销
        before = json.loads(self.get(origin, '/api/user-tags')['body'])
        undo_ids = []
        for tag in ('合成甲', '合成乙'):
            r = self.post(origin, '/api/tag', {'ids': ids[:2], 'tag': tag}, token)
            self.check(f'打标签 {tag}', r['status'] == 200, r['body'][:200])
            if r['status'] == 200:
                undo_ids.append(json.loads(r['body']).get('undo', {}).get('id'))
        for uid in reversed([u for u in undo_ids if u is not None]):
            r = self.post(origin, '/api/undo', {'id': uid}, token)
            self.check('连续撤销', r['status'] == 200, r['body'][:200])
        after = json.loads(self.get(origin, '/api/user-tags')['body'])
        self.check('撤销后标签恢复原状', after == before, f'{before} → {after}')
        # 安全：Host/Origin/CSRF
        evil = fetch(origin, {'url': '/api/bootstrap', 'headers': {'Host': 'evil.example'}}, origin)
        self.check('拒绝外部 Host', evil['status'] == 403)
        bad = self.post(origin, '/api/tag', {'ids': ids[:1], 'tag': 'x'}, 'wrong-token')
        self.check('拒绝错误 CSRF 令牌', bad['status'] == 403)
        # 备份、切库、令牌轮换、导出导入
        r = self.post(origin, '/api/backup', {}, token)
        backups = list(first.glob('backups/*')) + list(first.glob('library-backups/*'))
        self.check('手动备份', r['status'] == 200 and backups, r['body'][:200])
        r = self.post(origin, '/api/libraries/open', {'path': str(second)}, token)
        new_token = self.token(origin)
        self.check('运行中切库', r['status'] == 200 and json.loads(self.get(origin, '/api/libraries')['body'])['current'] == str(second))
        self.check('切库后会话令牌轮换', new_token != token)
        self.check('旧令牌写入被拒', self.post(origin, '/api/tag', {'ids': ids[:1], 'tag': 'x'}, token)['status'] == 403)
        r = self.post(origin, '/api/libraries/open', {'path': str(first)}, new_token)
        token = self.token(origin)
        r = self.post(origin, '/api/libraries/export', {}, token)
        exported = None
        if r['status'] == 200:
            export = lambda: json.loads(self.get(origin, '/api/libraries')['body'])['export']
            self.wait(lambda: export().get('status') in ('done', 'error'), 60)
            state = export()
            exported = state.get('path') if state.get('status') == 'done' else None
        self.check('导出图库归档', exported and Path(exported).is_file(), r['body'][:200])
        if exported:
            r = self.post(origin, '/api/libraries/import', {'path': exported}, token)
            self.check('导入图库归档', r['status'] == 200, r['body'][:200])
        code = self.stop(p)
        self.check('CTRL_BREAK 正常退出', code == 0, str(code))
        self.check('退出后端口释放', self.released(port))

    def shutdown_paths(self, library):
        port = self.port()
        p, origin, up = self.start(library, port, stdin=True, env={**self.env, 'ILLUSTASH_DESKTOP_STDIN': '1'})
        code = self.stop(p, 'stdin')
        self.check('桌面 stdin 关闭后正常退出', up and code == 0, str(code))
        self.check('stdin 退出后端口释放', self.released(port))
        # 端口冲突：第二个实例失败退出，不影响第一个
        port = self.port()
        p, origin, up = self.start(library, port)
        r = subprocess.run([str(self.binary), 'http', '--native', '--library', str(library), '--port', str(port),
                            '--no-open', '--no-auto-sync'], env=self.env, capture_output=True, timeout=20)
        self.check('端口冲突时第二实例失败', r.returncode != 0)
        self.check('端口冲突不影响已运行实例', p.poll() is None and self.get(origin, '/api/bootstrap')['status'] == 200)
        self.stop(p)
        # 失败启动：图库不存在
        missing = self.work / 'missing.pilib'
        r = subprocess.run([str(self.binary), 'http', '--native', '--library', str(missing), '--port', str(self.port()),
                            '--no-open'], env=self.env, capture_output=True, timeout=20)
        self.check('不存在的图库启动失败且不建目录', r.returncode != 0 and not missing.exists())
        # 异常终止：强杀后图库仍能正常打开
        port = self.port()
        p, origin, up = self.start(library, port)
        p.kill(); p.wait(5)
        self.check('强杀后端口释放', self.wait(lambda: self.released(port), 10))
        p, origin, up = self.start(library, self.port())
        self.check('强杀后图库可重新打开', up)
        if up:
            self.stop(p)

    def worker_absent(self, library):
        env = {**self.env, 'ILLUSTASH_RESEARCH_WORKER': str(self.work / 'no-such-worker.exe')}
        p, origin, up = self.start(library, self.port(), env=env)
        if not up:
            self.check('配置了缺失的研究 worker 仍能启动', False); return
        token = self.token(origin)
        r = self.post(origin, '/api/similar', {}, token)
        time.sleep(1)
        state = self.get(origin, '/api/similar')
        self.check('研究 worker 不可用时明确报告', r['status'] in (200, 400, 503), f'{r["status"]} {state["body"][:200]}')
        self.check('worker 不可用时浏览照常', self.get(origin, '/api/assets/count')['status'] == 200)
        self.check('worker 不可用时人工操作照常',
                   self.post(origin, '/api/tag', {'ids': [], 'tag': '合成丙'}, token)['status'] in (200, 400))
        self.stop(p)

    def performance(self, library):
        port = self.port()
        p, origin, up = self.start(library, port)
        if not up:
            self.check('性能测量启动', False); return
        rounds = []
        for _ in range(self.args.rounds):
            samples = []
            for _ in range(self.args.requests):
                t = time.perf_counter(); self.get(origin, '/api/assets'); samples.append((time.perf_counter() - t) * 1000)
            rounds.append(statistics.median(samples))
        ws = subprocess.run(['powershell', '-NoProfile', '-Command', f'(Get-Process -Id {p.pid}).PeakWorkingSet64'],
                            capture_output=True, text=True).stdout.strip()
        self.stop(p)
        self.perf = {'rounds_median_ms': [round(x, 3) for x in rounds], 'median_ms': round(statistics.median(rounds), 3),
                     'peak_working_set_mib': round(int(ws) / 1048576, 2) if ws.isdigit() else None,
                     'requests_per_round': self.args.requests, 'url': '/api/assets', 'assets': len(self.images)}
        self.check('性能测量（只记录）', True, json.dumps(self.perf, ensure_ascii=False))

    def desktop(self, library):
        exe = Path(self.args.desktop).resolve()
        port = self.port()
        out, shot = self.work / 'desktop.json', self.work / 'desktop.png'
        script = self.work / 'desktop.ps1'; script.write_text(DESKTOP, encoding='utf-8-sig')
        runner = self.work / 'desktop-run.ps1'
        # 计划任务继承不到本进程的环境；把合成目录写进启动脚本，用隐藏窗口的 PowerShell 运行
        lines = [f"$env:{k} = '{v}'" for k, v in self.env.items() if k not in ('PATH', 'SystemRoot', 'SystemDrive')]
        lines.append(f"& '{script}' '{exe}' '{library}' {port} '{out}' '{shot}'")
        runner.write_text('\r\n'.join(lines) + '\r\n', encoding='utf-8-sig')
        task = f'illustash-accept-desktop-{port}'
        command = f'powershell -NoProfile -WindowStyle Hidden -ExecutionPolicy Bypass -File "{runner}"'
        subprocess.run(['schtasks', '/Create', '/F', '/TN', task, '/TR', command, '/SC', 'ONCE', '/ST', '23:59',
                        '/IT', '/RL', 'HIGHEST'], capture_output=True)
        subprocess.run(['schtasks', '/Run', '/TN', task], capture_output=True)
        finished = self.wait(out.exists, 120)
        subprocess.run(['schtasks', '/Delete', '/F', '/TN', task], capture_output=True)
        if not finished:
            self.check('桌面窗口验收完成', False, '计划任务未产出结果（本机控制台没有已登录的管理员？）'); return
        time.sleep(0.5)
        result = json.loads(out.read_text(encoding='utf-8-sig'))
        self.desktop_result = result
        self.check('桌面启动且内置服务就绪', result['ready'], json.dumps(result, ensure_ascii=False))
        self.check('桌面窗口标题', result.get('title') == 'Illustash', str(result.get('title')))
        self.check('Windows 无原生菜单栏', result.get('menu_zero') is True)
        self.check('关闭窗口后桌面正常退出', result['exited'] and result['exit_code'] == 0, str(result.get('exit_code')))
        self.check('关闭后无残留桌面/服务进程', not result['remaining'], str(result['remaining']))
        self.check('关闭后端口释放', self.released(port))
        self.check('桌面截图已保存', shot.is_file(), str(shot))

    def python_never_spawned(self, log):
        if not log.exists():
            self.check('产品进程树没有启动 Python', False, '没有进程记录'); return
        rows = [line.split('\t') for line in log.read_text(encoding='utf-8', errors='replace').splitlines() if line.count('\t') == 2]
        children = {}
        for pid, parent, name in rows:
            children.setdefault(int(parent), []).append((int(pid), name))
        roots = set(self.roots) | {int(pid) for pid, _, name in rows if name.lower().startswith('illustash-')}
        seen, stack, names = set(), list(roots), []
        while stack:
            pid = stack.pop()
            for child, name in children.get(pid, []):
                if child not in seen:
                    seen.add(child); stack.append(child); names.append(name)
        bad = [n for n in names if n.lower() in PYTHON_NAMES]
        self.check('产品进程树没有启动 Python', not bad,
                   f'记录 {len(rows)} 个进程启动；产品后代 {len(names)} 个：{sorted(set(names))}')


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--bin', required=True)
    parser.add_argument('--work', required=True)
    parser.add_argument('--desktop')
    parser.add_argument('--rounds', type=int, default=3)
    parser.add_argument('--requests', type=int, default=100)
    args = parser.parse_args()
    if os.name != 'nt':
        raise SystemExit('只在 Windows 上运行')
    a = Acceptance(args)
    ok = a.run()
    summary = {'ok': ok, 'binary': str(a.binary), 'results': a.results, 'performance': getattr(a, 'perf', None),
               'desktop': getattr(a, 'desktop_result', None)}
    (a.work / 'result.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2), encoding='utf-8')
    passed = sum(r['ok'] for r in a.results)
    print(f'\n{passed}/{len(a.results)} 通过')
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
