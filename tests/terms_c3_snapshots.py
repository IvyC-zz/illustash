"""C3 新增固定快照：旧写路径仍在时录制；已有 tests/golden 的文件绝不重录。

时间列沿用 golden_writes 的排除规则。合成夹具的临时目录替换为固定占位符，二进制原型用十六进制无损保存。
其余列、历史信封里的旧快照、行的重复次数全部精确比较。旧写路径删除后不再提供录制入口，所有固定答案只读。
"""
import json
from unittest import mock

from tests import golden_writes
from web import terms_core


def deterministic(test):
    patch = mock.patch('uuid.uuid4', side_effect=golden_writes.Ids(0x1000))
    patch.start()
    test.addCleanup(patch.stop)


def value(v, root):
    if isinstance(v, bytes):
        return {'hex': v.hex()}
    if isinstance(v, str):
        if not root:
            return v
        return v.replace(str(root), '<临时目录>').replace(str(root).removeprefix('/private'), '<临时目录>')
    if isinstance(v, dict):
        return {k: value(x, root) for k, x in sorted(v.items())
                if k != 'ts' and not k.endswith('_at')}
    if isinstance(v, (tuple, list)):
        return [value(x, root) for x in v]
    return v


def history(v):
    """历史内嵌的旧行也按原列名去掉时间；不碰其他位置的文本或数值。"""
    if isinstance(v, dict):
        out = {k: history(x) for k, x in v.items()}
        if 'profile' in out:
            out['profile']['links'] = [row[:4] + ['<added_at>'] for row in out['profile']['links']]
        if 'identity' in out and isinstance(out['identity'], dict):
            row = out['identity'].get('identity_uncertain')
            if row:
                out['identity']['identity_uncertain'] = row[:2] + ['<created_at>']
        return out
    if isinstance(v, list):
        return [history(x) for x in v]
    return v


def state(store, tables):
    root = store.path.parent
    out = {}
    with store.connect() as c:
        for table in dict.fromkeys(tables):
            if not c.execute('SELECT 1 FROM sqlite_master WHERE name=?', (table,)).fetchone():
                continue
            cols = [r[1] for r in c.execute(f'PRAGMA table_info({table})')
                    if r[1] != 'ts' and not r[1].endswith('_at')]
            rows = []
            for row in c.execute(f'SELECT {",".join(cols)} FROM {table}'):
                values = []
                for col, v in zip(cols, row):
                    if col == 'before_json':
                        v = history(terms_core.legacy_before(json.loads(v)))
                    values.append(value(v, root))
                rows.append(values)
            out[table] = [cols] + sorted(rows, key=lambda r: json.dumps(r, ensure_ascii=False, sort_keys=True))
        out['身份元数据'] = list(map(list, c.execute("SELECT key,value FROM meta WHERE key LIKE 'identity_%' ORDER BY key")))
    return out


class Checkpoints:
    def __init__(self, test, suffix=""):
        self.test = test
        self.path = golden_writes.GOLDEN / ('terms_c3_' + test.id().removeprefix('tests.').replace('.', '_') + suffix + '.json')
        self.record = []
        self.previous = {}
        self.want = json.loads(self.path.read_text(encoding='utf-8')) if self.path.exists() else None
        test.addCleanup(self.finish)

    def capture(self, store, tables):
        after = state(store, tables)
        step = golden_writes.diff(self.previous, after)
        i = len(self.record)
        if self.want is not None:
            self.test.assertLess(i, len(self.want), self.path.name)
            self.test.assertEqual(step, self.want[i], f'{self.path.name} 第 {i + 1} 个检查点')
        self.record.append(step)
        self.previous = after

    def capture_value(self, kind, result):
        step = {kind: value(result, '')}
        i = len(self.record)
        if self.want is not None:
            self.test.assertLess(i, len(self.want), self.path.name)
            self.test.assertEqual(step, self.want[i], f'{self.path.name} 第 {i + 1} 个返回值')
        self.record.append(step)

    def finish(self):
        if self.want is not None:
            self.test.assertEqual(len(self.record), len(self.want), self.path.name)
        else:
            self.test.fail(f'缺少固定快照：{self.path.name}')
