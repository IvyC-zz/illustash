"""旧索引参考的只读值与合成输入包，不包含计算或录制入口。"""
import base64
import gzip
import hashlib
import json
import sqlite3
from functools import lru_cache
from pathlib import Path
from types import SimpleNamespace
import numpy as np

@lru_cache(None)
def records():
    with gzip.open(Path(__file__).parent/'fixtures/index_values.json.gz','rt',encoding='utf-8') as f:
        return json.load(f)

def encode(v):
    if isinstance(v,np.ndarray):
        # Memory order is part of the input: the old merge accumulated in it.
        order=['F'] if v.flags.f_contiguous and not v.flags.c_contiguous else []
        return {'array_key':[v.dtype.str,list(v.shape),hashlib.sha256(v.tobytes()).hexdigest()]+order}
    if isinstance(v,np.generic):return v.item()
    if isinstance(v,tuple):return {'tuple':[encode(x) for x in v]}
    if isinstance(v,list):return [encode(x) for x in v]
    if isinstance(v,dict):return {'dict':[[encode(k),encode(x)] for k,x in sorted(v.items(),key=lambda x:repr(x[0]))]}
    if isinstance(v,(str,Path)) and Path(v).is_file():
        data=Path(v).read_bytes()
        if data.startswith(b'SQLite format'):
            with sqlite3.connect(v) as c:data='\n'.join(c.iterdump()).encode()
        return {'file':hashlib.sha256(data).hexdigest()}
    return str(v) if isinstance(v,Path) else v

def decode(v):
    if isinstance(v,list):return [decode(x) for x in v]
    if isinstance(v,dict):
        if 'array' in v:
            dtype,shape,data=v['array']
            return np.frombuffer(base64.b64decode(data),dtype=np.dtype(dtype)).reshape(shape)
        if 'tuple' in v:return tuple(decode(x) for x in v['tuple'])
        if 'dict' in v:return {decode(k):decode(x) for k,x in v['dict']}
    return v

def value(label,*args,**kwargs):
    key=hashlib.sha256(json.dumps(encode((args,kwargs)),sort_keys=True,ensure_ascii=False).encode()).hexdigest()
    return decode(records()['functions'][label][key]['value'])

def restore(files,root):
    root.mkdir(exist_ok=True)
    for name,data in files.items():
        p=root/name;p.parent.mkdir(parents=True,exist_ok=True)
        if 'sql' in data:
            for suffix in ('','-wal','-shm','-journal'):Path(str(p)+suffix).unlink(missing_ok=True)
            with sqlite3.connect(p) as c:c.executescript(data['sql'].replace('@FOLDER@',str(root)))
        elif 'text' in data:p.write_text(data['text'].replace('@FOLDER@',str(root)),encoding='utf-8',newline='')
        else:p.write_bytes(base64.b64decode(data['bytes']))

def current_files(root):
    """The input folder as recorded entries spell it (root as @FOLDER@)."""
    result={}
    for p in root.rglob('*'):
        if not p.is_file() or p.name.endswith(('-wal','-shm','-journal')):continue
        data=p.read_bytes();name=p.relative_to(root).as_posix()
        if data.startswith(b'SQLite format'):
            with sqlite3.connect(p) as c:result[name]={'sql':'\n'.join(c.iterdump()).replace(str(root),'@FOLDER@')}
        else:
            # Inputs written in text mode carry CRLF on Windows; the recording was made with LF.
            try:result[name]={'text':data.decode().replace('\r\n','\n').replace(str(root),'@FOLDER@')}
            except UnicodeDecodeError:result[name]={'bytes':base64.b64encode(data).decode()}
    return result

def legacy(test,root):
    """The old index run's result on this input folder; its output files are restored into it.
    Entries of one test are taken in call order; calls recorded under a throwaway test instance
    (``runTest``) are told apart by their input files."""
    entries=records()['legacy'][test._testMethodName]
    if test._testMethodName=='runTest':
        now=current_files(root)
        # Entries hold the folder after the run, which rewrites the character index and adds outputs;
        # the plan, album map and features it only reads tell the inputs apart.
        decisive=lambda name:name in ('char-patch/plan.json','album-char-map.json','full-features.npy','full-manifest.csv')
        matches=[e for e in entries if all(e['files'].get(name)==data for name,data in now.items() if decisive(name))]
        if not matches:
            raise AssertionError('no recorded old index run for this input folder')
        entry=matches[0]
    else:
        i=getattr(test,'_fixed_index_cursor',0);test._fixed_index_cursor=i+1
        entry=entries[i]
    restore(entry['files'],root)
    return entry['value']

def large(test,root):
    entry=records()['large'][test._testMethodName];restore(entry['files'],root)
    return root/'tags.csv',decode(entry['centres']),decode(entry['edges'])

build=SimpleNamespace(merge=lambda *a,**kw:value('build.merge',*a,**kw))
char_patch=SimpleNamespace(core_of=lambda *a,**kw:value('char_patch.core_of',*a,**kw))
char_scan=SimpleNamespace(anchor_index=lambda *a,**kw:value('char_scan.anchor_index',*a,**kw),
                         claim_thresholds=lambda *a,**kw:value('char_scan.claim_thresholds',*a,**kw))
