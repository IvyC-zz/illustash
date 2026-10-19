"""整应用识别吞吐基准（#213）：一批合成新图从扫描到识别、导入完成的耗时、阶段占比和峰值内存。

只用本工具生成的合成图片与合成锚点，以及显式给出的二进制、模型和运行库；不读真实图库、账号或用户
目录（HOME、USERPROFILE、ILLUSTASH_APP_SUPPORT、临时目录都指向 --work 下）。线程数用产品默认值。

两部分，同一批图片：
  端到端：重定位布局里启动 release 宿主 illustash-terms，以 folder 来源新建图库，POST /api/sync，
    每 20 ms 轮询 GET /api/sync 直到完成；宿主照常拉起 illustash-recognition（即 illustash-worker）
    识别并边识别边导入。记录墙钟总时间与分段、宿主和 worker 的峰值内存与 CPU 时间。
  阶段分解：examples/recognition-compute 的 bench-stages 在一个进程里按 worker 的顺序逐张处理同一批图，给每一步计时：
    解码、WD（含预处理）、CCIP（含预处理）、检测与分级（WD 概率转标签与分级 + 角色锚点认领）、
    写结果包；写库取端到端里宿主在识别阶段用掉的 CPU 时间（导入校验、写 source/应用库、投影，
    含状态轮询的少量开销）。
每项跑 --trials 次（默认 3），报告中位数。

用法（仓库根目录）。计时必须占 heavy 槽，Windows 验收机经 SSH 发起时用 --slot windows：
  cargo build --release --locked --manifest-path native/terms/Cargo.toml \\
      --bin illustash-terms --bin illustash-worker --example recognition-compute
  collab_run.py run -- python3.12 tools/bench_recognition.py \\
      --terms native/terms/target/release/illustash-terms \\
      --worker native/terms/target/release/illustash-worker \\
      --compute native/terms/target/release/examples/recognition-compute \\
      --models <含 wd_swinv2_v3_emb.onnx 与 ccip/model_feat.onnx 的目录> \\
      --runtime <libonnxruntime.dylib 或 onnxruntime.dll> [--openblas <Windows 的 openblas.dll>] \\
      [--images 200] [--anchors 2000] [--characters 500] [--trials 3] [--work <空目录>] [--out result.json]
结果 JSON 写到 --out（默认 <work>/result.json），并打印摘要。只依赖标准库。
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from tools.verify_read_http import fetch  # noqa: E402

WINDOWS = os.name == 'nt'
EXE = '.exe' if WINDOWS else ''
RUNNING = ('scanning', 'importing', 'recognising')
POLL = 0.02
SPAWN = {'creationflags': subprocess.CREATE_NEW_PROCESS_GROUP} if WINDOWS else {}
INTERRUPT = signal.CTRL_BREAK_EVENT if WINDOWS else signal.SIGINT
RUNTIME = 'onnxruntime.dll' if WINDOWS else 'libonnxruntime.dylib'


# ---- 进程内存与 CPU：macOS 用 proc_pid_rusage，Windows 用进程句柄；都是进程生命周期内的峰值。

if sys.platform == 'darwin':
    class _Rusage(ctypes.Structure):  # struct rusage_info_v4，<sys/resource.h>
        _fields_ = [('uuid', ctypes.c_uint8 * 16)] + [(name, ctypes.c_uint64) for name in (
            'user_time', 'system_time', 'pkg_idle_wkups', 'interrupt_wkups', 'pageins', 'wired_size',
            'resident_size', 'phys_footprint', 'proc_start_abstime', 'proc_exit_abstime', 'child_user_time',
            'child_system_time', 'child_pkg_idle_wkups', 'child_interrupt_wkups', 'child_pageins',
            'child_elapsed_abstime', 'diskio_bytesread', 'diskio_byteswritten', 'qos_default',
            'qos_maintenance', 'qos_background', 'qos_utility', 'qos_legacy', 'qos_user_initiated',
            'qos_user_interactive', 'billed_system_time', 'serviced_system_time', 'logical_writes',
            'lifetime_max_phys_footprint', 'instructions', 'cycles', 'billed_energy', 'serviced_energy',
            'interval_max_phys_footprint', 'runnable_time')]

    class _Timebase(ctypes.Structure):
        _fields_ = [('numer', ctypes.c_uint32), ('denom', ctypes.c_uint32)]

    _libproc = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    _timebase = _Timebase()
    ctypes.CDLL('/usr/lib/libSystem.B.dylib').mach_timebase_info(ctypes.byref(_timebase))

    class Probe:
        """phys_footprint 是活动监视器「内存」一栏的口径；ri_*_time 以 mach 时间单位计。"""
        kind = 'phys_footprint（lifetime max）'

        def __init__(self, pid):
            self.pid, self.peak, self.cpu = pid, 0, 0.0

        def sample(self):
            info = _Rusage()
            if _libproc.proc_pid_rusage(self.pid, 4, ctypes.byref(info)) != 0:
                return False
            self.peak = max(self.peak, info.lifetime_max_phys_footprint)
            self.cpu = (info.user_time + info.system_time) * _timebase.numer / _timebase.denom / 1e9
            return True

        def close(self):
            pass

    def children(pid):
        buffer = (ctypes.c_int * 64)()
        count = _libproc.proc_listchildpids(pid, buffer, ctypes.sizeof(buffer))
        return [buffer[i] for i in range(max(count, 0))]

    def name(pid):
        buffer = ctypes.create_string_buffer(256)
        _libproc.proc_name(pid, buffer, 256)
        return buffer.value.decode(errors='replace')

elif WINDOWS:
    from ctypes import wintypes

    class _Counters(ctypes.Structure):
        _fields_ = [('cb', wintypes.DWORD), ('PageFaultCount', wintypes.DWORD)] + [
            (n, ctypes.c_size_t) for n in ('PeakWorkingSetSize', 'WorkingSetSize', 'QuotaPeakPagedPoolUsage',
                                           'QuotaPagedPoolUsage', 'QuotaPeakNonPagedPoolUsage',
                                           'QuotaNonPagedPoolUsage', 'PagefileUsage', 'PeakPagefileUsage')]

    class _Entry(ctypes.Structure):
        _fields_ = [('dwSize', wintypes.DWORD), ('cntUsage', wintypes.DWORD), ('th32ProcessID', wintypes.DWORD),
                    ('th32DefaultHeapID', ctypes.c_void_p), ('th32ModuleID', wintypes.DWORD),
                    ('cntThreads', wintypes.DWORD), ('th32ParentProcessID', wintypes.DWORD),
                    ('pcPriClassBase', ctypes.c_long), ('dwFlags', wintypes.DWORD),
                    ('szExeFile', ctypes.c_wchar * 260)]

    _kernel = ctypes.WinDLL('kernel32', use_last_error=True)
    _kernel.OpenProcess.restype = wintypes.HANDLE
    _kernel.CreateToolhelp32Snapshot.restype = wintypes.HANDLE

    class Probe:
        """句柄在进程退出后仍可读，所以峰值工作集与 CPU 时间都是进程整个生命周期的准确值。"""
        kind = 'PeakWorkingSetSize'

        def __init__(self, pid):
            self.pid, self.peak, self.cpu = pid, 0, 0.0
            self.handle = _kernel.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION

        def sample(self):
            if not self.handle:
                return False
            counters = _Counters(cb=ctypes.sizeof(_Counters))
            if _kernel.K32GetProcessMemoryInfo(self.handle, ctypes.byref(counters), counters.cb):
                self.peak = max(self.peak, counters.PeakWorkingSetSize)
            times = [wintypes.FILETIME() for _ in range(4)]
            if _kernel.GetProcessTimes(self.handle, *map(ctypes.byref, times)):
                self.cpu = sum((t.dwHighDateTime << 32 | t.dwLowDateTime) for t in times[2:]) / 1e7
            return True

        def close(self):
            if self.handle:
                _kernel.CloseHandle(self.handle)
                self.handle = None

    def _processes():
        snapshot = _kernel.CreateToolhelp32Snapshot(2, 0)  # TH32CS_SNAPPROCESS
        entry, rows = _Entry(dwSize=ctypes.sizeof(_Entry)), []
        try:
            ok = _kernel.Process32FirstW(snapshot, ctypes.byref(entry))
            while ok:
                rows.append((entry.th32ProcessID, entry.th32ParentProcessID, entry.szExeFile))
                ok = _kernel.Process32NextW(snapshot, ctypes.byref(entry))
        finally:
            _kernel.CloseHandle(snapshot)
        return rows

    def children(pid):
        return [p for p, parent, _ in _processes() if parent == pid]

    def name(pid):
        return next((n for p, _, n in _processes() if p == pid), '')

else:
    raise SystemExit('只支持 macOS 与 Windows（Linux 已冻结，见 README）')


class Sampler(threading.Thread):
    """每 20 ms 读一次宿主及其子进程；记录各自的生命周期峰值、CPU 时间和同一时刻的合计内存。"""

    def __init__(self, host):
        super().__init__(daemon=True)
        self.host = Probe(host)
        self.workers = {}
        self.marks = {}
        self.stop = threading.Event()

    def mark(self, label):
        self.marks[label] = (self.host.cpu, sum(p.cpu for p in self.workers.values()))

    def run(self):
        while not self.stop.is_set():
            self.host.sample()
            for pid in children(self.host.pid):
                if pid not in self.workers:
                    self.workers[pid] = Probe(pid)
                    self.workers[pid].name = name(pid)
            for probe in self.workers.values():
                probe.sample()
            time.sleep(POLL)

    def finish(self):
        self.stop.set()
        self.join()
        self.host.sample()
        for probe in self.workers.values():
            probe.sample()
            probe.close()


# ---- 测量条件

def conditions():
    out = {'os': platform.platform(), 'machine': platform.machine(), 'cpus': os.cpu_count(),
           'python': platform.python_version()}
    if sys.platform == 'darwin':
        def sysctl(key):
            return subprocess.run(['sysctl', '-n', key], capture_output=True, text=True).stdout.strip()
        out.update(cpu=sysctl('machdep.cpu.brand_string'), memory_bytes=int(sysctl('hw.memsize')),
                   load=list(os.getloadavg()))
        commands = subprocess.run(['ps', '-axo', 'comm='], capture_output=True, text=True).stdout.splitlines()
        out['vms'] = sorted({Path(c).name for c in commands
                             if 'Virtualization.VirtualMachine' in c or Path(c).name in ('tart', 'qemu-system-aarch64', 'UTM')})
    else:
        out['cpu'] = platform.processor()

        class Status(ctypes.Structure):
            _fields_ = [('dwLength', wintypes.DWORD), ('dwMemoryLoad', wintypes.DWORD),
                        ('ullTotalPhys', ctypes.c_uint64)] + [(n, ctypes.c_uint64) for n in (
                            'ullAvailPhys', 'ullTotalPageFile', 'ullAvailPageFile', 'ullTotalVirtual',
                            'ullAvailVirtual', 'ullAvailExtendedVirtual')]
        status = Status(dwLength=ctypes.sizeof(Status))
        _kernel.GlobalMemoryStatusEx(ctypes.byref(status))
        out['memory_bytes'] = status.ullTotalPhys

        def system_times():
            idle, kernel, user = wintypes.FILETIME(), wintypes.FILETIME(), wintypes.FILETIME()
            _kernel.GetSystemTimes(ctypes.byref(idle), ctypes.byref(kernel), ctypes.byref(user))
            return [t.dwHighDateTime << 32 | t.dwLowDateTime for t in (idle, kernel, user)]
        before = system_times(); time.sleep(1); after = system_times()
        idle, kernel, user = (b - a for a, b in zip(before, after))
        out['cpu_busy_percent_1s'] = round(100 * (1 - idle / (kernel + user)), 1) if kernel + user else None
        out['vms'] = sorted({n for _, _, n in _processes() if n.lower() in ('vmwp.exe', 'vmware-vmx.exe', 'virtualboxvm.exe', 'qemu-system-x86_64.exe')})
    return out


# ---- 布局、合成数据与宿主

def run_json(binary, request, folder, env, timeout=3600):
    request_path, response_path = folder / 'request.json', folder / 'response.json'
    request_path.write_text(json.dumps(request), encoding='utf-8')
    response_path.unlink(missing_ok=True)
    result = subprocess.run([str(binary), str(request_path), str(response_path)], env=env, cwd=folder,
                            capture_output=True, timeout=timeout)
    if result.returncode != 0:
        raise SystemExit(f'{binary.name} 失败：{result.stderr.decode(errors="replace")}')
    return json.loads(response_path.read_text(encoding='utf-8'))


def layout(args, work):
    base = work / 'layout'
    resources = base / 'resources'
    copies = [(args.terms, base / 'bin' / f'illustash-terms{EXE}'),
              # 宿主只对文件名为 illustash-recognition 的 worker 要求角色 sidecar，与安装布局相同。
              (args.worker, base / 'bin' / f'illustash-recognition{EXE}'),
              (args.runtime, resources / 'onnxruntime' / RUNTIME),
              (ROOT / 'tags/tags_v3.csv', resources / 'tags/tags_v3.csv'),
              (args.models / 'wd_swinv2_v3_emb.onnx', resources / 'models/wd_swinv2_v3_emb.onnx'),
              (args.models / 'ccip/model_feat.onnx', resources / 'models/ccip/model_feat.onnx')]
    if WINDOWS:
        copies.append((args.openblas, resources / 'openblas/openblas.dll'))
    for source, dest in copies:
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, dest)
    shutil.copytree(ROOT / 'web/static', resources / 'web/static')
    return base, resources


def environment(root, shared, resources, worker=None):
    env = {k: v for k, v in os.environ.items() if not k.startswith('ILLUSTASH_')}
    for key in ('HOME', 'USERPROFILE', 'TMPDIR', 'TEMP', 'TMP', 'APPDATA', 'LOCALAPPDATA'):
        (root / key.lower()).mkdir(parents=True, exist_ok=True)
        env[key] = str(root / key.lower())
    env.update(PATH='', ILLUSTASH_APP_SUPPORT=str(root / 'support'), ILLUSTASH_SHARED=str(shared))
    if WINDOWS:
        # 安装后的启动器提供 ROOT；后端只认 ROOT/openblas/openblas.dll。
        env['ILLUSTASH_ROOT'] = str(resources)
    if worker:
        env['ILLUSTASH_RECOGNITION_WORKER'] = str(worker)
    return env


def free_port():
    import socket
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def new_library(terms, folder, package, env, characters):
    made = subprocess.run([str(terms), 'library-create', '--dest', str(package), '--folder', str(folder),
                           '--name', 'synthetic'], env=env, capture_output=True, timeout=120)
    if made.returncode != 0:
        raise SystemExit(f'library-create 失败：{made.stderr.decode(errors="replace")}')
    # 与 platform_recognition_models 相同的合成角色元数据；锚点在共享目录。
    with sqlite3.connect(package / 'source.sqlite') as c:
        c.executemany("INSERT INTO characters(char_id,anchor_p90,copyright,post_count,gender_conf,anchor_p50,anchor_p75) "
                      "VALUES (?,.05,'synthetic',?,1,.01,.025)", [(v, 100 + i) for i, v in enumerate(characters)])
    c.close()


def end_to_end(args, base, resources, images, shared, characters, trial):
    root = args.work / f'trial-{trial}'
    env = environment(root, shared, resources, base / 'bin' / f'illustash-recognition{EXE}')
    package = root / 'library.pilib'
    terms = base / 'bin' / f'illustash-terms{EXE}'
    new_library(terms, images, package, env, characters)
    port = free_port()
    origin = f'http://127.0.0.1:{port}'
    log = (root / 'host.log').open('wb')
    host = subprocess.Popen([str(terms), 'http', '--no-auto-sync', '--library', str(package), '--port', str(port),
                             '--root', str(resources), '--no-names', '--no-index-check', '--no-open'],
                            env=env, cwd=root, stdout=log, stderr=log, **SPAWN)
    try:
        deadline = time.monotonic() + 30
        while True:
            try:
                answer = fetch(origin, {'url': '/api/bootstrap'}, origin)
                if answer['status'] == 200:
                    token = json.loads(answer['body'])['token']
                    break
            except OSError:
                pass
            if time.monotonic() > deadline or host.poll() is not None:
                raise SystemExit(f'宿主启动失败：{(root / "host.log").read_text(errors="replace")}')
            time.sleep(.05)
        sampler = Sampler(host.pid)
        sampler.start()
        time.sleep(.2)
        sampler.mark('start')
        started = time.perf_counter()
        answer = fetch(origin, {'url': '/api/sync', 'method': 'POST', 'body': b'{}',
                                'headers': {'Content-Type': 'application/json', 'X-CSRF-Token': token}}, origin)
        if answer['status'] != 200:
            raise SystemExit(f'POST /api/sync：{answer["status"]} {answer["body"][:300]!r}')
        timeline, marks, state = [], {}, {}
        while True:
            state = json.loads(fetch(origin, {'url': '/api/sync'}, origin)['body'])['state']
            now = (time.perf_counter() - started) * 1000
            row = [state.get(k) for k in ('status', 'stage', 'done', 'import_done', 'imported')]
            if not timeline or timeline[-1][1:] != row:
                timeline.append([round(now, 1)] + row)
            if state.get('status') == 'recognising' and 'recognising' not in marks:
                marks['recognising'] = now
                sampler.mark('recognising')
            if state.get('status') == 'recognising' and state.get('done') and 'first' not in marks:
                marks['first'] = now
            if state.get('total') and state.get('done') == state.get('total') and 'recognised' not in marks:
                marks['recognised'] = now
            if state.get('status') not in RUNNING:
                break
            if now > 3600_000:
                raise SystemExit('同步超过一小时未完成')
            time.sleep(POLL)
        total_ms = (time.perf_counter() - started) * 1000
        sampler.mark('end')
        sampler.finish()
    finally:
        if host.poll() is None:
            host.send_signal(INTERRUPT)
            try:
                host.wait(10)
            except subprocess.TimeoutExpired:
                host.kill()
                host.wait(5)
        log.close()
    count = len(list(images.iterdir()))
    if state.get('status') != 'done' or state.get('imported') != count or state.get('failed'):
        raise SystemExit(f'同步没有按预期完成：{state}')
    with sqlite3.connect(package / 'source.sqlite') as c:
        recognised = c.execute('SELECT count(*) FROM recognition_assets').fetchone()[0]
    c.close()
    if recognised != count:
        raise SystemExit(f'识别结果 {recognised} 张，应为 {count} 张')
    workers = [p for p in sampler.workers.values() if 'recog' in p.name.lower()]
    if len(workers) != 1:
        raise SystemExit(f'应当恰好看到一个识别 worker：{[(p.pid, p.name) for p in sampler.workers.values()]}')
    worker = workers[0]
    host_cpu = sampler.marks['end'][0] - sampler.marks['recognising'][0]
    return {'total_ms': total_ms, 'per_image_ms': total_ms / count,
            'phases_ms': {'scan_and_register': marks['recognising'],
                          'recognise_to_first_image': marks['first'] - marks['recognising'],
                          'recognise_rest': marks['recognised'] - marks['first'],
                          'final_import': total_ms - marks['recognised']},
            'host_peak_bytes': sampler.host.peak, 'worker_peak_bytes': worker.peak,
            'host_cpu_s_recognising': host_cpu, 'host_cpu_s_scan': sampler.marks['recognising'][0] - sampler.marks['start'][0],
            'worker_cpu_s': worker.cpu, 'state': state, 'timeline': timeline, 'package': str(package)}


def stage_run(args, resources, images, shared, source, trial, env):
    folder = args.work / f'stages-{trial}'
    folder.mkdir()
    identity = {'package': str(source.parent), 'library_id': 'synthetic', 'generation': 1,
                'session': '0' * 64, 'task_id': f'bench-{trial}'}
    return run_json(args.compute, {'mode': 'bench-stages', 'models': str(resources / 'models'),
                                 'runtime': str(resources / 'onnxruntime' / RUNTIME),
                                 'resources': str(resources), 'shared': str(shared), 'source': str(source),
                                 'images': [str(p) for p in images], 'output': str(folder / 'out'),
                                 'identity': identity}, folder, env)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    for name in ('terms', 'worker', 'compute', 'models', 'runtime'):
        parser.add_argument(f'--{name}', type=Path, required=True)
    parser.add_argument('--openblas', type=Path, required=WINDOWS)
    parser.add_argument('--images', type=int, default=200)
    parser.add_argument('--anchors', type=int, default=2000)
    parser.add_argument('--characters', type=int, default=500)
    parser.add_argument('--trials', type=int, default=3)
    parser.add_argument('--seed', type=int, default=213)
    parser.add_argument('--work', type=Path)
    parser.add_argument('--out', type=Path)
    parser.add_argument('--keep', action='store_true', help='保留 --work 下的图库与图片')
    args = parser.parse_args()
    for name in ('terms', 'worker', 'compute', 'models', 'runtime', 'openblas'):
        if getattr(args, name):
            setattr(args, name, getattr(args, name).resolve(strict=True))
    owned = args.work is None
    args.work = Path(tempfile.mkdtemp(prefix='illustash-bench-')) if owned else args.work.resolve()
    args.work.mkdir(parents=True, exist_ok=True)
    if any(args.work.iterdir()):
        raise SystemExit('--work 必须是空目录')
    out = args.out.resolve() if args.out else args.work / 'result.json'
    try:
        before = conditions()
        base, resources = layout(args, args.work)
        images, shared = args.work / 'images', args.work / 'shared'
        generated = run_json(args.compute, {'mode': 'bench-generate', 'folder': str(images), 'count': args.images,
                                          'seed': args.seed, 'shared': str(shared), 'anchors': args.anchors,
                                          'characters': args.characters}, args.work, os.environ.copy())
        files = sorted(Path(f['path']) for f in generated['files'])
        trials, stages = [], []
        for trial in range(1, args.trials + 1):
            trials.append(end_to_end(args, base, resources, images, shared, generated['characters'], trial))
            print(f'端到端第 {trial} 次：{trials[-1]["total_ms"] / 1000:.1f} s', flush=True)
        source = Path(trials[0]['package']) / 'source.sqlite'
        env = environment(args.work / 'stages-env', shared, resources)
        if WINDOWS:
            env['ILLUSTASH_RESEARCH_OPENBLAS_DLL'] = str(resources / 'openblas/openblas.dll')
        for trial in range(1, args.trials + 1):
            stages.append(stage_run(args, resources, files, shared, source, trial, env))
            print(f'阶段分解第 {trial} 次完成', flush=True)
        after = conditions()
        result = summarise(args, generated, before, after, trials, stages)
        result['inputs'] = {name: sha256(path) for name, path in (
            ('wd', resources / 'models/wd_swinv2_v3_emb.onnx'), ('ccip', resources / 'models/ccip/model_feat.onnx'),
            ('runtime', resources / 'onnxruntime' / RUNTIME))}
        out.write_text(json.dumps(result, ensure_ascii=False, indent=1), encoding='utf-8')
        report(result)
        print(f'结果：{out}')
    finally:
        if owned and not args.keep:
            shutil.rmtree(args.work, ignore_errors=True)


def sha256(path):
    import hashlib
    digest = hashlib.sha256()
    with path.open('rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            digest.update(block)
    return digest.hexdigest()


def summarise(args, generated, before, after, trials, stages):
    count = args.images
    median = statistics.median

    def per_image(run, key):
        return statistics.fmean(row[key] for row in run['images'])
    stage_keys = {'decode': '解码', 'wd': 'WD', 'ccip': 'CCIP', 'grading': '检测与分级', 'package': '写结果包',
                  'database': '写库（宿主 CPU）'}
    per_trial = []
    for run, e2e in zip(stages, trials):
        values = {k: per_image(run, k) for k in ('decode', 'wd', 'ccip')}
        values['grading'] = per_image(run, 'labels') + per_image(run, 'claims')
        values['package'] = sum(p['ms'] for p in run['packages']) / count
        values['database'] = e2e['host_cpu_s_recognising'] * 1000 / count
        per_trial.append(values)
    stage_ms = {k: median(v[k] for v in per_trial) for k in stage_keys}
    total = sum(stage_ms.values())
    files = generated['files']
    return {
        'format': 'illustash-recognition-bench/1',
        'dataset': {'images': count, 'seed': args.seed, 'anchors': args.anchors, 'characters': args.characters,
                    'bytes': sum(f['bytes'] for f in files),
                    'megapixels': sum(f['width'] * f['height'] for f in files) / 1e6},
        'conditions': {'before': before, 'after': after, 'memory_kind': Probe.kind, 'trials': args.trials},
        'end_to_end': {
            'total_ms': median(t['total_ms'] for t in trials),
            'per_image_ms': median(t['per_image_ms'] for t in trials),
            'phases_ms': {k: median(t['phases_ms'][k] for t in trials) for k in trials[0]['phases_ms']},
            'host_peak_mib': median(t['host_peak_bytes'] for t in trials) / 2**20,
            'worker_peak_mib': median(t['worker_peak_bytes'] for t in trials) / 2**20,
            'worker_cpu_s': median(t['worker_cpu_s'] for t in trials),
            'host_cpu_s_recognising': median(t['host_cpu_s_recognising'] for t in trials),
            'host_cpu_s_scan': median(t['host_cpu_s_scan'] for t in trials),
        },
        'stages': {
            'per_image_ms': stage_ms,
            'share': {k: v / total for k, v in stage_ms.items()},
            'wd_preprocess_ms': median(per_image(r, 'wd_preprocess') for r in stages),
            'model_load_ms': median(r['load_ms'] for r in stages),
            'snapshot_ms': median(r['snapshot_ms'] for r in stages),
            'labels': stage_keys,
        },
        'trials': [{k: v for k, v in t.items() if k != 'package'} for t in trials],
        'stage_trials': per_trial,
    }


def report(result):
    d, e, s = result['dataset'], result['end_to_end'], result['stages']
    print(f'\n合成 {d["images"]} 张（{d["megapixels"]:.0f} MP，{d["bytes"] / 2**20:.0f} MiB），锚点 {d["anchors"]} 条 / '
          f'{d["characters"]} 个角色；{result["conditions"]["trials"]} 次中位数')
    print(f'端到端：{e["total_ms"] / 1000:.1f} s，每张 {e["per_image_ms"]:.0f} ms')
    names = {'scan_and_register': '扫描与登记', 'recognise_to_first_image': '启动 worker 到第一张',
             'recognise_rest': '其余识别', 'final_import': '收尾导入'}
    print('  ' + '，'.join(f'{names[k]} {v / 1000:.1f} s' for k, v in e['phases_ms'].items()))
    print(f'  峰值内存（{result["conditions"]["memory_kind"]}）：宿主 {e["host_peak_mib"]:.0f} MiB，'
          f'worker {e["worker_peak_mib"]:.0f} MiB；worker CPU {e["worker_cpu_s"]:.0f} s')
    print('阶段（每张毫秒，占比）：')
    for key, label in s['labels'].items():
        print(f'  {label}：{s["per_image_ms"][key]:.1f} ms，{s["share"][key] * 100:.1f}%')
    print(f'  模型加载 {s["model_load_ms"]:.0f} ms，锚点快照 {s["snapshot_ms"]:.0f} ms（每次任务一次）')


if __name__ == '__main__':
    main()
