// 「整理」各页（桌面布局，#199）：未识别图片的相似分组、整个图库里的相似图片、相似的角色对，以及画师抽查。
import {edits, setLatest} from '../core/edits.js';
import {esc, number, stamp} from '../core/format.js';
import {t, tb} from '../core/i18n.js';
import {SIMILAR_ACTIVE, similarLine} from '../core/jobs.js';
import {RELATION_KINDS, RELATION_NAMES} from '../core/labels.js';
import {refreshMeta, setIdentityCount} from '../core/meta.js';
import {implement, onPage} from '../core/pages.js';
import {prefs} from '../core/prefs.js';
import {charHash, go, hashFor} from '../core/route.js';
import {api, post, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {closeInspector, peekAt, wirePeek} from './detail.js';
import {$, error, readFailure, reconcileSections, toast} from './dom.js';
import {mutate} from './grid.js';
import {markOc, openPicker, targets} from './palette.js';
import {navCount, renderHeading} from './shell.js';

// ---------- 未匹配图片的相似分组 ----------
// 'look'：中心接近；'trait'：离得更远，但有相同的独特 WD 特征（较弱的线索）。
export const nearText = g => g.near_kind === 'trait' ? t('外观相似：第 {n} 组', {n: g.near}) : t('可能和第 {n} 组是同一人', {n: g.near});
// 未识别，按组整理：CCIP 即使叫不出名字（45%），也能可靠地把同一个人的图放在一起（99%）。
// 大多数代表图意见一致的组整组问「是 TA 吗？」；其余的组给出「设为…」/「原创角色」/「跳过」。
// 点一张图，把它排除在这组的回答之外。回答过的组原地离开页面（不会因重新加载而跳动）。
export const groups = {list: [], shown: 40, off: new Map(), revision: 0};
// 各组的先后：一键就能确认的组排在前面（大的在前），然后是还要起名字的组。组页空了时的「下一组」也按它走。
export const groupOrder = clusters => [...clusters].sort((a, b) => !!b.lead - !!a.lead || b.size - a.size || a.cid - b.cid);
export async function renderGroups(keep = false) {
  if (!onPage(state.route, 'groups')) return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = t('点一下图片：不算在这组里 · 长按或 Space 看大图 · 组头的按钮对整组生效');
  const scroll = $('viewport').scrollTop, revision = ++groups.revision, nsfw = state.nsfw;
  let result; try { result = await api('/api/clusters?nsfw=' + state.nsfw); } catch (e) { if (onPage(state.route, 'groups') && revision === groups.revision && nsfw === state.nsfw) readFailure($('groupsPage'), e); return; }
  const {clusters, learner} = result;
  if (!onPage(state.route, 'groups') || revision !== groups.revision || nsfw !== state.nsfw) return;
  groups.list = groupOrder(clusters); groups.learner = learner;
  if (!keep) { groups.shown = 40; groups.off = new Map(); }
  drawGroups();
  if (keep) $('viewport').scrollTop = scroll;
}
export function drawGroups() {
  const list = groups.list, busy = ['learning', 'grouping', 'waiting'].includes(groups.learner?.status), images = list.reduce((s, g) => s + g.size, 0);
  const rest = Math.max(0, (state.pools.unmatched || 0) - images), named = list.filter(g => g.lead).length;
  if (!$('groupsPage').querySelector('.gsecs')) $('groupsPage').innerHTML = '<div class="groups-controls"></div><div class="gsecs"></div><div class="groups-more"></div>';
  $('groupsPage').querySelector('.groups-controls').innerHTML = `<div class="groups-bar"><span class="muted-note">${t('{n} 组 · 共 {images} 张', {n: number(list.length), images: number(images)})}${named ? ' · ' + t('{n} 组能直接确认是谁', {n: number(named)}) : ''}${list[0] ? ' · ' + t('分组于 {time}', {time: esc(stamp(list[0].built_at))}) : ''}${busy ? ' · ' + esc(groups.learner.status === 'waiting' ? groups.learner.message || '' : t('正在学习…')) : ''}</span><div class="spacer"></div>${rest ? `<a class="quiet link" href="#/unmatched">${t('没成组的 {n} 张', {n: number(rest)})} →</a>` : ''}<button class="quiet" id="regroup" title="${t('用当前的未识别图重新分组')}">${t('重新分组')}</button></div>`;
  reconcileSections($('groupsPage').querySelector('.gsecs'), list.slice(0, groups.shown).map(g => [String(g.cid), groupSection(g)]), t('没有成组的未识别图。'));
  $('groupsPage').querySelector('.groups-more').innerHTML = `${list.length > groups.shown ? `<div class="gsec-more"><button class="quiet" id="moreGroups">${t('再显示 {n} 组', {n: number(Math.min(40, list.length - groups.shown))})}</button></div>` : ''}`;
  $('regroup').onclick = async () => { try { await post('/api/regroup', {}); toast(t('正在重新分组…')); setTimeout(() => renderGroups(), 3000); } catch (e) { toast(e.message); } };
  $('moreGroups')?.addEventListener('click', () => { groups.shown += 40; drawGroups(); });
}
function groupSection(g) {
  const off = groups.off.get(g.cid) || new Set(), n = g.members.length - off.size, max = 16;
  // 「是 X 吗？」的组同样能整组标为原创角色或跳过（#386 的 2-41）；原创角色新建一个独立的，和组页的「整组标为原创角色」一样（2-29）。
  const ocSkip = `<button class="quiet" data-gact="oc" title="${t('整组都是同一个原创角色：新建一个原创角色，把这组图都设为它')}">${t('原创角色')}</button><button class="quiet" data-gact="skip">${t('跳过')}</button>`;
  const head = g.lead
    ? `<span class="gsec-q">${t('是 {name} 吗？', {name: `<a href="${charHash(g.lead.id)}">${esc(g.lead.name)}</a>`})}<small>${t('{n} 张的线索都指向 TA', {n: number(g.lead.count)})}</small></span>
       <button class="primary" data-gact="yes"${n ? '' : ' disabled'}>${off.size ? t('其余 {n} 张是', {n: number(n)}) : t('都是')} ✓</button><button class="secondary" data-gact="no">${t('都不是')} ✗</button><button class="quiet" data-gact="assign">${t('是别人…')}</button>${ocSkip}`
    : `<span class="gsec-q muted">${t('还不知道是谁')}</span><button class="primary" data-gact="assign"${n ? '' : ' disabled'}>${t('设为…')}</button>${ocSkip}`;
  const thumbs = g.members.slice(0, max).map(u => `<button class="gsec-img${off.has(u) ? ' off' : ''}" data-gimg="${esc(u)}" data-peek="${esc(u)}" title="${off.has(u) ? t('点一下：算回这组') : t('点一下：不算在这组里')} · ${t('长按或 Space 看大图')}"><img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="" draggable="false"><i aria-hidden="true">✕</i></button>`).join('');
  return `<section class="gsec" data-cid="${g.cid}"><div class="gsec-head"><b>${t('第 {id} 组', {id: g.cid})}</b><span class="muted-note">${t('{n} 张', {n: number(g.size)})}${g.traits?.length ? ' · ' + g.traits.slice(0, 2).map(esc).join(' · ') : ''}</span>${head}<a class="quiet link gsec-open" href="#/g/${g.cid}">${t('逐张看')} →</a></div>
    <div class="gsec-imgs">${thumbs}${g.members.length > max ? `<a class="gsec-rest" href="#/g/${g.cid}">+${number(g.members.length - max)}</a>` : ''}</div>${g.near ? `<div class="group-near">${nearText(g)}</div>` : ''}</section>`;
}
// ---------- 整个图库里的相似图片（native/terms/src/research_compute/similar.rs） ----------
let similarTimer = 0;
export async function renderSimilar() {
  if (!onPage(state.route, 'similar')) return;
  clearTimeout(similarTimer);
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = t('点一组进去看');
  const kind = state.route.kind || '';
  let result; try { result = await api('/api/similar?' + new URLSearchParams({kind, nsfw: state.nsfw})); } catch (e) { if (onPage(state.route, 'similar') && kind === (state.route.kind || '')) readFailure($('groupsPage'), e); return; }
  const {groups, state: s} = result;
  if (!onPage(state.route, 'similar')) return;
  const busy = SIMILAR_ACTIVE.includes(s.status);
  $('groupsPage').innerHTML = `<div class="similar-status"><span class="local-dot${busy ? ' busy' : ''}"></span><span class="muted-note">${t('{n} 组 · 共 {images} 张', {n: number(groups.length), images: number(groups.reduce((n, g) => n + g.size, 0))})}${similarLine(s) ? ' · ' + esc(similarLine(s)) : ''}</span></div>
    <div class="groups-bar"><div class="segmented">${RELATION_KINDS.map(([k, label]) => `<button class="${k === kind ? 'on' : ''}" data-kind="${k}">${label}</button>`).join('')}</div>
      <div class="spacer"></div><button class="quiet" id="similarScan" ${busy || s.status === 'off' ? 'disabled' : ''} title="${t('比较新加入的图和还没比较过的候选')}">${t('重新检查')}</button></div>
    ${busy && s.total ? `<div class="progress"><span style="width:${Math.round((s.done || 0) / s.total * 100)}%"></span></div>` : ''}
    <div class="group-grid">${groups.map(g => `<a class="group-card" href="#/s/${g.gid}"><div class="group-covers">${g.covers.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="">`).join('')}</div>
      <div class="group-meta"><b>${t('相似组 {id}', {id: g.gid})}</b><span>${t('{n} 张', {n: number(g.size)})}</span></div>
      <div class="group-traits">${Object.entries(g.kinds).sort((a, b) => b[1] - a[1]).map(([k, n]) => `<span>${RELATION_NAMES[k] || k}${n > 1 ? ' ×' + n : ''}</span>`).join('')}</div></a>`).join('')
      || `<p class="empty">${busy ? t('还在检查，找到的组会陆续出现。') : t('没有找到相似的图。')}</p>`}</div>`;
  for (const b of $('groupsPage').querySelectorAll('[data-kind]')) b.onclick = () => go({page: 'similar', kind: b.dataset.kind});
  $('similarScan').onclick = async () => { try { await post('/api/similar', {}); setTimeout(renderSimilar, 800); } catch (e) { toast(e.message); } };
  if (busy) similarTimer = setTimeout(renderSimilar, 3000);
}

// ---------- 身份复核：相似的角色对（/api/identity/queue，native/terms/src/http_characters.rs） ----------
// 一次点击决定一对；每个决定都是一条可撤销的关系，原始记录从不移动。
const IDENTITY_PAGE = 12;
const identity = {items: [], total: 0, offset: 0, busy: new Set()};
function identitySuggestion(x) {
  const h = x.hint; if (!h) return '';
  if (h.kind === 'duplicate_of') return 'same';
  if (h.kind === 'form_of') return h.subject && h.subject === x.b.tag ? 'b_of_a' : h.subject && h.subject === x.a.tag ? 'a_of_b' : '';
  return '';
}
function identityEvidence(x) {
  const h = x.hint, name = tag => tag === x.a.tag ? x.a.name : tag === x.b.tag ? x.b.name : tag.replaceAll('_', ' ');
  const parts = [];
  if (h?.kind === 'duplicate_of') parts.push(t('{source}：同一个 tag 的别名', {source: esc(h.source || t('外部资料'))}));
  else if (h?.kind === 'form_of') parts.push(t('{source}：「{subject}」是「{object}」的一个造型', {source: esc(h.source || t('外部资料')), subject: esc(name(h.subject)), object: esc(name(h.object))}));
  if (x.distance !== null && x.distance !== undefined) parts.push(t('参考图外观距离 {d}', {d: x.distance.toFixed(3)}) + (x.distance <= 0.01 ? t('（几乎分不开）') : ''));
  return parts.join(' · ') || t('参考图外观很接近');
}
function identitySide(s, label) {
  return `<div class="pair-side"><div class="pair-name"><span class="pair-label">${label}</span><a href="${charHash(s.id)}" title="${esc(s.tag || s.name)}">${esc(s.name)}</a><small>${t('{n} 张', {n: number(s.count)})}</small></div>
    <div class="pair-thumbs">${s.samples.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="" data-peek="${esc(u)}" title="${t('看大图')}">`).join('') || `<span class="muted-note">${t('没有可显示的图')}</span>`}</div></div>`;
}
function identityCard(x, i) {
  const s = identitySuggestion(x), btn = (act, text, title) => `<button class="${s === act ? 'primary' : 'quiet'}" data-act="${act}" data-i="${i}" title="${esc(title)}">${text}</button>`;
  return `<article class="pair-card" data-i="${i}">
    <div class="pair-body">${identitySide(x.a, 'A')}${identitySide(x.b, 'B')}</div>
    <p class="pair-evidence">${identityEvidence(x)}</p>
    <div class="pair-actions">
      ${btn('same', t('记为同一角色（不搬图）'), t('把两个角色合在一起显示，图和人工结论留在原处'))}
      ${btn('b_of_a', t('B 是 A 的一个造型'), t('B 挂到 A 下面作为造型（服装、异格、变身等）'))}
      ${btn('a_of_b', t('A 是 B 的一个造型'), t('A 挂到 B 下面作为造型'))}
      ${btn('different', t('不同角色'), t('记为不同角色，以后不再问这一对'))}
      <span class="spacer"></span>${btn('skip', t('暂不判断'), t('先放一边，队列里不再出现'))}
    </div></article>`;
}
// how：'more' 在后面再接一页；'keep' 判断或撤销之后，按现在显示的对数从头重读、留在原来的滚动位置，判过的那对原地
// 移走、撤销的那对回到原处，点过「再显示一些」的人不被带回前 12 对（#386 的 2-36）；不给就是从头显示前 12 对。
export async function renderIdentity(how = '') {
  if (!onPage(state.route, 'identity')) return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('hint').textContent = t('点角色名看全部图 · 高亮的是外部资料建议的做法');
  const keep = how === 'keep', scroll = $('viewport').scrollTop, limit = keep ? Math.max(IDENTITY_PAGE, identity.items.length) : IDENTITY_PAGE;
  const offset = how === 'more' ? identity.items.length : 0;
  let page;
  try { page = await api('/api/identity/queue?' + new URLSearchParams({limit, offset, nsfw: state.nsfw})); }
  catch (e) { $('groupsPage').innerHTML = `<p class="empty">${esc(readError(e))}</p>`; return; }
  if (!onPage(state.route, 'identity')) return;
  if (how !== 'more') identity.items = [];
  identity.items.push(...page.items); identity.total = page.total; setIdentityCount(page.total); navCount('countIdentity', page.total);
  $('resultCount').textContent = t('{n} 对待判断', {n: number(page.total)});
  renderHeading();
  $('groupsPage').innerHTML = `<div class="pair-list">${identity.items.map(identityCard).join('') || `<p class="empty">${t('没有需要判断的角色了。')}</p>`}</div>
    ${identity.items.length < identity.total ? `<div class="pair-more"><button class="quiet" id="identityMore">${t('再显示一些')}</button></div>` : ''}`;
  $('identityMore')?.addEventListener('click', () => renderIdentity('more'));
  for (const b of $('groupsPage').querySelectorAll('[data-act]')) b.onclick = () => decideIdentity(Number(b.dataset.i), b.dataset.act);
  if (keep) $('viewport').scrollTop = scroll;
}
async function decideIdentity(i, act) {
  const x = identity.items[i]; if (!x || identity.busy.has(i) || edits.busy) return;
  const card = $('groupsPage').querySelector(`.pair-card[data-i="${i}"]`);
  identity.busy.add(i); card?.classList.add('working');
  // 重复的角色显示在图多的那一边下面。
  const [small, big] = x.a.count <= x.b.count ? [x.a, x.b] : [x.b, x.a];
  const body = act === 'skip' ? null : {status: 'accepted', reason: '身份整理',
    ...{same: {a: small.id, b: big.id, kind: 'duplicate_of'}, b_of_a: {a: x.b.id, b: x.a.id, kind: 'form_of'},
        a_of_b: {a: x.a.id, b: x.b.id, kind: 'form_of'}, different: {a: x.a.id, b: x.b.id, kind: 'different_from'}}[act]};
  try {
    const r = act === 'skip' ? await post('/api/identity/skip', {a: x.a.id, b: x.b.id}) : await post('/api/identity/decision', body);
    setLatest(r.undo); toast(act === 'skip' ? t('已暂不判断') : {same: t('已把 {a} 和 {b} 记为同一角色', {a: small.name, b: big.name}), b_of_a: t('{a} 已作为 {b} 的一个造型', {a: x.b.name, b: x.a.name}),
      a_of_b: t('{a} 已作为 {b} 的一个造型', {a: x.a.name, b: x.b.name}), different: t('已记下 {a} 和 {b} 是不同角色', {a: x.a.name, b: x.b.name})}[act], true);
    card?.classList.add('leaving');
    await new Promise(done => setTimeout(done, 170));
    await renderIdentity('keep');
    refreshMeta().catch(e => error(readError(e)));
  } catch (e) { toast(e.message); card?.classList.remove('working'); }
  finally { identity.busy.delete(i); }
}

// ---------- 画师抽查：找回画师的验收（docs/PROVENANCE.md 第 8 节） ----------
// 一份固定的随机样本（服务端按 id 的哈希排序），用和详情面板一样的「是 TA」/「不是」回答，所以每个回答都进撤销历史，
// 同时也修正图库。
const AUDIT_METHODS = {pixel_hash: [t('像素哈希'), t('和 Danbooru 上的帖子是同一个文件，画师取自帖子的画师标签')],
  gallery_match: [t('本地比对'), t('和 TA 公开作品的小图逐张比对一致')], ocr: [t('签名识别'), t('图上的签名或水印')]};
const auditMethod = a => a.methods?.includes('pixel_hash') ? 'pixel_hash' : a.methods?.[0] || '';
export async function renderArtistAudit() {
  if (!onPage(state.route, 'artist-audit')) return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = '';
  $('hint').textContent = t('点原帖或画师主页对照 · 看不出来就先留着');
  const n = Number(prefs.get('artistAuditN', 50)) || 50, scroll = $('viewport').scrollTop;
  let d;
  try { d = await api('/api/artist-audit?n=' + n); } catch (e) { $('groupsPage').innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  if (!onPage(state.route, 'artist-audit')) return;
  const pairs = d.items.flatMap(it => it.artists.map(a => ({...a, method: auditMethod(a)})));
  const tally = list => { const yes = list.filter(a => a.confirmed).length, no = list.filter(a => a.rejected).length; return {yes, no, done: yes + no, all: list.length}; };
  const tl = tally(pairs), acc = tl.done ? tl.yes / tl.done : null;
  const byMethod = Object.keys(AUDIT_METHODS).map(m => [m, tally(pairs.filter(a => a.method === m))]).filter(([, x]) => x.all);
  const link = (url, label) => `<a class="link" href="${esc(url)}" target="_blank" rel="noreferrer noopener">${esc(label)}</a>`;
  const artistRow = (it, a) => {
    const m = auditMethod(a), [mName, mHint] = AUDIT_METHODS[m] || [m || t('其他'), ''];
    const data = `data-uuid="${esc(it.uuid)}" data-artist="${esc(a.id)}" data-name="${esc(a.name)}"`;
    const judged = a.confirmed ? `<span class="aa-verdict yes">✓ ${t('对')}</span><button class="aa-b" data-aa="" ${data}>${t('改')}</button>`
      : a.rejected ? `<span class="aa-verdict no">✗ ${t('不对')}</span><button class="aa-b" data-aa="" ${data}>${t('改')}</button>`
      : `<button class="aa-b yes" data-aa="yes" ${data}>${t('对')}</button><button class="aa-b" data-aa="no" ${data}>${t('不对')}</button>`;
    return `<div class="aa-artist"><div class="aa-top"><a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="${t('在图库里只看 TA 的图')}">${esc(a.name)}</a><small>${t('库里 {n} 张', {n: number(a.count)})}</small></div>
      ${a.homes.length ? `<div class="aa-links">${t('主页')} ${a.homes.map(h => link(h.url, h.label)).join(' · ')}</div>` : ''}
      <div class="aa-how" title="${esc(mHint)}">${esc(mName)}</div>
      <div class="aa-btns">${judged}</div></div>`;
  };
  const card = (it, i) => {
    const state_ = it.artists.some(a => a.rejected) ? ' no' : it.artists.length && it.artists.every(a => a.confirmed) ? ' yes' : '';
    return `<div class="aa-card${state_}"><a class="aa-img" href="/image?id=${encodeURIComponent(it.uuid)}&size=full" target="_blank" data-peek="${esc(it.uuid)}" title="${t('看大图')}"><img src="/image?id=${encodeURIComponent(it.uuid)}&size=medium" alt="" loading="lazy"><span class="aa-no">${i + 1}</span></a>
      <div class="aa-body">${it.artists.map(a => artistRow(it, a)).join('')}${it.posts?.length ? `<div class="aa-links">${t('原帖')} ${it.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</div></div>`;
  };
  $('groupsPage').innerHTML = `<div class="aa-summary">
      <div class="prov-stat"><b>${acc === null ? '—' : (acc * 100).toFixed(1) + '%'}</b><span>${t('准确率')}<small>${t('目标 ≥ 95%')}</small></span></div>
      <div class="prov-stat"><b>${number(tl.done)}<em> / ${number(tl.all)}</em></b><span>${t('已判断')}<small>${t('对 {yes} · 不对 {no}', {yes: number(tl.yes), no: number(tl.no)})}</small></span></div>
      ${byMethod.map(([m, x]) => `<div class="prov-stat"><b>${x.done ? (x.yes / x.done * 100).toFixed(0) + '%' : '—'}</b><span>${esc(AUDIT_METHODS[m][0])}<small>${t('{done} / {all} 已判', {done: number(x.done), all: number(x.all)})}</small></span></div>`).join('')}
    </div>
    <p class="muted-note">${t('从 {pool} 张有画师的图里按固定的随机顺序取的前 {n} 张。判断会写进图库，判为「不对」的画师不再显示在这张图上。', {pool: number(d.pool), n: number(d.items.length)})}</p>
    <div class="aa-grid">${d.items.map(card).join('')}</div>
    ${d.items.length < d.pool ? `<p class="aa-more"><button class="quiet" id="aaMore">${t('再加 10 张')}</button></p>` : ''}`;
  $('viewport').scrollTop = scroll;
  for (const b of $('groupsPage').querySelectorAll('[data-aa]')) b.onclick = async () => {
    if (edits.busy) return; edits.busy = true; b.disabled = true;
    try {
      const r = await post('/api/artist/verdict', {ids: [b.dataset.uuid], artist: b.dataset.artist, verdict: b.dataset.aa || null, name: b.dataset.name});
      setLatest(r.undo); toast(tb(r.undo.label), true);
    } catch (e) { toast(e.message); }
    finally { edits.busy = false; setLatest(edits.latest); }
    renderArtistAudit();
  };
  $('aaMore')?.addEventListener('click', () => { prefs.set('artistAuditN', n + 10); renderArtistAudit(); });
}

// 页面这一部分注册的处理函数。页面启动时调用一次。
// 面板页看大图时翻哪一组（ui/detail.js 的 wirePeek）：按组整理是这一组的全部图（不只是露出来的 16 张），身份整理是
// 这一边的样图，画师抽查是这一批。
function peekList(el) {
  const sec = el.closest('.gsec'), side = el.closest('.pair-thumbs');
  if (sec) return groups.list.find(x => x.cid === Number(sec.dataset.cid))?.members || [];
  if (side) return [...side.querySelectorAll('[data-peek]')].map(x => x.dataset.peek);
  return [...$('groupsPage').querySelectorAll('.aa-img[data-peek]')].map(x => x.dataset.peek);
}
// 大图关掉时把最后看的那张缩略图滚进视野（按组整理）。
function revealThumb(uuid) { [...$('groupsPage').querySelectorAll('[data-peek]')].find(x => x.dataset.peek === uuid)?.scrollIntoView({block: 'nearest'}); }
export function wireReview() {
  wirePeek($('groupsPage'), peekList);
  $('groupsPage').addEventListener('click', async e => {
    // 身份整理的样图、画师抽查的图：点一下看大图（#424 的 2-34）。画师抽查的图按住 ⌘／Ctrl、Shift 点还是在新窗口开原图。
    const look = e.target.closest('.pair-thumbs [data-peek], .aa-img[data-peek]');
    if (look && !e.metaKey && !e.ctrlKey && !e.shiftKey) { e.preventDefault(); peekAt(look); return; }
    const sec = e.target.closest('.gsec'); if (!sec) return;
    const g = groups.list.find(x => x.cid === Number(sec.dataset.cid)); if (!g) return;
    const img = e.target.closest('[data-gimg]');
    if (img) {
      const off = groups.off.get(g.cid) || new Set(), u = img.dataset.gimg;
      off.has(u) ? off.delete(u) : off.add(u); groups.off.set(g.cid, off);
      drawGroups(); return;
    }
    const act = e.target.closest('[data-gact]')?.dataset.gact; if (!act) return;
    const off = groups.off.get(g.cid) || new Set(), ids = g.members.filter(u => !off.has(u)), n = ids.length;
    const done = async ok => { if (!ok) return; groups.list = groups.list.filter(x => x !== g); groups.off.delete(g.cid); drawGroups(); };
    if (act === 'yes') done(await mutate('/api/change', {ids, action: 'confirmed', entity: g.lead.id}, false));
    else if (act === 'no') { if (await mutate('/api/change', {ids, action: 'rejected', entity: g.lead.id}, false)) renderGroups(true); }
    else if (act === 'skip') done(await mutate('/api/change', {ids, action: 'unknown'}, false));
    else if (act === 'oc') done(await markOc({ids}, false, true));
    // 面板顶上写「第 N 组 · M 张」，和缩略图一起（#424 的 2-3）；能多选，和选择栏的「设为…」一样（2-24）。
    else if (act === 'assign') openPicker({title: t('这 {n} 张是…', {n: number(n)}), subjectText: t('第 {id} 组', {id: g.cid}) + ' · ' + t('{n} 张', {n: number(n)}),
      sentence: x => t('把这 {n} 张设为 {name}', {n: number(n), name: x.name}), suggest: {ids}, exclude: g.lead ? [g.lead.id] : [], multi: true,
      pick: async list => done(await mutate('/api/change', {ids, action: 'confirmed', ...targets(list)}, false))});
  });
}

// 复核面板页登记：渲染入口，以及撤销、切换 R18 之后怎么重画。
implement({
  // 在这页开的大图里改了图（'edit' 时大图还开着）：重读分组，免得组里还留着已经设了角色、跳过或排除的图。组头按钮的写入
  // 不走这里，它们自己把答过的组从页面上拿掉。
  groups: {render: () => renderGroups(), reveal: revealThumb,
    refresh: why => { if (why === 'undo' || why === 'learned' || why === 'edit' && state.viewer >= 0) renderGroups(true); else if (why === 'nsfw') renderGroups(); }},
  similar: {render: () => renderSimilar(), refresh: why => { if (why === 'nsfw') renderSimilar(); }},
  identity: {render: () => renderIdentity(), refresh: why => { if (why === 'undo') renderIdentity('keep'); else if (why === 'nsfw') renderIdentity(); }},
  // 画师抽查不按 R18 筛选（固定的一批随机图）：切换 R18 只重画标题里的提醒（#386 的 2-35）。
  'artist-audit': {render: () => renderArtistAudit(), refresh: why => { if (why === 'undo') renderArtistAudit(); else if (why === 'nsfw') renderHeading(); }},
});
