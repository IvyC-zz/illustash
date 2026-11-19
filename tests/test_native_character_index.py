"""#607：基础角色索引包随识别模型下载，按图库启用、撤回。只用合成包、合成图库和真实的原生索引计算程序。

合成包与 tools/build_character_index.py 的格式相同，用 ILLUSTASH_TEST_CHARACTER_INDEX 换掉清单里的冻结版本。
"""
from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import time
import unittest

import numpy as np

from tests import library_fixture as library
from tests.native_worker_fixture import command as worker_command
from tests.test_native_human_labels import snapshot
from tools.build_character_index import FORMAT, archive
from tools.wait_limit import WAIT

CCIP = 'c' * 64
IDS = ['base_000', 'base_001', 'base_002']


def synthetic_package():
    rows = [dict(short_tag=cid, tag=f'{cid}_(work)', copyright='work', copyrights=['work'], core_tags=['synthetic_hair'],
                 gender={'boy': 0.0, 'girl': 1.0}, post_count=300 + i, n_ref=5,
                 anchor_p50=0.02, anchor_p75=0.03, anchor_p90=0.05) for i, cid in enumerate(IDS)]
    anchors = np.zeros((len(IDS), 768), '<f4')
    for i in range(len(IDS)):
        anchors[i, i] = 1
    files = {'characters.json': json.dumps(rows).encode(), 'anchors.f32': anchors.tobytes(),
             'LICENSE-Character-Index.txt': b'synthetic', 'NOTICE.txt': b'synthetic'}
    provenance = dict(format=FORMAT, version='character_index@2025-09-25', characters=len(IDS), dimension=768,
                      ccip_sha256=CCIP, files={k: hashlib.sha256(v).hexdigest() for k, v in files.items()})
    files['provenance.json'] = json.dumps(provenance).encode()
    return archive(files)


def vector(axis):
    v = np.zeros(768, '<f4'); v[axis] = 1
    return v.tobytes()


class CharacterIndexTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        from tests.test_native_index import NativeIndexTest
        path = os.environ.get('ILLUSTASH_INDEX_COMPUTE_BIN')
        if not path:
            raise RuntimeError('explicit index computation candidate ILLUSTASH_INDEX_COMPUTE_BIN required')
        cls.binary = Path(path).resolve(strict=True)
        NativeIndexTest.setUpClass()

    def setUp(self):
        from tests.test_native_index import NativeIndexTest
        self.fixture = NativeIndexTest('test_check_watch_apply_drop_formal_projection_history_names')
        self.addCleanup(self.fixture.doCleanups)
        self.fixture.setUp()
        self.addCleanup(self.fixture.tearDown)
        f = self.fixture
        f.stop(f.process)
        self.package = synthetic_package()
        self.models = f.root / 'models'
        self.manifest = f.root / 'character-index-fixture.json'
        self.manifest.write_text(json.dumps(dict(characters=len(IDS), ccip_sha256=CCIP, size=len(self.package),
                                                 sha256=hashlib.sha256(self.package).hexdigest(),
                                                 url='http://127.0.0.1:9/character-index-v1.bin')))
        tags = Path(__file__).resolve().parents[1] / 'tags/tags_v3.csv'
        f.env.update(ILLUSTASH_INDEX_WORKER=str(self.binary), ILLUSTASH_INDEX_TAGS=str(tags),
                     ILLUSTASH_MODELS=str(self.models), ILLUSTASH_TEST_CHARACTER_INDEX=str(self.manifest))
        with closing(sqlite3.connect(f.lib.source)) as c, c:
            for col in ('danbooru_tag TEXT', 'wd_tag TEXT', 'copyright TEXT', 'base_tag TEXT', 'skin TEXT',
                        'post_count INTEGER', 'gender TEXT', 'gender_conf REAL', 'core_tags TEXT', 'n_ref INTEGER',
                        'anchor_p50 REAL', 'anchor_p75 REAL', 'anchor_p90 REAL', 'anchor_row INTEGER',
                        'name_en TEXT', 'name_ja TEXT', 'name_zh TEXT'):
                if col.split()[0] not in [r[1] for r in c.execute('PRAGMA table_info(characters)')]:
                    c.execute('ALTER TABLE characters ADD COLUMN ' + col)
            c.execute('CREATE TABLE IF NOT EXISTS embeddings(uuid TEXT,model TEXT,vec BLOB)')
            for col in ('source', 'model_ver', 'run_id'):
                if col not in [r[1] for r in c.execute('PRAGMA table_info(labels)')]:
                    c.execute('ALTER TABLE labels ADD COLUMN ' + col + ' TEXT')
            # a、b 正好落在包里前两个角色的锚点上，c 不像任何角色。
            c.executemany("INSERT INTO embeddings VALUES (?,'ccip',?)", [('a', vector(0)), ('b', vector(1)), ('c', vector(9))])
        # 已有的两个角色在共享目录里有参考（新用户「检查新角色」补的角色就是这样），重建时留着；参考离包里的锚点和图片都远。
        for cid, axis in [('alice', 5), ('bob', 6)]:
            rows = np.zeros((6, 768), np.float32); rows[:, axis] = 1; rows[:, 7] = np.arange(6) * .002
            np.save(f.shared / 'char-index' / f'{cid}.npy', rows)
            (f.shared / 'char-index' / f'{cid}.json').write_text(json.dumps(dict(short_tag=cid, tag=cid + '_tag', gender={'girl': 1}, post_count=300)))
        self.human = snapshot(f.lib.db)['actions']

    def install(self, data=None):
        path = self.models / 'character-index/character-index-v1.bin'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(self.package if data is None else data)

    def base(self, enable=None):
        f = self.fixture
        r = f.request('/api/index/base', None if enable is None else {'enable': enable})
        return r['status'], json.loads(r['body'])

    def settled(self):
        """等到没有基础索引任务在跑，返回基础索引和索引任务的状态。"""
        deadline = time.monotonic() + WAIT
        while time.monotonic() < deadline:
            status, base = self.base()
            state = self.fixture.index()['state']
            if status == 200 and not base['running'] and state['status'] not in (
                    'checking', 'fetching', 'building', 'importing', 'cleaning', 'scanning', 'cleaning'):
                return base, state
            time.sleep(.05)
        self.fail('基础角色索引任务没有结束')

    def wait_enabled(self, value):
        deadline = time.monotonic() + WAIT
        while time.monotonic() < deadline:
            base, state = self.settled()
            if base['enabled'] == value:
                return base, state
            if state['status'] == 'error':
                self.fail(state)
            time.sleep(.05)
        self.fail(f'基础角色索引没有变成 enabled={value}')

    def tables(self):
        f = self.fixture
        out = {}
        with closing(sqlite3.connect(f.lib.source)) as c:
            for name, sql in [('characters', 'SELECT * FROM characters ORDER BY char_id'),
                              ('aliases', 'SELECT * FROM character_aliases ORDER BY 1,2'),
                              ('char_ext', "SELECT uuid,value,conf,source FROM labels WHERE dim='char_ext' ORDER BY 1,2")]:
                out[name] = c.execute(sql).fetchall()
        with closing(sqlite3.connect(f.lib.db)) as c:
            out['candidates'] = c.execute('SELECT uuid,entity,source,score FROM candidates ORDER BY 1,2,3').fetchall()
            out['memberships'] = c.execute('SELECT uuid,entity,status,sources FROM memberships ORDER BY 1,2').fetchall()
            out['meta'] = c.execute("SELECT value FROM meta WHERE key='character_index_base'").fetchall()
        return out

    def shared_ids(self):
        path = self.fixture.shared / 'char-anchors.txt'
        return path.read_text().split() if path.exists() else None

    def mark(self):
        path = self.fixture.shared / 'char-index-base.json'
        return json.loads(path.read_text()) if path.exists() else None

    def wd_only(self, tables):
        """不归基础索引管的部分：WD 候选和人工确认的归属。"""
        return ([r for r in tables['candidates'] if r[2] == 'WD'], [r for r in tables['memberships'] if r[2] == 'confirmed'])

    def test_fresh_library_enables_withdraws_and_enables_again(self):
        f = self.fixture
        before = self.tables()
        self.install(); f.restart()
        base, state = self.wait_enabled(True)
        self.assertEqual((base['mode'], base['characters'], state['kind'], state['enabled']), ('rebuilt', 3, 'base', True))
        enabled = self.tables()
        self.assertEqual([r[0] for r in enabled['characters']], ['alice', *IDS, 'bob'])
        self.assertEqual(enabled['char_ext'], [('a', 'base_000', 1.0, 'char_index'), ('b', 'base_001', 1.0, 'char_index')])
        # 判别式认领的启用条件：库里有外部索引的 CCIP 候选（docs/WEB_V2.md）。
        self.assertTrue({u for u, _, source, _ in enabled['candidates'] if source == 'CCIP'} >= {'a', 'b'})
        self.assertEqual(self.wd_only(enabled), self.wd_only(before))
        self.assertEqual(self.shared_ids(), ['alice', *IDS, 'bob'])
        self.assertEqual(self.mark()['enabled'], True)
        self.assertEqual(snapshot(f.lib.db)['actions'], self.human)
        # 撤回：基础索引给出的角色和候选都去掉，WD 候选和人工结论不变；这台电脑记下撤回，重启后不自动启用。
        status, base = self.base(False); self.assertEqual(status, 200, base)
        self.wait_enabled(False)
        withdrawn = self.tables()
        self.assertEqual(([r[0] for r in withdrawn['characters']], withdrawn['char_ext'], withdrawn['meta']), (['alice', 'bob'], [], []))
        self.assertFalse([r for r in withdrawn['candidates'] if r[2] == 'CCIP'])
        self.assertEqual(self.wd_only(withdrawn), self.wd_only(before))
        self.assertEqual(self.shared_ids(), ['alice', 'bob'])
        self.assertEqual(self.mark()['enabled'], False)
        self.assertEqual(snapshot(f.lib.db)['actions'], self.human)
        f.stop(f.process); f.restart(); time.sleep(2)
        base, _ = self.settled()
        self.assertEqual((base['enabled'], base['withdrawn'], base['available']), (False, True, True))
        # 往返：再启用、再撤回，两次的结果逐项相同。
        status, _ = self.base(True); self.assertEqual(status, 200)
        self.wait_enabled(True)
        self.assertEqual(self.tables(), enabled)
        status, _ = self.base(False); self.assertEqual(status, 200)
        self.wait_enabled(False)
        self.assertEqual(self.tables(), withdrawn)
        self.assertEqual(snapshot(f.lib.db)['actions'], self.human)

    def test_same_version_library_is_recognized_and_left_unchanged(self):
        f = self.fixture
        with closing(sqlite3.connect(f.lib.source)) as c, c:
            # 已有图库当年装载时个别角色没算出锚点（base_002 的阈值为空），仍算同一版本。
            for i, cid in enumerate(IDS):
                p90 = None if cid == 'base_002' else .051
                c.execute('INSERT INTO characters(char_id,danbooru_tag,anchor_p50,anchor_p75,anchor_p90,n_ref,anchor_row) '
                          'VALUES (?,?,?,?,?,?,?)', (cid, cid + '_(work)', .021, .031, p90, 7, i))
            c.execute("INSERT INTO labels(uuid,dim,value,conf,source,model_ver) VALUES "
                      "('c','char_ext','base_002',.9,'char_index','character_index@2025-09-25')")
        before = self.tables()
        self.install(); f.restart()
        base, state = self.wait_enabled(True)
        self.assertEqual((base['mode'], state.get('changed')), ('adopted', 0), state)
        after = self.tables()
        self.assertEqual(after['meta'], [(json.dumps({'mode': 'adopted', 'sha256': hashlib.sha256(self.package).hexdigest()},
                                                     separators=(',', ':')),)])
        after.pop('meta'); expected = dict(before); expected.pop('meta')
        self.assertEqual(after, expected, '同一版本的索引不重写证据、不动归属')
        self.assertEqual(self.shared_ids(), ['alice', *IDS, 'bob'], '只装上共享锚点')
        status, _ = self.base(False); self.assertEqual(status, 200)
        self.wait_enabled(False)
        self.assertEqual(self.tables(), before)
        self.assertEqual(snapshot(f.lib.db)['actions'], self.human)

    def test_damaged_or_wrong_size_package_is_refused_without_changes(self):
        f = self.fixture
        before = self.tables()
        tampered = bytearray(self.package); tampered[len(tampered) // 2] ^= 1
        self.install(bytes(tampered)); f.restart()
        deadline = time.monotonic() + WAIT
        while time.monotonic() < deadline:
            _, state = self.settled()
            if state['status'] == 'error':
                break
            time.sleep(.05)
        self.assertEqual((state['status'], state['kind']), ('error', 'base'), state)
        self.assertIn('基础角色索引文件缺失或损坏', state['message'])
        self.assertEqual(state['diagnostic']['publication'], 'not_committed')
        self.assertEqual(self.tables(), before)
        self.assertIsNone(self.mark())
        self.install(self.package[:-1])
        status, body = self.base(True)
        self.assertEqual(status, 409, body)
        self.assertEqual(self.tables(), before)
        self.assertEqual(self.base()[1]['available'], False)

    def test_pack_waiting_for_default_is_enabled_after_the_base_index(self):
        """#630：新图库第一次同步时角色资料包没到，补下载完成时基础角色索引还没启用：等它启用完再启用资料包，
        CCIP 部分记下的是启用以后的索引代数，不提示重新检查。"""
        from tests.test_native_pack_catalog import builtin_v1, github
        f = self.fixture
        files = {}
        with github(files) as (origin, _):
            wd, ccip, wd_bytes, ccip_bytes = builtin_v1(f.root / 'builtin', origin)
            files.update({'/builtin/wd-extension-v1.bin': wd_bytes, '/builtin/A-longtail-p75.zip': ccip_bytes})
            # 不带模型的安装包首次准备：识别模型还没到齐时，基础索引等着不启用（ILLUSTASH_MODELS_ON_DEMAND）。
            f.env.update(ILLUSTASH_TEST_WD_EXTENSION=str(wd), ILLUSTASH_TEST_CCIP_LONGTAIL=str(ccip),
                         ILLUSTASH_MODELS_ON_DEMAND='1')
            # 第一次同步开始时包不在本机，宿主记下的同一行。
            with closing(sqlite3.connect(f.lib.db)) as c, c:
                c.execute("INSERT INTO meta(key,value) VALUES ('character_pack_default_pending',CURRENT_TIMESTAMP)")
            self.install(); f.restart()
            packs = lambda: json.loads(f.request('/api/packs')['body'])
            self.assertEqual(packs()['library']['prompt'], 'pending')
            self.assertEqual(f.request('/api/packs', {'action': 'download'})['status'], 200)
            deadline = time.monotonic() + WAIT
            while packs()['current']['status'] != 'ready' and time.monotonic() < deadline:
                time.sleep(.05)
            # 资料包先到：识别模型没到齐、基础索引还没启用时不启用。
            time.sleep(2)  # 宿主每秒检查一次要不要默认启用。
            waiting = packs()
            self.assertEqual((waiting['current']['status'], waiting['library']['enabled'], waiting['task']), ('ready', False, None), waiting)
            self.assertEqual(self.base()[1]['enabled'], False)
            # 识别模型到齐（只看文件在不在）：基础索引启用，之后资料包才启用。
            for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx', 'ccip/model_metrics.onnx', 'ccip/metrics.json'):
                (self.models / name).parent.mkdir(parents=True, exist_ok=True)
                (self.models / name).write_bytes(b'synthetic')
            self.wait_enabled(True)
            deadline = time.monotonic() + WAIT
            while time.monotonic() < deadline:
                observed = packs()
                task = observed['task']
                if observed['library']['enabled'] and task and task['status'] != 'running':
                    break
                time.sleep(.05)
            else:
                self.fail(f'角色资料包没有默认启用：{observed}')
            self.assertEqual((task['kind'], task['status']), ('enable', 'done'), observed)
            self.assertEqual(observed['library']['prompt'], None)
            self.assertIs(observed['ccip']['library']['needs_scan'], False, observed['ccip']['library'])

    def test_offline_import_installs_the_package_and_enables_it(self):
        f = self.fixture
        # 原始模型已在（只看大小），基础角色索引从用户选的文件导入。
        manifest = json.loads((Path(__file__).resolve().parents[1] / 'native/terms/models.json').read_text())
        for entry in manifest['files']:
            path = self.models / entry['path']; path.parent.mkdir(parents=True, exist_ok=True)
            with path.open('wb') as out: out.truncate(entry['size'])
        chosen = f.root / 'downloaded-character-index.bin'; chosen.write_bytes(self.package)
        f.restart()
        r = f.request('/api/models', {'action': 'import', 'paths': [str(chosen)]})
        self.assertEqual(r['status'], 200, r['body'])
        deadline = time.monotonic() + WAIT
        while time.monotonic() < deadline:
            models = json.loads(f.request('/api/models')['body'])
            if models['job'].get('status') not in ('running', None):
                break
            time.sleep(.05)
        self.assertEqual(models['job']['results'][0]['result'], 'accepted', models['job'])
        self.assertEqual((self.models / 'character-index/character-index-v1.bin').read_bytes(), self.package)
        base, _ = self.wait_enabled(True)
        self.assertEqual(base['mode'], 'rebuilt')
        self.assertEqual(self.shared_ids(), ['alice', *IDS, 'bob'])


    def test_first_sync_during_base_enable_waits_and_then_starts(self):
        """首次准备（走查 2/2 复现）：模型一到齐，宿主就为图库启用基础索引；打开图库时的自动同步和页面同时发起的首次同步
        都撞上索引写入。它们排在后面，不报错，写完以后同步自己开始，不用再点「立即同步」。"""
        f = self.fixture
        hold, entered = f.root / 'hold-index', f.root / 'index-entered'
        hold.touch()
        wrapper = f.root / 'held-index-worker'
        wrapper.write_text('#!' + sys.executable + '\nimport subprocess,sys,time\nfrom pathlib import Path\n'
                           f'Path({str(entered)!r}).touch()\n'
                           f'while Path({str(hold)!r}).exists(): time.sleep(.02)\n'
                           f'sys.exit(subprocess.run([{str(self.binary)!r},*sys.argv[1:]]).returncode)\n')
        f.env['ILLUSTASH_INDEX_WORKER'] = worker_command(wrapper)
        # 新用户的图库：一个文件夹来源，打开时自动同步。
        folder = f.root / 'first-folder'; folder.mkdir()
        f.lib.config['sources'] = [library.folder_source(folder)]
        (f.lib.path / 'library.json').write_text(json.dumps(f.lib.config))
        with closing(sqlite3.connect(f.lib.source)) as c, c:
            library.ensure_source_schema(c)
            for col in ('hidden INTEGER', 'stable_hash TEXT', 'orig_size INTEGER'):
                if col.split()[0] not in [r[1] for r in c.execute('PRAGMA table_info(assets)')]:
                    c.execute('ALTER TABLE assets ADD COLUMN ' + col)
            # 夹具的旧识别结果没有来源列的值；同步刷新时要读。
            c.execute("UPDATE labels SET source=CASE dim WHEN 'char_ext' THEN 'char_index' ELSE 'wd_v3' END WHERE source IS NULL")
        with closing(sqlite3.connect(f.lib.db)) as c, c:
            c.execute("INSERT OR REPLACE INTO prefs VALUES ('startup',?)", (json.dumps({'sync': True}),))
        self.install()
        f.restart(('--auto-sync',))

        def sync():
            r = f.request('/api/sync')
            self.assertEqual(r['status'], 200, r['body'])
            return json.loads(r['body'])['state']

        deadline = time.monotonic() + WAIT
        while not entered.exists():
            self.assertLess(time.monotonic(), deadline, '基础角色索引没有开始启用')
            time.sleep(.02)
        status, base = self.base()
        self.assertTrue(base['running'], base)
        self.assertEqual(sync()['status'], 'waiting')
        r = f.request('/api/sync', {})
        self.assertEqual(r['status'], 200, r['body'])
        self.assertEqual(json.loads(r['body']), {'started': False, 'queued': True})
        self.assertEqual(sync(), {'status': 'waiting'})
        self.assertEqual(f.index()['state']['status'], 'building')
        hold.unlink()
        base, state = self.wait_enabled(True)
        self.assertEqual(state['status'], 'done', state)
        deadline = time.monotonic() + WAIT
        while (s := sync())['status'] in ('waiting', 'scanning', 'recognising', 'importing'):
            self.assertLess(time.monotonic(), deadline, s)
            time.sleep(.05)
        self.assertIn(s['status'], ('done', 'needs_recognition'), s)
        history = json.loads(f.request('/api/sync')['body'])['history']
        self.assertEqual(len(history), 1, history)
        self.assertGreaterEqual(history[0]['started_at'], state['finished_at'], '同步要等基础索引写完才开始')

if __name__ == '__main__':
    unittest.main()
