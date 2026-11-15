// #502：人工结论快照的界面（接口见 #441 的接口定稿）。设置 › 数据 › 备份与存储里的自动快照状态与清单、导出、从文件导入、
// 从自动快照恢复，完成后的报告、分页明细、撤销，以及 400/413/409/500 的说法。接口全部是这里的模拟回答，数据是合成的。
const {test, before, afterEach} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let human, ui, effects;
before(async () => {
  await web.ready();
  [human, ui, effects] = await Promise.all([web.load('core/human.js'), web.load('ui/human.js'), web.load('ui/effects.js')]);
});
const realTimeout = globalThis.setTimeout;
let cleanup = () => {};
afterEach(() => { globalThis.setTimeout = realTimeout; cleanup(); cleanup = () => {}; });

// 合成的状态：2026-11-15 的几个 UTC 秒数。
const T = Date.UTC(2026, 10, 15, 3, 0) / 1000;
const automatic = (extra = {}) => ({protected_revision: 7, pending_revision: null, pending_since: null, last_success: T, snapshot_id: 's2', state_sha256: 'x',
  hash_missing: 0, last_error: null, retry_at: null, next_attempt: null, cleanup_error: null, running: false, unreadable_snapshots: 0,
  snapshots: [
    {backup_id: 'b-old', created_at: '2026-11-14T03:00:00Z', state_sha256: 'a', hash_coverage: {total: 3, verified: 1, last_known: 0, missing: 2}, portable_complete: false},
    {backup_id: 'b-new', created_at: '2026-11-15T03:00:00Z', state_sha256: 'b', hash_coverage: {total: 3, verified: 3, last_known: 0, missing: 0}, portable_complete: true}],
  ...extra});
const summary = (extra = {}) => ({committed: true, snapshot_id: 's1', report_id: 'r1', undo: 12,
  counts: {applied: 2, already_present: 1, conflict: 3}, asset_counts: {matched: 2, unmatched_asset: 1}, definition_counts: {matched_dependency: 1},
  definition_fields: {link_conflict: 1}, hash_coverage: {total: 3, verified: 3, last_known: 0, missing: 0}, portable_complete: true,
  detail_count: 7, pending_effects: false, ...extra});
const ok = body => Promise.resolve({status: 200, ok: true, json: async () => body, headers: {get: () => null}});
const fail = (status, error) => Promise.resolve({status, ok: false, json: async () => ({error}), headers: {get: () => null}});

// 一个假页面：toast 记下文字，定时器记下间隔不执行，fetch 交给测试。
function page(routes) {
  const {state, session, edits} = web.core();
  session.token = 'session';
  Object.assign(state, {route: {page: 'settings', section: 'data'}, human: null, importTask: null});
  const p = web.fakePage(), toasts = [], timers = [], calls = [];
  Object.defineProperty(p.el('toastText'), 'textContent', {set: m => toasts.push(m), get: () => toasts.at(-1) || ''});
  globalThis.setTimeout = (fn, ms) => { timers.push(ms); return timers.length; };
  globalThis.fetch = (path, options = {}) => {
    calls.push([path, options.body ? JSON.parse(options.body) : undefined]);
    const route = routes[path.split('?')[0]];
    return route ? route(path, options.body ? JSON.parse(options.body) : undefined) : ok({});
  };
  return {state, session, edits, page: p, toasts, timers, calls, block: () => p.el('humanBlock').innerHTML,
    dialogs: () => p.document.body.children.filter(c => c.tagName === 'DIALOG')};
}

test('the automatic status says when it last succeeded, what is not protected yet, and lists snapshots newest first with restore', () => {
  const p = page({});
  p.state.human = {automatic: automatic({pending_revision: 8, pending_since: T + 60, next_attempt: T + 3600}), export: {status: 'idle'}};
  const html = ui.humanBlock();
  assert.match(html, /最近一次自动快照：/);
  assert.match(html, /有改动还没进快照，预计 .+ 生成/);
  assert.match(html, /最近的自动快照（2 份）/);
  assert.ok(html.indexOf('b-new') < html.indexOf('b-old'), 'newest first');
  assert.match(html, /data-human="restore" data-backup="b-new"/);
  assert.match(html, /2 张图缺内容摘要/);
  assert.match(html, /图片摘要齐全/);
  assert.match(html, /挡不住整个图库被删/, 'the export warning is always there');
  assert.doesNotMatch(html, /disabled/);
});

test('a failure shows its reason and retry time; cleanup errors, missing fingerprints and unreadable snapshots each say what they mean', () => {
  const p = page({});
  p.state.human = {automatic: automatic({last_error: '合成故障', retry_at: T + 300, hash_missing: 4, cleanup_error: '合成清理故障', unreadable_snapshots: 1}), export: {status: 'idle'}};
  const html = ui.humanBlock();
  assert.match(html, /local-dot warn/);
  assert.match(html, /自动快照失败：合成故障/);
  assert.match(html, /\d\d-\d\d \d\d:\d\d 重试/);
  assert.match(html, /上次成功/);
  assert.match(html, /4 张图当时读不到.*原图库还是换了位置的图库.*认不回来/);
  assert.match(html, /清理旧快照没有成功：合成清理故障。新快照照常生成/);
  assert.match(html, /另有 1 份快照读不出来/);
  for (const [extra, text] of [[{running: true}, '正在生成自动快照…'], [{last_success: null, snapshots: []}, '还没有自动快照']]) {
    p.state.human = {automatic: automatic(extra), export: {status: 'idle'}};
    assert.ok(ui.humanBlock().includes(text), text);
  }
});

test('an export shows its stage with a cancel button, then where the file is with the home folder as ~', () => {
  const p = page({});
  p.state.human = {automatic: automatic(), export: {id: 'e1', status: 'running', kind: 'human_export', stage: 'writing'}};
  let html = ui.humanBlock();
  assert.match(html, /正在导出人工结论：正在写文件…/);
  assert.match(html, /data-human="cancel-export"/);
  assert.match(html, /data-human="export" disabled/, 'one export at a time');
  assert.match(html, /data-human="restore" data-backup="b-new" data-at="[^"]+" disabled/);
  p.state.human.export = {id: 'e1', status: 'done', kind: 'human_export', result: {...summary({undo: null}), path: '/Users/someone/Downloads/Illustash-human-s1.illustash-human.jsonl',
    counts: {exported: 5}, asset_counts: {exported: 3}, definition_counts: {exported: 2}, hash_coverage: {total: 3, verified: 2, last_known: 0, missing: 1}, portable_complete: false}};
  html = ui.humanBlock();
  assert.match(html, /已导出到 <bdi>~\/Downloads\/Illustash-human-s1\.illustash-human\.jsonl<\/bdi>/);
  assert.match(html, /5 条人工结论 · 3 张图 · 2 个角色和作品/, 'three containers, never summed');
  assert.match(html, /1 张图当时读不到/);
  p.state.human.export = {id: 'e1', status: 'error', code: 500, error: '合成故障'};
  assert.match(ui.humanBlock(), /写导出文件时出错，没有生成文件。.*（合成故障）/);
});

test('export starts a task, polls until it ends, and announces the file once; cancel names that export', async () => {
  let reads = 0;
  const p = page({
    '/api/human-conclusions/export': () => ok({id: 'e1', status: 'running', kind: 'human_export', file: ''}),
    '/api/human-conclusions/export/cancel': () => ok({status: 'stopping'}),
    '/api/human-conclusions': () => ok({automatic: automatic(), export: ++reads < 2 ? {id: 'e1', status: 'running', kind: 'human_export', stage: 'reading'}
      : {id: 'e1', status: 'done', kind: 'human_export', result: {path: '/Users/someone/Downloads/x.illustash-human.jsonl'}}}),
  });
  cleanup = human.onHuman(({ended}) => { if (ended) p.toasts.push('ended'); });
  ui.wireHuman({undo: () => {}});
  await human.startExport();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(p.calls[0], ['/api/human-conclusions/export', {}]);
  assert.deepEqual(p.timers.filter(ms => ms === 1000), [1000], 'still running: polls again');
  await human.cancelExport();
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(p.calls.find(c => c[0] === '/api/human-conclusions/export/cancel'), ['/api/human-conclusions/export/cancel', {id: 'e1'}]);
  assert.equal(p.state.human.export.status, 'done');
  assert.ok(p.toasts.includes('人工结论已导出到 ~/Downloads/x.illustash-human.jsonl'));
  assert.equal(p.toasts.filter(m => m === 'ended').length, 1);
});

test('import opens the native picker (no path sent); cancelling the picker is not an error', async () => {
  const p = page({'/api/human-conclusions/import': () => ok({cancelled: true})});
  p.state.human = {automatic: automatic(), export: {status: 'idle'}};
  assert.equal(await human.startHumanImport(), null);
  assert.deepEqual(p.calls[0], ['/api/human-conclusions/import', {}]);
  assert.equal(p.state.importTask, null);
  assert.deepEqual(p.toasts, []);
});

test('a running import or restore uses the import slot: stage line, cancel button, other buttons off', () => {
  const p = page({});
  p.state.human = {automatic: automatic(), export: {status: 'idle'}};
  p.state.importTask = {id: 'i1', status: 'running', kind: 'human_conclusions', file: '', stage: 'matching', done: 3, total: 9};
  const html = ui.humanBlock();
  assert.match(html, /导入人工结论：正在核对图片 3\/9 张…/);
  assert.match(html, /data-human="cancel-import"/);
  assert.match(html, /data-human="import" disabled/);
  p.state.importTask = {...p.state.importTask, stage: 'validating'};
  assert.match(ui.humanBlock(), /导入人工结论：正在校验…/, 'no stale count after matching');
});

test('restore asks first, then sends only the snapshot id and reports as a restore', async () => {
  const p = page({
    '/api/human-conclusions/restore': () => ok({id: 'i2', status: 'running', kind: 'human_conclusions', file: ''}),
    '/api/imports': () => new Promise(() => {}),
  });
  const task = await human.restoreSnapshot('b-new');
  assert.deepEqual(p.calls[0], ['/api/human-conclusions/restore', {backup_id: 'b-new'}]);
  assert.equal(p.state.importTask.id, 'i2');
  assert.ok(human.isRestore(task));
  assert.ok(!human.isRestore({id: 'other'}));
});

test('the report counts each kind on its own, says conflicts kept this library, and offers undo only when something changed', () => {
  const html = ui.reportSummary(summary());
  assert.match(html, /有 3 条和这个图库已有的结论冲突，都保留了这个图库的结论/);
  assert.match(html, /另有 0 个名字组、1 个链接和已有的不同/);
  assert.match(html, /<h3 class="set-sub">人工结论<\/h3><table class="settings-table"><tr><td>补上了<\/td><td>2<\/td>/);
  assert.match(html, /涉及的图片.*认回了<\/td><td>2.*这个图库里找不到对应的图<\/td><td>1/s);
  assert.match(html, /依赖的角色和作品.*对上了已有的/s);
  assert.match(html, /口径不同，不能相加/);
  assert.doesNotMatch(html, /<td>7<\/td>/, 'nothing is summed across kinds');
  assert.match(html, /可以整步撤销/);
  const none = ui.reportSummary(summary({undo: null, counts: {already_present: 4}}));
  assert.match(none, /没有新内容/);
  assert.doesNotMatch(none, /可以整步撤销/);
  assert.match(ui.reportSummary(summary({pending_effects: true})), /不需要重新导入/);
  assert.match(ui.reportSummary(summary({definition_counts: {applied: 1}})), /新建了<\/td><td>1/);
});

test('details group by result and put reasons in words; pages move by the page size', () => {
  const html = ui.reportDetails({offset: 0, limit: 50, details: [
    {kind: 'image_verdict', record_key: 'k1', result: 'conflict', reason: null, target_key: ['a']},
    {kind: 'asset', record_key: 'k2', result: 'unverified_asset', reason: 'source_hash_missing', target_key: null},
    {kind: 'curation', record_key: 'k3', result: 'conflict', reason: null, target_key: ['b']},
    {kind: 'entity', record_key: 'k4', result: 'matched_dependency', reason: null, target_key: ['c'], field_counts: {name_group_applied: 2}},
    {kind: 'asset', record_key: 'k5', result: 'ambiguous_asset', reason: 'brand_new_reason', target_key: null}]}, 120);
  assert.match(html, /和这个图库已有的结论冲突，保留了已有的（2）/);
  assert.ok(html.indexOf('k1') < html.indexOf('k3') && html.indexOf('k3') < html.indexOf('k2'), 'grouped by result');
  assert.match(html, /导出时没能读到这张图/);
  assert.match(html, /补上的名字组 2/);
  assert.match(html, /其他原因（brand_new_reason）/, 'an unknown reason still shows its code');
  assert.match(html, /第 1–5 条，共 120 条/);
  assert.match(html, /data-page="-50" disabled/);
  assert.match(html, /data-page="5" >/);
});

test('400, 413, 409 and 500 each have their own sentence; an error after publishing says not to import again', () => {
  const said = new Set([400, 413, 409, 500].map(code => human.humanError(code, 'import')));
  assert.equal(said.size, 4);
  assert.match(human.humanError(413), /256 MiB/);
  assert.match(human.humanError(400, 'restore'), /快照已经不在了/);
  assert.match(human.humanError(409, 'export'), /不能导出/);
  assert.match(human.humanError(500, 'import', true, '合成'), /已经导入.*不要重复导入.*（合成）/);
});

test('a finished import opens the report, joins the undo control, and a refused undo explains why', async () => {
  const p = page({'/api/human-conclusions': () => ok({automatic: automatic(), export: {status: 'idle'}}), '/api/bootstrap': () => ok({})});
  const done = {id: 'i3', status: 'done', kind: 'human_conclusions', committed: true, result: summary()};
  p.state.importTask = done;
  await effects.afterJob('import', {was: 'running', ended: true});
  assert.deepEqual(p.edits.latest, {id: 12, label: human.IMPORT_LABEL});
  const [d] = p.dialogs();
  assert.ok(d, 'the report dialog is open');
  assert.match(d.innerHTML, /data-human-undo/);
  assert.match(d.innerHTML, /查看明细（7 条）/);
  const refused = Object.assign(new Error('合成'), {status: 409});
  assert.match(human.undoRefusal(refused, {id: 12}), /不能整步撤销/);
  assert.equal(human.undoRefusal(refused, {id: 99}), '合成', 'other steps keep the server message');
  // 没有改动：不接撤销，不给撤销按钮。
  p.edits.latest = null; d.remove();
  p.state.importTask = {...done, id: 'i4', result: summary({undo: null})};
  await effects.afterJob('import', {was: 'running', ended: true});
  assert.equal(p.edits.latest, null);
  assert.doesNotMatch(p.dialogs()[0].innerHTML, /data-human-undo/);
  // 失败：一句话说明，不开报告。
  p.dialogs()[0].remove();
  p.state.importTask = {id: 'i5', status: 'error', kind: 'human_conclusions', code: 413, error: '合成', committed: false};
  await effects.afterJob('import', {was: 'running', ended: true});
  assert.match(p.toasts.at(-1), /文件太大/);
  assert.equal(p.dialogs().length, 0);
});
