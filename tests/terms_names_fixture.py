"""名字投影（docs/TERMS_NAMES_PLAN.md）的共用夹具：一个把选名规则每条分支都走到的小图库，和全量名字快照。

快照是旧代码（tests/reference/web/names.py 的 View、search_hits、action_label）对这个库算出的全部结果：每个身份的完整名与皮肤短名、
每个文字键（作品 qualifier、皮肤 variant、角色 tag）的 choose、一批查询的搜索结果、全部历史的重新渲染。之后核心的
投影、搜索和历史文案都与它全量逐字对照。"""
import json

from web import names

# 身份：键是夹具里的短 id。
WORKS = {
    'w-ba': ('blue_archive', [('zh', 'bangumi', '碧蓝档案Bangumi')]),          # 词表 WORK_NAMES 压过 bangumi
    'w-th': ('touhou', [('zh', 'user', '东方')]),                             # 用户的中文名压过词表
    'w-q': ('series_q', [('zh', 'model', '模型名'), ('ja', 'index', 'シリーズ'), ('en', 'bangumi', 'Series Q en')]),
    'w-wiki': ('series_w', [('zh', 'wiki', '维基名'), ('ja', 'index', 'ウィキ')]),  # wiki 中文按 bangumi 排
    'w-none': ('series_none', []),                                             # 没有名字：可读的 tag
    'w-user': ('series_u', [('zh', 'bangumi', '词表外')]),                     # 作品自己有用户名字
}


def build(root, *, legacy=False):
    """从固定旧库恢复全部名字边界输入；被测状态默认完整核心，legacy 仅用于启动同步／切换验收。"""
    from tests import terms_seed
    store, attrs = terms_seed.load('names_writes', root)
    if not legacy:
        terms_seed.switch(store)
    return store, {'alice': attrs['alice'], 'bob': attrs['bob']}


QUERIES = ('蔚蓝', '碧蓝', 'b名', 'prio', 'Prio', '泳装', 'b名 · 泳装', 'series q', 'Q 系列', '本地', 'シリーズ', '借来',
           'maid', '女仆', 'en only', 'alice', '爱丽丝', '东方', 'x', '样式', 'wd only', 'retired')


def snapshot(c, view=names.View, search=names.search_hits_python):
    """旧代码对这个库算出的全部名字结果（JSON 往返，便于逐字比较）。名字切换之后，`view=names.view`、
    `search=names.search_hits` 就是 App 实际的读路径（核心的投影和核心的搜索）。"""
    view = view(c)
    view.load()
    ids = [r[0] for r in c.execute('SELECT id FROM entities ORDER BY id')]
    keys = sorted({('work', r[0]) for r in c.execute("SELECT key FROM links WHERE system='qualifier'")}
                  | {('skin', r[0]) for r in c.execute("SELECT variant FROM entities WHERE variant<>''")}
                  | {('character', view.entities[i]['tag']) for i in ids if view.entities[i]['tag']})
    out = {
        'entity': {i: view.entity(i) for i in ids},
        'skin_only': {i: view.entity(i, True) for i in ids},
        'choose': {f'{k}:{key}': view.choose(k, key) for k, key in keys},
        'search': {q: search(c, q) for q in QUERIES},
        'labels': {r['id']: names.action_label(c, r) for r in c.execute('SELECT * FROM actions ORDER BY id')},
    }
    return json.loads(json.dumps(out, ensure_ascii=False, sort_keys=True))
