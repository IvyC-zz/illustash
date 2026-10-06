"""词条核心写路由：Python 只构建参数、读取外部证据及返回形态，检查和历史由核心执行。

未完成四项启动切换时拒绝写；已删除旧 Python 方法体，不提供回退。
"""
from functools import wraps
import uuid

from . import names


def via_core(build, after=None):
    """完整切换后统一交给核心；build 返回命令或 Done，after 转换返回值，默认取最近历史。"""
    def decorate(fn):
        @wraps(fn)
        def call(self, *args, **kwargs):
            self.require_writable()
            with self._labels_lock:
                command = build(self, *args, **kwargs)
                if isinstance(command, Done):
                    return command.result
                out = self._core(command)
                if after is not None:
                    return after(self, out, command, *args, **kwargs)
                with self.connect() as c:
                    return self.last_action(c)
        return call
    return decorate


class Done:
    """A builder's answer when there is nothing to write (the old method returned early with `result`)."""
    def __init__(self, result):
        self.result = result


def asked(store, ids, filters, exclude, confirm):
    """The pictures the old method selects (filters are Python's), before copies are added (the core adds them itself).
    When the old method refuses the selection, it does so after the checks it makes first: the core gets the message
    and gives it at that same point."""
    from .store import Invalid
    # 显式小批图片由核心在同一写事务内检查存在性，免掉 Python 的重复连接与查询。
    # 筛选、排除、类型和超量确认仍走原检查，拒绝顺序与文案不变。
    if filters is None and (exclude is None or isinstance(exclude, list) and not exclude) \
            and isinstance(ids, list) and ids and all(isinstance(i, str) for i in ids) \
            and (len(ids) <= 500 or confirm is True):
        return {'asked': list(ids)}
    try:
        with store.connect() as c:
            return {'asked': store._resolve_ids(c, ids, filters, [] if exclude is None else exclude, confirm, copies=False)}
    except Invalid as exc:
        return {'asked_refused': str(exc)}


def returned(store, out, command, *args, **kwargs):
    """Structure edits return what the old method returned; the core answers the same shape."""
    return out


# ---- builders: the command for each Store method ----

def change(store, ids, action, entity=None, *, filters=None, exclude=None, confirm=False, entities=None):
    store._change_args(action, entity, entities)
    return {'op': 'change', **asked(store, ids, filters, exclude, confirm), 'action': action, 'entity': entity,
            'entities': entities, 'confirm': True, 'effect': 'verdict'}


def answer(store, entity, yes=(), no=(), kind='seed', switch=None, later=()):
    store._answer_args(yes, no, kind, switch, later)
    return {'op': 'answer', 'entity': entity, 'yes': list(yes), 'no': list(no), 'later': list(later), 'kind': kind,
            'switch': switch, 'effect': 'verdict'}


def answered(store, out, command, *args, **kwargs):
    """None when only「以后再说」was answered (no step), else the step."""
    if out['action'] is None:
        return None
    with store.connect() as c:
        return store.last_action(c)


def answer_audit(store, yes=(), no=()):
    store._audit_args(yes, no)
    return {'op': 'answer_audit', 'yes': list(yes), 'no': list(no), 'effect': 'verdict'}


def reassign(store, ids, old_entity, new_entity, *, filters=None, exclude=None, confirm=False):
    wanted = store._wanted(new_entity)
    return {'op': 'reassign', **asked(store, ids, filters, exclude, confirm), 'old_entity': old_entity,
            'new_entity': wanted, 'confirm': True, 'effect': 'verdict'}


def sweep(store, entity):
    return {'op': 'sweep', 'entity': entity, 'effect': 'verdict'}


def uncertain(store, ids, options, reason='人工暂不判断'):
    store._uncertain_args(options, reason)
    # Ambiguity is per image: the old method selects without copies and without a confirmation.
    return {'op': 'uncertain', **asked(store, ids, None, None, False), 'options': options, 'reason': reason,
            'effect': 'verdict'}


def decide(store, *, a=None, b=None, kind=None, relation_id=None, status='proposed', reason='', evidence=None):
    # Store.decide_identity checks the id's Python type first, before anything else; the core does the rest in order.
    if relation_id is not None and (type(relation_id) is not int or relation_id <= 0):
        from .store import Invalid
        raise Invalid('关系 ID 不正确')
    return {'op': 'decide', 'a': a, 'b': b, 'kind': kind, 'relation_id': relation_id, 'status': status, 'reason': reason,
            'evidence': evidence, 'effect': 'verdict'}


def merge(store, source, target):
    """Under hybrid the core makes one duplicate_of decision (label '已保存身份关系决议'); with no policy it merges and
    names both sides as they read after the merge."""
    return {'op': 'merge', 'source': source, 'target': target, 'effect': 'verdict'}


def _readable(copyright):
    return names.readable(copyright) if copyright else ''


def set_profile(store, entity, *, name=None, aliases=None, work=None, tag=None, copyright_of=None):
    """The old method's first steps: the tag's form, then the copyright asked over the network (only when a new work
    may be needed). A new work's id is asked for by the core when it gets there (Store._core)."""
    tag = store._tag(tag) if tag else tag
    copyright = copyright_of(tag) if tag and copyright_of and work is None and store._needs_copyright(tag) else ''
    return {'op': 'set_profile', 'entity': entity, 'name': name, 'aliases': aliases, 'work': work, 'tag': tag,
            'copyright': copyright, 'work_id': False, 'effect': 'verdict'}


def profiled(store, out, command, *args, **kwargs):
    with store.connect() as c:
        return {'undo': store.last_action(c), 'id': out['id']}


def link_local(store, items, copyright_of=None):
    plan = store._link_args(items)
    works = {tag: copyright_of(tag) if copyright_of and store._needs_copyright(tag) else '' for _, tag in plan}
    return {'op': 'link_local', 'items': [{'id': ident, 'tag': tag, 'copyright': works[tag], 'work_id': False}
                                          for ident, tag in plan], 'effect': 'verdict'}


def linked(store, out, command, *args, **kwargs):
    with store.connect() as c:
        return {'undo': store.last_action(c), 'ids': out['ids']}


def create_entity(store, name, kind='', artist=''):
    """The argument checks and the name lookups are the old method's own helpers, under the write lock; the core checks
    the rest. The id is drawn where the old method drew it."""
    name, kind, artist = store._entity_args(name, kind, artist)
    with store.connect() as c:
        name, row = store._entity_named(c, name)
        if row:
            return Done(store._display(row, c))      # an existing identity matched: nothing to write
    return {'op': 'create_entity', 'id': str(uuid.uuid4()), 'name': name, 'lang': names.lang_of(name), 'kind': kind,
            'artist': artist}


def shown_created(store, out, command, *args, **kwargs):
    """What Store.create_entity returns: the new identity as displayed, read after the core committed it."""
    with store.connect() as c:
        return store._display({k: command[k] for k in ('id', 'name', 'kind', 'artist')}, c)


def create_linked(store, tag, typed='', copyright_of=None):
    """The old method's checks and lookups in its order (tag, owner, the copyright asked only when no local work
    matches), ids drawn in its order (the character, then a new work); the core re-checks the owner."""
    tag = store._tag(tag)
    with store.connect() as c:
        owner = store._tag_owner(c, tag)
        if owner:
            return Done(store._display({'id': owner}, c))
    copyright = copyright_of(tag) if copyright_of and store._needs_copyright(tag) else ''
    ident = str(uuid.uuid4())
    with store.connect() as c:
        work = None if store._local_work(c, copyright, store._qualifier(tag)) or not copyright else str(uuid.uuid4())
    typed = typed.strip()[:120] if isinstance(typed, str) else ''
    alias = typed if typed and names.normalize(typed.replace(' ', '_')) != names.normalize(tag) else None
    return {'op': 'create_linked', 'id': ident, 'tag': tag, 'alias': alias, 'copyright': copyright,
            'readable': _readable(copyright), 'work_id': work}


def shown_linked(store, out, command, *args, **kwargs):
    with store.connect() as c:
        return store._display({'id': out['id']}, c)


def shared_oc(store):
    """The one OC mark: the existing one as displayed, else a new one (its id drawn where the old method drew it)."""
    from .store import NAME
    with store.connect() as c:
        row = c.execute(f"""SELECT e.id,{NAME} AS name,e.kind,e.artist FROM entities e
                            WHERE label_only=1 AND merged_into IS NULL ORDER BY rowid LIMIT 1""").fetchone()
        if row:
            return Done(store._display(row, c))
    return {'op': 'shared_oc', 'id': str(uuid.uuid4())}


def shown_shared(store, out, command, *args, **kwargs):
    with store.connect() as c:
        return store._display({'id': out['id'], 'name': store.SHARED_OC, 'kind': 'oc', 'artist': ''}, c)


def set_kind(store, entity, kind, artist=''):
    kind, artist = store._oc_fields(kind, artist)
    return {'op': 'set_kind', 'entity': entity, 'kind': kind, 'artist': artist}


def set_mascot(store, entity, mascot):
    store._mascot_arg(mascot)
    return {'op': 'set_mascot', 'entity': entity, 'mascot': mascot}


def set_work(store, entity, work):
    return {'op': 'set_work', 'entity': entity, 'work': work}


def edit_work(store, action, ident=None, name=None, parent=None, tag=None):
    """`Store.edit_work` returns the work's row read after its transaction; the command is built from the arguments and
    the core checks them with the old messages."""
    command = {'op': 'edit_work', 'action': action, 'ident': ident, 'name': name, 'parent': parent, 'tag': tag,
               'lang': names.lang_of(name) if isinstance(name, str) and name.strip() else None,
               'id': False}      # a new work's id is asked for by the core when it gets there (Store._core)
    if action == 'from_tag':
        tag = store._tag(tag)
        command |= {'tag': tag, 'readable': _readable(tag)}
    out = store._core(command)
    return next(r for r in store.works() if r['id'] == out['id'])
