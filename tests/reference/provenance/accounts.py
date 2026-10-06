"""Bound accounts: one set for the whole app (every library shares them), secrets kept in the macOS Keychain.

    pixiv     the person's own Pixiv login, read-only (works lists, related users), R18 included. Normally they log in
              themselves in the desktop app's Pixiv window and the login stays there (tests/reference/provenance/desktop.py; only who it is
              gets recorded here); pasting a PHPSESSID into the Keychain is left as a development entry for the plain web page.
    saucenao  the person's own SauceNAO API key (the image search switch; used from phase 2)

X is not bound by login (its API is too expensive for a personal library); its data archive is imported per library
(tests/reference/provenance/sources/x_archive.py). Other systems fall back to a file only the user can read until each has its own store.
"""
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

from provenance import desktop
from web.library import APP_SUPPORT

SERVICE = 'Illustash'
PIXIV_EXPIRED = 'Pixiv 登录已过期，请到设置页「绑定的账号」重新登录'
RECHECK_AFTER = 5   # seconds before asking once more, then the login is judged expired
PIXIV_NO_WINDOW = 'Pixiv 登录在桌面 app 的登录窗口里，现在不是从桌面 app 启动的'
KINDS = ('pixiv', 'saucenao')
LEGACY = {'pixiv': Path('~/.config/photo-intelligence/pixiv.cookie').expanduser(),
          'saucenao': Path('~/.config/photo-intelligence/saucenao.key').expanduser()}


class AccountError(Exception):
    pass


def _check(kind):
    if kind not in KINDS:
        raise AccountError(f'未知的账号类型：{kind}')


def _file(kind):
    return APP_SUPPORT / 'accounts' / kind


def _keychain():
    return sys.platform == 'darwin' and os.environ.get('ILLUSTASH_NO_KEYCHAIN') != '1'


def get(kind):
    _check(kind)
    if _keychain():
        done = subprocess.run(['security', 'find-generic-password', '-s', SERVICE, '-a', kind, '-w'], capture_output=True, text=True)
        return (done.stdout.strip() or None) if done.returncode == 0 else None
    try:
        return _file(kind).read_text().strip() or None
    except OSError:
        return None


def put(kind, secret):
    _check(kind)
    if _keychain():
        # -U updates an existing item; the secret goes through argv only to the local `security` tool.
        done = subprocess.run(['security', 'add-generic-password', '-U', '-s', SERVICE, '-a', kind, '-l', f'{SERVICE} · {kind}', '-w', secret],
                              capture_output=True, text=True)
        if done.returncode:
            raise AccountError('写入钥匙串失败：' + done.stderr.strip()[-120:])
        return
    path = _file(kind)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.touch(mode=0o600)
    path.write_text(secret)


def remove_secret(kind):
    _check(kind)
    if _keychain():
        subprocess.run(['security', 'delete-generic-password', '-s', SERVICE, '-a', kind], capture_output=True)
    else:
        _file(kind).unlink(missing_ok=True)


def remove(kind):
    """Unbind: the secret, the login in the app's Pixiv window, and what was recorded about it."""
    remove_secret(kind)
    if kind == 'pixiv' and pixiv_in_app() and desktop.available():
        desktop.logout()
    meta = _meta()
    meta.pop(kind, None)
    _save_meta(meta)


# ---------- non-secret facts about each binding (who, when checked) ----------
def _meta_file():
    return APP_SUPPORT / 'accounts.json'


def _meta():
    try:
        return json.loads(_meta_file().read_text(encoding='utf-8'))
    except (OSError, ValueError):
        return {}


def _save_meta(meta):
    APP_SUPPORT.mkdir(parents=True, exist_ok=True)
    tmp = _meta_file().with_suffix('.tmp')
    tmp.write_text(json.dumps(meta, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    os.replace(tmp, _meta_file())


def status():
    meta = _meta()
    out = {kind: {'bound': bool(get(kind)), **meta.get(kind, {})} for kind in KINDS}
    out['pixiv']['bound'] = pixiv_bound()
    return out


def pixiv_in_app():
    """The Pixiv login lives in the desktop app's Pixiv window (not a Keychain PHPSESSID)."""
    return _meta().get('pixiv', {}).get('via') == 'app'


def pixiv_bound():
    return pixiv_in_app() or bool(get('pixiv'))


# ---------- Pixiv ----------
def clean_pixiv(value):
    """Accept the bare value, 'PHPSESSID=…', or a pasted cookie header; return the PHPSESSID value."""
    value = (value or '').strip()
    if (m := re.search(r'PHPSESSID=([^;\s]+)', value)):
        value = m.group(1)
    if not re.fullmatch(r'\d+_[A-Za-z0-9]+', value):
        raise AccountError('PHPSESSID 的格式不对：应该是「数字_字母数字」，例如 12345678_AbCd…')
    return value


def _who(net, uid, headers=None, problem='Pixiv 没有接受这个登录状态（可能已退出或过期）'):
    """Who is logged in (two read-only requests); raises AccountError when Pixiv does not accept the login."""
    from provenance.net import LoginRejected
    why = None
    try:
        extra = net.get('https://www.pixiv.net/ajax/user/extra', {'lang': 'ja'}, extra=headers)
    except LoginRejected:
        extra, why = None, 'HTTP 401'
    if not extra or extra.get('error'):   # say what Pixiv answered: a wrong「过期」must be traceable
        why = why or ('HTTP 404' if extra is None else f"error：{str(extra.get('message') or '无说明')[:80]}")
        raise AccountError(f'{problem}（Pixiv 返回 {why}）')
    user = (net.get(f'https://www.pixiv.net/ajax/user/{uid}', {'lang': 'ja'}, extra=headers) or {}).get('body') or {}
    return {'user_id': uid, 'name': user.get('name') or '', 'following': (extra.get('body') or {}).get('following')}


def check_pixiv(session, net):
    """Who a pasted session belongs to (the development entry)."""
    return _who(net, session.split('_')[0], {'Cookie': f'PHPSESSID={session}', 'Referer': 'https://www.pixiv.net/'},
                'Pixiv 没有接受这个登录状态（可能已退出或过期），请重新复制')


def make_net(budget_mb, app=None):
    """A provenance.net.Net for the bound accounts. With the login in the app's window, www.pixiv.net goes through that
    window, or is stopped when the app is not there: never quietly logged out (no R18, and misses would be cached)."""
    from provenance.net import Net
    app = pixiv_in_app() if app is None else app
    out = Net(budget_mb, headers(), via=desktop.via() if app else None)
    if app and not desktop.available():
        out.stopped[desktop.HOST] = PIXIV_NO_WINDOW
    return out


def connect_pixiv(net):
    """After the person logged in in the app's Pixiv window: record who it is (the login itself stays in the window).
    None while the window has no login yet; AccountError when Pixiv does not accept it."""
    s = desktop.session()
    if not s.get('loggedIn'):
        return None
    try:
        who = _who(net, s['userId'], problem='Pixiv 没有接受登录窗口里的登录，请在窗口里重新登录')
    except ConnectionError:
        if s.get('loginOpen'):   # the cookie is there but the window is still on Pixiv's login pages: ask again later
            return None
        raise
    if get('pixiv'):   # the pasted development session is replaced by the window's login
        remove_secret('pixiv')
    meta = _meta()
    meta['pixiv'] = {**who, 'via': 'app', 'checked_at': time.strftime('%Y-%m-%d %H:%M')}
    _save_meta(meta)
    return meta['pixiv']


def bind_pixiv(value, net):
    session = clean_pixiv(value)
    who = check_pixiv(session, net)
    put('pixiv', session)
    meta = _meta()
    meta['pixiv'] = {**who, 'checked_at': time.strftime('%Y-%m-%d %H:%M')}   # drops via=app: the pasted session is used
    _save_meta(meta)
    return meta['pixiv']


def mark_pixiv(who=None, problem=None, detail=None):
    """Record a check of the bound session: `who` when Pixiv accepted it, `problem` (and what Pixiv answered) when not."""
    meta = _meta()
    entry = {**meta.get('pixiv', {}), **(who or {}), 'checked_at': time.strftime('%Y-%m-%d %H:%M')}
    if problem:
        entry['expired'] = problem
        entry['expired_detail'] = detail or ''
    else:
        entry.pop('expired', None)
        entry.pop('expired_detail', None)
    meta['pixiv'] = entry
    _save_meta(meta)
    return entry


def recheck_pixiv(net):
    """Ask Pixiv whether the bound session still works. True / False (marked expired) / None when nothing is bound.
    Refusals and network errors (Stopped, ConnectionError) propagate: they say nothing about the login."""
    if pixiv_in_app():
        if not desktop.available():
            raise ConnectionError(PIXIV_NO_WINDOW)
        for attempt in range(2):   # 2026-09-28: a valid login was once judged expired right after the app started
            s = desktop.session()
            try:
                if not s.get('loggedIn'):
                    raise AccountError('登录窗口里没有 Pixiv 登录')
                who = _who(net, s['userId'])
                break
            except AccountError as exc:
                if attempt:
                    mark_pixiv(problem=PIXIV_EXPIRED, detail=str(exc))
                    return False
                time.sleep(RECHECK_AFTER)
        mark_pixiv(who)
        return True
    if not (session := get('pixiv')):
        return None
    try:
        who = check_pixiv(session, net)
    except AccountError:
        mark_pixiv(problem=PIXIV_EXPIRED)
        return False
    mark_pixiv(who)
    return True


def bind_saucenao(key):
    key = (key or '').strip()
    if not re.fullmatch(r'[0-9a-fA-F]{40}', key):
        raise AccountError('SauceNAO 的 API key 应该是 40 位十六进制字符')
    put('saucenao', key)
    meta = _meta()
    meta['saucenao'] = {'checked_at': time.strftime('%Y-%m-%d %H:%M')}
    _save_meta(meta)
    return meta['saucenao']


def migrate_legacy():
    """Move the plain files the experiments used into the Keychain, then delete them. Returns the kinds moved."""
    moved = []
    for kind, path in LEGACY.items():
        if path.is_file() and not get(kind):
            secret = path.read_text().strip()
            if secret:
                put(kind, secret)
                moved.append(kind)
        if path.is_file() and get(kind):
            path.unlink()
    return moved


def pixiv_user_id(net):
    """The logged-in Pixiv user a Net would ask as, or None: through the app's window (the user recorded when it was
    bound and at every pre-run check), or from the development entry's PHPSESSID header. Steps use this, not the header."""
    if isinstance(getattr(net, 'via', None), dict) and desktop.HOST in net.via:
        return _meta().get('pixiv', {}).get('user_id') or None
    match = re.search(r'(?:^|;\s*)PHPSESSID=(\d+)_', net.headers.get(desktop.HOST, {}).get('Cookie', ''))
    return match[1] if match else None


def headers():
    """Request headers for provenance.net.Net from the bound accounts (the app's Pixiv window needs none)."""
    out = {'i.pximg.net': {'Referer': 'https://www.pixiv.net/'}}
    if not pixiv_in_app() and (session := get('pixiv')):
        out['www.pixiv.net'] = {'Cookie': f'PHPSESSID={session}', 'Referer': 'https://www.pixiv.net/'}
    return out
