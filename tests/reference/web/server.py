"""仅供合成测试及对照工具；不属于生产应用宿主。"""
"""Loopback-only HTTP server; no build tools or network dependencies."""
import argparse
import gzip
from contextlib import contextmanager
import hashlib
import json
import mimetypes
import os
import platform
import subprocess
import secrets
import socket
import socketserver
import sys
import threading
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

from PIL import Image, ImageOps, UnidentifiedImageError
import web
from web.store import SCHEMA_VERSION, Invalid, Store
from web.sync import PHOTOS, Syncer
from web.index_update import IndexUpdater
from web import relate
from web.similar import SimilarScanner
from web.features import Features
from web import artists, folder, library
from web.provenance_job import KINDS as PROVENANCE_KINDS, Runner as ProvenanceRunner
from web.library import backups_dir
from web.learn import Learner
from web.names import FetchError, NameUpdater

ROOT = Path(__file__).resolve().parents[3]
STATIC = ROOT / 'web' / 'static'
# Longest edge per requested size; `full` is capped by the local Photos copy.
SIZES = {'thumb': 400, 'medium': 800, 'preview': 1600, 'full': 3000}


class Server(ThreadingHTTPServer):
    daemon_threads = True
    def server_bind(self):
        # This loopback reference has an explicit numeric origin. HTTPServer's
        # getfqdn() can stall in macOS DNS before we even begin accepting requests.
        socketserver.TCPServer.server_bind(self)
        self.server_name, self.server_port = self.server_address[:2]

    def __init__(self, address, store, cache, *, public_origin=None):
        super().__init__(address, Handler)
        self.store, self.cache = store, Path(cache)
        self.cache.mkdir(parents=True, exist_ok=True)
        self.token = secrets.token_urlsafe(32)
        self.origin = public_origin or f'http://127.0.0.1:{self.server_port}'
        public = urlsplit(self.origin)
        if public.scheme != 'http' or public.hostname != '127.0.0.1' or not public.port or public.username or public.path or public.query or public.fragment:
            self.server_close()
            raise ValueError('HTTP 前门必须是明确的 127.0.0.1 HTTP origin')
        self.host_header = public.netloc
        self.image_slots = threading.BoundedSemaphore(4)
        self.image_locks_guard = threading.Lock()
        self.image_locks = {}
        self.syncer = None
        self.library = None   # the open library package (web/library.py), None for a bare source/app pair
        self.export_state = {'status': 'idle'}
        self.provenance = None   # web/provenance_job.Runner for a library package
        self.learner = None
        self.indexer = None
        self.names = NameUpdater(store)
        self.similar = None
        self.build = None  # git commit, looked up on first request
        self.store.on_effect = self._after_operation
        self.forced_off = set()  # background tasks switched off by a --no-* flag this launch


    def _after_operation(self, kind):
        if self.learner:
            self.learner.poke(regroup=kind == 'imported')
        if kind == 'imported':
            if self.similar:
                self.similar.poke()
            if self.provenance and self.store.startup()['provenance']:
                self.provenance.start('new')

    @contextmanager
    def image_lock(self, key):
        # Only in-flight keys live here; waiters share the same lock until the last exits.
        with self.image_locks_guard:
            entry = self.image_locks.setdefault(key, [threading.Lock(), 0])
            entry[1] += 1
        try:
            with entry[0]:
                yield
        finally:
            with self.image_locks_guard:
                entry[1] -= 1
                if not entry[1]:
                    del self.image_locks[key]


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def fetch_names(self, entity):
        """Names for a newly linked character (and its work) arrive in the background; a failure never blocks the edit."""
        try:
            self.server.names.start(entity=entity)
        except (ValueError, AttributeError):
            pass

    def respond(self, payload, status=200, kind='application/json; charset=utf-8', cache=False):
        if not isinstance(payload, bytes):
            payload = json.dumps(payload, ensure_ascii=False).encode()
        encoding = None
        if kind.startswith('application/json') and len(payload) >= 1024:
            accepted = {}
            for part in self.headers.get('Accept-Encoding', '').split(','):
                name, *params = part.strip().lower().split(';')
                try:
                    accepted[name.strip()] = next((float(p.strip()[2:]) for p in params if p.strip().startswith('q=')), 1)
                except ValueError:
                    accepted[name.strip()] = 0
            if accepted.get('gzip', accepted.get('*', 0)) > 0:
                compressed = gzip.compress(payload, compresslevel=1)
                if len(compressed) < len(payload):
                    payload, encoding = compressed, 'gzip'
        self.send_response(status)
        if kind.startswith('application/json'):
            self.send_header('Vary', 'Accept-Encoding')
        if encoding:
            self.send_header('Content-Encoding', encoding)
        self.send_header('Content-Type', kind)
        self.send_header('Content-Length', str(len(payload)))
        self.send_header('Cache-Control', 'private, max-age=86400' if cache else 'no-store')
        self.send_header('X-Content-Type-Options', 'nosniff')
        self.send_header('Referrer-Policy', 'no-referrer')
        self.send_header('Content-Security-Policy', "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'")
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def allowed(self, write=False):
        if self.headers.get('Host') != self.server.host_header:
            self.respond({'error': '仅允许本机地址访问'}, 403)
            return False
        origin = self.headers.get('Origin')
        if origin and origin != self.server.origin:
            self.respond({'error': '不允许跨站访问'}, 403)
            return False
        if write and (origin != self.server.origin or self.headers.get('X-CSRF-Token') != self.server.token):
            self.respond({'error': '会话已过期，请刷新页面'}, 403)
            return False
        return True

    def do_GET(self):
        if not self.allowed():
            return
        try:
            url = urlsplit(self.path)
            q = {k: v[-1] for k, v in parse_qs(url.query).items()}
            store = self.server.store
            if url.path == '/api/bootstrap':
                self.respond({**store.overview(q.get('nsfw', '')), 'token': self.server.token, 'read_only': store.write_refusal()})
            elif url.path == '/api/identity/status':
                self.respond(store.identity_status())
            elif url.path == '/api/identity/relations':
                self.respond(store.identity_relations(q.get('status', ''), int(q.get('limit', 100)), int(q.get('offset', 0))))
            elif url.path == '/api/identity/count':
                self.respond(store.identity_queue(nsfw=q.get('nsfw', ''), count_only=True))
            elif url.path == '/api/identity/queue':
                self.respond(store.identity_queue(int(q.get('limit', 20)), int(q.get('offset', 0)), q.get('nsfw', '')))
            elif url.path == '/api/identity/pairs':
                self.respond(store.identity_pairs(int(q.get('limit', 100)), int(q.get('offset', 0)), q.get('include_empty')=='1'))
            elif url.path == '/api/identity/assignments':
                self.respond(store.identity_assignments(q.get('id', '')))
            elif url.path == '/api/entities':
                # remote=1 asks Danbooru with the typed text only.
                self.respond(store.character_hints(self.server.names.character_search(q.get('q', ''))) if q.get('remote') else store.entities(q.get('q', '')))
            elif url.path == '/api/character/profile':
                self.respond(store.profile(q.get('entity', '')))
            elif url.path == '/api/local-links':
                self.respond(store.local_link_suggestions(self.server.names.character_tag, self.server.names.copyright_of, self.server.names.character_search))
            elif url.path == '/api/oc/groups':
                if not self.server.learner:
                    raise Invalid('图像特征未加载，暂时无法按相似细分')
                from web.learn import oc_groups
                self.respond(oc_groups(store, self.server.learner.features, store.entity_id(q.get('entity', ''))))
            elif url.path == '/api/tagged':
                self.respond(store.tagged_identities(q.get('nsfw', '')))
            elif url.path == '/api/artists':
                self.respond(store.artists())
            elif url.path == '/api/character/activity':
                self.respond(store.character_activity(q.get('nsfw', '')))
            elif url.path == '/api/ask':
                self.respond({'batch': store.ask_batch(q.get('nsfw', ''), skip=[x for x in q.get('skip', '').split(',') if x])})
            elif url.path == '/api/home':
                self.respond(store.home(q.get('nsfw', ''), q.get('before', ''), int(q.get('days', 5))))
            elif url.path == '/api/character/stats':
                self.respond(store.character_stats(q.get('id', ''), q.get('nsfw', ''), q.get('exclude', '')))
            elif url.path == '/api/characters':
                self.respond(store.characters(q.get('q', ''), int(q.get('min', 0)), q.get('id', ''), q.get('nsfw', ''), q.get('work', ''), summary=q.get('summary') == '1'))
            elif url.path == '/api/ids':
                offset, limit = int(q.pop('offset', 0)), int(q.pop('limit', 100))
                self.respond(store.ids(q, offset, limit))
            elif url.path == '/api/user-tags':
                self.respond(store.user_tags(q.get('q', '')))
            elif url.path == '/api/prefs':
                self.respond(store.prefs())
            elif url.path == '/api/sync':
                syncer = self.server.syncer
                self.respond({'state': syncer.status() if syncer else {'status': 'off'}, 'history': store.sync_history()})
            elif url.path == '/api/fold':
                self.respond({'kinds': store.fold_kinds()})
            elif url.path == '/api/similar':
                scanner = self.server.similar
                self.respond({'groups': store.similar_groups(q.get('kind', ''), q.get('nsfw', '')), 'state': scanner.state if scanner else {'status': 'off'}})
            elif url.path == '/api/names':
                self.respond(self.server.names.progress() if q.get('progress') == '1' else self.server.names.info())
            elif url.path == '/api/index':
                indexer = self.server.indexer
                self.respond((indexer.progress() if q.get('progress') == '1' else indexer.info())
                             if indexer else {'state': {'status': 'off'}})
            elif url.path == '/api/works':
                # Local first (no network); remote=1 also asks Danbooru with the typed text only.
                self.respond(self.server.names.work_search(q.get('q', '')) if q.get('remote') else store.works(q.get('q', '')))
            elif url.path == '/api/about':
                self.respond(about_info(self.server))
            elif url.path == '/api/settings':
                self.respond(settings_info(self.server))
            elif url.path == '/api/libraries':
                self.respond(libraries_info(self.server))
            elif url.path == '/api/accounts':
                self.respond(accounts_info(self.server))
            elif url.path == '/api/provenance':
                self.respond(provenance_info(self.server))
            elif url.path == '/api/clusters':
                learner = self.server.learner
                self.respond({'clusters': store.clusters(q.get('nsfw', '')), 'learner': learner.state if learner else {'status': 'off'}})
            elif url.path == '/api/review-pools':
                self.respond(store.review_pools(q.get('nsfw', '')))
            elif url.path == '/api/general-tags':
                self.respond(store.general_tags(q.get('q', '')))
            elif url.path == '/api/assets/count':
                self.respond(store.asset_count(q))
            elif url.path == '/api/assets':
                offset, limit = int(q.get('offset', 0)), int(q.get('limit', 100))
                if not 0 <= offset <= 10000000 or not 1 <= limit <= 200:
                    raise Invalid('分页参数不正确')
                self.respond(store.assets(q.get('entity', ''), q.get('status', 'all'), q.get('q', ''), offset, limit, q.get('order', 'newest'), q.get('pool', ''), q.get('tag', ''), q.get('variant', ''), q.get('utag', ''), q.get('nsfw', ''), q.get('dup', ''), q.get('reason', ''), q.get('cluster', ''), q.get('work', ''), include_total=q.get('count') != '0', day=q.get('day', ''), artist=q.get('artist', '')))
            elif url.path == '/api/detail':
                detail = store.detail(q.get('id', ''))
                detail.update(artists.for_asset(self.server.library, detail['uuid'], verdicts=store.artist_verdicts(detail['uuid']),
                                                rejected=store.artist_verdicts()) if self.server.library else {'artists': [], 'posts': []})
                self.respond(detail)
            elif url.path == '/api/artist-audit':
                n = int(q.get('n', 50))
                if not 1 <= n <= 500:
                    raise Invalid('抽查张数要在 1–500 之间')
                self.respond(artists.audit(self.server.library, store.uuids(), store.artist_verdicts(), n) if self.server.library else {'items': [], 'pool': 0})
            elif url.path == '/api/artist':
                name = artists.name_of(self.server.library, q.get('id', '')) if self.server.library else None
                self.respond({'id': q.get('id', ''), 'name': name})
            elif url.path == '/image':
                self.image(q.get('id', ''), q.get('size', 'thumb'))
            elif url.path in ('/', '/app.js', '/app.css', '/theme.js', '/favicon.svg', '/splash.js'):
                name = 'index.html' if url.path == '/' else url.path[1:]
                self.respond((STATIC / name).read_bytes(), kind=mimetypes.guess_type(name)[0] or 'application/octet-stream')
            else:
                self.respond({'error': '页面不存在'}, 404)
        except (Invalid, ValueError) as exc:
            self.respond({'error': str(exc)}, 400)
        except FetchError as exc:
            self.respond({'error': f'Danbooru 暂时连不上（{exc}），稍后再试'}, 502)
        except Exception as exc:
            print(f'GET failed: {type(exc).__name__}', file=sys.stderr)
            self.respond({'error': '读取失败，请重试；详情请查看启动终端'}, 500)

    def do_POST(self):
        if not self.allowed(write=True):
            # 关闭前读掉请求体：Windows 关闭仍有未读数据的连接会发 RST，客户端收不到这条 403。
            try:
                size = int(self.headers.get('Content-Length', 0))
                if 0 < size <= 100000:
                    self.rfile.read(size)
            except (ValueError, OSError):
                pass
            return
        try:
            size = int(self.headers.get('Content-Length', 0))
            if not 1 <= size <= 100000 or self.headers.get_content_type() != 'application/json':
                raise Invalid('请求格式不正确')
            body = json.loads(self.rfile.read(size))
            if not isinstance(body, dict):
                raise Invalid('请求格式不正确')
            reason = self.server.store.write_refusal()
            if reason and not self.path.startswith(READ_ONLY_ALLOWED):
                self.respond({'error': reason}, 409)
                return
            if self.path == '/api/change':
                self.respond({'undo': self.server.store.change(body.get('ids'), body.get('action'), body.get('entity'), filters=body.get('filter'), exclude=body.get('exclude'), confirm=body.get('confirm'), entities=body.get('entities'))})
            elif self.path == '/api/audit/answer':
                self.respond({'undo': self.server.store.answer_audit(body.get('yes') or [], body.get('no') or [])})
            elif self.path == '/api/ask/answer':
                self.respond({'undo': self.server.store.answer(body.get('entity'), body.get('yes') or [], body.get('no') or [], body.get('kind', 'seed'), body.get('switch'), body.get('later') or [])})
            elif self.path == '/api/identity/decision':
                self.respond({'undo': self.server.store.decide_identity(a=body.get('a'), b=body.get('b'), kind=body.get('kind'), relation_id=body.get('relation_id'), status=body.get('status'), reason=body.get('reason'), evidence=body.get('evidence'))})
            elif self.path == '/api/identity/skip':
                self.respond({'undo': self.server.store.skip_identity_pair(body.get('a'), body.get('b'))})
            elif self.path == '/api/identity/uncertain':
                self.respond({'undo': self.server.store.uncertain_identity(body.get('ids'), body.get('options'), body.get('reason', '人工暂不判断'))})
            elif self.path == '/api/correct':
                self.respond({'undo': self.server.store.correct(body.get('id'), body.get('old'), body.get('new'))})
            elif self.path == '/api/reassign':
                self.respond({'undo': self.server.store.reassign(body.get('ids'), body.get('old'), body.get('new'), filters=body.get('filter'), exclude=body.get('exclude'), confirm=body.get('confirm'))})
            elif self.path == '/api/sweep':
                self.respond({'undo': self.server.store.sweep(body.get('entity'))})
            elif self.path == '/api/libraries/open':
                self.respond(switch_library(self.server, body.get('path')))
            elif self.path.startswith('/api/libraries/'):
                self.respond(library_action(self.server, self.path.rsplit('/', 1)[1], body))
            elif self.path == '/api/recognition/import':
                from web.results import import_package, resume
                if not self.server.library:
                    raise Invalid('识别结果导入需要图库包')
                if body.get('resume') is True:
                    self.respond(resume(self.server.library, self.server.store))
                else:
                    if not isinstance(body.get('path'), str):
                        raise Invalid('需要结果包目录 path')
                    self.respond(import_package(self.server.library, body['path'], self.server.store))
            elif self.path == '/api/provenance':
                self.respond(provenance_action(self.server, body))
            elif self.path == '/api/artist/verdict':
                ids = body.get('ids')
                self.respond({'undo': self.server.store.judge_artist(ids, body.get('artist'), body.get('verdict'), body.get('name', ''))})
            elif self.path.startswith('/api/accounts/'):
                self.respond(account_action(self.server, self.path[len('/api/accounts/'):], body))
            elif self.path == '/api/undo':
                self.respond({'undo': self.server.store.undo(body.get('id'))})
            elif self.path == '/api/oc/shared':
                self.respond(self.server.store.shared_oc())
            elif self.path == '/api/entities':
                if body.get('tag'):
                    created = self.server.store.create_linked(body['tag'], body.get('typed', ''), self.server.names.copyright_of)
                    self.fetch_names(created['id'])
                    self.respond(created)
                else:
                    self.respond(self.server.store.create_entity(body.get('name'), body.get('kind', ''), body.get('artist', '')))
            elif self.path == '/api/character/profile':
                fields = {k: body[k] for k in ('name', 'aliases', 'work', 'tag') if k in body}
                result = self.server.store.set_profile(body.get('entity'), copyright_of=self.server.names.copyright_of, **fields)
                self.fetch_names(result['id'])
                self.respond(result)
            elif self.path == '/api/local-links':
                result = self.server.store.link_local(body.get('items'), self.server.names.copyright_of)
                self.fetch_names(None)
                self.respond(result)
            elif self.path == '/api/works':
                self.respond(self.server.store.edit_work(body.get('action'), body.get('id'), body.get('name'), body.get('parent'), body.get('tag')))
            elif self.path == '/api/character-work':
                self.respond(self.server.store.set_work(body.get('entity'), body.get('work')))
            elif self.path == '/api/character/mascot':
                self.respond(self.server.store.set_mascot(body.get('entity'), body.get('mascot')))
            elif self.path == '/api/kind':
                self.respond(self.server.store.set_kind(body.get('entity'), body.get('kind', ''), body.get('artist', '')))
            elif self.path == '/api/tag':
                self.respond({'undo': self.server.store.tag(body.get('ids'), body.get('tag'), body.get('add', True) is not False, filters=body.get('filter'), exclude=body.get('exclude'), confirm=body.get('confirm'))})
            elif self.path == '/api/sync':
                if not self.server.syncer:
                    raise Invalid('本次启动关闭了同步（--no-sync）')
                self.respond({'started': self.server.syncer.start()})
            elif self.path == '/api/names':
                updater, action = self.server.names, body.get('action')
                if action == 'refresh':
                    updater.start(entity=body.get('entity'), refresh=True)
                elif action == 'model_api':
                    updater.configure(body['key'] if 'key' in body else ..., body.get('model'), body.get('base'))
                elif action == 'model_api_test':
                    self.respond(updater.test())
                    return
                else:
                    raise Invalid('未知显示名操作')
                self.respond(updater.info())
            elif self.path == '/api/index':
                indexer, action = self.server.indexer, body.get('action')
                if not indexer:
                    raise Invalid('本次启动关闭了角色索引更新')
                if action == 'check':
                    indexer.start_check()
                elif action == 'apply':
                    indexer.start_apply(body.get('tags'))
                elif action == 'drop':
                    indexer.start_drop(body.get('char'), body.get('reason'))
                elif action == 'watch':
                    indexer.set_watch(body.get('works'), body.get('min_posts'))
                else:
                    raise Invalid('未知操作')
                self.respond(indexer.info())
            elif self.path == '/api/fold':
                self.respond({'kinds': self.server.store.set_fold_kinds(body.get('kinds'))})
            elif self.path == '/api/startup':
                startup = self.server.store.set_startup(body)
                if self.server.syncer:
                    self.server.syncer.local_limit = startup['local_limit']
                self.respond(settings_info(self.server))
            elif self.path == '/api/cache':
                if body.get('action') != 'clear':
                    raise Invalid('未知操作')
                removed = 0
                with self.server.image_slots:
                    for f in Path(self.server.cache).glob('*.jpg'):
                        f.unlink(missing_ok=True)
                        removed += 1
                self.respond({'removed': removed, **settings_info(self.server)})
            elif self.path == '/api/similar':
                if not self.server.similar:
                    raise Invalid('本次启动关闭了相似检测')
                self.server.similar.poke()
                self.respond({'started': True})
            elif self.path == '/api/relate':
                self.respond(self.relate(body.get('ids')))
            elif self.path == '/api/backup':
                self.respond({'path': str(self.server.store.backup())})
            elif self.path == '/api/regroup':
                if not self.server.learner:
                    raise Invalid('学习模块未启动')
                self.server.learner.poke(regroup=True)
                self.respond({'started': True})
            elif self.path == '/api/merge':
                self.respond({'undo': self.server.store.merge(body.get('source'), body.get('target'))})
            elif self.path == '/api/curate':
                self.respond({'undo': self.server.store.curate(body.get('ids'), body.get('action'), filters=body.get('filter'), exclude=body.get('exclude'), confirm=body.get('confirm'))})
            elif self.path == '/api/prefs':
                self.respond(self.server.store.set_pins(body.get('pins')))
            elif self.path == '/api/suggest':
                self.respond(self.server.store.suggest(body.get('ids'), body.get('filter'), body.get('exclude')))
            else:
                self.respond({'error': '接口不存在'}, 404)
                return
        except (Invalid, ValueError, TypeError) as exc:
            self.respond({'error': str(exc)}, 400)
        except FetchError as exc:
            self.respond({'error': f'Danbooru 暂时连不上（{exc}），稍后再试'}, 502)
        except Exception as exc:
            print(f'POST failed: {type(exc).__name__}', file=sys.stderr)
            self.respond({'error': '保存失败，未提交的操作已回滚；请查看启动终端'}, 500)

    def relate(self, ids):
        """How the selected images relate: same picture, part of another, variant, edited, unrelated (web/relate.py)."""
        if not isinstance(ids, list) or not 2 <= len(set(ids)) <= relate.MAX_IMAGES or not all(isinstance(i, str) for i in ids):
            raise Invalid(f'请选择 2–{relate.MAX_IMAGES} 张图')
        images, missing = {}, []
        for ident in dict.fromkeys(ids):
            source = self.server.store.image_path(ident)
            try:
                if not source:
                    raise OSError
                with self.server.image_slots:
                    images[ident] = relate.load(source)
            except (OSError, UnidentifiedImageError, Image.DecompressionBombError):
                missing.append(ident)
        if len(images) < 2:
            raise Invalid('能读到的图不足两张：原图可能还在 iCloud')
        return relate.relate_all(images) | {'missing': missing}

    def image(self, ident, size):
        if size not in SIZES:
            raise Invalid('图片尺寸不正确')
        source = self.server.store.image_path(ident)
        if not source:
            self.respond({'error': '图片不存在'}, 404)
            return
        try:
            stat = source.stat()
            key = hashlib.sha256(f'{ident}:{stat.st_mtime_ns}:{stat.st_size}:{size}'.encode()).hexdigest()
            target = self.server.cache / (key + '.jpg')
            try:
                data = target.read_bytes()  # Cache hits never wait for image decoders.
            except FileNotFoundError:
                with self.server.image_lock(key):
                    try:
                        data = target.read_bytes()
                    except FileNotFoundError:
                        with self.server.image_slots:
                            with Image.open(source) as im:
                                im.draft('RGB', (SIZES[size], SIZES[size]))
                                im = ImageOps.exif_transpose(im)
                                im.thumbnail((SIZES[size], SIZES[size]))
                                if 'A' in im.getbands() or im.mode == 'P':
                                    rgba = im.convert('RGBA')
                                    flat = Image.new('RGB', im.size, '#f4f4f2')
                                    flat.paste(rgba, mask=rgba.getchannel('A'))
                                    im = flat
                                else:
                                    im = im.convert('RGB')
                                temp = target.with_suffix('.' + secrets.token_hex(6) + '.tmp')
                                try:
                                    im.save(temp, format='JPEG', quality=84)
                                    data = temp.read_bytes()
                                    os.replace(temp, target)
                                finally:
                                    temp.unlink(missing_ok=True)
            self.respond(data, kind='image/jpeg', cache=True)
        except (OSError, UnidentifiedImageError, Image.DecompressionBombError):
            self.respond({'error': '原图暂不可读，请检查 Photos 下载状态与访问权限'}, 404)


def build_id():
    """Short commit of the running code, when it runs from a git checkout."""
    try:
        out = subprocess.run(['git', '-C', str(ROOT), 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True, timeout=3)
        return out.stdout.strip() if out.returncode == 0 else ''
    except (OSError, subprocess.SubprocessError):
        return ''


def about_info(server):
    """Version, build and environment for the About page and bug reports. No personal data."""
    libraries = {}
    for label, module in (('Pillow', 'PIL'), ('NumPy', 'numpy'), ('ONNX Runtime', 'onnxruntime')):
        try:
            libraries[label] = __import__(module).__version__
        except Exception:
            libraries[label] = None
    if server.build is None:
        server.build = build_id()
    return {'name': web.NAME, 'version': web.__version__, 'channel': web.CHANNEL, 'build': server.build,
            'schema': SCHEMA_VERSION, 'python': platform.python_version(),
            'platform': f'{platform.system()} {platform.release()} ({platform.machine()})', 'libraries': libraries}


def settings_info(server):
    """Where the data lives and how big it is, the manual backups, and what runs in the background."""
    store, cache = server.store, server.cache
    size = lambda p: sum(f.stat().st_size for f in [p, Path(str(p) + '-wal')] if f.exists())
    thumbs = [f.stat().st_size for f in Path(cache).iterdir() if f.suffix == '.jpg'] if Path(cache).exists() else []
    folder = backups_dir(store.path)
    backups = sorted(folder.glob('library-*.sqlite'), reverse=True) if folder.exists() else []
    lib = getattr(server, 'library', None)
    return {'package': lib.info() if lib else None,
            'source': {'path': str(store.source), 'bytes': size(store.source)},
            'library': {'path': str(store.path), 'bytes': size(store.path)},
            'thumbnails': {'path': str(cache), 'bytes': sum(thumbs), 'files': len(thumbs)},
            'backups': [{'name': b.name, 'bytes': b.stat().st_size, 'time': b.stat().st_mtime} for b in backups[:20]],
            'backup_count': len(backups),
            'startup': store.startup(), 'forced_off': sorted(server.forced_off),
            'tasks': {'similar': server.similar.state if server.similar else {'status': 'off'}}}


def libraries_info(server):
    """The open library and the recently opened ones (the sidebar switcher)."""
    lib = getattr(server, 'library', None)
    items = []
    for item in library.recent():
        try:
            config = library.open_library(item['path']).config
        except library.LibraryError:
            continue
        items.append({'path': item['path'], 'name': config['name'], 'test': (config.get('test') or {}).get('persona')})
    return {'current': str(lib.path) if lib else None, 'info': lib.info() | {'test': (lib.config.get('test') or {}).get('persona')} if lib else None,
            'libraries': items, 'export': server.export_state, 'home': str(library.HOME)}


CHOOSE = {  # macOS native pickers, run by this local server; the browser cannot hand over a folder's path
    'folder': 'POSIX path of (choose folder with prompt "选择图片文件夹")',
    'library': 'POSIX path of (choose folder with prompt "选择图库（.pilib）")',
    'zip': 'POSIX path of (choose file with prompt "选择导出的图库（.zip）" of type {"zip", "public.zip-archive"})',
    'archive': 'POSIX path of (choose file with prompt "选择 X 数据归档（.zip）" of type {"zip", "public.zip-archive"})',
}


def choose(kind):
    """A native file dialog; None when cancelled. Blocks this request until the person answers."""
    if kind not in CHOOSE:
        raise Invalid('未知的选择类型')
    if sys.platform != 'darwin':
        raise Invalid('这台电脑上请直接填写路径')
    done = subprocess.run(['osascript', '-e', CHOOSE[kind]], capture_output=True, text=True, timeout=600)
    if done.returncode:
        if '-128' in done.stderr:
            return None
        raise Invalid('无法打开选择窗口：' + done.stderr.strip()[-120:])
    return done.stdout.strip().rstrip('/') or None


def library_action(server, action, body):
    """The settings page's library management (docs/LIBRARIES.md L3). Opening a library goes through switch_library."""
    try:
        if action == 'choose':
            path = choose(body.get('kind'))
            out = {'path': path}
            if path and body.get('kind') == 'folder':
                out.update(preset=library.guess_preset(path), name=Path(path).name)
            return out
        if action == 'new':
            name = str(body.get('name') or '').strip() or '新图库'
            dest = library.unique_path(library.HOME, name)
            if body.get('kind') == 'folder':
                lib = library.new_folder_library(body.get('folder') or '', dest, name, body.get('preset') or None)
            elif body.get('kind') == 'photos':
                lib = library.new_photos_library(dest, name)
            else:
                raise Invalid('请选择来源')
            library.remember(lib, front=False)
            return {'path': str(lib.path), 'name': lib.name}
        if action == 'preset':
            if not server.library:
                raise Invalid('当前不是图库包')
            return library.set_preset(server.library, body.get('source'), body.get('preset'))
        if action == 'forget':
            if server.library and Path(str(body.get('path'))) == server.library.path:
                raise Invalid('不能移出当前打开的图库')
            library.forget(body.get('path'))
            return {'ok': True}
        if action == 'import':
            lib = library.import_library(body.get('path') or '')
            library.remember(lib, front=False)
            return {'path': str(lib.path), 'name': lib.name}
        if action == 'export':
            return start_export(server)
    except (library.LibraryError, OSError, ValueError) as exc:
        raise Invalid(str(exc)) from exc
    raise Invalid('未知操作')


def start_export(server):
    """Export the open library to ~/Downloads in the background; the state shows on the library card."""
    lib = server.library
    if not lib:
        raise Invalid('当前不是图库包')
    if server.export_state.get('status') == 'running':
        raise Invalid('正在导出')
    import datetime as _dt
    dest = Path.home() / 'Downloads' / f"{lib.name}-{_dt.date.today():%Y%m%d}.pilib.zip"
    server.export_state = {'status': 'running', 'path': str(dest)}
    def run():
        try:
            out = library.export_library(lib, dest)
            server.export_state = {'status': 'done', 'path': str(out), 'bytes': out.stat().st_size}
        except Exception as exc:
            server.export_state = {'status': 'error', 'path': str(dest), 'error': f'{type(exc).__name__}: {exc}'[:200]}
    threading.Thread(target=run, daemon=True).start()
    return server.export_state


def accounts_info(server):
    """Bound accounts (the whole app) and the open library's X archive import (settings page, 账号)."""
    from provenance import accounts, desktop, db as pdb
    out = accounts.status()
    out['desktop'] = desktop.available()   # started by the desktop app: Pixiv is bound by logging in in its window
    if out['desktop'] and out['pixiv'].get('expired'):
        try:   # local cookie read, no request: tells the page the person has logged in again in the window
            out['pixiv']['window_logged_in'] = bool(desktop.session().get('loggedIn'))
        except ConnectionError:
            pass
    out['x'] = {'imported': 0}
    lib = server.library
    if lib and pdb.path_of(lib).exists():
        with pdb.connect(lib) as c:
            n = dict(c.execute("SELECT status,count(*) FROM leads WHERE source='x_following' GROUP BY status").fetchall())
            when = c.execute("SELECT max(added_at) FROM leads WHERE source='x_following'").fetchone()[0]
        out['x'] = {'imported': sum(n.values()), 'resolved': n.get('resolved', 0), 'pending': n.get('pending', 0), 'imported_at': when}
    out['library'] = lib.name if lib else None
    return out


def account_action(server, action, body):
    from provenance import accounts, desktop
    from provenance.net import Net, Stopped
    from provenance.sources import x_archive
    try:
        if action == 'pixiv':      # pasted PHPSESSID: the development entry of the plain web page
            return accounts.bind_pixiv(body.get('value'), Net(5))
        if action == 'pixiv/login':    # bring up the desktop app's Pixiv window; the page then polls pixiv/connect
            desktop.login(force=bool(accounts.status()['pixiv'].get('expired')))
            return {'ok': True}
        if action == 'pixiv/connect':
            who = accounts.connect_pixiv(accounts.make_net(5, app=True))
            return {'bound': bool(who), **(who or {}), 'login_open': bool(desktop.session().get('loginOpen'))}
        if action == 'pixiv/check':
            ok = accounts.recheck_pixiv(accounts.make_net(5))
            if ok is None:
                raise Invalid('还没有绑定 Pixiv')
            return {'ok': ok, **accounts.status()['pixiv']}
        if action == 'saucenao':
            return accounts.bind_saucenao(body.get('value'))
        if action in ('pixiv/unbind', 'saucenao/unbind'):
            accounts.remove(action.split('/')[0])
            return {'ok': True}
        if action == 'x/import':
            if not server.library:
                raise Invalid('当前不是图库包')
            path = body.get('path') or choose('archive')
            if not path:
                return {'cancelled': True}
            return {'new': x_archive.import_archive(server.library, path), 'path': path}
    except (accounts.AccountError, Stopped, ValueError, OSError, ConnectionError) as exc:
        raise Invalid(str(exc)) from exc
    raise Invalid('未知操作')


def upgrade_terms(store):
    """A library the terms core already holds (any part switched) gets the core's schema upgrade at every start: its
    commands refuse a library on an older schema, and the switches that would otherwise run the migration are done.
    Without the core program the library stays as it is; writes to the switched parts then report the missing program."""
    from web import terms_core
    with store.connect() as c:
        held = c.execute("SELECT 1 FROM meta WHERE key='terms_schema'").fetchone()
    if not held or terms_core.binary() is None:
        return None
    try:
        return terms_core.run('migrate', '--db', store.path)
    except (terms_core.Failed, RuntimeError) as exc:
        print(f'词条核心的表升级失败：{exc}', flush=True)
        return None


def open_store(source, db, build_general=True):
    """Startup: the Python schema, then the terms core's. An existing library gets the core's upgrade before initialize,
    which already writes through the core (album-title aliases once names are the core's) and the core refuses a
    library on an older terms schema; a new one is born in the core during initialize."""
    store = Store(source, db, initialize=False)
    if Path(db).exists():
        upgrade_terms(store)
    store.initialize(build_general=build_general)
    upgrade_terms(store)
    return store


# POST endpoints that do not change the open library: switching or managing libraries, accounts, the image cache, a
# backup, suggestions (a read). Everything else is refused while the library is read-only (switch_failed).
READ_ONLY_ALLOWED = ('/api/libraries/', '/api/accounts/', '/api/cache', '/api/backup', '/api/suggest')


def adopt_artists(store, lib):
    """docs/TERMS_ARTISTS.md: hand this library's artists to the terms core, once, after a backup of the human
    records. Without the core program (or if the hand-over fails) the library keeps using the old tables."""
    from web import terms_core
    from provenance import db as pdb
    with store.connect() as c:
        if terms_core.artists_in_core(c):
            return True
    if terms_core.binary() is None:
        return switch_failed(store, '找不到词条核心程序')
    print(f'画师切到词条核心前已备份：{store.backup()}', flush=True)
    try:
        result = terms_core.cutover_artists(store.path, pdb.path_of(lib))
    except (terms_core.Failed, RuntimeError) as exc:
        return switch_failed(store, f'画师切到词条核心失败：{exc}')
    report = result.get('report') or {}
    print(f"画师已切到词条核心：{report.get('terms_created', 0)} 位画师、{report.get('evidence_added', 0)} 条证据、"
          f"{report.get('verdicts_added', 0)} 条人工判断", flush=True)
    return True


def adopt_labels(store):
    """docs/TERMS_LABELS.md: hand user tags, ratings and WD topics to the terms core, once. The first delivery of
    every picture's WD results takes minutes, so it runs in the background while the app keeps using the old tables;
    pictures synced meanwhile are delivered under the store's labels lock, right before the switch. Without the core
    program, or if anything fails, the old tables stay."""
    from web import terms_core
    import time
    with store.connect() as c:
        if terms_core.labels_in_core(c):
            return True
    if terms_core.binary() is None:
        return switch_failed(store, '找不到词条核心程序')
    try:
        started = time.monotonic()
        terms_core.run('migrate', '--db', store.path)
        print('正在把 WD 分级和题材交给词条核心（第一次要几分钟，完成前只读）…', flush=True)
        store._deliver_wd(store.library_uuids())
        with store._labels_lock:
            store._deliver_wd(store.wd_undelivered())
            print(f'标签切到词条核心前已备份：{store.backup()}', flush=True)
            report = terms_core.cutover_labels(store.path).get('report') or {}
    except (terms_core.Failed, Invalid, RuntimeError) as exc:
        return switch_failed(store, f'标签和分级切到词条核心失败：{exc}')
    print(f"标签和分级已切到词条核心：{report.get('rated_assets', 0)} 张图，{report.get('verdicts_added', 0)} 条用户标签记录，"
          f'用时 {time.monotonic() - started:.0f} 秒', flush=True)
    return True


def adopt_characters(store):
    """docs/TERMS_CHARACTERS_PLAN.md step S4: hand characters, skins and works to the terms core, once, after labels.
    The switch holds the store's write lock (writes wait, reads keep reading the old tables) and is one core
    transaction: a last synchronization that must match the old tables exactly, then write protection. A backup is
    taken first; rolling back means restoring it, losing what was done after the switch. If anything fails, the old
    tables stay and the next start tries again."""
    from web import terms_core
    import time
    with store.connect() as c:
        if terms_core.characters_in_core(c):
            return True
        if not terms_core.labels_in_core(c):
            return False
    if terms_core.binary() is None:
        return switch_failed(store, '找不到词条核心程序')
    try:
        started = time.monotonic()
        with store._labels_lock:
            print(f'角色切到词条核心前已备份：{store.backup()}', flush=True)
            report = terms_core.cutover_characters(store.path).get('report') or {}
    except (terms_core.Failed, Invalid, RuntimeError) as exc:
        return switch_failed(store, f'角色切到词条核心失败：{exc}')
    print(f"角色已切到词条核心：{report.get('terms_created', 0)} 个身份、{report.get('verdicts_added', 0)} 条人工结论与决议，"
          f'用时 {time.monotonic() - started:.0f} 秒', flush=True)
    return True


def adopt_names(store):
    """docs/TERMS_NAMES_PLAN.md: hand the name tables and the display rules to the terms core, once, after characters
    (names hang on their terms). One core transaction under the store's write lock; a backup first. If anything fails,
    the old tables stay and the next start tries again."""
    from web import terms_core
    import time
    with store.connect() as c:
        if terms_core.names_in_core(c):
            return True
        if not terms_core.characters_in_core(c):
            return False
    if terms_core.binary() is None:
        return switch_failed(store, '找不到词条核心程序')
    try:
        started = time.monotonic()
        with store._labels_lock:
            print(f'名字切到词条核心前已备份：{store.backup()}', flush=True)
            terms_core.cutover_names(store.path)
    except (terms_core.Failed, Invalid, RuntimeError) as exc:
        return switch_failed(store, f'名字切到词条核心失败：{exc}')
    print(f'名字已切到词条核心，用时 {time.monotonic() - started:.0f} 秒', flush=True)
    return True


def switch_failed(store, reason):
    """A library that cannot be switched to the terms core opens read-only (user's decision 2026-09-30): its old tables
    are not written any more. The backup taken before the switch stays; the next start tries the switch again."""
    message = f'{reason}。图库已只读打开，这次启动不能修改；切换前的备份在 library-backups/，下次启动会再试一次切换'
    print(message, flush=True)
    store.refuse_writes(message)
    return False


def adopt_terms(store, on_ready=None):
    """Labels first, then characters (the character import relies on labels being the core's), then names."""
    if store.read_only:
        return
    if adopt_labels(store) and adopt_characters(store) and adopt_names(store):
        if on_ready is not None:
            on_ready()


def provenance_info(server):
    """找回画师 on the settings page: the current run, the latest runs and their coverage, whether it runs after a sync."""
    runner = server.provenance
    if not runner:
        return {'available': False}
    summary = artists.runs(server.library) or {'runs': [], 'coverage': None}
    state = runner.status()
    if state.get('status') not in ('running', 'stopping'):   # a run cut off (app quit mid-run) stays 'running' in its row
        for r in summary['runs']:
            if r['status'] == 'running':
                r['status'], r['reason'] = 'stopped', r['reason'] or '中途退出'
    return {'available': True, 'state': state, **summary, 'auto': server.store.startup()['provenance'],
            'budgets': {k: v[1] for k, v in PROVENANCE_KINDS.items()}, 'frozen': server.library.frozen,
            'auto_network': runner.allow_network is True,   # automatic runs reach outside only with --allow-provenance-network
            'delivery': runner.delivery}                    # the last hand-over to the terms core, None before the first


def provenance_action(server, body):
    if not server.provenance:
        raise Invalid('当前不是图库包，不能找回画师')
    action = body.get('action')
    if action == 'start':
        if not server.provenance.start('full', allow_network=body.get('allow_network') is True):
            raise Invalid('正在查找，稍等')
    elif action == 'stop':
        server.provenance.stop()
    elif action in ('lookup', 'artist'):
        # 按需查找 from the library: the click authorises this one run; a background run makes way and comes back later.
        if action == 'lookup':
            ids = body.get('ids')
            if not isinstance(ids, list) or not ids or len(ids) > 500 or not all(isinstance(x, str) and 0 < len(x) <= 64 for x in ids):
                raise Invalid('要查找的图片不对（一次最多 500 张）')
            narrow = {'only': ids}
        else:
            artist = body.get('artist')
            if not isinstance(artist, str) or not 0 < len(artist) <= 64:
                raise Invalid('画师不对')
            narrow = {'artists': [artist]}
        if not server.provenance.preempt():
            raise Invalid('后台查找正在停下，稍等再试')
        if not server.provenance.start('pick' if action == 'lookup' else 'artist', allow_network=True, **narrow):
            server.provenance.holding = False   # let the run that made way come back
            raise Invalid('正在查找，稍等')
    else:
        raise Invalid('未知操作')
    return provenance_info(server)


def switch_library(server, path):
    """Open another library: remember it, answer, then restart this process on it (docs/LIBRARIES.md section 6).
    Refused while a sync is writing, so a restart never cuts an import in half."""
    if not isinstance(path, str) or not library.is_library(path):
        raise Invalid('不是图库包')
    target = library.open_library(path)
    if server.syncer and server.syncer.status().get('status') in ('scanning', 'recognising', 'importing'):
        raise Invalid('正在同步，稍后再切换图库')
    library.remember(target)
    threading.Timer(0.3, restart_on, (server, target.path)).start()
    return {'path': str(target.path), 'name': target.name}


def restart_argv(argv, path):
    """This launch's arguments pointed at another library: drop the old library choice and one-shot flags."""
    out, skip = [], {'--library', '--source', '--db'}
    rest = iter(argv)
    for a in rest:
        if a in skip:
            next(rest, None)
        elif a.split('=')[0] in skip or a in ('--refresh', '--backup'):
            continue
        else:
            out.append(a)
    if '--no-open' not in out:
        out.append('--no-open')
    return [*out, '--library', str(path)]


def restart_on(server, path):
    """Replace this process with a fresh server on `path`; same pid and port, all background threads gone."""
    invalidate_http_library()
    server.server_close()
    if server.provenance:
        server.provenance.close()   # resumable: the next run picks up where this one stopped
    os.execv(sys.executable, [sys.executable, '-m', 'tests.reference.web.server', *restart_argv(sys.argv[1:], path)])


def merge_scans(results):
    out = {'new': 0, 'returned': 0, 'gone': 0, 'sources': {}}
    for r in results:
        for k in ('new', 'returned', 'gone'):
            out[k] += r[k]
        out['sources'].update(r['sources'])
    return out


def resolve_library(args):
    """Which library to open: --library, else the old --source/--db pair, else the last one opened,
    else the old review/ layout (whose pi.db becomes a symlink into the package once adopted)."""
    if args.library:
        return library.open_library(args.library)
    if args.source or args.db:
        args.source = args.source or ROOT / 'review' / 'pi.db'
        args.db = args.db or ROOT / 'review' / 'library.sqlite'
        return None
    if (last := library.last()):
        return library.open_library(last)
    legacy = ROOT / 'review' / 'pi.db'
    if legacy.is_symlink() and library.is_library(legacy.resolve().parent):
        return library.open_library(legacy.resolve().parent)
    args.source, args.db = legacy, ROOT / 'review' / 'library.sqlite'
    return None


def follow_desktop_app(parent):
    """Started by the desktop app (desktop/): when it is gone, even force-quit, close as with Ctrl+C instead of lingering."""
    import signal
    import time
    while os.getppid() == parent:
        time.sleep(1)
    os.kill(os.getpid(), signal.SIGINT)


def invalidate_http_library():
    """The front door must not retain a DB across an exec, failed start, or library switch."""
    if state := os.environ.get('ILLUSTASH_HTTP_STATE'):
        Path(state).unlink(missing_ok=True)


def publish_http_library(server):
    """Private, atomic per-process readiness; no token or library contents in HTTP response headers."""
    if state := os.environ.get('ILLUSTASH_HTTP_STATE'):
        target = Path(state)
        temp = target.with_suffix('.new')
        fd = os.open(temp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, 'w') as stream:
            json.dump({'db': str(server.store.path.resolve()), 'generation': secrets.token_hex(16),
                       'library': str(server.library.path) if server.library else None,
                       'tags': str(ROOT / 'tags' / 'tags_v3.csv'),
                       'source': str(server.store.source.resolve()), 'token': server.token,
                       'read_only': server.store.read_only}, stream)
        os.replace(temp, target)


def main():
    invalidate_http_library()
    parser = argparse.ArgumentParser()
    parser.add_argument('--port', type=int, default=8765)
    parser.add_argument('--library', type=Path, help='图库包（.pilib）；不给时打开上次打开的图库')
    parser.add_argument('--source', type=Path, help='（旧用法）源库路径；和 --db 一起代替 --library')
    parser.add_argument('--db', type=Path, help='（旧用法）应用库路径')
    parser.add_argument('--refresh', action='store_true', help='重新读取源库；保留全部应用内人工结论')
    parser.add_argument('--no-open', action='store_true')
    parser.add_argument('--backup', action='store_true', help='一致性备份应用人工知识后退出')
    parser.add_argument('--photos', type=Path, help='Apple Photos 图库（只读）；默认取图库包里的来源')
    parser.add_argument('--allow-provenance-network', action='store_true', help='明确授权本次进程内的自动溯源外部请求')
    parser.add_argument('--no-names', action='store_true', help='关闭显示名后台拉取')
    parser.add_argument('--no-sync', action='store_true', help='启动时不与 Photos 同步')
    parser.add_argument('--no-similar', action='store_true', help='启动时不在后台找相似的图')
    parser.add_argument('--no-index-check', action='store_true', help='启动时不自动检查外部角色索引有没有新角色')
    parser.add_argument('--sync-limit', type=int, default=0, help='每次同步最多识别几张（试跑用，0 为不限）')
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error('port 必须在 1–65535 之间')
    try:
        lib = resolve_library(args)
    except library.LibraryError as exc:
        parser.error(str(exc))
    if lib:
        args.source, args.db, thumbnails = lib.source, lib.db, lib.thumbnails
        library.upgrade_source(lib)
        args.photos = args.photos or lib.photos() or PHOTOS
    else:
        thumbnails = args.db.parent / 'web-thumbnails'
        args.photos = args.photos or PHOTOS
    if args.backup:
        if not args.db.exists():
            parser.error('应用数据库尚未建立')
        print(Store.open(args.source, args.db).backup())
        return
    # Fail early on an occupied port. Never stop or reuse another process.
    with socket.socket() as probe:
        probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            probe.bind(('127.0.0.1', args.port))
        except OSError as exc:
            if exc.errno == 48 or exc.errno == 98:
                parser.error(f'端口 {args.port} 已占用；使用 --port 指定其他端口')
            parser.error(f'无法监听本机端口：{exc.strerror}；请在本机终端启动或允许本地网络监听')
    store = open_store(args.source, args.db, build_general=not args.refresh)
    refresh_needed = args.refresh or not store.ready()
    refresh_deferred = refresh_needed and store.write_refusal() is not None
    if refresh_needed and not refresh_deferred:
        if store.ready():
            print(f'人工知识已备份：{store.backup()}', flush=True)
        print('正在建立图库读索引；原始数据库只读，人工记录独立保存…', flush=True)
        print(f'索引完成：{store.refresh()} 秒', flush=True)
    if store.write_refusal() is None and (drawn := store.make_audit()):
        print(f'随机抽查：已抽出 {drawn} 张', flush=True)
    if lib:
        adopt_artists(store, lib)
        store.artist_assets = lambda artist: artists.assets_of(lib, artist, rejected=store.artist_verdicts())
    server = Server(('127.0.0.1', args.port), store, thumbnails,
                    public_origin=os.environ.get('ILLUSTASH_HTTP_ORIGIN'))
    server.library = lib
    if lib:
        server.provenance = ProvenanceRunner(lib, allow_network=args.allow_provenance_network)
    if lib:
        library.remember(lib)
    # Settings page switches, overridden by --no-* flags for this launch only.
    startup = store.startup()
    if lib and lib.frozen:
        args.no_sync = args.no_index_check = True   # a test library stays the snapshot it was cut as
    server.forced_off = {k for k, flag in (('sync', args.no_sync), ('similar', args.no_similar),
                         ('index_check', args.no_index_check), ('names', args.no_names)) if flag}
    args.no_sync, args.no_similar = args.no_sync or not startup['sync'], args.no_similar or not startup['similar']
    args.no_index_check, args.no_names = args.no_index_check or not startup['index_check'], args.no_names or not startup['names']
    pending_starts = []
    def start_when_ready(call):
        if store.write_refusal() is None:
            call()
        else:
            pending_starts.append(call)

    server.names = NameUpdater(store, enabled=not args.no_names)
    if not args.no_names:
        start_when_ready(server.names.start)
    server.learner = Learner(store, Features(args.source))
    start_when_ready(server.learner.start)
    def index_changed():
        server.learner.reload()
        if not args.no_names:
            start_when_ready(server.names.start)
    server.indexer = IndexUpdater(store, on_change=index_changed,
                                  busy=lambda: bool(server.syncer) and server.syncer.status().get('status') in ('scanning', 'recognising', 'importing'))
    if not args.no_similar:
        server.similar = SimilarScanner(store, server.learner.features, server.image_slots)
        start_when_ready(server.similar.start)
    def imported():
        if not args.no_names:
            start_when_ready(server.names.start)
    if not args.no_sync:
        sources = lib.config['sources'] if lib else []
        photos_source = next((s for s in sources if s.get('kind') == 'photos'), None)
        folders = [s for s in sources if s.get('kind') == 'folder']
        scanner = (lambda: merge_scans([folder.scan(s, store.source) for s in folders])) if folders and not photos_source else None
        server.syncer = Syncer(store, args.photos, local_limit=startup['local_limit'], per_run=args.sync_limit, on_import=imported,
                               source=photos_source, scanner=scanner)
        server.syncer.blocked = server.indexer.writing
        start_when_ready(server.syncer.start)
    if not args.no_index_check:
        start_when_ready(server.indexer.maybe_check)
    if server.provenance:
        start_when_ready(server.provenance.deliver)      # results of runs that ended while the core was not taking them yet
        start_when_ready(server.provenance.resume_pending)
    from web import effects
    start_when_ready(lambda: effects.drain(store, server._after_operation))
    publish_http_library(server)
    print(f'图库已启动：{server.origin}\n按 Ctrl+C 停止。', flush=True)
    def switch_ready():
        if refresh_deferred:
            store.refresh()
        store.album_titles = store._album_titles(add_aliases=True)
        store.make_audit()
        for call in pending_starts:
            call()
        pending_starts.clear()
    threading.Thread(target=adopt_terms, args=(store, switch_ready), daemon=True, name='adopt-terms').start()
    if not args.no_open:
        webbrowser.open(server.origin)
    if os.environ.get('ILLUSTASH_DESKTOP_BRIDGE') and os.name != 'nt':
        import signal   # the app stops us with SIGINT; a launcher may have left it ignored (background jobs do)
        signal.signal(signal.SIGINT, signal.default_int_handler)
        threading.Thread(target=follow_desktop_app, args=(os.getppid(),), daemon=True).start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        invalidate_http_library()
        if server.provenance:
            server.provenance.close()
        server.server_close()


if __name__ == '__main__':
    main()
