// 运行：node --test tests/sync_progress.test.cjs
// 同步的文案在 core/jobs.js；损坏图片的列表属于任务面板（ui/tasks.js；#199）。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
const context = {};
before(async () => {
  await web.ready();
  const {syncLine, syncBar} = web.core(), {syncDamaged} = await web.load('ui/tasks.js');
  Object.assign(context, {syncLine, syncBar, syncDamaged});
});
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

test('a long estimate is told in hours from two hours on, in minutes below that (#360)', () => {
  const line = (total, ms) => context.syncLine({status: 'recognising', stage: 'recognising', total, done: 0, ms_per_image: ms});
  // 约 5 万张、每张 0.94 秒：约 13 小时，而不是「约 783 分钟」。
  assert.equal(line(50000, 940), '正在识别新图片 0/50,000 · 约 13 小时');
  assert.equal(line(119, 60000), '正在识别新图片 0/119 · 约 119 分钟');
  // 四舍五入到 120 分钟就按小时说，不会出现「约 120 分钟」。
  assert.equal(line(239, 30000), '正在识别新图片 0/239 · 约 2 小时');
  assert.equal(line(120, 60000), '正在识别新图片 0/120 · 约 2 小时');
  assert.equal(line(149, 60000), '正在识别新图片 0/149 · 约 2 小时');
  assert.equal(line(150, 60000), '正在识别新图片 0/150 · 约 3 小时');
  assert.equal(line(1, 100), '正在识别新图片 0/1 · 约 1 分钟');
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
  assert.equal(context.syncLine({status: 'done', finished_at: '2026-10-11T21:00:00'}), '上次同步：10-11 21:00');
  assert.equal(context.syncLine({status: 'done', finished_at: '2026-10-11T21:00:00', unreadable: 2}),
    '上次同步：10-11 21:00 · 2 处暂时读不到，已有图片保留不动');
  assert.equal(context.syncLine({status: 'needs_recognition', total: 3, unreadable: 1}),
    '有 3 张图等待识别（识别服务未启动） · 1 处暂时读不到，已有图片保留不动');
});

test('a finished run tells unreadable pictures, retried next time, apart from damaged ones, skipped (#112)', () => {
  const done = {status: 'done', finished_at: '2026-10-13T09:00:00'};
  assert.equal(context.syncLine({...done, failed: 2}),
    '上次同步：10-13 09:00 · 2 张暂时读不到（可能还在 iCloud），下次同步重试');
  assert.equal(context.syncLine({...done, damaged: 1}), '上次同步：10-13 09:00 · 1 张已损坏或格式不支持，未识别');
  assert.equal(context.syncLine({...done, failed: 2, damaged: 1, unreadable: 1}),
    '上次同步：10-13 09:00 · 1 处暂时读不到，已有图片保留不动 · 2 张暂时读不到（可能还在 iCloud），下次同步重试 · 1 张已损坏或格式不支持，未识别');
  assert.equal(context.syncLine({...done, failed: 0, damaged: 0}), '上次同步：10-13 09:00');
});

test('damaged pictures are listed where they are, the rest counted', () => {
  assert.equal(context.syncDamaged(undefined), '');
  assert.equal(context.syncDamaged({count: 0, paths: []}), '');
  const html = context.syncDamaged({count: 3, paths: ['/Users/someone/Pictures/a<b>.png', 'D:\\Pictures\\c.jpg']});
  assert.match(html, /^<details class="more" data-fold-key="task-sync-damaged"><summary>3 张图片已损坏或格式不支持<\/summary>/);
  assert.match(html, /修好或换成正常的图片后，下次同步会重新识别/);
  assert.match(html, /title="\/Users\/someone\/Pictures\/a&lt;b&gt;.png"><bdi>~\/Pictures\/a&lt;b&gt;.png<\/bdi>/);
  assert.match(html, /<bdi>D:\\Pictures\\c.jpg<\/bdi>/);
  assert.match(html, /另有 1 张未列出。/);
  assert.doesNotMatch(context.syncDamaged({count: 1, paths: ['/data/x.png']}), /未列出/);
});

test('a picture that cannot be decoded shows a plain placeholder; the viewer keeps its own message', async () => {
  // wireDom()（ui/dom.js）在整个页面上监听图片错误。
  const dom = await web.load('ui/dom.js'), listeners = [];
  class HTMLImageElement {
    constructor(src, id = '') { this.id = id; this.src = src; this.replaced = null; }
    getAttribute() { return this.src; }
    replaceWith(node) { this.replaced = node; }
  }
  globalThis.HTMLImageElement = HTMLImageElement;
  const page = web.fakePage();
  page.document.addEventListener = (type, f, capture) => { if (type === 'error') { assert.equal(capture, true); listeners.push(f); } };
  dom.wireDom();
  const [listener] = listeners;
  const card = new HTMLImageElement('/image?id=a&size=thumb');
  listener({target: card});
  assert.equal(card.replaced.className, 'img-broken');
  assert.equal(card.replaced.textContent, '无法打开');
  const viewer = new HTMLImageElement('/image?id=a&size=preview', 'viewerImg'), other = new HTMLImageElement('/favicon.svg');
  listener({target: viewer}); listener({target: other});
  assert.equal(viewer.replaced, null); assert.equal(other.replaced, null);
});

test('a sync the system refused the Photos library says what to do, by its code and not its words (#202)', async () => {
  const message = '[Errno 1] Operation not permitted: \'/Users/someone/Pictures/合成.photoslibrary/database/Photos.sqlite\'';
  const refused = {status: 'error', problem: 'photos_permission', message};
  assert.equal(context.syncLine(refused), '同步失败：没有访问照片图库的权限');
  assert.equal(context.syncLine({status: 'error', message}), '同步失败');
  // 任务面板请求这项权限，带一个去设置页的按钮；点击只打开那个地址。
  const core = web.core(), {el} = web.fakePage();
  Object.assign(core.state, {sync: refused, syncHistory: []});
  globalThis.fetch = () => new Promise(() => {});
  globalThis.location = {href: ''};
  const {toggleTasks, wireTasks} = await web.load('ui/tasks.js');
  wireTasks(); toggleTasks(true);
  const html = el('taskPanel').innerHTML;
  assert.match(html, /<h6>等你确认<\/h6>.*没有访问照片图库的权限。到「系统设置 › 隐私与安全性 › 照片」里打开 Illustash，退出并重新打开后再同步。/s);
  assert.match(html, /data-task="photos-privacy">打开系统设置</);
  assert.doesNotMatch(html, /Operation not permitted/);
  const button = {dataset: {task: 'photos-privacy'}, disabled: false};
  for (const click of el('taskPanel').listeners.click) await click({stopPropagation() {}, target: {closest: selector => selector === '[data-task]' ? button : null}});
  assert.equal(globalThis.location.href, core.PHOTOS_PRIVACY);
  assert.equal(core.PHOTOS_PRIVACY, /PHOTOS_PRIVACY: &str =\s*"([^"]+)"/.exec(require('node:fs').readFileSync(`${__dirname}/../desktop/src/server.rs`, 'utf8'))[1]);
  // 其他失败不请求任何东西。
  Object.assign(core.state, {sync: {status: 'error', message}});
  toggleTasks(false); toggleTasks(true);
  assert.doesNotMatch(el('taskPanel').innerHTML, /photos-privacy/);
  toggleTasks(false);
});
