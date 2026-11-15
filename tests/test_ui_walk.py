"""界面走查工具（tools/ui_walk，#453）自身的检查：不起宿主、不开浏览器。

真正的走查要宿主和无头浏览器，在本机按 docs/LOCAL_CHECKS.md「界面走查」运行；这里只核对离线的部分：合成识别结果包的
向量文件格式、比较两次运行的逻辑、流程清单。
"""
import io
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import numpy as np

from tools.ui_walk import compare, fixture

ROOT = Path(__file__).resolve().parent.parent


class FixtureTest(unittest.TestCase):
    def test_vectors_are_valid_npy_with_designed_similarity(self):
        names = ['a-01', 'a-02', 'b-01', 'b-01-small']
        dirs = ['a', 'a', 'b', 'b']
        data = fixture.vectors(names, dirs, 64, 7)
        arr = np.load(io.BytesIO(data))
        self.assertEqual(arr.shape, (4, 64))
        self.assertEqual(arr.dtype, np.dtype('<f4'))
        np.testing.assert_allclose(np.linalg.norm(arr, axis=1), 1, rtol=1e-5)
        cos = arr @ arr.T
        self.assertGreater(cos[0, 1], 0.86)       # 同一方向：分到一组（服务端阈值 0.86）
        self.assertGreater(cos[2, 3], 0.988)      # 变体：进相似候选
        self.assertLess(abs(cos[0, 2]), 0.5)      # 不同方向：几乎正交
        self.assertEqual(data, fixture.vectors(names, dirs, 64, 7))   # 确定的

    def test_items_are_unique_and_deterministic(self):
        items = fixture.items()
        self.assertEqual(len({i.name for i in items}), len(items))
        self.assertEqual([i.name for i in items], [i.name for i in fixture.items()])
        for name in fixture.R18:
            self.assertIn(name, {i.name for i in items})

    def test_scene_is_a_png_and_scaled_copy_differs_only_in_size(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / 'a.png', Path(d) / 'b.png'
            fixture.scene(a, 120, 90, 5)
            fixture.scene(b, 120, 90, 5)
            self.assertTrue(a.read_bytes().startswith(b'\x89PNG'))
            self.assertEqual(a.read_bytes(), b.read_bytes())


def report(steps, checks=(), lang='zh', flow='f'):
    return {'source': {'kind': 'worktree', 'commit': '0' * 40}, 'started': 'x',
            'flows': [{'flow': flow, 'lang': lang, 'steps': steps, 'checks': list(checks)}]}


def step(label, **snap):
    return {'index': 1, 'label': label, 'snap': snap, 'console': []}


class CompareTest(unittest.TestCase):
    def test_identical_runs_have_no_difference(self):
        r = report([step('a', focused='x-01', layers=['viewer'])], [{'name': 'c', 'ok': True}])
        self.assertEqual(compare.compare(r, json.loads(json.dumps(r))), [])

    def test_field_check_and_console_differences_are_reported(self):
        a = report([step('a', focused='x-01')], [{'name': 'c', 'ok': True}])
        b = report([step('a', focused='x-02')], [{'name': 'c', 'ok': False, 'detail': 'd'}])
        b['flows'][0]['steps'][0]['console'] = [{'kind': 'exception', 'text': 'boom'}]
        [entry] = compare.compare(a, b)
        self.assertEqual([c[0] for c in entry['checks']], ['c'])
        fields = {k: (x, y) for k, x, y in entry['steps'][0]['fields']}
        self.assertEqual(fields['focused'], ('x-01', 'x-02'))
        self.assertEqual(fields['控制台'], ([], ['boom']))

    def test_misaligned_steps_and_missing_flows(self):
        a = report([step('a'), step('b')])
        b = report([step('a')])
        b['flows'].append({'flow': 'g', 'lang': 'zh', 'steps': [], 'checks': []})
        result = compare.compare(a, b)
        self.assertIn('步骤不对应', result[0]['steps'][0]['structure'])
        self.assertEqual(result[1]['only'], 'B')

    def test_main_writes_markdown_and_html(self):
        with tempfile.TemporaryDirectory() as d:
            a, b = Path(d) / 'a', Path(d) / 'b'
            for run, value in ((a, 'x-01'), (b, 'x-02')):
                run.mkdir()
                (run / 'report.json').write_text(json.dumps(report([step('a', focused=value)])), encoding='utf-8')
            self.assertEqual(compare.main(a, b, None), 1)
            self.assertIn('x-02', (b / 'compare' / 'compare.md').read_text(encoding='utf-8'))
            self.assertTrue((b / 'compare' / 'compare.html').exists())


@unittest.skipUnless(shutil.which('node'), '没有 node')
class FlowListTest(unittest.TestCase):
    def test_walker_keeps_errors_and_completed_steps(self):
        script = '''
            import {Walker} from './tools/ui_walk/walker.mjs';
            let errors = [{kind: 'exception', text: '启动错误'}];
            const page = {settle: async () => {}, eval: async () => ({}),
                drainConsole: () => { const old = errors; errors = []; return old; }};
            const w = new Walker(page, {flow: 'synthetic', lang: 'zh', shots: false});
            await w.step('失败动作', async () => { throw new Error('点击失败'); });
            page.eval = async () => { throw new Error('快照失败'); };
            await w.step('失败快照', null);
            errors.push({kind: 'exception', text: '收尾错误'});
            process.stdout.write(JSON.stringify(w.result({crashed: '流程被打断'})));
        '''
        run = subprocess.run(['node', '--input-type=module'], input=script, cwd=ROOT,
                             capture_output=True, text=True, check=True)
        result = json.loads(run.stdout)
        self.assertEqual(result['crashed'], '流程被打断')
        self.assertEqual([c['detail'] for c in result['checks']], ['点击失败', '快照失败'])
        self.assertTrue(all(not c['ok'] for c in result['checks']))
        self.assertEqual([s['label'] for s in result['steps']], ['失败动作', '失败快照', '流程收尾'])
        self.assertEqual([e['text'] for s in result['steps'] for e in s['console']], ['启动错误', '收尾错误'])

    def test_flows_are_listed_with_unique_names(self):
        out = subprocess.run(['node', str(ROOT / 'tools/ui_walk/run.mjs'), '--list'], capture_output=True, text=True,
                             check=True)
        flows = json.loads(out.stdout)
        names = [f['name'] for f in flows]
        self.assertEqual(len(names), len(set(names)))
        self.assertTrue(all(f['title'] for f in flows))
        self.assertIn('viewer-detail', names)


if __name__ == '__main__':
    unittest.main()
