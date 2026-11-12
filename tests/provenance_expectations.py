"""旧溯源参考的固定结果与合成状态，只读恢复；不执行计算或网络。"""
from contextlib import closing
import json
from pathlib import Path
from functools import lru_cache
import sqlite3
from types import SimpleNamespace

from tests.image_expectations import verify as image_verify
from tests.fixed_expectations import fixed_values

_active = None


class Stopped(RuntimeError):
    pass


@lru_cache(None)
def records():
    return fixed_values('provenance_values.json.gz')


def begin(test):
    global _active
    ident = test.id()
    if not ident.startswith('tests.'):
        ident = 'tests.' + ident
    entries = records().get(ident)
    if entries is None:
        matches = [v for k,v in records().items() if k.endswith('.'+test._testMethodName)]
        entries = matches[0] if len(matches)==1 else []
    _active = iter(entries)


def decode(v):
    if isinstance(v,dict):
        if set(v)=={'hex'}:return bytes.fromhex(v['hex'])
        return {k:decode(x) for k,x in v.items()}
    if isinstance(v,list):return [decode(x) for x in v]
    return v


def call(label,lib,*args,**kwargs):
    entry = next(_active)
    if entry['label'] != label:
        raise AssertionError((entry['label'],label))
    if 'error' in entry:
        kind,message = entry['error']
        raise {'AttributeError':AttributeError,'ValueError':ValueError,'IntegrityError':sqlite3.IntegrityError}[kind](message)
    path = Path(lib['cache']) if isinstance(lib,dict) else lib.path
    # 只还原历史的机器缓存；core/source 数据库不动。
    with closing(sqlite3.connect(path/'provenance.sqlite')) as c, c:
        for name,table in entry.get('tables',{}).items():
            c.execute('DELETE FROM "'+name+'"')
            rows = decode(table['rows'])
            if rows:
                cols = ','.join('"'+x+'"' for x in table['columns'])
                c.executemany('INSERT INTO "'+name+'" ('+cols+') VALUES ('+','.join('?' for _ in table['columns'])+')',rows)
    if 'net' in entry:
        args[0].calls = [tuple(r) for r in entry['net']['calls']]
        args[0].used = entry['net']['used']
    return decode(entry['result'])


native_worker = SimpleNamespace(execute=lambda *a,**kw:call('provenance.native_worker.execute',*a,**kw))
groups = SimpleNamespace(run=lambda *a,**kw:call('provenance.sources.groups.run',*a,**kw))
verify = SimpleNamespace(signature=image_verify.signature,_sig=image_verify._sig,_colour=image_verify._colour,
                         local_run=lambda *a,**kw:call('provenance.verify.local_run',*a,**kw),
                         run=lambda *a,**kw:call('provenance.verify.run',*a,**kw))
