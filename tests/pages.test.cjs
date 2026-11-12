const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const {readFileSync} = require('node:fs');
const path = require('node:path');
const web = require('./web.cjs');
// 页面注册表（core/pages.js，#342）：docs/WEB_UI_MAP.md 2.1 总表的每一行都能从注册表读出，并与文档一致；
// 注册表派生出的 cleanup()／browsing()／侧栏高亮／地址解析与改造前手写的判断逐页相同。
let pages, route, state;
before(async () => {
  await web.ready();
  pages = await web.load('core/pages.js');
  ({state} = web.core()); route = web.core();
  globalThis.location = {hash: ''};
});

// 总表一行：页面名（文档里的第一列）→ 示例地址、注册表里的页面名，以及从文档读出来要对上的值。
// doc 是地址列里应当出现的文字。
const ROWS = {
  '首页': {hash: '#/', page: 'home', doc: '`#/`'},
  '图库': {hash: '#/library?tag=x', page: 'library', doc: '`#/library`'},
  '角色页': {hash: '#/library?char=c1', page: 'library', char: true, doc: '`#/library?char='},
  '资料库 · 角色': {hash: '#/characters', page: 'characters', doc: '`#/characters`'},
  '资料库 · 作品': {hash: '#/characters/works', page: 'characters', doc: '`#/characters/works`'},
  '设置': {hash: '#/settings/data', page: 'settings', doc: '`#/settings/'},
  '未识别 · 全部': {hash: '#/unmatched', page: 'unmatched', doc: '`#/unmatched`'},
  '未识别 · 已跳过': {hash: '#/skipped', page: 'skipped', doc: '`#/skipped`'},
  '未识别 · 按组整理': {hash: '#/groups', page: 'groups', doc: '`#/groups`'},
  '未识别 · 第 N 组': {hash: '#/g/12', page: 'group', doc: '`#/g/'},
  '重复 · 全部 / 角色不一致': {hash: '#/duplicates?dup=mixed', page: 'duplicates', doc: '`#/duplicates'},
  '重复 · 相似的图': {hash: '#/similar?kind=near', page: 'similar', doc: '`#/similar'},
  '重复 · 相似组 N': {hash: '#/s/7', page: 'sgroup', doc: '`#/s/'},
  '已排除 · 全部 / 按原因': {hash: '#/excluded?reason=screenshot', page: 'excluded', doc: '`#/excluded'},
  '已排除 · 待确认': {hash: '#/pending', page: 'pending', doc: '`#/pending`'},
  '新加入': {hash: '#/recent', page: 'recent', doc: '`#/recent`'},
  '随机抽查': {hash: '#/audit', page: 'audit', doc: '`#/audit`'},
  '身份整理': {hash: '#/identity', page: 'identity', doc: '`#/identity`'},
  '画师抽查': {hash: '#/artist-audit', page: 'artist-audit', doc: '`#/artist-audit`'},
};
// 文档里的文字 → 注册表的取值。认不出的文字让测试失败，提醒同时改两边。
const CLICK = [[/^浏览/, 'browse'], [/^整理/, 'cleanup'], [/^\*\*选中\*\*/, 'select']];
const NAV = [[/^首页/, 'home'], [/^图库/, 'library'], [/^资料库/, 'characters'], [/^未识别/, 'unmatched'], [/^重复/, 'duplicates'], [/^已排除/, 'excluded'],
  [/^新加入/, 'recent'], [/^随机抽查/, 'audit'], [/^身份整理/, 'identity'], [/^无（设置链接高亮）/, 'settings'], [/^无/, '']];
const TOOLBAR = [[/^空$/, 'none'], [/^仅子页签$/, 'tabs'], [/^角色｜作品/, 'index'], [/^搜索、/, 'library'], [/^同图库/, 'library'], [/^(子页签|三个子页签|排序)/, 'grid'], [/^\*\*排序/, 'grid']];
const first = (table, text, what) => { const hit = table.find(([re]) => re.test(text)); assert.ok(hit, `文档里的${what}「${text}」认不出来，改了文档要同步改 tests/pages.test.cjs`); return hit[1]; };

function docRows() {
  const text = readFileSync(path.join(__dirname, '../docs/WEB_UI_MAP.md'), 'utf8'), start = text.indexOf('### 2.1 总表');
  const lines = text.slice(start).split('\n').slice(2);
  const rows = [];
  for (const line of lines) {
    if (!line.startsWith('|')) break;
    const cells = line.split('|').slice(1, -1).map(c => c.trim());
    if (cells[0] === '页面' || /^-+$/.test(cells[0])) continue;
    rows.push(cells);
  }
  return rows;
}
const spec = hash => { globalThis.location.hash = hash; const r = route.parseHash(); return {r, view: pages.viewOf(r)}; };

test('docs/WEB_UI_MAP.md 2.1 总表的每一行都能从注册表读出，且与文档一致', () => {
  const rows = docRows();
  assert.deepEqual(rows.map(c => c[0]), Object.keys(ROWS), '总表的行和测试里的清单一一对应');
  let before = '';
  for (const [label, address, area, toolbarCell, selection, inspector, , nav] of rows) {
    const toolbar = toolbarCell === '同上' ? before : toolbarCell;   // 「同上」是上一行的工具栏
    before = toolbar;
    const want = ROWS[label], {r, view} = spec(want.hash);
    assert.equal(r.page, want.page, `${label}：地址 ${want.hash} 落在哪一页`);
    assert.equal(!!r.char, !!want.char, `${label}：是不是角色页`);
    assert.ok(address.includes(want.doc), `${label}：地址列 ${address} 里应有 ${want.doc}`);
    assert.equal('#' + view.area, area.replace(/`/g, ''), `${label}：主区`);
    assert.equal(view.nav, first(NAV, nav, '侧栏高亮'), `${label}：侧栏高亮`);
    assert.equal(view.toolbar, first(TOOLBAR, toolbar, '工具栏'), `${label}：工具栏`);
    // 选择栏、详情栏：网格页才有（首页可以点图开详情栏）
    assert.equal(selection.startsWith('有'), view.area === 'grid', `${label}：选择栏`);
    assert.equal(inspector.startsWith('有'), view.area === 'grid' || view.area === 'homePage', `${label}：详情栏`);
  }
});

test('文档点击列里没有归到浏览／整理／选中的，注册表里是页面自己处理', () => {
  for (const [label, , , , , , click] of docRows()) {
    const {view} = spec(ROWS[label].hash), kind = CLICK.find(([re]) => re.test(click))?.[1] || 'own';
    assert.equal(view.click, kind, label);
  }
});

test('注册表里的每个页面都在总表里有一行，反过来也是', () => {
  const inTable = new Set(Object.values(ROWS).map(x => x.page));
  for (const name of pages.pageNames()) assert.ok(inTable.has(name), `${name} 没有出现在 docs/WEB_UI_MAP.md 2.1 总表（新增页面要加一行）`);
  for (const name of inTable) assert.ok(pages.PAGES[name], `${name} 没有在注册表里登记`);
});

// ---------- 与改造前手写判断逐页相同 ----------
const OLD = {
  cleanup: ['unmatched', 'skipped', 'excluded', 'pending', 'group', 'audit'],
  browsing: ['library', 'duplicates', 'recent'],
  panel: ['home', 'characters', 'settings', 'groups', 'similar', 'identity'],
  lead: ['unmatched', 'skipped', 'group'],
  nav: page => ({skipped: 'unmatched', groups: 'unmatched', group: 'unmatched', pending: 'excluded', similar: 'duplicates', sgroup: 'duplicates'})[page] || page,
  valid: ['home', 'unmatched', 'skipped', 'duplicates', 'excluded', 'pending', 'recent', 'audit', 'groups', 'similar', 'identity', 'artist-audit'],
};
test('cleanup()、browsing()、panelPage、LEAD_PAGES、侧栏高亮和改造前的手写清单一致', () => {
  const {cleanup, browsing, defaultSort} = route, {prefs} = web.core();
  for (const name of pages.pageNames()) for (const char of ['', 'c1']) {
    state.route = {page: name, char};
    assert.equal(cleanup(), !!char || OLD.cleanup.includes(name), `cleanup ${name} ${char}`);
    assert.equal(browsing(), !char && OLD.browsing.includes(name), `browsing ${name} ${char}`);
    // 角色筛选只存在于图库
    const view = pages.viewOf({page: name, char});
    assert.equal(!!pages.PAGES[name].panel, OLD.panel.includes(name), `panel ${name}`);
    assert.equal(!!(!char && view.lead), OLD.lead.includes(name) && !char, `lead ${name} ${char}`);
    // 侧栏里没有 settings、artist-audit 这两项的 .nav：改造前它们不会点亮任何一项；设置另点亮底部的链接
    if (!char) assert.equal(view.nav, name === 'settings' ? 'settings' : name === 'artist-audit' ? '' : OLD.nav(name), `nav ${name}`);
  }
  // 默认排序：角色页偏好、待认页共用偏好、其余图库偏好
  assert.equal(defaultSort({page: 'library', char: 'c1'}), prefs.get('sortCharacter', 'suspicious'));
  assert.equal(defaultSort({page: 'unmatched'}), prefs.get('sortUnmatched', prefs.get('sortLibrary', 'newest')));
  assert.equal(defaultSort({page: 'excluded'}), prefs.get('sortLibrary', 'newest'));
  // 现状，见已知问题 2、4：artist-audit 的主区是 #groupsPage，却不算面板页；sgroup 既不是浏览也不是整理
  assert.equal(pages.PAGES['artist-audit'].area, 'groupsPage');
  assert.equal(pages.PAGES['artist-audit'].panel, false);
  state.route = {page: 'sgroup', char: ''};
  assert.deepEqual([route.cleanup(), route.browsing(), pages.PAGES.sgroup.click], [false, false, 'select']);
  // 现状，见已知问题 1：新加入页的侧栏入口存在于声明里（index.html 里是隐藏的）
  assert.equal(pages.PAGES.recent.nav, 'recent');
});

test('地址：能认出的页面名、带编号的页面、子页和旧角色地址；写回去再读得到同一页', () => {
  const reads = {
    '': 'home', '#': 'home', '#/': 'home', '#/?artist=a1': 'library', '#/home': 'home', '#/library': 'library', '#/nope': 'library',
    '#/characters': 'characters', '#/characters/works': 'characters', '#/settings/network': 'settings', '#/g/3': 'group', '#/s/4': 'sgroup', '#/c/c9/skin': 'library',
    '#/g': 'library', '#/s/': 'library', '#/sgroup': 'library', '#/group': 'library', '#/character': 'library',
  };
  for (const name of OLD.valid) reads['#/' + name] = name;
  for (const [hash, page] of Object.entries(reads)) { const {r} = spec(hash); assert.equal(r.page, page, hash); }
  const legacy = spec('#/c/c9/skin').r;
  assert.deepEqual([legacy.char, legacy.variant, legacy.legacy], ['c9', 'skin', true]);
  assert.equal(spec('#/g/a%20b').r.id, 'a b');
  assert.equal(spec('#/characters/works').r.section, 'works');
  // 只有图库带角色、画师、检查这些筛选
  assert.equal(spec('#/unmatched?char=c1&artist=a').r.char, '');
  assert.equal(spec('#/library?char=c1').r.char, 'c1');
  for (const hash of ['#/home', '#/library?day=2026-10-02', '#/characters/works', '#/settings/data', '#/g/3', '#/s/4', '#/groups', '#/duplicates?dup=mixed', '#/excluded?reason=screenshot', '#/library?char=c1&variant=v']) {
    const {r} = spec(hash);
    assert.equal(route.hashFor(r), hash, hash);
  }
  assert.equal(route.charHash('c1', 'v'), '#/library?char=c1&variant=v');
});

test('implement() 只接受登记过的页面名；每个页面都有标题以外的必要字段', () => {
  assert.throws(() => pages.implement({nope: {}}), /没有登记过的页面/);
  for (const name of pages.pageNames()) {
    const p = pages.PAGES[name];
    assert.ok(['grid', 'homePage', 'charIndex', 'settingsPage', 'groupsPage'].includes(p.area), `${name}.area`);
    assert.ok(['browse', 'cleanup', 'select', 'own'].includes(p.click), `${name}.click`);
    assert.ok(['none', 'tabs', 'index', 'library', 'grid'].includes(p.toolbar), `${name}.toolbar`);
    if (p.toolbar === 'grid' || p.toolbar === 'library') assert.ok(p.sorts.every(s => ['suspicious', 'newest', 'oldest', 'hinted'].includes(s)), `${name}.sorts`);
    if (p.tabs) assert.ok(p.tab, `${name}.tab`);
    if (p.pool) assert.equal(p.area, 'grid', `${name}：有 pool 的页面用网格`);
  }
});

test('后端查询参数：pool、分组编号、筛选都从注册表来', () => {
  const params = (page, extra = {}) => { const p = pages.PAGES[page]; return {...(p.pool ? {pool: p.pool} : {}), ...(p.params?.({page, ...extra}) || {})}; };
  assert.deepEqual(params('unmatched'), {pool: 'unmatched'});
  assert.deepEqual(params('skipped'), {pool: 'skipped'});
  assert.deepEqual(params('duplicates', {dup: 'mixed'}), {pool: 'duplicates', dup: 'mixed'});
  assert.deepEqual(params('duplicates'), {pool: 'duplicates'});
  assert.deepEqual(params('excluded', {reason: 'screenshot'}), {pool: 'excluded', reason: 'screenshot'});
  assert.deepEqual(params('pending'), {pool: 'pending'});
  assert.deepEqual(params('recent'), {pool: 'recent'});
  assert.deepEqual(params('audit'), {pool: 'audit'});
  assert.deepEqual(params('group', {id: '3'}), {pool: 'cluster', cluster: '3'});
  assert.deepEqual(params('sgroup', {id: '4'}), {pool: 'similar', cluster: '4'});
  assert.deepEqual(pages.PAGES.library.params({day: 'd', work: 'w', q: 'q', tag: 't', utag: 'u'}), {day: 'd', work: 'w', q: 'q', tag: 't', utag: 'u'});
  assert.equal(pages.poolIs({page: 'skipped'}, 'unmatched', 'skipped'), true);
  assert.equal(pages.poolIs({page: 'group'}, 'unmatched', 'skipped'), false);
});
