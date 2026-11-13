// 运行：node --test tests/models.test.cjs
// 不带识别模型的安装包在首次使用时下载模型（#168）：ui/models.js 和 core/models.js。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
const context = {};
before(async () => {
  await web.ready();
  const ui = await web.load('ui/models.js'), core = await web.load('core/models.js');
  Object.assign(context, ui, core, {state: web.core().state, syncLine: web.core().syncLine});
});
const MB = 1e6;
const models = (fields = {}) => ({on_demand: true, status: 'missing', total: 618 * MB, have: 0, partial: false, missing: ['a'], job: null,
  manual: 'https://github.com/shio-vinyl/illustash/blob/main/docs/INSTALL.md#识别模型', mirror: 'https://hf-mirror.com', mirror_default: 'https://hf-mirror.com',
  sources: ['hf-mirror.com', 'modelscope.cn'], files: [], location: '/synthetic', ...fields});

test('only an install that downloads its models waits for them', () => {
  assert.equal(context.modelsWaiting(models()), true);
  assert.equal(context.modelsWaiting(models({status: 'ready'})), false);
  assert.equal(context.modelsWaiting(models({on_demand: false})), false);
  assert.equal(context.modelsWaiting(undefined), false);
});

test('the download button says how much; nothing starts without it', () => {
  const m = models();
  assert.equal(context.modelsLine(m), '还没下载（约 618.0 MB）');
  const actions = context.modelsActions(m);
  assert.match(actions, /data-models="download">下载模型（约 618.0 MB）</);
  assert.match(actions, /data-models="import"/);
  assert.doesNotMatch(actions, /discard/);
  // 已经有一部分：可以继续，没下完的部分可以删除。
  const part = models({have: 100 * MB, partial: true, missing: ['wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx']});
  assert.equal(context.modelsLine(part), '还缺 wd_swinv2_v3_emb.onnx、ccip/model_feat.onnx（已有 100.0 MB）');
  assert.match(context.modelsActions(part), /继续下载（还差 518.0 MB）/);
  // 只剩一个小文件：按字节显示大小，而不是 0 KB。
  assert.match(context.modelsActions(models({have: 618 * MB - 147, partial: true})), /继续下载（还差 147 字节）/);
  // 第一次尝试失败只留下空文件夹：没有可删的。
  assert.doesNotMatch(context.modelsActions(models({have: 0, partial: true})), /discard/);
  assert.match(context.modelsActions(part), /data-models="discard"/);
  assert.equal(context.modelsActions(part, true).includes('import'), false, 'the task panel shows the main button only');
  assert.equal(context.modelsActions(models({status: 'ready'})), '');
});

test('a running download shows progress, speed, source and a cancel button', () => {
  const m = models({job: {kind: 'download', status: 'running', done: 300 * MB, total: 618 * MB, speed: 7 * MB, source: 'modelscope.cn', proxy: null}});
  assert.equal(context.modelsLine(m), '正在下载 300.0 MB / 618.0 MB · 7.0 MB/秒 · 来自 modelscope.cn');
  assert.match(context.modelsBar(m), /width:48.5%/);
  assert.equal(context.modelsActions(m), '<button class="quiet small" data-models="cancel">取消</button>');
  m.job.proxy = '127.0.0.1:7890';
  assert.match(context.modelsLine(m), /经代理 127.0.0.1:7890$/);
});

test('a failed download names the file and each source with its reason', () => {
  const m = models({job: {kind: 'download', status: 'failed', failed_file: 'ccip/model_feat.onnx', message: 'x',
    failures: [{source: 'hf-mirror.com', why: 'HTTP 404'}, {source: 'modelscope.cn', why: 'SHA-256 与清单不符，已丢弃'}]}});
  assert.equal(context.modelsLine(m), 'ccip/model_feat.onnx 没能下载，每个来源都试过了');
  assert.match(context.modelsFailures(m), /hf-mirror.com：HTTP 404/);
  assert.match(context.modelsFailures(m), /modelscope.cn：SHA-256 与清单不符，已丢弃/);
  assert.equal(context.modelsLine(models({job: {status: 'failed', message: '磁盘空间不足：还需要约 700 MB，可用约 12 MB'}})),
    '下载没有完成：磁盘空间不足：还需要约 700 MB，可用约 12 MB');
  assert.equal(context.modelsLine(models({job: {status: 'cancelled'}, have: 1})), '已取消，下好的部分会保留，下次接着下');
});

test('an import lists what it made of each chosen file', () => {
  const job = {kind: 'import', status: 'done', results: [{name: 'a.zip/model.onnx', result: 'accepted'}, {name: 'metrics.json', result: 'mismatch'}, {name: 'x.txt', result: 'unknown'}]};
  const html = context.importSummary(job);
  assert.match(html, /a.zip\/model.onnx<\/bdi> · 已接收/);
  assert.match(html, /metrics.json<\/bdi> · SHA-256 不符，已拒绝/);
  assert.match(html, /x.txt<\/bdi> · 不是需要的模型文件/);
  assert.equal(context.importSummary({kind: 'download', results: []}), '');
});

test('the guide bar explains what works without the models, and goes once they are in place', () => {
  context.state.models = models();
  const html = context.modelsNotice();
  assert.match(html, /识别要用的模型还没下载（约 618.0 MB）。图片会先加入图库，可以浏览和手动整理；模型就位后会自动开始识别。/);
  assert.match(html, /手动下载说明/);
  context.state.models = models({status: 'ready'});
  assert.equal(context.modelsNotice(), '');
  context.state.models = models({on_demand: false});
  assert.equal(context.modelsNotice(), '');
});

test('a finished job is reported once', () => {
  const running = models({job: {status: 'running'}}), done = models({job: {status: 'done'}});
  assert.deepEqual(context.modelsFinished(running, done), {status: 'done'});
  assert.equal(context.modelsFinished(done, done), null);
  assert.equal(context.modelsFinished(undefined, done), null);
});

test('pictures waiting for the models say so in the sync line', () => {
  context.state.models = models();
  assert.equal(context.syncLine({status: 'needs_recognition', total: 3}), '有 3 张图等待识别（识别模型还没下载）');
  context.state.models = models({on_demand: false});
  assert.equal(context.syncLine({status: 'needs_recognition', total: 3}), '有 3 张图等待识别（识别服务未启动）');
});

test('downloaded models can be deleted, but not while something uses them', () => {
  context.state.models = models({status: 'ready'});
  context.state.sync = {status: 'done'};
  assert.match(context.deleteButton(), /data-models="delete" >删除已下载的模型</);
  context.state.sync = {status: 'recognising'};
  assert.match(context.deleteButton(), /data-models="delete" disabled title="正在识别或下载，等它结束再删除"/);
  context.state.sync = {status: 'done'};
  assert.match(context.deleteButton(models({status: 'ready', job: {status: 'running'}})), /disabled/);
  assert.equal(context.deleteButton(models({status: 'ready', on_demand: false})), '', 'models that came with the install stay');
  assert.equal(context.deleteButton(models()), '', 'nothing to delete yet');
});

test('the acceleration switch shows only where there is an accelerator, and says when it fell back (#270)', () => {
  assert.equal(context.accelerationLine(models()), '', 'an older host without the field');
  assert.equal(context.accelerationLine(models({acceleration: {available: false, backend: null, on: true, last: {}}})), '');
  const on = models({acceleration: {available: true, backend: 'coreml', on: true, last: {}}});
  assert.match(context.accelerationLine(on), /<b>使用 GPU 加速识别<\/b>/);
  assert.match(context.accelerationLine(on), /Core ML 加速标签与角色特征/);
  assert.match(context.accelerationLine(on), /data-acceleration checked/);
  assert.doesNotMatch(context.accelerationLine(on), /<em>/);
  const off = models({acceleration: {available: true, backend: 'directml', on: false, last: {}}});
  assert.match(context.accelerationLine(off), /用显卡计算/);
  assert.match(context.accelerationLine(off), /data-acceleration >/);
  const fell = {ccip: {requested: 'coreml', used: 'cpu', reason: 'coreml <ccip>: failed', at: 1}};
  assert.match(context.accelerationLine(models({acceleration: {available: true, backend: 'coreml', on: true, last: fell}})),
    /<em>上次没能用上加速，已改用 CPU：coreml &lt;ccip&gt;: failed<\/em>/);
  assert.doesNotMatch(context.accelerationLine(models({acceleration: {available: true, backend: 'coreml', on: false, last: fell}})), /<em>/,
    'switched off: nothing to explain');
});

test('WD 加速文件可补下载，缺失不阻塞识别，下载中仍可取消', () => {
  const m = models({status: 'ready', optional_missing: true, total: 1085 * MB, have: 618 * MB});
  assert.equal(context.modelsWaiting(m), false);
  assert.match(context.modelsLine(m), /WD 暂用 CPU/);
  assert.match(context.modelsActions(m), /下载 WD 加速文件（约 467.0 MB）/);
  assert.match(context.modelsActions(m), /data-models="import"/);
  assert.match(context.modelsActions({...m, job: {kind: 'download', status: 'running'}}), /data-models="cancel"/);
  assert.match(context.modelsActions({...m, optional_missing: false, acceleration: {backend: 'coreml', on: true, last: {wd: {reason: '损坏'}}}}), /修复 WD 加速文件/);
});

// 依次读到这些状态（core/models.js 的 adopt 的做法），返回每次读取后「模型刚到」的判断（#375）。第一个 before 为空：
// 页面刚打开。
function arrivals(...seen) {
  let before, started = null;
  return seen.map(after => {
    const start = started;
    started = context.readyWhenStarted(before, after, start);
    const arrived = context.modelsArrived(before, start, after);
    before = after;
    return arrived;
  });
}
test('only a job that brought the models in starts recognition (#375)', () => {
  const missing = models(), ready = models({status: 'ready'});
  const run = (m, kind = 'download') => ({...m, job: {kind, status: 'running'}});
  const end = (m, status, kind = 'download') => ({...m, job: {kind, status}});
  // 首次下载：原始四文件在任务中途激活（status 先变 ready），之后下加速文件；成功、失败、取消都要开始识别。
  for (const status of ['done', 'failed', 'cancelled'])
    assert.deepEqual(arrivals(missing, run(missing), run(ready), end(ready, status)), [false, false, false, true], status);
  // 原始模型没下完就失败：模型还没就位。
  assert.deepEqual(arrivals(missing, run(missing), end(missing, 'failed')), [false, false, false]);
  // 导入补齐了模型。
  assert.deepEqual(arrivals(missing, run(missing, 'import'), end(ready, 'done', 'import')), [false, false, true]);
  // 页面在任务中途打开（原始模型已激活）：无从判断，照旧开始识别。
  assert.deepEqual(arrivals(run(ready), end(ready, 'done')), [false, true]);
  assert.deepEqual(arrivals(run(missing), end(ready, 'cancelled')), [false, true]);
  // 模型本来就在：补下或修复加速文件、导入，不管结局如何都不算。
  for (const status of ['done', 'failed', 'cancelled'])
    assert.deepEqual(arrivals(ready, run(ready), run(ready), end(ready, status)), [false, false, false, false], status);
  assert.deepEqual(arrivals(ready, run(ready, 'import'), end(ready, 'done', 'import')), [false, false, false]);
  // 只看到上一个任务的结局、没看到它在运行：没有「刚结束」的任务。
  assert.deepEqual(arrivals(missing, end(ready, 'done')), [false, false]);
});

test('models that came with the install offer nothing that writes into the app (#375)', () => {
  const bundled = models({on_demand: false, status: 'ready', optional_missing: true, total: 1085 * MB, have: 618 * MB});
  assert.match(context.modelsLine(bundled), /WD 暂用 CPU/, 'the status line still says why WD uses the CPU');
  assert.equal(context.modelsActions(bundled), '');
  const fell = {available: true, backend: 'coreml', on: true, last: {wd: {reason: '损坏'}}};
  assert.equal(context.modelsActions({...bundled, optional_missing: false, acceleration: fell}), '');
  // 开发用的不完整文件夹：原有的下载和导入照旧。
  const partial = context.modelsActions(models({on_demand: false}));
  assert.match(partial, /data-models="download">下载模型/);
  assert.match(partial, /data-models="import"/);
});

test('repairing the WD acceleration file is offered only while acceleration is on (#375)', () => {
  const m = models({status: 'ready', optional_missing: false, acceleration: {available: true, backend: 'coreml', on: true, last: {wd: {reason: '损坏'}}}});
  assert.match(context.modelsActions(m), /data-models="download">修复 WD 加速文件</);
  assert.equal(context.modelsActions({...m, acceleration: {...m.acceleration, on: false}}), '');
  assert.equal(context.modelsActions({...m, acceleration: {...m.acceleration, backend: 'directml'}}), '');
});

test('the mirror note mentions the WD acceleration file only where the platform has one (#375)', () => {
  const {el} = web.fakePage();
  const file = (path, optional) => ({path, size: 1, sha256: '0'.repeat(64), optional});
  context.state.models = models({files: [file('wd/model.onnx', false)]});
  context.renderModelsCard();
  assert.match(el('setModels').innerHTML, /先从这个地址下载，不行再换 ModelScope。/);
  assert.doesNotMatch(el('setModels').innerHTML, /WD 加速文件/);
  context.state.models = models({files: [file('wd/model.onnx', false), file('wd/model-coreml.onnx', true)]});
  context.renderModelsCard();
  assert.match(el('setModels').innerHTML, /原始模型先从这个地址下载.*WD 加速文件从项目的 GitHub Release 下载。/);
});

test('the note when a job ends says both things in one toast, and calls the optional file the WD acceleration file (#375)', () => {
  const files = [{path: 'wd/model.onnx', size: 1, sha256: '0', optional: false}, {path: 'coreml/wd-swinv2-v3-v1.onnx', size: 1, sha256: '0', optional: true}];
  const ready = models({status: 'ready', files}), missing = models({files, missing: ['wd/model.onnx']});
  const failed = file => ({kind: 'download', status: 'failed', failed_file: file, message: 'x'});
  // 刚到且成功、取消（取消的只能是加速文件）。
  assert.equal(context.finishedNote({kind: 'download', status: 'done'}, true, ready), '识别模型已就位，开始识别');
  assert.equal(context.finishedNote({kind: 'download', status: 'cancelled'}, true, ready), '识别模型已就位，开始识别');
  // 刚到而加速文件没下成：一句里说清两件事，不写路径。
  const both = context.finishedNote(failed('coreml/wd-swinv2-v3-v1.onnx'), true, ready);
  assert.equal(both, '识别模型已就位，开始识别；WD 加速文件没能下载，WD 暂用 CPU。');
  assert.doesNotMatch(both, /coreml\//);
  // 刚到而失败的原因没有对应文件（如磁盘空间）：原因接在后面。
  assert.equal(context.finishedNote({kind: 'download', status: 'failed', message: '磁盘空间不足'}, true, ready), '识别模型已就位，开始识别。下载没有完成：磁盘空间不足');
  // 模型本来就在：只说失败。
  assert.equal(context.finishedNote(failed('coreml/wd-swinv2-v3-v1.onnx'), false, ready), 'WD 加速文件没能下载，WD 暂用 CPU。');
  assert.equal(context.finishedNote({kind: 'download', status: 'cancelled'}, false, ready), '');
  assert.equal(context.finishedNote({kind: 'import', status: 'done', results: []}, false, ready), '', 'an import once the models are in place');
  // 原始文件没下成：模型没就位，照旧写路径。
  assert.equal(context.finishedNote(failed('wd/model.onnx'), false, missing), 'wd/model.onnx 没能下载，每个来源都试过了');
  // 导入后模型还没齐。
  assert.equal(context.finishedNote({kind: 'import', status: 'done', results: []}, false, missing), '还缺 1 个文件');
});

test('a WD fallback recorded before the acceleration file was put in place no longer asks for a repair (#375)', () => {
  const files = [{path: 'wd/model.onnx', optional: false}, {path: 'coreml/wd-swinv2-v3-v1.onnx', optional: true}];
  const accel = (at, reason = '加速文件损坏') => ({available: true, backend: 'coreml', on: true, last: {wd: {requested: 'coreml', used: 'cpu', reason, at}}});
  const m = (job, fields = {}) => models({status: 'ready', files, optional_missing: false, acceleration: accel(5), job, ...fields});
  const running = m({kind: 'import', status: 'running'});
  const imported = result => m({kind: 'import', status: 'done', results: [{name: 'x.onnx', result, path: result === 'accepted' ? 'coreml/wd-swinv2-v3-v1.onnx' : null}]});
  // 导入接收了加速文件、下载（补下或修复）完成：那时的记录过期。
  assert.equal(context.settledWd(running, imported('accepted'), null), 5);
  assert.equal(context.settledWd(m({kind: 'download', status: 'running'}), m({kind: 'download', status: 'done'}), null), 5);
  // 导入的不是它、任务失败或取消、结束时仍缺加速文件、任务没结束：沿用原来的。
  assert.equal(context.settledWd(running, imported('unknown'), null), null);
  const accepted = {kind: 'import', status: 'done', results: [{result: 'accepted', path: 'wd/model.onnx'}]};
  assert.equal(context.settledWd(running, m(accepted), null), null, 'only the original file was accepted');
  assert.equal(context.settledWd(m({kind: 'download', status: 'running'}), m({kind: 'download', status: 'failed'}), 3), 3);
  assert.equal(context.settledWd(m({kind: 'download', status: 'running'}), m({kind: 'download', status: 'cancelled'}), 3), 3);
  assert.equal(context.settledWd(running, {...imported('accepted'), optional_missing: true}, null), null);
  assert.equal(context.settledWd(running, running, 3), 3);
  // 过期的 WD 记录不算；新的识别写入新的 at 后恢复；别的模型的记录不受影响；关掉加速就都不算。
  const ccip = {...accel(5), last: {...accel(5).last, ccip: {reason: 'ccip 失败', at: 5}}};
  assert.deepEqual(Object.keys(context.liveFallbacks(m(null), 5)), []);
  assert.deepEqual(Object.keys(context.liveFallbacks(m(null), null)), ['wd']);
  assert.deepEqual(Object.keys(context.liveFallbacks(m(null, {acceleration: accel(9)}), 5)), ['wd']);
  assert.deepEqual(Object.keys(context.liveFallbacks(m(null, {acceleration: ccip}), 5)), ['ccip']);
  assert.deepEqual(context.liveFallbacks(m(null, {acceleration: {...accel(5), on: false}}), null), {});
  assert.deepEqual(Object.keys(context.liveFallbacks(m(null, {acceleration: accel(9, '')}), 5)), [], 'a record without a reason');
});

test('the settings card stops offering a repair once the import put the file in place, until recognition runs again (#375)', async () => {
  const files = [{path: 'wd/model.onnx', size: 1, sha256: '0', optional: false}, {path: 'coreml/wd-swinv2-v3-v1.onnx', size: 1, sha256: '0', optional: true}];
  const accel = at => ({available: true, backend: 'coreml', on: true, last: {wd: {requested: 'coreml', used: 'cpu', reason: '加速文件损坏', at}}});
  const m = (job, at = 5) => models({status: 'ready', files, optional_missing: false, acceleration: accel(at), job});
  const done = {kind: 'import', status: 'done', results: [{name: 'x.onnx', result: 'accepted', path: 'coreml/wd-swinv2-v3-v1.onnx'}]};
  const replies = [m(null), m({kind: 'import', status: 'running'}), m(done), m(done, 9)];
  globalThis.fetch = async () => ({ok: true, status: 200, json: async () => replies.shift()});
  const read = async () => { await context.loadModels(); return [/修复 WD 加速文件/.test(context.modelsActions()), /<em>/.test(context.accelerationLine())]; };
  assert.deepEqual(await read(), [true, true], 'the fallback from the last recognition');
  await read();
  assert.deepEqual(await read(), [false, false], 'the import was accepted: that record is out of date');
  assert.deepEqual(await read(), [true, true], 'a later recognition fell back again');
  delete globalThis.fetch;
});

test('a sync that ends reads the models again, so the card follows the latest recognition (#375)', async () => {
  web.fakePage();
  const reads = [];
  globalThis.fetch = async path => {
    reads.push(path);
    const body = path === '/api/sync' ? {state: {status: 'done', imported: 0}, history: [], damaged: null} : models({status: 'ready'});
    return {ok: true, status: 200, json: async () => body};
  };
  context.wireModels();
  await new Promise(done => setTimeout(done, 0));
  const before = reads.filter(p => p === '/api/models').length;
  context.state.sync = {status: 'recognising'};
  await web.core().pollSync();
  await new Promise(done => setTimeout(done, 0));
  assert.equal(reads.filter(p => p === '/api/models').length, before + 1);
  // 没有在进行的同步，读到的状态也没变：不再读。
  await web.core().pollSync();
  await new Promise(done => setTimeout(done, 0));
  assert.equal(reads.filter(p => p === '/api/models').length, before + 1);
  delete globalThis.fetch;
});
