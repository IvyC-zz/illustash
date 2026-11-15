"""九个获批方法的完整持久化断言；不计算预期业务结果。"""
from contextlib import closing
from datetime import datetime, timezone
import json
import sqlite3

import numpy as np


def assert_npy(test, actual, expected):
    """下游读取的 dtype、形状和值全部一致，不接受等值的 float64 替代 float32。"""
    a, b = np.load(actual), np.load(expected)
    test.assertEqual(a.dtype, b.dtype)
    test.assertEqual(a.shape, b.shape)
    np.testing.assert_array_equal(a, b)


def assert_index_files(test, actual, expected, cached=False):
    assert_npy(test, actual / 'char-anchors.npy', expected / 'char-anchors.npy')
    # 读取顺序就是锚点行与角色 ID 的绑定，不能排序后比较。
    test.assertEqual((actual / 'char-anchors.txt').read_text().splitlines(),
                     (expected / 'char-anchors.txt').read_text().splitlines())
    test.assertEqual((actual / 'char-scan.csv').read_bytes(), (expected / 'char-scan.csv').read_bytes())
    if cached:
        for name in ('char-index/new.npy', 'char-patch/raw/new.npy', 'char-patch/raw/twin.npy'):
            assert_npy(test, actual / name, expected / name)
        for name in ('char-index/new.json', 'char-patch/rejected/twin.json'):
            test.assertEqual(json.loads((actual / name).read_text()), json.loads((expected / name).read_text()))


def utc_second():
    return datetime.now(timezone.utc).replace(tzinfo=None, microsecond=0)


def cache_state(folder):
    """保留全部表、列、行、schema 和版本标记，不以投影掩盖未预期写入。"""
    with closing(sqlite3.connect(folder / 'provenance.sqlite')) as db:
        schema = db.execute('SELECT type,name,tbl_name,sql FROM sqlite_schema ORDER BY type,name').fetchall()
        tables = {}
        for kind, name, _, _ in schema:
            if kind != 'table':
                continue
            quoted = '"' + name.replace('"', '""') + '"'
            columns = db.execute(f'PRAGMA table_info({quoted})').fetchall()
            primary = [i for _, i in sorted((row[5], i) for i, row in enumerate(columns) if row[5])]
            tables[name] = dict(columns=[row[1] for row in columns], primary=primary,
                                rows=sorted(db.execute(f'SELECT * FROM {quoted}').fetchall(), key=repr))
        return dict(schema=schema, user_version=db.execute('PRAGMA user_version').fetchone()[0],
                    application_id=db.execute('PRAGMA application_id').fetchone()[0], tables=tables)


GENERATED_TIME = {'image_sources': 'fetched_at', 'image_artists': 'updated_at', 'lookups': 'last_at'}
COMPARISON_TIME = GENERATED_TIME | {'artists': 'created_at'}


def assert_cache_preserved(test, before, after, started, finished):
    """只允许追加三张结果表；旧行（含时间戳）逐字段保留，新时间落在调用窗口内。"""
    test.assertEqual(before['schema'], after['schema'])
    test.assertEqual(before['user_version'], after['user_version'])
    test.assertEqual(before['application_id'], after['application_id'])
    test.assertEqual(before['tables'].keys(), after['tables'].keys())
    for name, old in before['tables'].items():
        new = after['tables'][name]
        if name not in GENERATED_TIME:
            test.assertEqual(new, old, name)
            continue
        test.assertEqual(old['columns'], new['columns'], name)
        test.assertTrue(old['primary'], name)
        key = lambda row: tuple(row[i] for i in old['primary'])
        previous, current = ({key(row): row for row in table['rows']} for table in (old, new))
        for identity, row in previous.items():
            test.assertEqual(current.get(identity), row, (name, identity))
        clock = new['columns'].index(GENERATED_TIME[name])
        for identity in current.keys() - previous.keys():
            value = current[identity][clock]
            stamp = datetime.strptime(value, '%Y-%m-%d %H:%M:%S')
            test.assertGreaterEqual(stamp, started)
            test.assertLessEqual(stamp, finished)


def comparable_cache(state):
    """仅归一化明确由时钟生成的四列；next_due_at/cursor 等确定值原样比较。

    历史记录的生成时间不会等于本次运行。实际输出先通过 assert_cache_preserved
    验证旧时间完全不变、新时间有效，再与冻结/NumPy 参考比较其余全部内容。
    """
    tables = {}
    for name, table in state['tables'].items():
        rows = []
        clock = table['columns'].index(COMPARISON_TIME[name]) if name in COMPARISON_TIME else None
        for original in table['rows']:
            row = list(original)
            if clock is not None:
                value = row[clock]
                stamp = datetime.strptime(value, '%Y-%m-%d %H:%M:%S')
                if stamp.strftime('%Y-%m-%d %H:%M:%S') != value:
                    raise AssertionError('缓存时间戳格式不规范')
                row[clock] = '<runtime timestamp>'
            # SQLite 值类型也属于比较，防止 Python 把 int/float 的等值当作同一值。
            rows.append(tuple((type(value).__name__, value) for value in row))
        tables[name] = table | {'rows': sorted(rows, key=repr)}
    return state | {'tables': tables}
