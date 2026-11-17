// 编辑资料里的「所属作品」（ui/palette.js 的 editProfile，#464）：角色原本没有作品时，选「新建作品」或 Danbooru 作品
// 再保存，要把作品带进 POST /api/character/profile；选回同一个已有作品才算没改，不发请求。
// 对话框、输入框和下拉菜单用 web.cjs 的普通对象拼成；搜索的 160 毫秒防抖改成立即执行。
const {test, before} = require('node:test');
const assert = require('node:assert/strict');
const web = require('./web.cjs');
let palette;
const tick = () => new Promise(r => setImmediate(r));

before(async () => {
  await web.ready();
  web.fakePage();
  globalThis.window = {addEventListener() {}};
  globalThis.ResizeObserver = class { observe() {} };
  globalThis.requestAnimationFrame = () => 0;
  const store = new Map();
  globalThis.localStorage = {getItem: k => store.has(k) ? store.get(k) : null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  palette = await web.load('ui/palette.js');
});

// 打开 editProfile，返回 {d, posts}。`profile` 覆盖 GET /api/character/profile 的回应；`works` 和 `remote` 是本地和
// Danbooru 作品搜索的结果。对话框的 querySelector 按它和已建元素的 innerHTML 里出现过的 id 建元素，所以没画出的控件
// （label_only 时的 #pfTag）是 null，和页面一样。
async function open(profile, {works = [], remote = []} = {}) {
  const posts = [];
  globalThis.fetch = async (path, options = {}) => {
    const reply = body => ({status: 200, ok: true, headers: {get: () => null}, json: async () => body});
    if (options.method === 'POST') { posts.push({path, body: JSON.parse(options.body)}); return reply({undo: {label: '编辑资料'}}); }
    if (path.startsWith('/api/character/profile')) return reply({id: 'c1', name: '合成角色', user_name: '合成角色', aliases: [], other_aliases: 0,
      tag: '', label_only: true, oc: false, work_id: null, work_name: '', ...profile});
    if (path.startsWith('/api/works')) return reply(path.includes('remote=1') ? remote : works);
    return reply({});
  };
  const doc = globalThis.document, create = doc.createElement;
  let d;
  doc.createElement = tag => {
    if (tag !== 'dialog') return create(tag);
    const nodes = new Map();
    d = Object.assign(new web.FakeElement('dialog'), {
      querySelector(sel) {
        if (!nodes.has(sel)) {
          const id = sel.startsWith('#') ? sel.slice(1) : '';
          const html = this.innerHTML + [...nodes.values()].map(n => n.innerHTML).join('');
          if (id && !html.includes(`id="${id}"`)) return null;
          nodes.set(sel, new web.FakeElement(id ? 'input' : sel, id));
        }
        return nodes.get(sel);
      },
    });
    return d;
  };
  const setTimeout = globalThis.setTimeout; globalThis.setTimeout = (fn, ms) => ms === 160 ? (fn(), 0) : setTimeout(fn, ms);
  try { await palette.editProfile({id: 'c1'}); } finally { doc.createElement = create; }
  d.querySelector('#pfName').value = d.innerHTML.match(/id="pfName"[^>]* value="([^"]*)"/)[1];  // 页面上 value 属性给出的初值
  return {d, posts, restore: () => { globalThis.setTimeout = setTimeout; }};
}
// 在「所属作品」里输入 text，在下拉菜单里点名为 label 的一项（按下时焦点留在输入框里，松开时选中，#432）。
async function pick(d, text, label) {
  const input = d.querySelector('#pfWork'), menu = d.querySelector('#pfWorkMenu');
  input.value = text; for (const fn of input.listeners.input) fn();
  await tick(); await tick(); await tick();
  const labels = [...menu.innerHTML.matchAll(/data-i="(\d+)"><span class="watch-name">([^<]*)</g)];
  const hit = labels.find(m => m[2] === label);
  assert.ok(hit, `菜单里没有「${label}」：${labels.map(m => m[2]).join('、')}`);
  const target = {closest: () => ({dataset: {i: hit[1]}})};
  for (const fn of menu.listeners.mousedown) fn({target, preventDefault() {}});
  for (const fn of menu.listeners.click) fn({target, type: 'click', preventDefault() {}});
}
const save = async d => { await d.querySelector('form').onsubmit({preventDefault() {}}); await tick(); };

test('原本没作品 → 新建作品：保存时带上 {create}', async () => {
  const {d, posts, restore} = await open({});
  try {
    await pick(d, '合成作品', '新建作品「合成作品」');
    await save(d);
    assert.deepEqual(posts.map(x => x.body), [{entity: 'c1', work: {create: '合成作品'}}]);
  } finally { restore(); }
});
test('原本没作品 → Danbooru 作品：保存时带上 {tag}', async () => {
  const {d, posts, restore} = await open({}, {remote: [{tag: 'synthetic_work', posts: 12}]});
  try {
    await pick(d, 'synthetic', 'synthetic work');  // 菜单里显示的是 workName(tag)
    await save(d);
    assert.deepEqual(posts.map(x => x.body), [{entity: 'c1', work: {tag: 'synthetic_work'}}]);
  } finally { restore(); }
});
test('原本没作品 → 已有作品：带上作品 id', async () => {
  const {d, posts, restore} = await open({}, {works: [{id: 'w2', name: '已有作品', tag: '', count: 3}]});
  try {
    await pick(d, '已有', '已有作品');
    await save(d);
    assert.deepEqual(posts.map(x => x.body), [{entity: 'c1', work: 'w2'}]);
  } finally { restore(); }
});
test('选回原来的作品不算改动，什么都没改就不发请求', async () => {
  const {d, posts, restore} = await open({work_id: 'w1', work_name: '原作品'}, {works: [{id: 'w1', name: '原作品', tag: '', count: 3}]});
  try {
    await pick(d, '原作', '原作品');
    await save(d);
    assert.deepEqual(posts, []);
    assert.equal(d.open, false);
  } finally { restore(); }
});
test('原本有作品 → 新建作品', async () => {
  const {d, posts, restore} = await open({work_id: 'w1', work_name: '原作品'});
  try {
    await pick(d, '另一部', '新建作品「另一部」');
    await save(d);
    assert.deepEqual(posts.map(x => x.body), [{entity: 'c1', work: {create: '另一部'}}]);
  } finally { restore(); }
});
test('原本有作品 → 不归属任何作品：带上空字符串', async () => {
  const {d, posts, restore} = await open({work_id: 'w1', work_name: '原作品'});
  try {
    d.querySelector('#pfNoWork').listeners.click[0]();
    await save(d);
    assert.deepEqual(posts.map(x => x.body), [{entity: 'c1', work: ''}]);
  } finally { restore(); }
});
