// 运行：node --test tests/wd_extension.test.cjs
// WD 角色扩展（#550）：「设置 › 角色资料」卡片的各个状态、三个确认框、任务面板的一项、结束时的提示（ui/wd_extension.js），
// 状态的读取和轮询（core/wd_extension.js），以及详情栏的「WD 扩展」来源和「扩展」小字（ui/detail.js）。后端的路由还没有，
// 这里的回复都是照 #550「接口与界面方案」第 3 节写的桩。
const {test, before, beforeEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
const c = {};
before(async () => {
  await web.ready();
  Object.assign(c, await web.load('ui/wd_extension.js'), await web.load('core/wd_extension.js'), {state: web.core().state, session: web.core().session});
});

const SHA = '9da0365457e479eff189150926289c6cf8325cd9bb98c0bd2504c26d7dcae471';
const pkg = (fields = {}) => ({version: 'wd-extension-v1', heads: 228, file: {path: 'wd-extension-v1.bin', size: 940701, sha256: SHA}, license: 'Apache-2.0',
  manual: 'https://github.com/shio-vinyl/illustash/blob/main/docs/INSTALL.md#wd-角色扩展', status: 'missing', have: 0, partial: false,
  location: '/Users/synthetic/Library/Application Support/Illustash/Shared/extensions/wd-extension-v1', sources: ['github.com'], ...fields});
const lib = (fields = {}) => ({enabled: false, package_sha256: null, enabled_at: null, checked_at: null, scored: 0, images: 0, assigned: 0, blocked: null, vectors: 120, ...fields});
const ext = ({package: p = {}, library: l = {}, download = null, task = null} = {}) => ({package: pkg(p), download, library: lib(l), task});
const READY = {status: 'ready', have: 940701};
const ENABLED = {enabled: true, package_sha256: SHA, enabled_at: '2026-11-16 08:00:00', checked_at: '2026-11-16 08:00:00', scored: 120, images: 7, assigned: 3};
const buttons = html => [...html.matchAll(/<button class="([^"]+)" data-wdx="([^"]+)"([^>]*)>([^<]*)</g)].map(m => ({cls: m[1], act: m[2], attrs: m[3], label: m[4]}));
const labels = html => buttons(html).map(b => b.label);

test('没下载：主按钮写明大小，可以从文件导入，有手动下载说明', () => {
  const x = ext();
  assert.equal(c.extLine(x), '扩展包还没下载（约 941 KB）');
  assert.equal(c.extDot(x), 'off');
  const b = buttons(c.extActions(x));
  assert.deepEqual(b.map(x => [x.act, x.label]), [['download', '下载扩展包（约 941 KB）'], ['import', '从文件导入…']]);
  assert.match(b[0].cls, /primary/);
  assert.match(c.extHint(x), /手动下载说明/);
  // 还没有下载或任务：状态行不放「在任务面板查看」。
  assert.doesNotMatch(c.extStatus(x), /data-open-tasks/);
  assert.equal(c.extBar(x), '');
  assert.deepEqual(labels(c.extActions(x, true)), ['下载扩展包（约 941 KB）'], '任务面板只放主按钮');
  // 没就位时不能删除扩展包。
  assert.doesNotMatch(c.extPackage(x), /data-wdx="delete"/);
});

test('下了一部分：继续下载，未完成的部分可以删除', () => {
  const x = ext({package: {status: 'partial', have: 400000, partial: true}});
  assert.equal(c.extLine(x), '扩展包还没下载完（已有 400 KB，共 941 KB）');
  assert.deepEqual(labels(c.extActions(x)), ['继续下载（还差 541 KB）', '从文件导入…', '删除未完成的部分']);
});

test('下载中：进度、速度、来源、代理和取消', () => {
  const download = {kind: 'download', status: 'running', done: 470000, total: 940701, speed: 120000, source: 'github.com', proxy: null};
  const x = ext({package: {status: 'partial', have: 470000, partial: true}, download});
  assert.equal(c.extLine(x), '正在下载 470 KB / 941 KB · 120 KB/秒 · 来自 github.com');
  download.proxy = '127.0.0.1:7890';
  assert.match(c.extLine(x), / · 经代理 127.0.0.1:7890$/);
  assert.equal(c.extDot(x), 'busy');
  assert.match(c.extBar(x), /width:50.0%/);
  assert.deepEqual(buttons(c.extActions(x)).map(b => b.act), ['package-cancel']);
  assert.match(c.extStatus(x), /data-open-tasks/);
  assert.equal(c.extHint(x), '', '下载时不显示手动下载说明');
  // 导入时核对文件。
  assert.equal(c.extLine(ext({download: {kind: 'import', status: 'running', done: 300000, total: 940701}})), '正在核对所选文件 300 KB / 941 KB');
});

test('下载失败：原因、每个来源的失败、重试；已取消', () => {
  const download = {kind: 'download', status: 'failed', message: '连接超时', failures: [{source: 'github.com', why: 'HTTP 404'}, {source: 'github.com', why: 'SHA-256 与清单不符，已丢弃'}]};
  const x = ext({package: {status: 'partial', have: 1000, partial: true}, download});
  assert.equal(c.extLine(x), '扩展包没能下载：连接超时');
  assert.equal(c.extDot(x), 'warn');
  assert.match(c.extFailures(x), /github.com：HTTP 404/);
  assert.match(c.extFailures(x), /github.com：SHA-256 与清单不符，已丢弃/);
  assert.deepEqual(labels(c.extActions(x)), ['重试', '从文件导入…', '删除未完成的部分']);
  assert.equal(c.extLine(ext({download: {kind: 'download', status: 'failed', failures: []}})), '扩展包没能下载：每个来源都试过了');
  assert.equal(c.extLine(ext({download: {kind: 'download', status: 'cancelled'}})), '已取消');
  // 失败时任务面板打警告标记，诊断跟着走。
  const task = c.extTask(ext({download: {...download, diagnostic: {id: 'd1'}}}));
  assert.equal(task.warn, true);
  assert.deepEqual(task.diagnostic, {id: 'd1'});
});

test('这个页面导入以后逐个列出所选文件的结果；别处的导入（换了图库、别的窗口）不列', async () => {
  const download = {kind: 'import', status: 'done', results: [{name: 'wd-extension-v1.zip/wd-extension-v1.bin', result: 'accepted'},
    {name: 'edited.bin', result: 'mismatch'}, {name: 'notes.txt', result: 'unknown'}, {name: 'again.bin', result: 'already'}]};
  const imported = ext({package: READY, download});
  assert.equal(c.extImportSummary(imported), '', '页面刚载入，结果是别处导入留下的');
  const page = web.fakePage();
  c.wireWdExtension();
  server([{body: {cancelled: true}}, {body: imported}]);
  // 监听不返回 promise：点完等请求和回调都走完。
  const click = async act => {
    for (const fn of page.el('settingsPage').listeners.click) fn({target: {closest: () => ({dataset: {wdx: act}, disabled: false})}});
    await new Promise(resolve => setTimeout(resolve, 10));
  };
  await click('import');
  assert.equal(c.extImportSummary(imported), '', '关掉选择框不算导入');
  await click('import');
  const html = c.extImportSummary(imported);
  assert.match(html, /wd-extension-v1.zip\/wd-extension-v1.bin<\/bdi> · 已接收/);
  assert.match(html, /edited.bin<\/bdi> · SHA-256 不符，已拒绝/);
  assert.match(html, /notes.txt<\/bdi> · 不是扩展包/);
  assert.match(html, /again.bin<\/bdi> · 已经有了/);
  assert.equal(c.extImportSummary(ext({download: {kind: 'download', status: 'done', results: []}})), '');
});

test('已下载、本图库未启用：启用…；不能启用时按钮禁用，提示写原因', () => {
  const x = ext({package: READY});
  assert.equal(c.extLine(x), '本图库未启用');
  assert.deepEqual(buttons(c.extActions(x)).map(b => [b.act, b.label]), [['enable-confirm', '启用…']]);
  assert.doesNotMatch(c.extActions(x), /disabled/);
  for (const [code, why] of [['frozen', '图库只读'], ['no_vectors', '这个图库还没有 WD 识别结果'], ['syncing', '正在同步，等它结束'], ['busy', '图库正在导入或导出']]) {
    const b = buttons(c.extActions(ext({package: READY, library: {blocked: code}})))[0];
    assert.equal(b.label, '启用…');
    assert.match(b.attrs, new RegExp(`disabled title="${why}"`), code);
  }
  // 扩展包信息：存放位置写明所有图库共用，SHA-256 显示前 12 位，悬停看全；已就位时可以删除。
  const fold = c.extPackage(x);
  assert.match(fold, /~\/Library\/Application Support\/Illustash\/Shared\/extensions\/wd-extension-v1 · 所有图库共用/);
  assert.match(fold, new RegExp(`title="SHA-256 ${SHA}">941 KB · 9da0365457e4…`));
  assert.match(fold, /wd-extension-v1 · 228 个角色/);
  assert.match(fold, /Apache-2.0/);
  assert.match(fold, /data-wdx="delete"[^>]*>删除扩展包/);
});

test('启用中：扫描阶段报进度、可以取消；写入阶段不报百分比、不能取消', () => {
  const task = {id: 't1', kind: 'enable', status: 'running', phase: 'scan', done: 30, total: 120};
  const x = ext({package: READY, task});
  assert.equal(c.extLine(x), '正在检查 30 / 120 张');
  assert.equal(c.extDot(x), 'busy');
  assert.match(c.extBar(x), /width:25.0%/);
  assert.deepEqual(buttons(c.extActions(x)).map(b => [b.act, b.attrs]), [['cancel', '']]);
  assert.equal(c.extLine(ext({package: READY, task: {...task, done: 0, total: 0}})), '正在检查…', '还没数出张数时不写 0 / 0');
  task.phase = 'write';
  assert.equal(c.extLine(x), '正在写入…');
  assert.equal(c.extBar(x), '');
  assert.match(buttons(c.extActions(x))[0].attrs, /disabled title="正在写入，不能取消"/);
  // 进行中不能删除扩展包。
  assert.match(c.extPackage(x), /data-wdx="delete" disabled title="正在下载或检查，等它结束再删除"/);
  // 任务面板的忙碌文字。
  assert.equal(c.extTask(x).busy, '检查 WD 角色扩展中');
  assert.equal(c.extTask(ext({package: READY, download: {kind: 'download', status: 'running', done: 1, total: 2}})).busy, '下载 WD 角色扩展包中');
});

test('已启用：数字、上次检查、重新检查和撤回…', () => {
  const x = ext({package: READY, library: ENABLED});
  assert.equal(c.extLine(x), '已启用 · 7 张图有扩展候选，其中 3 张由它补上归属');
  assert.equal(c.extDot(x), '');
  assert.match(c.extHint(x), /^上次检查 \d+-\d\d \d\d:\d\d$/);
  assert.deepEqual(buttons(c.extActions(x)).map(b => [b.act, b.label]), [['enable', '重新检查'], ['withdraw', '撤回…']]);
  assert.equal(c.extLine(ext({package: READY, library: {...ENABLED, assigned: 0}})), '已启用 · 7 张图有扩展候选');
  assert.equal(c.extLine(ext({package: READY, library: {...ENABLED, images: 0, assigned: 0}})), '已启用 · 没有扩展候选');
  // 同步进行时不能重新检查，撤回不受影响。
  const busy = buttons(c.extActions(ext({package: READY, library: {...ENABLED, blocked: 'syncing'}})));
  assert.match(busy[0].attrs, /disabled title="正在同步，等它结束"/);
  assert.doesNotMatch(busy[1].attrs, /disabled/);
});

test('已启用、扩展包不在这台电脑上：写明新识别的图不检查，可以下载或撤回', () => {
  const x = ext({library: ENABLED});
  assert.equal(c.extLine(x), '已启用 · 7 张图有扩展候选，其中 3 张由它补上归属');
  assert.match(c.extHint(x), /^扩展包不在这台电脑上，新识别的图不检查 · 上次检查 .+ · <a class="link" href="[^"]+"[^>]*>手动下载说明<\/a>$/);
  const b = buttons(c.extActions(x));
  assert.deepEqual(b.map(x => x.label), ['下载扩展包（约 941 KB）', '从文件导入…', '撤回…']);
  assert.doesNotMatch(b[0].cls, /primary/, '已启用时下载不是主按钮');
});

test('撤回中没有按钮；启用或撤回失败写明图库没有改动，可以重试；取消后可以再启用', () => {
  const withdrawing = ext({package: READY, library: ENABLED, task: {id: 't2', kind: 'withdraw', status: 'running', phase: 'write'}});
  assert.equal(c.extLine(withdrawing), '正在撤回…');
  assert.equal(c.extActions(withdrawing), '');
  assert.equal(c.extTask(withdrawing).busy, '撤回 WD 角色扩展中');
  const failed = ext({package: READY, task: {id: 't3', kind: 'enable', status: 'failed', message: 'WD 底座模型不匹配', diagnostic: {id: 'x'}}});
  assert.equal(c.extLine(failed), '启用没有完成，图库没有改动：WD 底座模型不匹配');
  assert.equal(c.extDot(failed), 'warn');
  assert.deepEqual(buttons(c.extActions(failed)).map(b => [b.act, b.label]), [['enable-retry', '重试']]);
  assert.equal(c.extTask(failed).warn, true);
  const wfailed = ext({package: READY, library: ENABLED, task: {id: 't4', kind: 'withdraw', status: 'failed', message: '磁盘已满'}});
  assert.equal(c.extLine(wfailed), '撤回没有完成，图库没有改动：磁盘已满');
  assert.deepEqual(buttons(c.extActions(wfailed)).map(b => b.act), ['withdraw-retry']);
  const cancelled = ext({package: READY, task: {id: 't5', kind: 'enable', status: 'cancelled'}});
  assert.equal(c.extLine(cancelled), '已取消，图库没有改动');
  assert.deepEqual(buttons(c.extActions(cancelled)).map(b => b.act), ['enable-confirm']);
  // 没有下载也没有任务时不进任务面板。
  assert.equal(c.extTask(ext({package: READY})), null);
});

test('卡片正文和说明：还没读到时只有省略号', () => {
  assert.equal(c.extCardBody(null), '<p class="muted-note">…</p>');
  assert.equal(c.extNote(ext()), '多认 228 个 WD 词表里没有的较新角色。');
  const body = c.extCardBody(ext({package: READY, library: ENABLED}));
  assert.match(body, /class="setting-line set-status"/);
  assert.match(body, /<div class="models-acts"><button class="quiet small" data-wdx="enable">重新检查/);
  assert.match(body, /data-fold-key="wdExtPackage"/);
});

test('确认框写明做决定前要知道的事实', () => {
  assert.equal(c.enableMessage(ext({package: READY})), '用扩展包检查本图库 120 张图。还没有归属、也没有人工确认的图会补上一个机器归属，已有的归属和人工结论不变。以后可以撤回。');
  assert.equal(c.enableMessage(ext({package: READY, library: {vectors: undefined}})), '用扩展包检查本图库的图。还没有归属、也没有人工确认的图会补上一个机器归属，已有的归属和人工结论不变。以后可以撤回。');
  assert.equal(c.withdrawMessage(ext({library: ENABLED})), '去掉本图库的全部扩展候选，以及由它补上的 3 个机器归属。你确认过、否定过的保留。撤回不进撤销历史，要恢复就重新启用。');
  assert.equal(c.withdrawMessage(ext({library: {...ENABLED, assigned: 0}})), '去掉本图库的全部扩展候选。你确认过、否定过的保留。撤回不进撤销历史，要恢复就重新启用。');
  assert.equal(c.DELETE_MESSAGE(), '删除扩展包？已启用的图库保留现有的扩展候选，新识别的图不再检查；再用要重新下载。');
});

test('结束时的提示', () => {
  const before = ext({package: READY}), after = l => ext({package: READY, library: l});
  const done = (kind, changed) => ({id: 't', kind, status: 'done', result: {changed}});
  assert.equal(c.finishedText(done('enable', 9), before, after(ENABLED)), 'WD 角色扩展已启用：7 张图有扩展候选');
  assert.equal(c.finishedText(done('enable', 0), before, after({...ENABLED, images: 0})), 'WD 角色扩展已启用，没有找到扩展候选');
  assert.equal(c.finishedText(done('enable', 0), after(ENABLED), after(ENABLED)), '没有变化', '已启用时是重新检查');
  assert.equal(c.finishedText(done('enable', 2), after(ENABLED), after(ENABLED)), 'WD 角色扩展已更新：7 张图有扩展候选');
  assert.equal(c.finishedText(done('withdraw', 5), after(ENABLED), after({})), '已撤回 WD 角色扩展');
  const failed = ext({package: READY, task: {id: 't', kind: 'enable', status: 'failed', message: '识别交付没有完成'}});
  assert.equal(c.finishedText(failed.task, before, failed), '启用没有完成，图库没有改动：识别交付没有完成');
  // 同步后对新识别的图自动检查（kind 为 auto）：没有改动不提示，有改动或失败时提示。
  assert.equal(c.finishedText(done('auto', 0), after(ENABLED), after(ENABLED)), '');
  assert.equal(c.finishedText(done('auto', 2), after(ENABLED), after(ENABLED)), 'WD 角色扩展已更新：7 张图有扩展候选');
  const autoFailed = ext({package: READY, library: ENABLED, task: {id: 'a', kind: 'auto', status: 'failed', message: '磁盘已满'}});
  assert.equal(c.finishedText(autoFailed.task, after(ENABLED), autoFailed), '检查新识别的图没有完成，图库没有改动：磁盘已满');
  assert.equal(c.extLine(ext({package: READY, library: ENABLED, task: {id: 'a', kind: 'auto', status: 'running', phase: 'scan', done: 1, total: 4}})), '正在检查 1 / 4 张');
  assert.equal(c.downloadText({kind: 'download', status: 'done'}, ext({package: READY})), '扩展包已就位');
  assert.equal(c.downloadText({kind: 'import', status: 'done'}, ext()), '', '导入没凑齐时不提示，结果列在卡片里');
});

// ---------- core/wd_extension.js：读取、POST 和轮询 ----------
function server(replies) {
  const calls = [];
  globalThis.fetch = async (path, options = {}) => {
    calls.push([options.method || 'GET', path, options.body ? JSON.parse(options.body) : null]);
    const reply = replies.shift();
    return {status: reply.status || 200, ok: !reply.status, headers: {get: () => null}, json: async () => reply.body};
  };
  return calls;
}
beforeEach(() => { c.state.wdExtension = null; c.session.token = 'library-a'; c.session.stale = false; });

test('启用：POST 后轮询到结束，提示一次；换了图库的回复丢掉', async () => {
  const running = ext({package: READY, task: {id: 't1', kind: 'enable', status: 'running', phase: 'scan', done: 0, total: 120}});
  const done = ext({package: READY, library: ENABLED, task: {id: 't1', kind: 'enable', status: 'done', result: {changed: 3}}});
  const calls = server([{body: running}, {body: done}]);
  const seen = [];
  const off = c.onWdExtension(before => { seen.push(c.extFinished(before)); });
  await c.enableExtension();
  assert.deepEqual(calls[0], ['POST', '/api/wd-extension/enable', {}]);
  assert.equal(c.state.wdExtension.task.status, 'running');
  await c.loadWdExtension();
  assert.deepEqual(calls[1], ['GET', '/api/wd-extension', null]);
  assert.deepEqual(seen, [null, done.task], '只在任务从运行变成结束时报一次');
  // 再读一次不会再报。
  server([{body: done}]); await c.loadWdExtension();
  assert.equal(seen.length, 3); assert.equal(seen[2], null);
  // 回复到达前切换了图库：不采用。
  globalThis.fetch = async () => { c.session.token = 'library-b'; return {status: 200, ok: true, json: async () => ext()}; };
  await c.loadWdExtension();
  assert.equal(c.state.wdExtension, done);
  off();
});

test('各个 POST 的路由和请求体', async () => {
  const x = ext({package: READY});
  const calls = server(Array.from({length: 7}, () => ({body: x})));
  for (const action of ['download', 'import', 'cancel', 'discard', 'delete']) await c.packageAction(action);
  await c.withdrawExtension();
  c.state.wdExtension = ext({task: {id: 't9', kind: 'enable', status: 'done'}});
  await c.cancelExtension();
  assert.deepEqual(calls.map(([m, p, b]) => [m, p, b]), [
    ['POST', '/api/wd-extension/package', {action: 'download'}], ['POST', '/api/wd-extension/package', {action: 'import'}],
    ['POST', '/api/wd-extension/package', {action: 'cancel'}], ['POST', '/api/wd-extension/package', {action: 'discard'}],
    ['POST', '/api/wd-extension/package', {action: 'delete'}], ['POST', '/api/wd-extension/withdraw', {}], ['POST', '/api/wd-extension/cancel', {id: 't9'}]]);
  // 关掉选择框：回复 {cancelled}，状态不变。
  c.state.wdExtension = x;
  server([{body: {cancelled: true}}]);
  assert.deepEqual(await c.packageAction('import'), {cancelled: true});
  assert.equal(c.state.wdExtension, x);
});

test('409（扩展包没就位、图库忙）原样抛出后端的原文', async () => {
  server([{status: 409, body: {error: '已有任务在运行'}}]);
  await assert.rejects(c.enableExtension(), {message: '已有任务在运行', status: 409});
});

// ---------- 按钮和确认框 ----------
test('启用、撤回、删除扩展包都先确认；取消什么都不发', async () => {
  const page = web.fakePage();
  const answers = [], titles = [];
  page.document.body.append = d => { titles.push(d.attributes['aria-label']); d.showModal = () => { d.returnValue = answers.shift(); d.onclose(); }; };
  c.wireWdExtension();
  const click = async (root, act) => {
    const b = {dataset: {wdx: act}, disabled: false};
    for (const fn of page.el(root).listeners.click) await fn({target: {closest: s => s === '[data-wdx]' ? b : null}});
  };
  c.state.route = {page: 'settings'};
  c.state.wdExtension = ext({package: READY, library: ENABLED});
  const calls = server(Array.from({length: 3}, () => ({body: ext({package: READY, library: ENABLED})})));
  answers.push('cancel', 'cancel', 'cancel');
  await click('settingsPage', 'enable-confirm'); await click('settingsPage', 'withdraw'); await click('taskPanel', 'delete');
  assert.deepEqual(titles, ['启用 WD 角色扩展', '撤回 WD 角色扩展', '删除扩展包']);
  assert.deepEqual(calls, []);
  answers.push('ok', 'ok', 'ok');
  await click('settingsPage', 'enable-confirm'); await click('settingsPage', 'withdraw'); await click('taskPanel', 'delete');
  assert.deepEqual(calls.map(x => x[1]), ['/api/wd-extension/enable', '/api/wd-extension/withdraw', '/api/wd-extension/package']);
  // 重新检查、重试和取消不再确认。
  server(Array.from({length: 3}, () => ({body: ext({package: READY, library: ENABLED})})));
  titles.length = 0;
  await click('settingsPage', 'enable'); await click('taskPanel', 'enable-retry'); await click('taskPanel', 'package-cancel');
  assert.deepEqual(titles, []);
});
