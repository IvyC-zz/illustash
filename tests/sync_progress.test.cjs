// Run with: node --test tests/sync_progress.test.cjs
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const source = readFileSync(require('node:path').join(__dirname, '../web/static/app.js'), 'utf8');
const code = source.slice(source.indexOf('const syncBar ='), source.indexOf('let syncTimer'));
const context = {number: n => String(n), stamp: t => t};
vm.createContext(context);
vm.runInContext(code + ';this.syncLine = syncLine; this.syncBar = syncBar;', context);
const bar = (done, total) => `${done}/${total}`;

test('model loading shows the stage, not a count of recognised images', () => {
  const s = {status: 'recognising', stage: 'preparing', total: 30, done: 0};
  assert.equal(context.syncLine(s), '正在加载识别模型（共 30 张）…');
  assert.equal(context.syncBar(s, bar), '');
});

test('recognition shows the worker count, failures and the estimate while the worker runs', () => {
  const s = {status: 'recognising', stage: 'recognising', total: 30, done: 26, failed: 1, ms_per_image: 60000};
  assert.equal(context.syncLine(s), '正在识别新图片 26/30 · 1 张无法识别 · 约 4 分钟');
  assert.equal(context.syncBar(s, bar), '26/30');
});

test('a worker without progress keeps the old line', () => {
  assert.equal(context.syncLine({status: 'recognising', total: 3, done: 0}), '正在识别新图片 0/3');
});

test('import is counted apart from recognition', () => {
  const verifying = {status: 'importing', stage: 'verifying', done: 30, total: 30, import_done: 0, import_total: 29};
  assert.equal(context.syncLine(verifying), '正在校验识别结果（29 张）…');
  const importing = {...verifying, stage: 'importing', import_done: 25};
  assert.equal(context.syncLine(importing), '正在导入识别结果 25/29');
  assert.equal(context.syncBar(importing, bar), '25/29');
  assert.equal(context.syncLine({status: 'importing'}), '正在导入…');
  assert.equal(context.syncBar({status: 'importing'}, bar), '');
});

test('an interrupted run says why it stopped', () => {
  assert.equal(context.syncLine({status: 'interrupted', message: '任务超时：10 分钟没有进展'}),
    '同步已中断（任务超时：10 分钟没有进展），可重新扫描');
  assert.equal(context.syncLine({status: 'interrupted'}), '同步已中断，可重新扫描');
});

test('batches imported during recognition are counted while it runs', () => {
  assert.equal(context.syncLine({status: 'recognising', stage: 'recognising', total: 60, done: 51, import_done: 50}),
    '正在识别新图片 51/60 · 已导入 50');
});

test('a finished run says which pictures it kept because their paths could not be read', () => {
  assert.equal(context.syncLine({status: 'done', finished_at: '10-11 21:00'}), '上次同步：10-11 21:00');
  assert.equal(context.syncLine({status: 'done', finished_at: '10-11 21:00', unreadable: 2}),
    '上次同步：10-11 21:00 · 2 处暂时读不到，已有图片保留不动');
  assert.equal(context.syncLine({status: 'needs_recognition', total: 3, unreadable: 1}),
    '有 3 张图等待识别（识别服务未启动） · 1 处暂时读不到，已有图片保留不动');
});
