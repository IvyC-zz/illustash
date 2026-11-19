const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let core, diagnostics;
before(async () => { await web.ready(); core = web.core(); diagnostics = await web.load('core/diagnostics.js'); });
test('an HTTP failure retains its original comparison text and displays its diagnostic ID', async () => {
  const d = {id: 'D-synthetic', stored: false, publication: 'unknown', retry: 'check_state'};
  globalThis.fetch = async () => ({ok: false, status: 500, headers: {get: () => JSON.stringify(d)}, json: async () => ({error: '原始业务错误'})});
  await assert.rejects(core.api('/api/settings'), e => {
    assert.equal(e.said, '原始业务错误');
    assert.match(e.message, /D-synthetic/); assert.match(e.message, /未保存/);
    assert.deepEqual(e.diagnostic, d); return true;
  });
  globalThis.fetch = async path => { assert.equal(path, '/api/diagnostics'); return {ok: true, json: async () => ({format: 1, records: []})}; };
  const report = await diagnostics.diagnosticReport();
  assert.deepEqual(report.current, d);
  assert.doesNotMatch(diagnostics.diagnosticText(report), /原始业务错误/);
});
test('a committed import failure tells the user to check the result before retrying', () => {
  const line = core.importLine({status: 'error', kind: 'archive', error: '登记失败', diagnostic: {id: 'D-committed', stored: true, publication: 'committed', retry: 'check_state'}});
  assert.match(line, /图库已更新/); assert.match(line, /核对结果后再重试/); assert.match(line, /D-committed/);
});
test('malformed diagnostic metadata cannot replace the original HTTP error', async () => {
  globalThis.fetch = async () => ({ok: false, status: 400, headers: {get: () => 'invalid json'}, json: async () => ({error: '未知操作'})});
  await assert.rejects(core.api('/api/cache'), e => e.said === '未知操作' && e.message === '未知操作');
});

test('publication reports what happened to the library, when applicable', () => {
  const line = publication => diagnostics.diagnosticLine({id: 'D-state', publication});
  assert.equal(line('not_applicable'), '错误编号：D-state');
  assert.match(line('not_committed'), /图库没有改动/);
  assert.match(line('may_be_partial'), /图库可能已改动一部分/);
  assert.match(line('unknown'), /图库是否改动尚不确定/);
});

test('diagnostics read as sentences in Chinese and English and appear once in the task panel', async () => {
  const {taskList} = await web.load('ui/tasks.js');
  const english = require('../web/static/locales/en.json');
  const {I18N, state} = core;
  const {language, table} = I18N;
  const {fetch} = globalThis;
  const before = state.importTask;
  const d = {id: 'D-test', publication: 'not_committed', retry: 'check_state', stored: false};
  try {
    for (const [code, translations, failure, expected, message] of [
      ['zh-CN', {}, '请求失败！', '错误编号：D-test。图库没有改动。核对结果后再重试。诊断记录未保存。', '请求失败！错误编号：D-test。诊断记录未保存。'],
      ['en', english, 'Request failed!', 'Error ID: D-test. No changes to the library. Check the result before retrying. Diagnostic record was not saved.',
        'Request failed! Error ID: D-test. Diagnostic record was not saved.'],
    ]) {
      Object.assign(I18N, {language: code, table: translations});
      state.importTask = {status: 'error', kind: 'archive', error: failure, diagnostic: d};
      assert.equal(diagnostics.diagnosticLine(d), expected);
      const row = taskList().find(task => task.key === 'import');
      assert.doesNotMatch(row.line, /D-test/);
      assert.equal(row.body, `<p class="task-side">${expected}</p>`);
      assert.ok(core.importLine(state.importTask).endsWith(expected));
      globalThis.fetch = async () => ({ok: false, status: 500, headers: {get: () => JSON.stringify(d)}, json: async () => ({error: failure})});
      await assert.rejects(core.api('/api/settings'), {message});
    }
  } finally {
    Object.assign(I18N, {language, table});
    state.importTask = before;
    globalThis.fetch = fetch;
  }
});
