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
