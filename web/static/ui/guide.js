// The new library's guide (desktop layout, #96, #199): the tour, the checklist and the questions asked when they matter.
import {WORK_NAMES} from '../core/catalog.js';
import {esc, number, shortcut} from '../core/format.js';
import {said, t, tb} from '../core/i18n.js';
import {SYNC_ACTIVE, pollSync, startSync, syncLine} from '../core/jobs.js';
import {go} from '../core/route.js';
import {api, post, session} from '../core/session.js';
import {allowNames, updatesPost, updatesTick} from '../core/settings.js';
import {state} from '../core/state.js';
import {$, toast} from './dom.js';
import {openRelease} from './settings.js';
import {taskList} from './tasks.js';
import {modelsNotice} from './models.js';
import {modelsWaiting, modelsFinished, onModels} from '../core/models.js';

// ---------- 新图库的首次引导 (#96, native/terms/src/app_onboarding.rs) ----------
// Taught on the real interface: a few stops lit up one at a time, a checklist that ticks itself as things get done,
// and questions asked when they matter (names once characters show, a work once the first sync is through). The
// guide's place is kept per library; nothing here grants network access or starts a task without the person's click.
const TOUR = ['welcome', 'tasks', 'home', 'characters', 'unmatched', 'end'];
const CHECKLIST = [['look', t('看看认出的角色'), '#/characters'], ['correct', t('纠正一张认错的图'), '', t('选中后按 C')], ['undo', t('撤销一次操作'), '', shortcut('Z')],
  ['group', t('确认一组没认出的图'), '#/groups'], ['follow', t('关注一部作品'), '#/characters/works']];
let guideChecked = false, guideLoadedAt = 0, guideWorksAt = 0, tourAt = -1, checklistOpen = false;
const guideOn = () => !!state.guide && state.guide.guide.status !== 'off';
const guideActive = () => ['new', 'active'].includes(state.guide?.guide?.status);
export async function loadGuide(works = false) {
  const token = session.token, value = await api('/api/onboarding' + (works ? '?works=1' : ''));
  if (token !== session.token) return null;
  state.guide = {...value, works: value.works || state.guide?.works};
  guideLoadedAt = Date.now();
  return value;
}
async function guideAction(body) {
  const token = session.token;
  try {
    const value = await api('/api/onboarding', {method: 'POST', body: JSON.stringify(body)});
    if (token === session.token) state.guide = {...value, works: value.works || state.guide?.works};
    return true;
  } catch (e) { toast(e.message); return false; }
}
// On opening: a new library records that its guide started; the tour itself waits for the splash to finish.
export async function guideOnOpen() {
  try { await loadGuide(); } catch (e) { return; }
  if (state.guide?.guide.status === 'new') await guideAction({action: 'step', step: 'welcome'});
}
const splashGone = () => new Promise(done => {
  const started = Date.now(), wait = () => (!document.getElementById('splash') || Date.now() - started > 8000) ? done() : setTimeout(wait, 150);
  wait();
});
export async function guideAfterBoot() {
  if (!guideOn()) return;
  await splashGone();
  if (guideActive()) showTour(Math.max(0, TOUR.indexOf(state.guide.guide.step)));
  drawGuide();
}
// Something about the library changed (a sync step, a mutation): read the guide again, at most every few seconds.
export async function guideRefresh(force = false) {
  if (!guideOn() || (!force && Date.now() - guideLoadedAt < 4000)) return;
  try { await loadGuide(); } catch (e) { return; }
  drawGuide();
}
export function drawGuide() {
  drawGuideNotice(); drawNudge(); drawChecklist();
  if (tourAt >= 0) placeTour(false);
}

// ----- the problem bar: recognition missing or its self-check failed, or a sync that stopped (while it lasts, #97) -----
function syncTrouble() {
  const s = state.sync, message = s.message ? `（${esc(tb(s.message))}）` : '', missing = said(s.message, '文件夹不存在或无法访问：{path}');
  if (s.status === 'interrupted') return t('同步没有做完{said}：已经加入和识别的图都保留了，再同步一次会接着处理剩下的。', {said: message});
  if (s.status !== 'error') return '';
  return t('同步出错了{said}：已经加入和识别的图都保留了。', {said: message}) + (missing
    ? t('检查来源文件夹是否还在、外接硬盘是否接上，或者在 {link} 里换一个文件夹，然后再同步一次。', {link: `<a class="link" href="#/settings/library">${t('设置 › 图库')}</a>`})
    : t('可以再同步一次；还不行的话，反馈时附上「设置 › 关于」里的诊断信息。'));
}
export function drawGuideNotice() {
  const el = $('guideNotice'), g = state.guide; if (!el) return;
  // An install without its models (#168): shown in every library until they are in place, guide or not.
  const models = modelsNotice();
  if (models || el.dataset.models) {
    el.dataset.models = models ? '1' : '';
    if (models) { el.hidden = false; if (el.dataset.html !== models) { el.dataset.html = models; el.innerHTML = models; } return; }
    el.dataset.html = '';
  }
  const c = g?.capability, check = c?.check || {};
  if (guideOn() && c?.recognition && !modelsWaiting() && check.status === 'idle' && !guideChecked) { guideChecked = true; guideAction({action: 'check'}).then(() => setTimeout(() => guideRefresh(true), 1500)); }
  if (guideOn() && check.status === 'checking') setTimeout(() => guideRefresh(true), 1500);
  const problem = !guideOn() || modelsWaiting() ? '' : !c.recognition ? t('这个安装没有识别组件：图片会加入图库、可以手动整理，但不会自动认出角色。安装 Illustash 完整版后就能自动识别。')
    : check.status === 'failed' ? t('识别组件没能启动（{reason}）。图片仍会加入图库；可以重新检查，或重新安装完整版。', {reason: esc(tb(check.message || ''))}) : '';
  const trouble = guideOn() && !problem ? syncTrouble() : '';
  el.hidden = !problem && !trouble;
  el.innerHTML = problem ? `<span>${problem}</span>${c.recognition ? `<button class="quiet small" data-guide-check>${t('重新检查')}</button>` : ''}`
    : trouble ? `<span>${trouble}</span><button class="quiet small" data-guide-sync>${t('再同步一次')}</button>` : '';
}
// ----- the tour: one real element lit up at a time -----
export function tourStop(id) {
  const g = state.guide, s = state.sync, r = g.results;
  const counting = SYNC_ACTIVE.includes(s.status) ? syncLine(s) : r.total ? t('已经有 {n} 张图，认出了 {chars} 个角色。', {n: number(r.total), chars: number(r.characters)}) : '';
  return {
    welcome: SYNC_ACTIVE.includes(s.status) || !r.total
      ? {title: t('图库建好了'), text: t('正在读取并识别你的图片，全部在这台电脑上完成，图片不会发出去。花一分钟认识一下界面？'), next: t('带我看看'), skip: t('自己逛逛')}
      : {title: t('认识一下界面'), text: t('花一分钟看看几个常用的地方？'), next: t('带我看看'), skip: t('不用了')},
    tasks: {target: () => $('taskButton'), title: t('进度在这里'), text: `${counting ? esc(counting) + '<br>' : ''}${t('同步和识别在后台进行，识别好的图会陆续出现在首页，不用等全部完成。')}`},
    home: {route: 'home', target: () => document.querySelector('#homePage .home-j') || $('homePage'), title: t('首页'),
      text: document.querySelector('#homePage .home-j') ? t('新加入的图按日期排在这里，图上写着认出的角色。认错了就选中它按 C 换成正确的角色，{key} 随时撤销。', {key: shortcut('Z')}) : t('图片导入后会按日期排在这里，图上写着认出的角色。')},
    characters: {target: () => document.querySelector('.nav[data-page="characters"]'), title: t('资料库'), text: t('认出的角色按作品放在这里，点进去只看 TA 的图。')},
    unmatched: {target: () => document.querySelector('.nav[data-page="unmatched"]'), title: t('还没认出的图'), text: t('长得像的会分成一组，一次确认一整组。确认得越多，认得越准。')},
    end: {title: t('就这些'), text: t('右下角的「上手」清单会跟着你做过的事打勾，全做完就自己消失。以后想再看一遍，在「设置 › 关于」里。'), next: t('开始用吧')},
  }[id];
}
function showTour(at) {
  tourAt = at; drawNudge(); drawChecklist();
  const root = $('tour'); root.hidden = false; requestAnimationFrame(() => root.classList.add('on'));
  placeTour(true);
}
function closeTour() {
  tourAt = -1; const root = $('tour'); root.classList.remove('on');
  setTimeout(() => { if (tourAt < 0) root.hidden = true; }, 260);
  drawGuide();
}
async function placeTour(moved) {
  if (tourAt < 0 || !state.guide) return;
  const id = TOUR[tourAt], stop = tourStop(id), root = $('tour'), hole = root.querySelector('.tour-hole'), bubble = root.querySelector('.tour-bubble');
  if (moved && stop.route && state.route.page !== stop.route) { go({page: stop.route}); await new Promise(done => setTimeout(done, 450)); if (TOUR[tourAt] !== id) return; }
  // While a stop stays, only its text follows the live counts; its buttons are never rebuilt under a press.
  if (!moved && bubble.dataset.stop === id) {
    const text = bubble.querySelector('.tour-text'); if (text && text.innerHTML !== stop.text) text.innerHTML = stop.text;
  }
  const html = `<b class="tour-title">${stop.title}</b><p class="tour-text">${stop.text}</p>
    <div class="tour-foot"><span class="tour-dots">${TOUR.map((_, i) => `<i class="${i === tourAt ? 'on' : ''}"></i>`).join('')}</span><div class="spacer"></div>
      ${stop.skip || tourAt === TOUR.length - 1 ? '' : `<button class="quiet small" data-tour="skip">${t('跳过')}</button>`}${stop.skip ? `<button class="quiet small" data-tour="skip">${stop.skip}</button>` : ''}
      ${tourAt && tourAt < TOUR.length - 1 ? `<button class="quiet small" data-tour="back">${t('上一步')}</button>` : ''}
      <button class="primary small" data-tour="${tourAt === TOUR.length - 1 ? 'done' : 'next'}">${stop.next || t('下一步')}</button></div>`;
  if (bubble.dataset.stop !== id || moved && bubble.dataset.html !== html) {
    if (moved && bubble.dataset.html) { bubble.classList.add('swap'); await new Promise(done => setTimeout(done, 120)); }
    bubble.innerHTML = html; bubble.dataset.html = html; bubble.dataset.stop = id; bubble.classList.remove('swap');
  }
  const target = stop.target?.(), r = target?.getBoundingClientRect(), pad = 6, W = innerWidth, H = innerHeight, bw = bubble.offsetWidth, bh = bubble.offsetHeight;
  if (!r || !r.width) {
    Object.assign(hole.style, {left: W / 2 + 'px', top: H / 2 + 'px', width: '0px', height: '0px'}); hole.classList.add('none');
    Object.assign(bubble.style, {left: (W - bw) / 2 + 'px', top: (H - bh) / 2 + 'px'});
    return;
  }
  // A large area (the home page) is lit only at its top, where the first pictures are.
  const box = {left: r.left - pad, top: r.top - pad, width: r.width + pad * 2, height: Math.min(r.height, H * .45) + pad * 2};
  Object.assign(hole.style, {left: box.left + 'px', top: box.top + 'px', width: box.width + 'px', height: box.height + 'px'}); hole.classList.remove('none');
  const right = box.left + box.width + 14, below = box.top + box.height + 14;
  const [x, y] = right + bw < W - 12 ? [right, box.top] : below + bh < H - 12 ? [box.left, below] : [box.left, box.top - bh - 14];
  Object.assign(bubble.style, {left: Math.max(12, Math.min(x, W - bw - 12)) + 'px', top: Math.max(12, Math.min(y, H - bh - 12)) + 'px'});
}
// ----- questions asked when they matter, once each -----
function nextNudge() {
  if (tourAt >= 0) return null;
  const g = state.guide, idle = !SYNC_ACTIVE.includes(state.sync.status);
  if (guideOn()) {
    const seen = g.guide.seen || [], net = state.names?.network;
    if (!seen.includes('names') && net && !net.granted_at && !net.this_run && g.results.characters > 0) return {key: 'names'};
    // App-wide and asked once: answering either way records the choice, so no other library asks again (#191).
    if (state.updates && state.updates.auto === null) return {key: 'updates'};
    if (!seen.includes('works') && idle && g.results.with_character > 0) {
      if (!g.works && Date.now() - guideWorksAt > 30000) { guideWorksAt = Date.now(); loadGuide(true).then(drawNudge).catch(() => {}); }
      const w = (g.works || []).find(x => x.watchable && !x.watched);
      if (w && !(state.index?.watch?.works || []).length) return {key: 'works', w};
    }
  }
  // A newer release waits until no task is running.
  if (state.updates?.newer && !state.updates.dismissed && !taskList().some(task => task.active)) return {key: 'update'};
  return null;
}
export function drawNudge() {
  const el = $('nudge'), n = nextNudge();
  if (guideOn() && !state.names && !el.dataset.names) { el.dataset.names = '1'; api('/api/names').then(v => { state.names = v; drawNudge(); }).catch(() => {}); }
  const html = !n ? '' : n.key === 'update'
    ? `<div><b>${t('有新版本 {version}', {version: esc(state.updates.latest)})}</b><small>${t('现在是 {current}。在浏览器里打开下载页，下载后覆盖安装即可。', {current: esc(state.updates.current)})}</small></div>
      <div class="nudge-acts"><button class="quiet small" data-nudge="later">${t('暂不')}</button><button class="primary small" data-nudge="open">${t('打开下载页')}</button></div>`
    : n.key === 'updates'
    ? `<div><b>${t('要自动检查新版本吗？')}</b><small>${t('启动时和之后每天向 GitHub 查一次有没有新版本，有就提示你；只是一次普通的 HTTPS 请求，不发送图库里的任何信息。可以在「设置 › 网络与隐私」里随时改。')}</small></div>
      <div class="nudge-acts"><button class="quiet small" data-nudge="later">${t('不用')}</button><button class="primary small" data-nudge="allow">${t('自动检查')}</button></div>`
    : n.key === 'names'
    ? `<div><b>${t('认出了 {n} 个角色。要联网查它们的中文名吗？', {n: number(state.guide.results.characters)})}</b><small>${t('只发送角色的 tag 和作品名，不发送图片。允许后对这个图库一直有效，可以在「设置 › 网络与隐私」里撤销。')}</small></div>
      <div class="nudge-acts"><button class="quiet small" data-nudge="later">${t('暂不')}</button><button class="primary small" data-nudge="allow">${t('允许')}</button></div>`
    : `<div><b>${t('你的图里《{work}》最多（{n} 张）', {work: esc(guideWorkName(n.w)), n: number(n.w.count)})}</b><small>${t('关注它，就能从 Danbooru 补充这部作品里本地还不认识的角色，让它们也能按长相认出来。检查时会再问一次联网。')}</small></div>
      <div class="nudge-acts"><button class="quiet small" data-nudge="later">${t('暂不')}</button><button class="primary small" data-nudge="follow">${t('关注')}</button></div>`;
  if (el.dataset.html === html) return;
  el.dataset.html = html; el.dataset.key = n?.key || '';
  if (!html) { el.classList.remove('on'); setTimeout(() => { if (!el.dataset.html) el.hidden = true; }, 260); return; }
  el.innerHTML = html; el.hidden = false; requestAnimationFrame(() => el.classList.add('on'));
}
// ----- the checklist: ticks itself as things get done -----
export function guideTick(item) {
  const g = state.guide; if (!guideOn() || g.guide.ticked.includes(item)) return;
  g.guide.ticked = [...g.guide.ticked, item]; drawChecklist(item);
  guideAction({action: 'tick', item}).then(() => drawChecklist());
}
// Called after every successful change: which checklist item it shows was learned.
export function guideAfterPost(path, body) {
  if (!guideOn()) return;
  if (path === '/api/undo') guideTick('undo');
  else if (path === '/api/change' && body?.action === 'confirmed' && ['groups', 'group'].includes(state.route.page)) guideTick('group');
  else if (path === '/api/change' || path === '/api/reassign') guideTick('correct');
  else if (path === '/api/index' && body?.action === 'watch' && body.works?.length) guideTick('follow');
  if (Date.now() - guideLoadedAt > 4000) guideRefresh();
}
export function drawChecklist(just = '') {
  const el = $('checklist'), g = state.guide;
  const ticked = g?.guide.ticked || [], all = ticked.length === CHECKLIST.length;
  const show = guideOn() && !g.guide.hidden && tourAt < 0 && (!all || just);
  el.hidden = !show; if (!show) return;
  const ring = `<svg class="cl-ring" viewBox="0 0 20 20" aria-hidden="true"><circle cx="10" cy="10" r="8"/><circle cx="10" cy="10" r="8" class="cl-done" style="stroke-dasharray:${(ticked.length / CHECKLIST.length * 50.3).toFixed(1)} 50.3"/></svg>`;
  el.classList.toggle('open', checklistOpen);
  el.innerHTML = `<button class="cl-pill" data-cl="toggle" aria-expanded="${checklistOpen}">${ring}<span>${all ? t('都会了') : t('上手')}</span><b>${ticked.length}/${CHECKLIST.length}</b></button>
    ${checklistOpen ? `<div class="cl-panel"><p class="cl-head">${t('边用边学：做过的会自动打勾')}</p>${CHECKLIST.map(([k, label, href, hint]) => `<div class="cl-item${ticked.includes(k) ? ' done' : ''}${k === just ? ' just' : ''}"><span class="cl-check" aria-hidden="true">${ticked.includes(k) ? '✓' : ''}</span>${href && !ticked.includes(k) ? `<a class="link" href="${href}">${label}</a>` : `<span>${label}</span>`}${hint ? `<kbd>${esc(hint)}</kbd>` : ''}</div>`).join('')}
      <button class="link-button" data-cl="hide">${t('不再显示')}</button></div>` : ''}`;
  if (just) { el.classList.add('pulse'); setTimeout(() => el.classList.remove('pulse'), 900); }
  if (all && just) setTimeout(() => guideAction({action: 'hide'}).then(() => drawChecklist()), 2500);
}
// A work the library has keeps its own name; a suggested Danbooru tag shows the built-in Chinese name when there is one.
const guideWorkName = w => (!w.id && WORK_NAMES[w.tag]) || w.name;
// Following a suggested work: the local work for its Danbooru tag (made when missing), added to the watch list.
async function guideWatch(w) {
  try {
    const id = w.id || (await post('/api/works', {action: 'from_tag', tag: w.tag})).id;
    const x = await api('/api/index');
    state.index = x.watch.works.includes(id) ? x : await post('/api/index', {action: 'watch', works: [...x.watch.works, id], min_posts: x.watch.min_posts});
    toast(t('已关注「{work}」，可以在任务面板里检查它的新角色', {work: guideWorkName(w)}));
  } catch (e) { toast(e.message); }
  await loadGuide(true).catch(e => toast(e.message));
}
export async function replayGuide() {
  if (!await guideAction({action: 'reopen'})) return;
  if (state.route.page === 'settings') go({page: 'home'});
  await new Promise(done => setTimeout(done, 300));
  showTour(0);
}

// The handlers this part of the page registers. Called once, when the page starts.
export function wireGuide() {
  // The models just arrived: check that recognition starts, as on opening a new library.
  onModels(async before => {
    if (modelsFinished(before) && state.models.status === 'ready' && guideOn()) { await guideAction({action: 'check'}); setTimeout(() => guideRefresh(true), 1500); }
    drawGuideNotice();
  });
  $('guideNotice').addEventListener('click', async e => {
    if (e.target.closest('[data-guide-check]')) { await guideAction({action: 'check'}); drawGuide(); setTimeout(() => guideRefresh(true), 1500); }
    if (e.target.closest('[data-guide-sync]')) { try { await startSync(); pollSync(); } catch (err) { toast(err.message); } drawGuide(); }
  });

  $('tour').addEventListener('click', async e => {
    const act = e.target.closest('[data-tour]')?.dataset.tour; if (!act) return;
    if (act === 'skip' || act === 'done') { closeTour(); await guideAction({action: act === 'done' ? 'done' : 'skip'}); drawGuide(); return; }
    tourAt = Math.max(0, Math.min(TOUR.length - 1, tourAt + (act === 'next' ? 1 : -1)));
    placeTour(true); guideAction({action: 'step', step: TOUR[tourAt]});
  });
  addEventListener('resize', () => { if (tourAt >= 0) requestAnimationFrame(() => placeTour(false)); });
  document.addEventListener('keydown', e => { if (tourAt >= 0 && e.key === 'Escape') { e.stopImmediatePropagation(); closeTour(); guideAction({action: 'skip'}); } }, true);

  $('nudge').addEventListener('click', async e => {
    const act = e.target.closest('[data-nudge]')?.dataset.nudge, key = $('nudge').dataset.key; if (!act || !key) return;
    for (const b of $('nudge').querySelectorAll('button')) b.disabled = true;
    // Not guide prompts: the answer is the update choice or this release, kept app-wide by the backend.
    if (key === 'update' || key === 'updates') {
      if (key === 'update' && act === 'open') openRelease();
      try { await updatesPost(key === 'update' ? {action: 'dismiss'} : {action: 'auto', allow: act === 'allow'}); } catch (err) { toast(err.message); }
      if (key === 'updates' && act === 'allow') { toast(t('已打开自动检查更新')); updatesTick(); }
      drawGuide(); drawNudge();
      return;
    }
    if (act === 'allow') {
      try { await allowNames(true); toast(t('已允许这个图库联网查询名称')); }
      catch (err) { toast(err.message); }
    }
    if (act === 'follow') { const w = nextNudge()?.w; if (w) await guideWatch(w); }
    await guideAction({action: 'seen', key});
    drawGuide();
  });

  $('checklist').addEventListener('click', async e => {
    const act = e.target.closest('[data-cl]')?.dataset.cl;
    if (act === 'toggle') { checklistOpen = !checklistOpen; drawChecklist(); }
    else if (act === 'hide') { await guideAction({action: 'hide'}); drawChecklist(); }
    else if (e.target.closest('a[href]')) { checklistOpen = false; drawChecklist(); }
  });
}
