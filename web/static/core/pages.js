// 页面注册表（#342）：每个页面一个声明，其余代码从这里读「这是哪类页面」，不再各自写页面名判断。
// 这里只有数据和不碰 DOM 的小函数：地址形态、主区、点击语义、侧栏高亮归属、子页签、工具栏组成、后端 pool 参数、
// 空状态文案、底栏提示、选择栏种类。需要 DOM 或 ui/ 的部分（渲染入口、标题、数据到达后的重画）由 ui/ 的各模块
// 在加载时用 implement() 登记进来，所以 core/ 不反向依赖 ui/。
// 新增一个页面要做什么，见 docs/WEB_UI_MAP.md「新增一个页面」。
import {isMac} from './format.js';
import {t} from './i18n.js';

// 字段：
//   prefix     地址里的前缀，#/g/<id>、#/s/<id>；没有前缀的页面用页面名作路径（#/unmatched）
//   sections   地址可以带子页（#/characters/works、#/settings/library）
//   root       不带路径也不带参数的地址（#/）落在这页
//   area       主区：'grid' 或 index.html 里的某个面板 id
//   panel      不用 #grid 的页面：reload()、R、同步到达新图、R18 切换后的重绘都按它判断
//   keys       单图快捷键在这页的范围：'grid' 网格页全部生效（主区是 #grid 的页面不写也是它）；'shown' 只对详情栏或大图里
//              那张图生效，没有网格光标（首页）；主区不是网格、也没写的页面不响应（keyScope）
//   click      点卡片做什么：'browse' 打开详情（选择要主动进入）、'cleanup' 切换选中、'own' 页面自己处理
//   nav        侧栏哪一项亮（.nav 的 data-page；设置是侧栏底部的链接）；'' 没有
//   lead       未识别一类的「待认」页（全部未识别、已跳过、组页）：排序偏好共用、默认可按「有线索优先」排；在这些页上设角色之后
//              那张图会离开本页（详情栏、大图、卡片「+ 角色」建议都先飞走再消失）
//   skip       能跳过当前这张的页面（全部未识别、组页）：详情栏对还没有角色的图显示「跳过 S」，不选图时 S 跳过当前这张
//   tabs/tab   同一个侧栏入口下的子页签组，以及本页对应哪个页签
//   toolbar    工具栏组成：'none' 空、'tabs' 只有子页签、'index' 资料库、'library' 图库（搜索和筛选）、'grid' 网格页
//   sorts      排序下拉的选项
//   pool/params 后端 /api/assets 的 pool 参数，以及该页其余查询参数
//   bar        选择栏的种类；同时决定 S／X／Y／N 几个快捷键在这页是否生效
//   card       卡片上额外的角标
//   directory  需要完整的角色目录（不是摘要行）
//   guideTick  进入本页算上手清单的哪一步
const NEWEST = ['newest', 'oldest'];
const narrowed = r => !!(r.q || r.tag || r.utag || r.check || r.doubt);

// 图库和角色页共用的一组字段：角色页是图库加 char 参数，不是独立页面。
const libraryBase = {
  area: 'grid', panel: false, nav: 'library', toolbar: 'library', sorts: NEWEST,
  params: r => {
    const f = {};
    if (r.day) f.day = r.day; if (r.work) f.work = r.work; if (r.artist) f.artist = r.artist; if (r.q) f.q = r.q; if (r.tag) f.tag = r.tag; if (r.utag) f.utag = r.utag;
    return f;
  },
  empty: () => t('没有符合条件的图片。试试别的搜索或标签。'),
  hint: () => t('点击查看 · 拖动或 {key} 点击多选 · Space 看大图', {key: isMac ? '⌘' : 'Ctrl'}),
  bar: 'default',
};

const list = [
  {name: 'home', root: true, area: 'homePage', panel: true, keys: 'shown', click: 'own', nav: 'home', toolbar: 'none'},
  {name: 'library', ...libraryBase, click: 'browse'},
  {name: 'characters', sections: true, area: 'charIndex', panel: true, click: 'own', nav: 'characters', toolbar: 'index', directory: true, guideTick: 'look'},
  {name: 'settings', sections: true, area: 'settingsPage', panel: true, click: 'own', nav: 'settings', toolbar: 'none'},
  {name: 'unmatched', area: 'grid', panel: false, click: 'cleanup', nav: 'unmatched', lead: true, tabs: 'unmatched', tab: 'all',
    toolbar: 'grid', sorts: ['hinted', ...NEWEST], pool: 'unmatched', bar: 'unmatched', skip: true,
    empty: () => t('全部认出来了 🎉'), hint: () => t('点击或拖动选择 · Space 看大图 · C 设为角色 · S 跳过'), card: {hintMin: 70}},
  {name: 'skipped', area: 'grid', panel: false, click: 'cleanup', nav: 'unmatched', lead: true, tabs: 'unmatched', tab: 'skipped',
    toolbar: 'grid', sorts: ['hinted', ...NEWEST], pool: 'skipped', bar: 'skipped',
    empty: () => t('没有跳过的图片。'), hint: () => t('点击或拖动选择 · Space 看大图')},
  {name: 'groups', area: 'groupsPage', panel: true, click: 'own', nav: 'unmatched', tabs: 'unmatched', tab: 'groups', toolbar: 'tabs'},
  {name: 'group', prefix: 'g', area: 'grid', panel: false, click: 'cleanup', nav: 'unmatched', lead: true, tabs: 'unmatched', tab: 'groups',
    toolbar: 'grid', sorts: ['hinted', ...NEWEST], bar: 'unmatched', skip: true,
    pool: 'cluster', params: r => ({cluster: r.id}), empty: () => t('这一组都整理完了。'),
    hint: () => t('点击或拖动选择 · Space 看大图 · C 设为角色 · S 跳过')},
  {name: 'duplicates', area: 'grid', panel: false, click: 'browse', nav: 'duplicates', tabs: 'duplicates', tab: 'all',
    toolbar: 'grid', sorts: NEWEST, pool: 'duplicates', bar: 'default', card: {dup: true},
    params: r => r.dup ? {dup: r.dup} : {}, empty: r => r.dup ? t('所有副本的角色都一致。') : t('没有重复的图片。'),
    hint: () => t('同色角标为同一组 · 修改会同步到整组 · 角色不一致的组对齐后自动折叠')},
  {name: 'similar', area: 'groupsPage', panel: true, click: 'own', nav: 'duplicates', tabs: 'duplicates', tab: 'similar', toolbar: 'tabs'},
  {name: 'sgroup', prefix: 's', area: 'grid', panel: false, click: 'cleanup', nav: 'duplicates', tabs: 'duplicates', tab: 'similar',
    toolbar: 'grid', sorts: NEWEST, pool: 'similar', params: r => ({cluster: r.id}), bar: 'default',
    hint: () => t('点击或拖动选择 · Space 看大图')},
  {name: 'excluded', area: 'grid', panel: false, click: 'cleanup', nav: 'excluded', tabs: 'excluded', tab: 'all',
    toolbar: 'grid', sorts: NEWEST, pool: 'excluded', bar: 'excluded', card: {reason: true},
    params: r => r.reason ? {reason: r.reason} : {}, empty: () => t('没有被排除的图片。'), hint: () => t('点击或拖动选择 · Space 看大图 · X 恢复收录')},
  {name: 'pending', area: 'grid', panel: false, click: 'cleanup', nav: 'excluded', tabs: 'excluded', tab: 'pending', toolbar: 'grid', sorts: NEWEST, pool: 'pending', bar: 'pending',
    empty: () => t('没有待确认的图片。'), hint: () => t('点击或拖动选择 · Space 看大图 · X 排除')},
  {name: 'audit', area: 'grid', panel: false, click: 'cleanup', nav: 'audit', toolbar: 'grid', sorts: NEWEST, pool: 'audit', bar: 'audit', card: {audit: true},
    empty: () => t('抽查做完了，谢谢！结果会用来衡量认人准不准。'), hint: () => t('点一张或 Space 看大图 · Y 是 · N 不是 · 看不清可以跳过，换一张')},
  {name: 'identity', area: 'groupsPage', panel: true, click: 'own', nav: 'identity', toolbar: 'none'},
  {name: 'artist-audit', area: 'groupsPage', panel: true, click: 'own', nav: '', toolbar: 'none'},
];

// 不再有的页面的旧地址 → 现在落在哪一页。新加入（#/recent）在 2026-10-02 由首页接手，页面本身在 #353 删掉。
const RETIRED = {recent: 'home'};

// 角色页：不是 route.page，而是 library 加 char 参数。点击即选择；排序多「可疑优先」；旧地址 #/c/<id>/<皮肤> 改写成它。
const character = {name: 'character', virtual: true, legacyPrefix: 'c', ...libraryBase, click: 'cleanup', sorts: ['suspicious', ...NEWEST],
  empty: r => narrowed(r) ? PAGES.library.empty(r) : t('这个角色已经没有图片了。'),
  hint: () => t('点击或拖动选择 · Space 看大图 · ⌫ 不是 TA · C 换成')};

export const PAGES = Object.fromEntries(list.map(p => [p.name, p]));
export const CHARACTER = character;
// 路由里的页面名没登记时（只有测试会这样）按没有特殊性质的网格页处理。
const UNKNOWN = {name: '', area: 'grid', panel: false, click: 'own', nav: '', toolbar: 'grid', sorts: NEWEST, bar: 'default'};
export const pageOf = route => PAGES[route.page] || UNKNOWN;
// 屏幕上看到的那一页：角色筛选下是角色页，否则就是 route.page。char 默认取路由的 char；渲染标题和元数据重画时
// 要看角色有没有读到，传 state.char。
export const viewOf = (route, char = route.char) => char ? CHARACTER : pageOf(route);
export const pageNames = () => list.map(p => p.name);
// 单图快捷键在这页的范围（见上面的 keys 字段）：'grid'、'shown' 或 ''。
export const keyScope = view => view.keys || (view.area === 'grid' ? 'grid' : '');
// 本页的后端 pool 是不是这几个之一（条目离开本页的判断：设为角色之后离开未识别、排除之后离开已排除……）。
// 异步回应回来时：用户是不是还在（启动它的）这一页。守卫用它，不要在别处直接比较页面名（tools/check_frontend.py 规则 6）。
export const onPage = (route, ...names) => names.includes(route.page);
export const poolIs = (route, ...pools) => pools.includes(pageOf(route).pool);

// ui/ 登记页面的处理函数：{页面名: {render, refresh, heading, ...}}。
//   render({samePage})   进入本页时画主区；没有的页面是网格页，由 shell 调 reload(true)
//   prepare(route)       进入本页前要先读的数据；返回 false 表示已经导航走了
//   heading(r, ctx)      [标题, 副标题, 右侧链接 HTML]；角色页另用 drawHeading
//   onMeta(info)         计数等元数据到达后重画什么；没有的页面重画工具栏
//   refresh(why)         数据变了之后重画面板页：'edit' 改动之后、'undo' 撤销之后、'sync' 同步到新图之后、
//                        'nsfw' 切换 R18 之后、'view' 视图选项之后、'ask' 「偶尔问一句」开关之后
export function implement(hooks) {
  for (const [name, h] of Object.entries(hooks)) {
    const page = name === 'character' ? character : PAGES[name];
    if (!page) throw new Error(`没有登记过的页面：${name}`);
    Object.assign(page, h);
  }
}

// ---------- 地址 ----------
// 把 #/ 之后的路径段读成页面：{page, id?, section?}、旧角色页的 {char, variant, legacy} 或已删页面的 {page, legacy}；
// legacy 的地址由 applyRoute 就地改写。认不出的是图库。
export function matchPath(parts, hasQuery) {
  if (!parts.length && !hasQuery) return {page: list.find(p => p.root).name};
  const [head, next] = parts;
  for (const p of list) {
    if (p.prefix) { if (head === p.prefix && next) return {page: p.name, id: decodeURIComponent(next)}; continue; }
    if (head !== p.name || p === PAGES.library) continue;   // 图库是认不出地址时的默认，不用自己的名字匹配
    return p.sections ? {page: p.name, section: parts[1] || ''} : {page: p.name};
  }
  if (Object.hasOwn(RETIRED, head)) return {page: RETIRED[head], legacy: true};
  if (head === character.legacyPrefix && next) return {char: decodeURIComponent(next), variant: parts[2] ? decodeURIComponent(parts[2]) : '', legacy: true};
  return {};
}
// 路由对应的路径（不含查询）。
export function pathFor(r) {
  const p = PAGES[r.page];
  return p?.prefix ? `#/${p.prefix}/${encodeURIComponent(r.id)}` : '#/' + r.page + (r.section ? '/' + r.section : '');
}
