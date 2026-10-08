"""tests/golden 快照的公用部分：固定的 uuid4 替身与表差分。快照只读，不提供重录入口。

写入口的逐步快照已改为 native/terms/tests/replay.rs 的重放（#9）；这里只剩 test_terms_service 的检查点在用。
"""
import json
import uuid
from collections import Counter
from pathlib import Path


GOLDEN = Path(__file__).resolve().parent / 'golden'


class Ids:
    """uuid.uuid4 的替身：固定序列。"""
    def __init__(self, start):
        self.n = start

    def __call__(self):
        self.n += 1
        return uuid.UUID(int=self.n)


def diff(before, after):
    """每张表：去掉的行、加上的行（多重集）。"""
    out = {}
    for table in after:
        a, b = before.get(table), after[table]
        if a == b:
            continue
        if a is None or b is None or a[0] != b[0]:
            out[table] = {'replaced': b}
            continue
        key = lambda r: json.dumps(r, ensure_ascii=False)
        old, new = Counter(map(key, a[1:])), Counter(map(key, b[1:]))
        out[table] = {'-': [json.loads(k) for k in sorted((old - new).elements())],
                      '+': [json.loads(k) for k in sorted((new - old).elements())]}
    return out

