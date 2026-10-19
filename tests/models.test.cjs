// Run with: node --test tests/models.test.cjs
// The recognition models an installer without them downloads on first use (#168): ui/models.js and core/models.js.
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
  // A part already here: continue, and the unfinished part can be deleted.
  const part = models({have: 100 * MB, partial: true});
  assert.match(context.modelsActions(part), /继续下载（还差 518.0 MB）/);
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
