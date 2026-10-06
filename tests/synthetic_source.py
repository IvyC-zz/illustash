"""固定合成 source 输入；不构造 Store/Server，不调用候选原生建库。

source_fixture 保留 67d11a65:tests/test_web.py 的 SQL/行与相册映射；
synthetic_reference 保留同提交 native create 输入的 schema 和三张参考表。
这里只生成输入，不生成被测实现的业务期望或历史升级产出。
"""
from contextlib import closing
import json
from pathlib import Path
import sqlite3


def source_fixture(root):
    source = root / 'source.db'
    c = sqlite3.connect(source)
    c.executescript('''
    CREATE TABLE assets(uuid TEXT,filename TEXT,src_path TEXT,width INTEGER,height INTEGER,added_at TEXT,src_kind TEXT,in_scope INTEGER,trashed INTEGER);
    CREATE TABLE labels(uuid TEXT,dim TEXT,value TEXT,conf REAL);
    CREATE TABLE decisions(uuid TEXT,dim TEXT,value TEXT,note TEXT);
    CREATE TABLE characters(char_id TEXT,name_zh TEXT,name_ja TEXT,name_en TEXT,copyright TEXT,wd_tag TEXT);
    CREATE TABLE character_aliases(char_id TEXT,alias TEXT);
    INSERT INTO characters VALUES ('alice', 'Alice',NULL,NULL,'Example','alice_tag'),('bob','Bob',NULL,NULL,'Example','bob_tag');
    INSERT INTO character_aliases VALUES ('alice','爱丽丝'),('alice','a_li_ce');
    INSERT INTO decisions VALUES ('a','character','alice_album','album:Alice album pk=1'),('outside','character','alice_album','album:Alice album pk=1');
    INSERT INTO labels VALUES ('a','character','alice_tag',0.9),('b','character','alice_tag',0.8),('b','char_ext','alice',0.95),('b','character','bob_tag',0.7),('c','character','bob_tag',0.2),('a','general','blue_hair',0.98),('outside','char_ext','alice',0.9);
    ''')
    c.executemany('INSERT INTO assets VALUES (?,?,?,?,?,?,?,?,?)', [(i,i+'.png',str(root / (i+'.png')),120,150,'2026-09-22','orig',1,0) for i in 'abc'])
    c.execute("INSERT INTO assets VALUES ('trash','trash.png','',0,0,'','orig',1,1)")
    c.commit();c.close()
    (root/'album-char-map.json').write_text(json.dumps({'map':{'alice_album':{'char_id':'alice'}}}))
    return source


def synthetic_reference(root):
    """固定本地 reference 输入；只保留角色、别名与合成 run 的行。"""
    root = Path(root)
    seed = root / 'reference-seed'
    seed.mkdir()
    source = source_fixture(seed)
    with closing(sqlite3.connect(source)) as c:
        for column in ('hidden INTEGER', 'stable_hash TEXT', 'orig_size INTEGER', 'z_pk INTEGER'):
            c.execute('ALTER TABLE assets ADD COLUMN ' + column)
        for column in ("source TEXT DEFAULT 'legacy'", 'model_ver TEXT', 'run_id TEXT'):
            c.execute('ALTER TABLE labels ADD COLUMN ' + column)
        c.execute('CREATE TABLE embeddings(uuid TEXT NOT NULL,model TEXT NOT NULL,vec BLOB NOT NULL,PRIMARY KEY(uuid,model))')
        c.execute('CREATE TABLE runs(id INTEGER PRIMARY KEY, kind TEXT, started_at TEXT)')
        c.execute("INSERT INTO runs VALUES (1,'synthetic-reference','2026-10-01')")
        c.commit()
    # 固定输入的七张表，无索引/触发器；不复用候选的 export/create 路径。
    dest = root / 'reference.sqlite'
    with closing(sqlite3.connect(dest.resolve().as_uri(), uri=True)) as c:
        c.execute('ATTACH ? AS seed', (source.resolve().as_uri() + '?mode=ro',))
        tables = c.execute("SELECT name,sql FROM seed.sqlite_master WHERE type='table' ORDER BY rowid").fetchall()
        for name, ddl in tables:
            c.execute(ddl)
            if name in ('characters', 'character_aliases', 'runs'):
                c.execute(f'INSERT INTO main.{name} SELECT * FROM seed.{name}')
        c.commit()
        c.execute('DETACH seed')
    return dest
