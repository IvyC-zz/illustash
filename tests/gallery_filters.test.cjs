// 运行：node --test tests/gallery_filters.test.cjs
// 组合筛选与画廊（#445）：条件怎么读写（core/filters.js）、地址怎么带着条件（core/route.js、core/pages.js），以及画廊的封面
// 和张数怎么读（core/galleries.js）。回复照 #445 的接口方案、#590 的前端对接格式写；条件都是合成的。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
const c = {};
before(async () => {
  await web.ready();
  Object.assign(c, await web.load('core/filters.js'), await web.load('core/galleries.js'), {pages: await web.load('core/pages.js')}, web.core());
});
const reply = (body, status = 200) => ({status, ok: status < 400, json: async () => body, headers: {get: () => null}});
const json = list => JSON.stringify(list);

test('旧的单值参数算作 has 里的条件；在筛选条上一改就整体改写成 has', () => {
  const r = {page: 'library', tag: 'swimsuit', utag: '看过了', work: 'w1', artist: 'a1', has: ['source:pixiv'], not: []};
  assert.deepEqual(c.conditions(r).has, ['source:pixiv', 'tag:swimsuit', 'utag:看过了', 'work:w1', 'artist:a1']);
  const flipped = c.flipCondition(r, 'utag:看过了');
  assert.deepEqual([flipped.tag, flipped.utag, flipped.work, flipped.artist], ['', '', '', '']);
  assert.deepEqual(flipped.has, ['source:pixiv', 'tag:swimsuit', 'work:w1', 'artist:a1']);
  assert.deepEqual(flipped.not, ['utag:看过了']);
  // 再点一下改回「含」，排在 has 的末尾；× 两边都去掉
  assert.deepEqual(c.flipCondition(flipped, 'utag:看过了').has.at(-1), 'utag:看过了');
  const dropped = c.dropCondition(flipped, 'utag:看过了');
  assert.deepEqual([dropped.has.includes('utag:看过了'), dropped.not.includes('utag:看过了')], [false, false]);
  // 加一个已经在「不含」里的条件：挪到「含」，不重复
  const added = c.addCondition(flipped, 'utag:看过了');
  assert.deepEqual([added.has.filter(x => x === 'utag:看过了').length, added.not.length], [1, 0]);
  assert.equal(c.count(added), 5);
});

test('日期：年份展开成整年，换日期时去掉旧的 day；起止不对时拒绝', () => {
  const r = c.setDates({page: 'library', day: '2025-03-04'}, ...c.yearDates(2025));
  assert.deepEqual([r.from, r.to, r.day], ['2025-01-01', '2025-12-31', '']);
  assert.equal(c.datesLabel(r.from, r.to), '2025 年');
  assert.equal(c.datesLabel('2025-02-01', '2025-03-01'), '2025-02-01 至 2025-03-01');
  assert.equal(c.datesLabel('2025-02-01', ''), '2025-02-01 起');
  assert.equal(c.datesLabel('', '2025-02-01'), '2025-02-01 之前');
  assert.equal(c.validDates('2025-03-01', '2025-02-01'), false);
  assert.equal(c.validDates('', ''), false);
  assert.equal(c.validDates('2025-02-01', ''), true);
});

test('存成画廊的条件：完成标准的那组条件原样存下，角色页的角色（或造型）也是条件；排序和 R18 不存', () => {
  const r = {page: 'library', has: ['tag:swimsuit', 'tag:beach', 'source:pixiv'], not: ['utag:看过了'], from: '2025-01-01', to: '2025-12-31', sort: 'oldest', q: ''};
  const p = c.savedParams(r);
  assert.deepEqual(p, {has: json(['tag:swimsuit', 'tag:beach', 'source:pixiv']), not: json(['utag:看过了']), from: '2025-01-01', to: '2025-12-31'});
  assert.ok(c.savable(r));
  assert.equal(c.savable({page: 'library'}), false);
  assert.deepEqual(JSON.parse(c.savedParams({page: 'library', char: 'c1', variant: 'c1-swim', q: '海'}).has), ['char:c1-swim']);
  assert.equal(c.savedParams({page: 'library', q: '海'}).q, '海');
  // 重新打开：保存的条件回到路由上；顺序不同、后端规范化过的条件算相同
  const back = c.routeFields(p);
  assert.deepEqual([back.has, back.not, back.from, back.to], [r.has, r.not, r.from, r.to]);
  assert.ok(c.sameParams(p, {...p, has: json(['source:pixiv', 'tag:beach', 'tag:swimsuit'])}));
  assert.equal(c.sameParams(p, {...p, not: json([])}), false);
  assert.equal(c.sameParams(p, {...p, q: '海'}), false);
});

test('默认名按胶囊上的文字连起来，最多 60 字', () => {
  const r = {page: 'library', has: ['tag:swimsuit', 'source:pixiv'], not: ['utag:看过了'], from: '2025-01-01', to: '2025-12-31'};
  const label = x => ({'tag:swimsuit': '泳装', 'source:pixiv': 'Pixiv', 'utag:看过了': '# 看过了'})[x];
  assert.equal(c.defaultName(r, label), '泳装 · Pixiv · 不含 # 看过了 · 2025 年');
  const long = c.defaultName({page: 'library', has: Array.from({length: 12}, (_, i) => `tag:t${i}`)}, x => '很长的标签名字' + x);
  assert.equal([...long].length, 60);
  assert.ok(long.endsWith('…'));
});

test('条件写进地址：后退、前进照常；画廊页的地址总带 has；别的页面不带条件', () => {
  const {hashFor, parseHash} = c;
  const r = {page: 'library', has: ['tag:swimsuit', 'utag:a:b'], not: ['source:x'], from: '2025-01-01', to: ''};
  const hash = hashFor(r);
  globalThis.location = {hash};
  const back = parseHash();
  assert.deepEqual([back.page, back.has, back.not, back.from, back.to], ['library', r.has, r.not, '2025-01-01', '']);
  assert.equal(hashFor({page: 'library'}), '#/library');
  assert.equal(hashFor({page: 'gallery', id: '3', has: []}), '#/gallery/3?has=%5B%5D');
  globalThis.location = {hash: '#/gallery/3?has=%5B%22fav%22%5D&from=2024-01-01'};
  assert.deepEqual((({page, id, has, from}) => ({page, id, has, from}))(parseHash()), {page: 'gallery', id: '3', has: ['fav'], from: '2024-01-01'});
  // 资料库没有图库工具栏：条件不留在路由上
  globalThis.location = {hash: '#/characters?has=%5B%22fav%22%5D'};
  assert.deepEqual(parseHash().has, []);
  // 坏的 JSON 当没有
  globalThis.location = {hash: '#/library?has=%5Bnope'};
  assert.deepEqual(parseHash().has, []);
});

test('图库和画廊页发给 /api/assets 的参数带着条件', () => {
  const {pages} = c, r = {page: 'gallery', id: '3', has: ['tag:beach'], not: ['fav'], from: '2025-01-01', to: '2025-12-31', q: '海', tag: 'smile'};
  assert.deepEqual(pages.pageOf(r).params(r), {q: '海', tag: 'smile', has: json(['tag:beach']), not: json(['fav']), from: '2025-01-01', to: '2025-12-31'});
  assert.equal(pages.pageOf(r).click, 'browse');
  assert.equal(pages.pageOf({page: 'galleries'}).panel, true);
  // 有条件时空状态不是「这个角色已经没有图片了」
  assert.equal(pages.viewOf({page: 'library', char: 'c1', has: ['fav']}).empty({page: 'library', char: 'c1', has: ['fav']}), '没有符合条件的图片。');
});

test('封面按可见的画廊批量取；没有封面接口（#590 之前的 main）时退回读带封面的列表', async () => {
  const calls = [];
  globalThis.fetch = async path => {
    calls.push(path);
    if (path.startsWith('/api/galleries/covers?')) return reply([{id: 1, cover: 'u1'}, {id: 2, cover: null}]);
    return reply({error: '?'}, 404);
  };
  c.forgetCovers();
  assert.deepEqual(await c.coversFor([1, 2], 'hide'), {1: 'u1', 2: null});
  assert.equal(new URLSearchParams(calls[0].split('?')[1]).get('ids'), '[1,2]');
  assert.equal(new URLSearchParams(calls[0].split('?')[1]).get('nsfw'), 'hide');
  // 同一组、同一筛选只读一次
  await c.coversFor([1, 2], 'hide');
  assert.equal(calls.length, 1);
  calls.length = 0;
  globalThis.fetch = async path => {
    calls.push(path);
    if (path.startsWith('/api/galleries?')) return reply([{id: 1, name: 'a', params: {}, cover: 'u9', cover_manual: false}, {id: 3, name: 'b', params: {}, cover: 'u3', cover_manual: true}]);
    return reply({error: '没有这个接口'}, 404);
  };
  c.forgetCovers();
  assert.deepEqual(await c.coversFor([1, 2], ''), {1: 'u9'});
  assert.deepEqual(calls.map(p => p.split('?')[0]), ['/api/galleries/covers', '/api/galleries']);
  // 列表里也没有封面：报的是封面接口自己的错
  globalThis.fetch = async path => path.startsWith('/api/galleries?') ? reply([{id: 1, name: 'a', params: {}, cover_manual: false}]) : reply({error: '读取失败'}, 500);
  c.forgetCovers();
  await assert.rejects(c.coversFor([1]), /读取失败/);
});

test('张数：条件失效时回 400，给出原因而不是报错', async () => {
  globalThis.fetch = async path => path.includes('work%3Agone') ? reply({error: '作品不存在'}, 400) : reply({total: 12});
  assert.deepEqual(await c.countOf({has: json(['tag:beach'])}, 'hide'), {n: 12});
  assert.deepEqual(await c.countOf({has: json(['work:gone'])}, ''), {error: '作品不存在'});
});

test('写入之后的列表：回复里的画廊换进原来的位置，删除时去掉', () => {
  const {state} = c;
  state.galleries = [{id: 1, name: 'a'}, {id: 2, name: 'b'}];
  c.setGallery(2, {id: 2, name: 'b2'});
  c.setGallery(5, {id: 5, name: 'new'});
  assert.deepEqual(state.galleries.map(g => g.name), ['a', 'b2', 'new']);
  c.setGallery('1', null);
  assert.deepEqual(state.galleries.map(g => g.id), [2, 5]);
  assert.equal(c.galleryOf('5').name, 'new');
});
