"""在导入层屏蔽退役参考实现，再运行完整 unittest discover。

默认按测试类分给多个子进程并行执行（`--jobs N`，默认 CPU 核数；`--jobs 1` 为原串行）。
每组的 `-v` 输出整段回放、不交错；父进程核对实际运行的测试与发现的完全一致，
子进程崩溃、漏跑或捕获到退役导入均判失败。

`--select 前缀`（可重复）只运行 ID 等于前缀或以「前缀.」开头的测试，如 `test_native_research.NativeResearchTest`，
供 Windows 快检和定向复跑使用（#501）。发现和导入照旧覆盖全部测试文件，导入失败仍会报出；
某个前缀一项也没选中时判失败，不把空选择当成通过。定向结果不代替全量。

隔离名单（`tests/quarantine.json`，#501）：名单里的测试照跑，在本平台的失败不计入结果，单列「隔离名单中的失败」；
名单格式错误、条目到期或找不到对应测试时入口失败。`--count-quarantined` 照样校验名单，但名单里的失败也计入结果，
供定向复现使用。规则见 tools/quarantine.py。
"""
import importlib.abc
import importlib.machinery
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

from tools import quarantine

ROOT=Path(__file__).resolve().parents[1]
BLOCKED=(ROOT/'tests/reference',ROOT/'web')
RETIRED_TOOLS={ROOT/'tools'/name for name in ('index_worker.py','char_patch.py','build_characters.py','char_scan.py','ccip.py','validation_worksheet.py')}
ATTEMPTS=[]
RULE='-'*70
# 对 CPU 负载敏感、并发下会偶发失败的测试类：并行池跑完后逐个单独运行。每项仍只运行一次，不重试；
# 修好其中的时序问题后从这里移除。#132 修完后为空。
SERIAL=frozenset()
# 子进程自行把仓库根插到 sys.path 最前再按模块运行：带 ._pth 的嵌入式 Python 与 -I 一样不把 cwd 放进
# sys.path，也忽略 PYTHONPATH，`-m tests.run_fixed_suite` 在那里找不到 tests 包。插入后与 `-m` 的 sys.path[0] 相同。
WORKER=("import runpy,sys;sys.path.insert(0,sys.argv.pop(1));"
        "runpy.run_module('tests.run_fixed_suite',run_name='__main__',alter_sys=True)")

def worker_command(summary,passthrough):
    # 沿用父进程的 -I、-X utf8、-W 等解释器选项（multiprocessing 的 spawn 也用这个函数），子进程日志与父进程同一编码。
    return [sys.executable,*subprocess._args_from_interpreter_flags(),'-c',WORKER,str(ROOT),
            '--worker',str(summary),*passthrough]

def retired(origin):
    return origin in RETIRED_TOOLS or any(origin.is_relative_to(root) for root in BLOCKED)
class RetiredReferenceFinder(importlib.abc.MetaPathFinder):
    def find_spec(self,fullname,path=None,target=None):
        spec=importlib.machinery.PathFinder.find_spec(fullname,path)
        if spec and spec.origin and spec.origin not in ('built-in','frozen'):
            origin=Path(spec.origin).resolve()
            if retired(origin):
                ATTEMPTS.append(fullname)
                raise ImportError(f'已退役的参考模块不可导入：{fullname}')
        return None

def guard():
    for name,module in tuple(sys.modules.items()):
        origin=getattr(module,'__file__',None)
        if origin and retired(Path(origin).resolve()):
            raise RuntimeError(f'退役参考已被提前导入：{name}')
    sys.meta_path.insert(0,RetiredReferenceFinder())

def flatten(suite):
    for item in suite:
        if isinstance(item,unittest.TestSuite):
            yield from flatten(item)
        else:
            yield item

class RecordingResult(unittest.TextTestResult):
    def __init__(self,*args,**kwargs):
        super().__init__(*args,**kwargs); self.ran=[]
    def startTest(self,test):
        self.ran.append(test.id()); super().startTest(test)

def owner(test):
    """失败所属测试的 ID；subTest 的失败记到所在的测试方法上。"""
    return getattr(test,'test_case',test).id()

def failed_ids(result):
    return [owner(test) for test,_ in result.failures+result.errors]+[owner(test) for test in result.unexpectedSuccesses]

def counts(result):
    return {'run':result.testsRun,'failures':len(result.failures),'errors':len(result.errors),
            'skipped':len(result.skipped),'expected':len(result.expectedFailures),
            'unexpected':len(result.unexpectedSuccesses),'ran':result.ran,'failed':failed_ids(result)}

def held_entries(tests):
    """读取并核对隔离名单：Python 条目都要对应一个发现到的测试。返回本平台适用的条目（按 ID）。"""
    entries=[entry for entry in quarantine.load() if entry['kind']=='python']
    known={test.id() for test in tests}
    missing=[entry for entry in entries if entry['id'] not in known]
    if missing:
        raise quarantine.QuarantineError('隔离名单里的 Python 测试找不到，删掉或改正这些条目：\n'
                                         +'\n'.join('  '+quarantine.describe(entry) for entry in missing))
    return {entry['id']:entry for entry in entries if quarantine.platform() in entry['platforms']}

def split_failures(ids,held,counting=False):
    """把失败分成计入结果的和隔离名单里的；后者输出单独一节。"""
    if counting:
        return ids,0
    counted=[name for name in ids if name not in held]
    isolated={name:ids.count(name) for name in dict.fromkeys(ids) if name in held}
    quarantine.report([f'{quarantine.describe(held[name])}：{times} 处失败' for name,times in isolated.items()])
    return counted,sum(isolated.values())

def run_in_process(tests,verbosity,buffer):
    """在本进程执行并输出逐项结果和失败详情，不打印 Ran/OK 汇总。"""
    result=RecordingResult(unittest.runner._WritelnDecorator(sys.stderr),True,verbosity)
    result.buffer=buffer
    result.startTestRun()
    try:
        unittest.TestSuite(tests)(result)
    finally:
        result.stopTestRun()
    result.printErrors()
    sys.stderr.flush()
    return counts(result)

def worker(summary,verbosity,buffer):
    guard()
    # 与 discover -s tests 相同：tests/ 作为顶层目录，测试模块按顶层名导入。
    sys.path.insert(0,str(ROOT/'tests'))
    ids=json.load(sys.stdin)
    tests=list(flatten(unittest.defaultTestLoader.loadTestsFromNames(ids)))
    data=run_in_process(tests,verbosity,buffer)
    data['attempts']=list(ATTEMPTS)
    Path(summary).write_text(json.dumps(data))

def location(cls):
    """类实际可按名加载的模块；有的测试类改写了 __module__，只能从持有它的模块取。"""
    head=cls.__qualname__.split('.')[0]
    module=sys.modules.get(cls.__module__)
    if module is not None and getattr(module,head,None) is not None and loadable(module,cls):
        return cls.__module__
    for name,module in sorted(sys.modules.items()):
        if module is not None and getattr(module,head,None) is not None and loadable(module,cls):
            return name
    raise RuntimeError(f'找不到可加载测试类的模块：{cls.__module__}.{cls.__qualname__}')

def loadable(module,cls):
    value=module
    for part in cls.__qualname__.split('.'):
        value=getattr(value,part,None)
    return value is cls

def groups(tests):
    """按测试类分组；导入失败等加载器占位项留在父进程执行。"""
    grouped={};local=[]
    for test in tests:
        if type(test).__module__=='unittest.loader':
            local.append(test); continue
        name=f'{location(type(test))}.{type(test).__qualname__}'
        grouped.setdefault(name,[]).append(test)
    # 动态领取：测试多的类先发，减少最后单个长组拖尾。
    return sorted(grouped.items(),key=lambda item:-len(item[1])),local

def chosen(test,prefixes):
    name=test.id()
    return any(name==prefix or name.startswith(prefix+'.') for prefix in prefixes)

def select(tests,prefixes):
    """按 ID 前缀筛选；加载器占位项（导入失败）总是保留。返回筛选结果和没选中任何测试的前缀。"""
    if not prefixes:
        return tests,[]
    kept=[test for test in tests if type(test).__module__=='unittest.loader' or chosen(test,prefixes)]
    empty=[prefix for prefix in prefixes if not any(chosen(test,[prefix]) for test in kept)]
    sys.stderr.write(f'定向选择：{", ".join(prefixes)}；选中 {sum(type(t).__module__!="unittest.loader" for t in kept)} 项\n')
    return kept,empty

def parallel(jobs,verbosity,buffer,passthrough,prefixes=(),counting=False):
    start=time.perf_counter()
    loader=unittest.defaultTestLoader
    discovered=list(flatten(loader.discover(str(ROOT/'tests'))))
    try:
        held=held_entries(discovered)
    except quarantine.QuarantineError as error:
        sys.stderr.write(f'{error}\n'); return False
    tests,empty=select(discovered,prefixes)
    for prefix in empty: sys.stderr.write(f'定向选择没有匹配任何测试：{prefix}\n')
    queue,local=groups(tests)
    expected={test.id() for _,members in queue for test in members}
    total={'run':0,'failures':0,'errors':0,'skipped':0,'expected':0,'unexpected':0}
    ran=[];attempts=list(ATTEMPTS);broken=[];failing=[]
    def add(data):
        for key in total: total[key]+=data[key]
        ran.extend(data['ran']);attempts.extend(data.get('attempts',()));failing.extend(data['failed'])
    if local:
        add(run_in_process(local,verbosity,buffer))
    pool=[item for item in queue if item[0] not in SERIAL];serial=[item for item in queue if item[0] in SERIAL]
    for name in sorted(SERIAL-{name for name,_ in queue}):
        sys.stderr.write(f'串行名单中的测试类未被发现：{name}\n')
    workdir=tempfile.TemporaryDirectory(prefix='illustash-suite-')
    count=len(queue);index=0;done=0

    def run(queue,limit):
        nonlocal index,done
        running=[]
        try:
            while queue or running:
                while queue and len(running)<limit:
                    name,members=queue.pop(0);index+=1
                    log=Path(workdir.name)/f'{index}.log';summary=Path(workdir.name)/f'{index}.json'
                    handle=log.open('wb')
                    process=subprocess.Popen(worker_command(summary,passthrough),cwd=ROOT,stdin=subprocess.PIPE,stdout=handle,stderr=subprocess.STDOUT)
                    process.stdin.write(json.dumps([f'{name}.{test._testMethodName}' for test in members]).encode());process.stdin.close()
                    running.append((name,members,process,handle,log,summary,time.perf_counter()))
                finished=[item for item in running if item[2].poll() is not None]
                if not finished:
                    time.sleep(.05); continue
                for item in finished:
                    running.remove(item)
                    name,members,process,handle,log,summary,began=item
                    handle.close();done+=1
                    mode='，单独运行' if limit==1 and name in SERIAL else ''
                    sys.stderr.write(f'[{done}/{count}] {name}（{len(members)} 项，{time.perf_counter()-began:.1f}s{mode}）\n')
                    sys.stderr.write(log.read_text(errors='replace'))
                    if process.returncode or not summary.exists():
                        broken.append(f'{name}：子进程退出码 {process.returncode}')
                    else:
                        add(json.loads(summary.read_text()))
                    sys.stderr.flush()
        except BaseException:
            for item in running: item[2].kill()
            raise

    try:
        run(pool,jobs)
        run(serial,1)
    finally:
        workdir.cleanup()
    missing=expected-set(ran);extra=[test for test in ran if test not in expected]
    elapsed=time.perf_counter()-start
    for line in broken: sys.stderr.write(f'并行执行失败：{line}\n')
    if missing: sys.stderr.write(f'未运行的测试 {len(missing)} 项：{", ".join(sorted(missing)[:20])}\n')
    sys.stderr.write(f'{RULE}\nRan {total["run"]} tests in {elapsed:.3f}s（并行 {jobs} 进程）\n\n')
    counted,isolated=split_failures(failing,held,counting)
    failed=counted or broken or missing or empty
    parts=[f'{label}={total[key]}' for key,label in (('failures','failures'),('errors','errors'),('skipped','skipped'),
           ('expected','expected failures'),('unexpected','unexpected successes')) if total[key]]
    if isolated: parts.append(f'其中隔离名单={isolated}')
    sys.stderr.write(('FAILED' if failed else 'OK')+(f' ({", ".join(parts)})' if parts else '')+'\n')
    sys.stderr.flush()
    print(f'退役参考导入尝试：{len(attempts)}',flush=True)
    return not failed and not attempts and not extra

def options(argv):
    jobs=os.cpu_count() or 1;rest=[];prefixes=[];counting=False
    args=iter(argv)
    for arg in args:
        if arg in ('-j','--jobs'): jobs=int(next(args))
        elif arg.startswith('--jobs='): jobs=int(arg.split('=',1)[1])
        elif arg=='--select': prefixes.append(next(args))
        elif arg.startswith('--select='): prefixes.append(arg.split('=',1)[1])
        elif arg=='--count-quarantined': counting=True
        else: rest.append(arg)
    if jobs<1: raise SystemExit('--jobs 至少为 1')
    if any(not prefix for prefix in prefixes): raise SystemExit('--select 不能为空')
    return jobs,rest,prefixes,counting

if __name__=='__main__':
    if sys.argv[1:2]==['--worker']:
        rest=sys.argv[3:]
        worker(sys.argv[2],2 if '-v' in rest else 1,'-b' in rest)
        sys.exit(0)
    jobs,rest,prefixes,counting=options(sys.argv[1:])
    guard()
    if jobs==1 and prefixes: raise SystemExit('--select 只用于并行模式；--jobs 1 请直接传 unittest 参数')
    if jobs==1:
        try:
            held=held_entries(list(flatten(unittest.defaultTestLoader.discover(str(ROOT/'tests')))))
        except quarantine.QuarantineError as error:
            raise SystemExit(str(error))
        program=unittest.main(module=None,argv=[sys.argv[0],'discover','-s','tests',*rest],exit=False)
        print(f'退役参考导入尝试：{len(ATTEMPTS)}')
        counted,_=split_failures(failed_ids(program.result),held,counting)
        sys.exit(bool(counted) or bool(ATTEMPTS))
    unknown=[arg for arg in rest if arg not in ('-v','-b')]
    if unknown: raise SystemExit(f'并行模式只支持 -v/-b，其余参数请配合 --jobs 1：{" ".join(unknown)}')
    sys.exit(not parallel(jobs,2 if '-v' in rest else 1,'-b' in rest,rest,prefixes,counting))
