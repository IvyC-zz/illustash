"""旧索引参考的只读值与合成输入包，不包含计算或录制入口。"""
import base64
from contextlib import closing
import hashlib
import json
import sqlite3
from functools import lru_cache
from pathlib import Path
from types import SimpleNamespace
import numpy as np
from tests.fixed_expectations import fixed_values

@lru_cache(None)
def records():
    return fixed_values('index_values.json.xz')

def encode(v):
    if isinstance(v,np.ndarray):
        # 内存顺序也是输入的一部分：旧的 merge 按这个顺序累加。
        order=['F'] if v.flags.f_contiguous and not v.flags.c_contiguous else []
        return {'array_key':[v.dtype.str,list(v.shape),hashlib.sha256(v.tobytes()).hexdigest()]+order}
    if isinstance(v,np.generic):return v.item()
    if isinstance(v,tuple):return {'tuple':[encode(x) for x in v]}
    if isinstance(v,list):return [encode(x) for x in v]
    if isinstance(v,dict):return {'dict':[[encode(k),encode(x)] for k,x in sorted(v.items(),key=lambda x:repr(x[0]))]}
    if isinstance(v,(str,Path)) and Path(v).is_file():
        data=Path(v).read_bytes()
        if data.startswith(b'SQLite format'):
            with closing(sqlite3.connect(v)) as c, c:data=database_dump(c).encode()
        return {'file':hashlib.sha256(data).hexdigest()}
    return str(v) if isinstance(v,Path) else v

def database_dump(connection):
    return '\n'.join(connection.iterdump())

def database_state(connection):
    """精确比较 schema、类型、值和 rowid，不依赖 SQLite 的浮点 SQL 文本写法。"""
    schema=connection.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name').fetchall()
    tables=[]
    def cell(value):
        return (type(value).__name__,value.hex() if isinstance(value,float) else value)
    for kind,name,_,sql in schema:
        if kind!='table':continue
        columns='*' if 'WITHOUT ROWID' in (sql or '').upper() else 'rowid,*'
        quoted='"'+name.replace('"','""')+'"'
        rows=connection.execute(f'SELECT {columns} FROM {quoted}').fetchall()
        tables.append((name,sorted((tuple(cell(v) for v in row) for row in rows),key=repr)))
    return (schema,connection.execute('PRAGMA user_version').fetchone(),
            connection.execute('PRAGMA application_id').fetchone(),tables)

@lru_cache(None)
def recorded_database_state(sql):
    with closing(sqlite3.connect(':memory:')) as c, c:
        c.executescript(sql)
        return database_state(c)

def anchor_index(*args,**kwargs):
    """SQL 导出格式变化时，只接受与冻结数据库逐项完全一致的输入。"""
    try:
        return value('char_scan.anchor_index',*args,**kwargs)
    except KeyError:
        # 同一 REAL 在不同 SQLite 版本里会导出不同小数位。先核对整库，
        # 再用冻结 SQL 的原始哈希查找；数组、ID、参数和期望值仍按原来的精确键匹配。
        if len(args)!=3 or kwargs:raise
        with closing(sqlite3.connect(args[2])) as c:
            state=database_state(c)
        encoded=encode((args,kwargs))
        for entries in records()['legacy'].values():
            for entry in entries:
                sql=entry['files'].get('pi.db',{}).get('sql')
                if sql is None or '@FOLDER@' in sql or recorded_database_state(sql)!=state:continue
                encoded['tuple'][0]['tuple'][2]={'file':hashlib.sha256(sql.encode()).hexdigest()}
                key=hashlib.sha256(json.dumps(encoded,sort_keys=True,ensure_ascii=False).encode()).hexdigest()
                if key in records()['functions']['char_scan.anchor_index']:
                    return decode(records()['functions']['char_scan.anchor_index'][key]['value'])
        raise

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
            with closing(sqlite3.connect(p)) as c, c:c.executescript(data['sql'].replace('@FOLDER@',str(root)))
        elif 'text' in data:p.write_text(data['text'].replace('@FOLDER@',str(root)),encoding='utf-8',newline='')
        else:p.write_bytes(base64.b64decode(data['bytes']))

def current_files(root):
    """按录制条目的写法表示的输入文件夹（根目录写作 @FOLDER@）。"""
    result={}
    for p in root.rglob('*'):
        if not p.is_file() or p.name.endswith(('-wal','-shm','-journal')):continue
        data=p.read_bytes();name=p.relative_to(root).as_posix()
        if data.startswith(b'SQLite format'):
            with closing(sqlite3.connect(p)) as c, c:result[name]={'sql':'\n'.join(c.iterdump()).replace(str(root),'@FOLDER@')}
        else:
            # 在 Windows 上以文本模式写入的输入带 CRLF；录制时用的是 LF。
            try:result[name]={'text':data.decode().replace('\r\n','\n').replace(str(root),'@FOLDER@')}
            except UnicodeDecodeError:result[name]={'bytes':base64.b64encode(data).decode()}
    return result

def legacy(test,root):
    """旧的索引运行在这个输入文件夹上的结果；它的输出文件还原到这个文件夹里。
    同一个测试的条目按调用顺序取用；在临时测试实例（``runTest``）下录制的调用按输入文件区分。"""
    entries=records()['legacy'][test._testMethodName]
    if test._testMethodName=='runTest':
        now=current_files(root)
        # 条目记录的是运行之后的文件夹，运行会改写角色索引并增加输出；
        # 靠它只读取的 plan、album map 和特征来区分输入。
        decisive=lambda name:name in ('char-patch/plan.json','album-char-map.json','full-features.npy','full-manifest.csv')
        # Windows 的录制里文件名用 \\，文本输入带 CRLF。
        same=lambda files:{name.replace('\\','/'):({'text':data['text'].replace('\r\n','\n')} if 'text' in data else data)
                           for name,data in files.items()}
        matches=[e for e in entries if all(same(e['files']).get(name)==data for name,data in now.items() if decisive(name))]
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
char_scan=SimpleNamespace(anchor_index=anchor_index,
                         claim_thresholds=lambda *a,**kw:value('char_scan.claim_thresholds',*a,**kw))
