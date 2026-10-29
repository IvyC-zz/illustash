"""旧参考冻结的研究输入和期望，合成状态只通过原生命令继续演化。"""
from contextlib import closing
import json
from functools import lru_cache
from pathlib import Path
import sqlite3
import tempfile

import numpy as np
from tests import native_commands as core
from tests.fixed_expectations import fixed_values, upgrade_recorded


@lru_cache(None)
def records():
    return fixed_values('research_values.json.gz')


def decode(value, root):
    if isinstance(value, dict):
        if set(value) == {'hex'}:
            return bytes.fromhex(value['hex'])
        return {k: decode(v, root) for k,v in value.items()}
    if isinstance(value, list):
        return [decode(v, root) for v in value]
    return value.replace('@ROOT@', str(root)) if isinstance(value, str) else value


class Features:
    def __init__(self, source, vectors, uuids):
        self.source = source
        self.X = np.asarray(vectors, np.float32)
        self.X = self.X / np.maximum(np.linalg.norm(self.X, axis=1, keepdims=True), 1e-20)
        self.uuids = list(uuids)
        self.row = {u:i for i,u in enumerate(self.uuids)}


class Store:
    def __init__(self, root, record):
        self.root, self.record = root, record
        self.path, self.source = root / 'app.sqlite', root / 'source.db'
        self.package_position = 0

    def connect(self):
        c = sqlite3.connect(self.path)
        c.row_factory = sqlite3.Row
        return c

    def command(self, op, **fields):
        return core.character_command(self.path, {'op':op, **fields})

    def last_action(self, result):
        with self.connect() as c:
            row = c.execute('SELECT id,label FROM actions WHERE id=?', (result['action'],)).fetchone()
            return dict(row) if row else None

    def change(self, ids, action, entity=None):
        return self.last_action(self.command('change', asked=list(ids), action=action, entity=entity,
                                             entities=None, confirm=True, effect='verdict'))

    def undo(self, action):
        return self.command('undo', action=action, effect='verdict')

    def curate(self, ids, action):
        import os, subprocess
        from tools.verify_read_http import enter, fetch
        from tests.test_read_http import SPAWN, free_port, wait_until, stop_process
        package = Path(tempfile.mkdtemp(prefix='curation-input-', dir=self.root)).with_suffix('.pilib')
        package.mkdir()
        for source,name in ((self.path,'library.sqlite'),(self.source,'source.sqlite')):
            with closing(sqlite3.connect(source)) as src, closing(sqlite3.connect(package/name)) as dst:
                src.backup(dst)
        (package/'library.json').write_text(json.dumps({'format':1,'id':'local','name':'Synthetic','sources':[]}))
        with closing(sqlite3.connect(package/'library.sqlite')) as c, c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('startup',?)", (json.dumps({'similar':False}),))
        port = free_port()
        origin = f'http://127.0.0.1:{port}'
        env = os.environ | {'ILLUSTASH_APP_SUPPORT':str(self.root/'support'),
                            'ILLUSTASH_SHARED':str(self.root/'absent-resources'),'ILLUSTASH_NO_KEYCHAIN':'1'}
        log = tempfile.TemporaryFile()
        process = subprocess.Popen([str(core.binary()),'http','--library',str(package),'--root',
                                    str(Path(__file__).resolve().parents[1]),'--port',str(port),
                                    '--no-open','--no-auto-sync','--no-names','--no-provenance','--no-index-check'],
                                   env=env,stdout=log,stderr=subprocess.DEVNULL,**SPAWN)
        try:
            enter(origin,log,process,timeout=10)
            token = json.loads(fetch(origin,{'url':'/api/bootstrap'},origin)['body'])['token']
            result = fetch(origin,{'url':'/api/curate','method':'POST',
                                   'body':json.dumps({'ids':ids,'action':action}).encode(),
                                   'headers':{'Content-Type':'application/json','X-CSRF-Token':token}},origin)
            if result['status']!=200:
                raise AssertionError(result)
        finally:
            stop_process(process)
            log.close()
        with closing(sqlite3.connect(package/'library.sqlite')) as src, closing(sqlite3.connect(self.path)) as dst:
            src.backup(dst)
        return json.loads(result['body'])

    def shared_oc(self):
        return self.command('shared_oc', id=self.record['attrs']['oc_id'])

    def set_mascot(self, entity, mascot):
        return self.command('set_mascot', entity=entity, mascot=mascot)

    def import_changes(self, *, removed):
        return self.command('import', rows=[], removed=removed, run_id=None, new_ids={}, wd_tags=[],
                            labels=[], decisions=[], album_map={}, duplicates=[], realistic=[],
                            indexed='2026-10-01 00:00:00')

    def refresh_general_hits(self):
        # 已提交的旧 WD 包是输入；当前核心负责校验、交付和重算。
        for index, files in enumerate(self.record['packages']):
            package = Path(tempfile.mkdtemp(prefix='wd-input-', dir=self.root))
            for name, text in files.items():
                (package / name).write_text(text, encoding='utf-8', newline='')
            core.run('import', '--db', self.path, '--library-id', 'local', '--package', package)
        self.command('rederive', realistic=[], rating_indexed='2026-10-01 00:00:00',
                     general_indexed='2026-10-01 00:00:00')


def seed(test, root):
    name = test._testMethodName
    # 安装版 worker 的子类用共用的基础输入增加协议拒绝用例。
    record = records().get(name) or records()['test_prototypes_claims_weak_and_unmatched_cache_exact']
    for kind, filename in [('library','app.sqlite'), ('source','source.db')]:
        with closing(sqlite3.connect(root / filename)) as c, c:
            c.executescript(decode(record['seed'][kind], root))
    upgrade_recorded(root / 'app.sqlite')
    test.fixed = decode(record, root)
    test.fixed_position = test.oc_position = 0
    return Store(root, test.fixed), record['attrs']
