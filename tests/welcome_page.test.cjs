// #95: the desktop welcome page. Picked folders are checked on the spot; a folder already read with another of the
// same origin goes, an overlap across origins is pointed out; a failed import or creation keeps what was filled in.
const {test} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const vm = require('node:vm');
const zh = require('./i18n_zh.cjs');   // t() of a Chinese page (#173)
const html = readFileSync(require('node:path').join(__dirname, '../desktop/dist/index.html'), 'utf8');
const script = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].pop()[1];
const section = script.slice(script.indexOf('// 欢迎页：'), script.indexOf('// 外壳在服务启动失败时调用'));

function page(answers) {
  const nodes = {}, calls = [];
  const c = {
    calls, nodes, setInterval: () => 0, clearInterval() {},
    document: {getElementById: id => nodes[id] ||= {innerHTML: '', value: ''}, body: {classList: {add() {}, remove() {}}}},
    busy() {}, window: {},
    invoke: async (cmd, args) => { calls.push([cmd, args]); const a = answers[cmd]; return typeof a === 'function' ? a(args) : a; },
  };
  vm.createContext(Object.assign(c, {t: zh}));
  vm.runInContext(section, c);
  c.window.showWelcome({origins: [{id: 'x', label: 'X'}, {id: 'pixiv', label: 'Pixiv'}], recent: [], home: '/Users/me/Pictures'});
  return c;
}
const ok = (path, images, more = {}) => ({path, status: 'ok', images, partial: false, unreadable: 0, inside: null, counted: true, ...more});
const click = (c, data) => c.nodes.welcome.onclick({target: {closest: () => ({dataset: data})}});

test('a folder inside another of the same origin is not added twice', async () => {
  let picked = ['/a'];
  const c = page({
    welcome_pick: () => ({paths: picked}),
    welcome_inspect: ({paths}) => paths.map(p => p === '/a/b' ? ok(p, 0, {inside: '/a', counted: false}) : ok(p, 5)),
  });
  await click(c, {pick: 'x'});
  picked = ['/a/b'];
  await click(c, {pick: 'x'});
  assert.deepEqual(JSON.parse(vm.runInContext('JSON.stringify(W.seq)', c)), [['x', '/a']]);
  assert.match(c.nodes.wcol.innerHTML, /已包含在.*\/a.*里，不重复添加/);
  assert.match(c.nodes.wcol.innerHTML, /5 张图片/);
});

test('an overlap across origins stays and is pointed out; empty and unreadable folders are explained', async () => {
  let picked = ['/a'];
  const c = page({
    welcome_pick: () => ({paths: picked}),
    welcome_inspect: ({paths}) => paths.map(p => ({'/a': ok(p, 3, {partial: true, unreadable: 2}), '/a/b': ok(p, 1, {inside: '/a'}),
      '/empty': ok(p, 0, {status: 'empty'}), '/locked': ok(p, 0, {status: 'denied'})}[p])),
  });
  await click(c, {pick: 'x'});
  picked = ['/a/b', '/empty', '/locked'];
  await click(c, {pick: 'pixiv'});
  const shown = c.nodes.wcol.innerHTML;
  assert.equal(vm.runInContext('W.seq.length', c), 4);
  assert.match(shown, /和「X」里的 <bdi>\/a<\/bdi> 重叠/);
  assert.match(shown, /至少 3 张图片，还没数完，2 处读不到/);
  assert.match(shown, /没有找到图片/);
  assert.match(shown, /没有访问权限/);
});

test('a failed import and a failed creation keep the form as it was filled in', async () => {
  const c = page({
    welcome_pick: ({kind}) => ({paths: kind === 'zip' ? ['/Downloads/old.pilib.zip'] : ['/a']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_import: () => { throw '导出文件里有不认识的内容，已拒绝导入'; },
    welcome_create: () => { throw '保存失败'; },
  });
  await click(c, {pick: 'x'});
  await click(c, {actW: 'import'});
  assert.equal(vm.runInContext('W.importing', c), null);
  assert.match(c.nodes.wcol.innerHTML, /已拒绝导入/);
  assert.deepEqual(JSON.parse(vm.runInContext('JSON.stringify(W.seq)', c)), [['x', '/a']]);
  await click(c, {actW: 'create'});
  assert.match(c.nodes.wcol.innerHTML, /保存失败/);
  assert.match(c.nodes.wcol.innerHTML, /data-drop="\/a"/);
  assert.deepEqual(JSON.parse(JSON.stringify(c.calls.find(([cmd]) => cmd === 'welcome_create')[1].sources)), [{kind: 'folder', path: '/a', preset: 'x'}]);
});

test('a library made but not listed is offered instead of making a second one (#97)', async () => {
  const c = page({
    welcome_pick: () => ({paths: ['/a']}),
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_create: () => ({created: '/Users/me/Pictures/Illustash/新图库.pilib', error: '写入失败（合成）'}),
    welcome_open: () => null,
  });
  await click(c, {pick: 'x'});
  await click(c, {actW: 'create'});
  const shown = c.nodes.wcol.innerHTML;
  assert.match(shown, /图库已经建好/);
  assert.match(shown, /写入失败（合成）/);
  assert.doesNotMatch(shown, /data-act-w="create"/, 'creating again is not the first offer');
  await click(c, {actW: 'open-created'});
  assert.deepEqual(JSON.parse(JSON.stringify(c.calls.at(-1))), ['welcome_open', {path: '/Users/me/Pictures/Illustash/新图库.pilib'}]);
  await click(c, {actW: 'create-again'});
  assert.match(c.nodes.wcol.innerHTML, /data-act-w="create"/);
});

test('a double click opens one picker and each import or creation carries its own request id (#115)', async () => {
  let choose;
  const c = page({
    welcome_pick: ({kind}) => kind === 'zip' ? new Promise(r => { choose = r; }) : {paths: ['/a']},
    welcome_inspect: ({paths}) => paths.map(p => ok(p, 2)),
    welcome_import: () => { throw '合成失败'; },
    welcome_create: () => { throw '合成失败'; },
  });
  const first = click(c, {actW: 'import'}), second = click(c, {actW: 'import'});
  await second;
  choose({paths: ['/Downloads/a.pilib.zip']});
  await first;
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_pick').length, 1);
  assert.equal(c.calls.filter(([cmd]) => cmd === 'welcome_import').length, 1);
  const again = click(c, {actW: 'import'});
  choose({paths: ['/Downloads/a.pilib.zip']});
  await again;
  await click(c, {pick: 'x'});
  await click(c, {actW: 'create'});
  const ids = c.calls.filter(([cmd]) => cmd === 'welcome_import' || cmd === 'welcome_create').map(([, a]) => a.request);
  assert.equal(ids.length, 3);
  assert.ok(ids.every(id => typeof id === 'string' && id.length > 4));
  assert.equal(new Set(ids).size, 3, 'a new click is a new request');
});
