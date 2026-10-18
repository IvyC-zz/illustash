// Illustash front end: plain JS, no build step. Machine identities count by default;
// the user only removes what is wrong. Every write is undoable.
// Native ES modules (#199): core/ holds what any layout can reuse and never touches the DOM (requests and the
// session, edits and undo, formatting, preferences); ui/ is this desktop layout. This file is still the rest.
import {t, tb, said, I18N} from './core/i18n.js';
import {esc, isMac, shortcut, number, dateText, monthText, work, stamp} from './core/format.js';
import {prefs} from './core/prefs.js';
import {state} from './core/state.js';
import {REASONS, RELATION_NAMES, RELATION_KINDS, nameTip} from './core/labels.js';
import {session, adoptToken, onPost, api, post, shared, readError} from './core/session.js';
import {edits, setLatest, undoLatest, undoneText} from './core/edits.js';
import {SYNC_ACTIVE, SIMILAR_ACTIVE, onJob, syncLine, similarLine, importLine, pollSync, startSync} from './core/jobs.js';
import {$, platformHints, toast, wireToast, error, readFailure, reconcileSections, searchIcon, wireDom} from './ui/dom.js';
import {wireTasks, taskList, taskTick} from './ui/tasks.js';
import {confirmAction} from './ui/dialog.js';
import {wireSettings, namesAllowButton, renderSettings, settingsSection, renderIndexCard, renderNamesCard, renderLibrariesCard, renderProvenanceCard, openRelease} from './ui/settings.js';
import {loadLibraries, allowNames, remoteNames, remoteNote, updatesPost, updatesTick} from './core/settings.js';
import {WORK_NAMES, workName, workKey, forgetActivity} from './core/catalog.js';
import {wireCatalog, cast, shelf, renderIndex, renderIndexBody, drawCast, drawShelf, searchIndex, loadActivity, refreshLocalLinks, createWork, openIndexAdd} from './ui/catalog.js';
import {onMeta, fetchChar, metaWritten, metaSoon, refreshMeta} from './core/meta.js';
import {app} from './ui/app_parts.js';
import {LEAD_PAGES, browsing, charHash, charRoute, cleanup, defaultSort, go, hashFor, panelPage, parseHash, replaceRoute} from './core/route.js';
import {applyRoute, navCount, renderChrome, renderHeading, wireShell} from './ui/shell.js';
import {PAGE, clearSelection, currentItem, filters, kept, loadPage, mutate, openTagPicker, reload, renderGrid, rowHeight, rowOf, rowTop, selCount, selectAll, setNsfw, short, sweep, syncCards, toggleFit, toggleSelecting, undo, viewOpts, wireGrid} from './ui/grid.js';
platformHints(document.body);
wireDom();
let pickerTimer, detailGen = 0;

// A write the backend accepted: the counts and reads that may have changed, and the guide's next step.
onPost((path, body) => {
  guideAfterPost(path, body);
  state.countDirty = true; state.readEpoch++;
  // Prevent an older metadata read from overwriting the just-completed mutation.
  metaWritten(path === '/api/tag');
});
// What a background job's answer changes outside the task panel: the library's counts and pictures, the settings
// cards that show the job, and the guide.
onJob(async (job, change) => {
  if (job === 'sync') {
    if (change.arrived) {
      state.countDirty = true; state.readEpoch++;
      await refreshMeta().catch(e => error(readError(e)));
      if (!panelPage(state.route.page) && !selCount()) reload();
      // The home page's day groups and counts are drawn from their own read; redraw them once the sync settles.
      else if (state.route.page === 'home' && !SYNC_ACTIVE.includes(state.sync.status)) renderHome(true);
    }
    guideRefresh(change.ended);
  } else if (job === 'index') {
    const s = state.index.state;
    if (change.ended && s.status === 'done') { state.countDirty = true; state.readEpoch++; await refreshMeta().catch(e => error(readError(e))); }
    if (s.status !== change.was && state.route.page === 'settings') renderIndexCard(false);
    if (s.status !== change.was && state.route.section === 'works') drawShelf();
  } else if (job === 'names') {
    renderNamesCard(false);
    if (change.ended) refreshMeta();
  } else if (job === 'provenance') renderProvenanceCard();
  else if (job === 'import') {
    const task = state.importTask;
    if (change.ended) { toast(importLine(task)); if (task.status === 'done' && task.kind === 'archive') loadLibraries(); }
    renderLibrariesCard();
  }
});

// A picture the app cannot decode (a damaged file): a plain placeholder instead of the browser's broken-image glyph.
// The viewer has its own message (#viewerMissing).
document.addEventListener('error', e => {
  const img = e.target;
  if (!(img instanceof HTMLImageElement) || img.id || !img.getAttribute('src')?.startsWith('/image?')) return;
  const ph = document.createElement('span'); ph.className = 'img-broken'; ph.textContent = t('无法打开'); img.replaceWith(ph);
}, true);

// ---------- toast with undo ----------
wireToast(() => undo());


// ---------- command palette (换成 / 也是 / 设为角色 / 跳到角色) ----------
let palette = null;
// Multi pickers (设为角色 / 也是): ⇧+click, ⇧+↵ or a row's ＋ collects characters; ↵ applies all of them at once.
const targets = list => list.length > 1 ? {entities: list.map(e => e.id)} : {entity: list[0].id};
// 换成 accepts several characters; a single one is sent as before.
const newIds = list => list.length > 1 ? list.map(e => e.id) : list[0].id;
const stageKey = e => e.id || (e.remote ? 'tag:' + e.tag : 'new:' + (e.oc ? 'oc:' : '') + e.create);
async function openPicker(opts) {
  palette = {...opts, list: [], index: 0, generation: 0, staged: []};
  renderStaged();
  $('pickerSearch').value = ''; $('pickerSearch').placeholder = opts.placeholder || t('输入角色 tag 或别名…');
  $('picker').showModal(); $('pickerSearch').focus();
  await paletteSearch();
}
async function paletteSearch() {
  const p = palette; if (!p) return;
  const gen = ++p.generation, q = $('pickerSearch').value.trim();
  let sections = [];
  if (p.tagMode) {
    const low = q.toLowerCase(), mine = state.userTags.filter(x => !q || x.tag.toLowerCase().includes(low));
    if (p.present.size && !q) sections.push({label: t('去掉已有标签'), items: [...p.present.keys()].map(tag => ({tag, name: tag, remove: true, hint: t('所选图片中 {n} 张有', {n: number(p.present.get(tag))}), counted: true}))});
    const recent = prefs.get('tagPicks', []).filter(tag => !q || tag.toLowerCase().includes(low));
    if (recent.length) sections.push({label: t('最近使用'), items: recent.map(tag => ({tag, name: tag, hint: t('我的标签')}))});
    const rest = mine.filter(x => !recent.includes(x.tag));
    if (rest.length) sections.push({label: t('我的标签'), items: rest.map(x => ({tag: x.tag, name: x.tag, hint: t('{n} 张', {n: number(x.count)}), counted: true}))});
    if (q) {
      let wd; try { wd = await api('/api/general-tags?q=' + encodeURIComponent(q)); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: t('本地标签'), items: [], note: readError(e)}], q); return; }
      if (gen !== p.generation) return;
      const known = new Set(state.userTags.map(x => x.tag));
      if (wd.length) sections.push({label: t('采纳 WD 通用标签'), items: wd.filter(tag => !known.has(tag)).slice(0, 8).map(tag => ({tag, name: tag, hint: t('机器词表，采纳后成为你的标签')}))});
      if (!known.has(q) && !wd.includes(q)) sections.push({label: '', items: [{tag: q, name: q, create: q, hint: t('新建标签')}]});
    }
  } else if (p.items) sections = [{label: '', items: p.items.filter(e => !q || e.name.toLowerCase().includes(q.toLowerCase()))}];
  else if (q) {
    let list; try { list = await api('/api/entities?q=' + encodeURIComponent(q)); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: t('本地角色'), items: [], note: readError(e)}], q); return; }
    if (gen !== p.generation) return;
    const items = list.filter(e => !(p.exclude || []).includes(e.id)).map(e => ({...e, hint: e.matched ? t('别名：{name}', {name: e.matched}) : charHint(e), count: e.count}));
    sections = [{label: '', items}];
    if (!p.jump) {
      // Not in the library yet: Danbooru characters (typed text only) come in a moment; local-only creation stays last.
      const create = list.some(e => e.name.toLowerCase() === q.toLowerCase()) ? [] : [{create: q, name: q, hint: t('只在本地新建，不关联 Danbooru')}, {create: q, oc: true, name: q, hint: t('画师的原创角色，不属于任何作品')}];
      const remote = {label: t('Danbooru 角色 · 选中即新建并关联'), items: [], note: t('正在查 Danbooru…（只发送你输入的文字）')};
      sections.push(remote, {label: '', items: create});
      clearTimeout(p.remoteTimer);
      p.remoteTimer = setTimeout(async () => {
        const result = await remoteNames('/api/entities?' + new URLSearchParams({q, remote: 1}));
        if (gen !== p.generation || palette !== p) return;
        remote.items = (result.found || []).map(r => ({...r, remote: true, typed: q, hint: [r.work_name, t('D 站 {n} 张', {n: number(r.posts)})].filter(Boolean).join(' · '), counted: true}));
        remote.note = result.found ? '' : result.unauthorized ? t('{reason}，也可以先只在本地新建', {reason: remoteNote(result)}) : t('{reason}，可以稍后再试，或先只在本地新建', {reason: remoteNote(result)});
        remote.allow = result.unauthorized;
        // Untouched: the best match wins the highlight (the first local hit, else the first Danbooru result).
        renderPalette(sections, q, p.touched && p.index > 0);
      }, 380);
    }
  } else {
    if (p.suggest) {
      let sug; try { sug = await post('/api/suggest', p.suggest); } catch (e) { if (gen === p.generation && palette === p) renderPalette([{label: t('本地候选'), items: [], note: readError(e)}], q); return; }
      if (gen !== p.generation) return;
      if (sug.length) sections.push({label: t('也可能是'), items: sug.filter(e => !(p.exclude || []).includes(e.id)).map(e => ({...e, hint: e.count > 1 ? t('{n} 张的机器候选', {n: number(e.count)}) : t('机器候选'), counted: e.count > 1}))});
    }
    const seen = new Set(sections.flatMap(s => s.items.map(e => e.id)));
    const recent = prefs.get('picks', []).filter(e => !seen.has(e.id) && !(p.exclude || []).includes(e.id));
    if (recent.length) sections.push({label: t('最近使用'), items: recent}); recent.forEach(e => seen.add(e.id));
    const top = state.chars.filter(e => !seen.has(e.id) && !(p.exclude || []).includes(e.id)).slice(0, 8);
    if (top.length) sections.push({label: t('常用'), items: top.map(e => ({...e, hint: charHint(e)}))});
  }
  renderPalette(sections, q);
}
function renderPalette(sections, q, keep = false) {
  const p = palette, current = keep ? p.list[p.index] : null;
  p.list = sections.flatMap(s => s.items);
  p.index = current ? Math.max(0, p.list.indexOf(current)) : 0;
  // Local-only creation is a fallback: never the default when anything else matched.
  if (!current && p.list[p.index]?.create) p.index = Math.max(0, p.list.findIndex(e => !e.create));
  let n = 0;
  $('pickerResults').innerHTML = sections.filter(s => s.items.length || s.note).map(s => (s.label ? `<div class="palette-label">${s.label}</div>` : '') + (s.note && !s.items.length ? `<p class="palette-note">${esc(s.note)}${s.allow ? ' ' + namesAllowButton : ''}</p>` : '') + s.items.map(e => {
    const i = n++, char = e.id && state.charMap.get(e.id), cover = char?.cover;
    const staged = p.multi && p.staged.some(x => stageKey(x) === stageKey(e));
    const add = p.multi ? `<span class="palette-add${staged ? ' on' : ''}" data-add="${i}" title="${staged ? t('已加入，再点一次取消') : t('再加一个角色（⇧+点击）')}">${staged ? '✓' : '＋'}</span>` : '';
    return `<button class="palette-item${staged ? ' staged' : ''}" data-i="${i}" role="option">${cover ? `<img src="/image?id=${encodeURIComponent(cover)}" alt="" loading="lazy">` : `<span class="palette-glyph${e.remote ? ' remote' : ''}">${e.remote ? 'D' : e.create ? '＋' : e.remove ? '−' : e.tag ? '#' : '◌'}</span>`}<span class="palette-text"><span class="palette-name">${e.create ? (e.oc ? t('新建原创角色「{name}」', {name: esc(e.name)}) : t('新建「{name}」', {name: esc(e.name)})) : esc(e.name)}</span><small>${esc(e.hint || '')}${char && !e.counted ? ' ' + t('{n} 张', {n: number(char.count)}) : ''}</small></span>${i < 9 && !q ? `<kbd>${i + 1}</kbd>` : ''}${add}</button>`;
  }).join('')).join('') || `<p class="palette-empty">${q ? t('没有匹配的角色') : t('输入角色 tag 或别名')}</p>`;
  for (const img of $('pickerResults').querySelectorAll('img')) img.addEventListener('error', () => img.replaceWith(Object.assign(document.createElement('span'), {className: 'palette-glyph', textContent: '◌'})), {once: true});
  highlight();
}
function highlight() {
  const p = palette; if (!p) return;
  for (const el of $('pickerResults').querySelectorAll('.palette-item')) el.classList.toggle('on', Number(el.dataset.i) === p.index);
  $('pickerResults').querySelector('.palette-item.on')?.scrollIntoView({block: 'nearest'});
  const e = p.list[p.index], chosen = p.multi ? finalList(e, p.touched) : e ? [e] : [];
  const sentence = chosen.length ? (p.sentence ? p.sentence({...chosen[0], name: chosen.map(x => x.name).join(t('、'))}) : chosen.map(x => x.name).join(t('、'))) : '';
  $('pickerFoot').innerHTML = chosen.length ? `<kbd>↵</kbd> ${esc(sentence)}${p.multi ? `<span class="palette-tip"><kbd>⇧↵</kbd> ${p.staged.length ? t('继续加') : t('多选')}</span>` : ''}` : `<span>${esc(p.title || '')}</span>`;
}
// Staged characters plus the highlighted one (unless it is already staged).
// Right after staging, ↵ applies the staged ones only; the highlight counts again once the user moves, types or clicks.
function finalList(e, include) {
  const p = palette, list = [...p.staged];
  if (e && (include || !list.length) && !e.remove && !list.some(x => stageKey(x) === stageKey(e))) list.push(e);
  return list;
}
function renderStaged() {
  const p = palette, box = $('pickerStaged');
  box.hidden = !p?.staged.length;
  box.innerHTML = (p?.staged || []).map((e, i) => `<button class="staged-chip" data-unstage="${i}" title="${t('移除')}">${esc(e.create ? t('新建「{name}」', {name: e.name}) : e.name)} ×</button>`).join('');
}
function stage(e) {
  const p = palette, k = stageKey(e), at = p.staged.findIndex(x => stageKey(x) === k);
  if (at >= 0) p.staged.splice(at, 1); else p.staged.push(e);
  p.touched = false; renderStaged(); $('pickerSearch').value = ''; $('pickerSearch').focus(); paletteSearch();
}
async function resolveEntity(e) {
  const entity = e.remote ? await post('/api/entities', {tag: e.tag, typed: e.typed})
    : e.create ? await post('/api/entities', e.oc ? {name: e.create, kind: 'oc'} : {name: e.create}) : e;
  if (!palette?.items) prefs.set('picks', [{id: entity.id, name: entity.name}, ...prefs.get('picks', []).filter(x => x.id !== entity.id)].slice(0, 8));
  return entity;
}
async function choose(i, more = false, explicit = false) {
  const p = palette, e = p?.list[i];
  if (p?.multi) {
    if (more) { if (e) stage(e); return; }
    const list = finalList(e, explicit || p.touched); if (!list.length) return;
    const entities = [];
    try { for (const x of list) entities.push(await resolveEntity(x)); } catch (err) { toast(err.message); return; }
    palette = null; $('picker').close(); p.pick(entities); return;
  }
  if (!e) return;
  if (p.tagMode) {
    if (!e.remove) prefs.set('tagPicks', [e.tag, ...prefs.get('tagPicks', []).filter(t => t !== e.tag)].slice(0, 8));
    palette = null; $('picker').close(); p.pick(e); return;
  }
  let entity; try { entity = await resolveEntity(e); } catch (err) { toast(err.message); return; }
  palette = null; $('picker').close(); p.pick(entity);
}
$('pickerSearch').addEventListener('input', () => { if (palette) palette.touched = true; clearTimeout(pickerTimer); pickerTimer = setTimeout(paletteSearch, 140); });
$('pickerSearch').addEventListener('keydown', e => {
  const p = palette; if (!p) return;
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') p.touched = true;
  if (e.key === 'ArrowDown') { e.preventDefault(); p.index = Math.min(p.list.length - 1, p.index + 1); highlight(); }
  else if (e.key === 'ArrowUp') { e.preventDefault(); p.index = Math.max(0, p.index - 1); highlight(); }
  else if (e.key === 'Enter') { e.preventDefault(); choose(p.index, e.shiftKey); }
  else if (/^[1-9]$/.test(e.key) && !$('pickerSearch').value) { e.preventDefault(); choose(Number(e.key) - 1, false, true); }
});
$('pickerResults').addEventListener('click', e => { const b = e.target.closest('[data-i]'); if (b) choose(Number(b.dataset.i), e.shiftKey || !!e.target.closest('[data-add]'), true); });
$('pickerStaged').addEventListener('click', e => { const b = e.target.closest('[data-unstage]'); if (b && palette) stage(palette.staged[Number(b.dataset.unstage)]); });
$('pickerResults').addEventListener('mousemove', e => { const b = e.target.closest('[data-i]'); if (b && palette && palette.index !== Number(b.dataset.i)) { palette.index = Number(b.dataset.i); highlight(); } });
$('picker').addEventListener('close', () => { palette = null; });
function mergeCharacter(c) {
  openPicker({title: t('把 {name} 合并到…', {name: c.name}), placeholder: t('输入同一个人的另一个角色'), jump: true, exclude: [c.id],
    sentence: e => t('把 {name} 并入 {target}：图、人工结论、子标签和别名都过去，可撤销', {name: c.name, target: e.name}),
    pick: async e => {
      try { const r = await post('/api/merge', {source: c.id, target: e.id}); setLatest(r.undo); toast(tb(r.undo.label), true); await refreshMeta(); go(charRoute(e.id)); }
      catch (err) { toast(err.message); }
    }});
}
// ---------- original characters ----------
// An original character (kind='oc'): an artist's own character, belonging to no work. Unnamed ones are 原创角色 N.
const charHint = e => e.kind === 'oc' ? t('原创角色') : e.work_name || workName(e.work);
// Quick tagging uses the one shared「原创角色（未细分）」; a look-alike group (all one person) still gets its own OC.
async function markOc(body, leaving, separate = false) {
  let entity; try { entity = await post(separate ? '/api/entities' : '/api/oc/shared', {kind: 'oc'}); } catch (err) { toast(err.message); return; }
  prefs.set('picks', [{id: entity.id, name: entity.name}, ...prefs.get('picks', []).filter(x => x.id !== entity.id)].slice(0, 8));
  if (await mutate('/api/change', {...body, action: 'confirmed', entity: entity.id}, leaving) && state.route.page === 'group' && !state.total) go(charRoute(entity.id));
}
// Split the shared OC mark: look-alike groups computed on demand, each one click to its own OC (undoable).
async function splitOc(c) {
  const d = $('ocSplitDialog'), body = $('ocSplitBody');
  $('ocSplitTitle').textContent = t('按相似细分 · {name}', {name: c.name});
  body.innerHTML = `<p class="muted-note">${t('正在比较长相…只在本机计算')}</p>`;
  d.showModal();
  let r; try { r = await api('/api/oc/groups?' + new URLSearchParams({entity: c.id})); } catch (e) { body.innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  const grouped = r.groups.reduce((n, g) => n + g.length, 0);
  body.innerHTML = `<p class="muted-note">${t('{n} 张里有 {groups} 组长得像（共 {grouped} 张）；其余的不像任何一张，留在这里。', {n: number(r.total), groups: number(r.groups.length), grouped: number(grouped)})}</p>` +
    (r.groups.map((g, i) => `<section class="oc-split-group" data-g="${i}"><div class="oc-split-thumbs">${g.slice(0, 8).map(u => `<img src="/image?id=${encodeURIComponent(u)}" alt="" loading="lazy">`).join('')}${g.length > 8 ? `<span>+${number(g.length - 8)}</span>` : ''}</div><div class="oc-split-meta"><span>${t('{n} 张', {n: number(g.length)})}</span><button class="quiet" data-split="${i}">${t('设为新的原创角色')}</button></div></section>`).join('') || `<p class="palette-empty">${t('没有长得像、能成组的图。可以在图库里选中后用「换成…」新建原创角色。')}</p>`);
  for (const b of body.querySelectorAll('[data-split]')) b.onclick = async () => {
    const ids = r.groups[Number(b.dataset.split)]; b.disabled = true;
    let entity; try { entity = await post('/api/entities', {kind: 'oc'}); } catch (e) { toast(e.message); b.disabled = false; return; }
    if (await mutate('/api/reassign', {ids, old: c.id, new: entity.id}, true)) b.closest('.oc-split-group').remove();
    else b.disabled = false;
  };
}
$('ocSplitClose').onclick = () => $('ocSplitDialog').close();
// ---------- character profile (编辑资料) ----------
// A search box with a result menu: `search(q)` resolves to sections [{label, items, note}], `pick(item)` takes one.
function combo(input, menu, search, pick) {
  let list = [], index = 0, gen = 0, timer = 0;
  const render = sections => {
    list = sections.flatMap(x => x.items); index = Math.min(index, Math.max(0, list.length - 1));
    let i = 0;
    menu.innerHTML = sections.filter(x => x.items.length || x.note).map(x => (x.label ? `<div class="palette-label">${esc(x.label)}</div>` : '') + (x.note ? `<p class="palette-note">${esc(x.note)}${x.allow ? ' ' + namesAllowButton : ''}</p>` : '') +
      x.items.map(r => `<button type="button" class="picker-item watch-option${i === index ? ' on' : ''}" data-i="${i++}"><span class="watch-name">${esc(r.label)}</span><small>${esc(r.detail || '')}</small><em>${esc(r.extra || '')}</em></button>`).join('')).join('');
    menu.hidden = !menu.innerHTML;
  };
  const run = () => { const g = ++gen; clearTimeout(timer); timer = setTimeout(() => search(input.value.trim(), sections => { if (g === gen) render(sections); }), 160); };
  const choose = i => { const r = list[i]; if (!r) return; menu.hidden = true; pick(r); };
  input.addEventListener('input', () => { index = 0; run(); });
  input.addEventListener('focus', run);
  input.addEventListener('blur', () => setTimeout(() => { menu.hidden = true; }, 150));
  input.addEventListener('keydown', e => {
    if (menu.hidden || !list.length) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); index = (index + (e.key === 'ArrowDown' ? 1 : list.length - 1)) % list.length; for (const b of menu.querySelectorAll('[data-i]')) b.classList.toggle('on', Number(b.dataset.i) === index); }
    else if (e.key === 'Enter') { e.preventDefault(); choose(index); }
    else if (e.key === 'Escape') { e.stopPropagation(); menu.hidden = true; }
  });
  menu.addEventListener('mousedown', e => { const b = e.target.closest('[data-i]'); if (b) { e.preventDefault(); choose(Number(b.dataset.i)); } });
}
// Local results first (instant), then Danbooru for the typed text only.
function twoStage(local, remote, extra = () => []) {
  return (q, show) => {
    local(q).then(mine => {
      if (!q) { show([{label: '', items: mine}, ...extra(q)]); return; }
      show([{label: '', items: mine}, {label: 'Danbooru', items: [], note: t('正在查 Danbooru…（只发送你输入的文字）')}, ...extra(q)]);
      remote(q).then(result => {
        // A Danbooru entry the library already has shows once, as the local one.
        const found = result.found, fresh = found && found.filter(r => !mine.some(m => m.detail && m.detail === r.detail));
        show([{label: '', items: mine}, {label: 'Danbooru', items: fresh || [], allow: result.unauthorized,
          note: found === null ? remoteNote(result) : fresh.length ? '' : found.length ? '' : t('没有找到，可以换英文或日文写法')}, ...extra(q)]);
      });
    }).catch(e => show([{label: t('本地资料'), items: [], note: readError(e)}]));
  };
}
async function editProfile(c) {
  let p; try { p = await api('/api/character/profile?' + new URLSearchParams({entity: c.id})); } catch (e) { toast(e.message); return; }
  const draft = {aliases: [...p.aliases], work: p.work_id ? {id: p.work_id, label: p.work_name} : null, tag: p.tag};
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
  const autoName = p.tag && !p.user_name ? p.name : '';
  d.innerHTML = `<form method="dialog">
    <div class="dialog-top"><h2>${t('编辑资料')}</h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
    ${p.label_only ? '' : `<label class="field"><span>${t('Danbooru 角色')}</span><div class="combo"><input id="pfTag" autocomplete="off" spellcheck="false" placeholder="${t('搜索角色名或 tag，如 karen blue、カレン')}"><div class="combo-menu" id="pfTagMenu" hidden></div></div>
      <small id="pfTagNote"></small></label>`}
    <label class="field"><span>${t('显示名')}</span><input id="pfName" maxlength="120" value="${esc(p.user_name)}" placeholder="${esc(autoName ? t('留空：用自动名称「{name}」', {name: autoName}) : t('角色名称'))}">
      <small>${p.tag ? t('关联了 Danbooru 时可以留空，中文名会自动补全') : t('没有关联 Danbooru 时需要一个名称')}</small></label>
    <div class="field"><span>${t('别名')}</span><div class="alias-box" id="pfAliases"></div><input id="pfAlias" maxlength="120" placeholder="${t('输入别名后按回车，搜索时都能找到')}">
      <small>${p.other_aliases ? t('另有 {n} 个来自 Danbooru 等来源的别名，会自动更新', {n: number(p.other_aliases)}) : ''}</small></div>
    <label class="field"><span>${t('所属作品')}</span>${p.oc ? `<small>${t('原创角色不属于任何作品')}</small>` : `<div class="combo"><input id="pfWork" autocomplete="off" placeholder="${t('搜索作品，找不到可以新建')}"><div class="combo-menu" id="pfWorkMenu" hidden></div></div><small id="pfWorkNote"></small>`}</label>
    <div class="dialog-actions"><button type="button" class="quiet" data-close>${t('取消')}</button><button class="primary" id="pfSave">${t('保存')}</button></div></form>`;
  document.body.append(d);
  const close = () => { d.close(); d.remove(); };
  for (const b of d.querySelectorAll('[data-close]')) b.onclick = close;
  d.addEventListener('close', () => d.remove());
  const q = sel => d.querySelector(sel);
  const aliases = () => { q('#pfAliases').innerHTML = draft.aliases.map((a, i) => `<button type="button" class="staged-chip" data-a="${i}" title="${t('移除')}">${esc(a)} ×</button>`).join(''); };
  q('#pfAliases').onclick = e => { const b = e.target.closest('[data-a]'); if (b) { draft.aliases.splice(Number(b.dataset.a), 1); aliases(); } };
  q('#pfAlias').onkeydown = e => { const v = e.target.value.trim(); if (e.key === 'Enter') { e.preventDefault(); if (v && !draft.aliases.includes(v)) draft.aliases.push(v); e.target.value = ''; aliases(); } };
  aliases();
  const tagNote = () => { if (!q('#pfTagNote')) return; q('#pfTag').value = draft.tag ? draft.tag.replaceAll('_', ' ') : ''; q('#pfTagNote').innerHTML = draft.tag ? `${t('已关联 {tag}', {tag: `<code>${esc(draft.tag)}</code>`})} · <button type="button" class="link-button" id="pfUnlink">${t('取消关联')}</button>` : t('关联后自动补中文名、别名和所属作品'); q('#pfUnlink')?.addEventListener('click', () => { draft.tag = ''; tagNote(); }); };
  const workNote = () => { if (!q('#pfWork')) return; q('#pfWork').value = draft.work?.label || ''; q('#pfWorkNote').innerHTML = draft.work ? `${draft.work.create ? t('保存时新建这个作品') + ' · ' : draft.work.tag ? t('保存时从 Danbooru 建立这个作品') + ' · ' : ''}<button type="button" class="link-button" id="pfNoWork">${t('不归属任何作品')}</button>` : (draft.tag && draft.tag !== p.tag && !p.work_id ? (draft.tagWork ? t('留空：按 Danbooru 自动设置（{work}）', {work: draft.tagWork}) : t('留空：按 Danbooru 自动设置')) : t('未归类')); q('#pfNoWork')?.addEventListener('click', () => { draft.work = null; workNote(); }); };
  tagNote(); workNote();
  if (q('#pfTag')) combo(q('#pfTag'), q('#pfTagMenu'), (text, show) => {
    if (!text) { show([]); return; }
    show([{label: 'Danbooru', items: [], note: t('正在查 Danbooru…（只发送你输入的文字）')}]);
    remoteNames('/api/entities?' + new URLSearchParams({q: text, remote: 1})).then(({found, ...result}) => show([found
      ? {label: 'Danbooru', items: found.map(r => ({...r, label: r.name, detail: r.tag, extra: [r.work_name, t('D 站 {n}', {n: number(r.posts)})].filter(Boolean).join(' · ')})), note: found.length ? '' : t('没有找到，或已经在图库里（那就用「合并到另一个角色」）')}
      : {label: 'Danbooru', items: [], note: remoteNote(result), allow: result.unauthorized}]));
  }, r => { draft.tag = r.tag; draft.tagWork = r.work_name; tagNote(); workNote(); });
  if (q('#pfWork')) combo(q('#pfWork'), q('#pfWorkMenu'), twoStage(
    text => api('/api/works?' + new URLSearchParams({q: text})).then(ws => ws.slice(0, 8).map(w => ({id: w.id, label: w.name, detail: w.tag || '', extra: w.count ? t('你有 {n} 张', {n: number(w.count)}) : ''}))),
    text => remoteNames('/api/works?' + new URLSearchParams({q: text, remote: 1})).then(r => r.found ? {found: r.found.map(w => ({tag: w.tag, label: workName(w.tag) || w.tag, detail: w.tag, extra: t('D 站 {n}', {n: number(w.posts)})}))} : r),
    text => text ? [{label: '', items: [{create: text, label: t('新建作品「{name}」', {name: text}), detail: t('只在本地')}]}] : []),
    r => { draft.work = r; workNote(); });
  q('form').onsubmit = async e => {
    e.preventDefault();
    if (q('#pfSave').disabled) return;  // one save at a time: no second work or profile from a double click
    const body = {entity: p.id};
    const name = q('#pfName').value.trim(); if (name !== p.user_name) body.name = name;
    if (JSON.stringify(draft.aliases) !== JSON.stringify(p.aliases)) body.aliases = draft.aliases;
    if (q('#pfTag') && draft.tag !== p.tag) body.tag = draft.tag;
    // A new work is made in the same request (and transaction) as the profile it belongs to.
    if (q('#pfWork') && (draft.work?.id || null) !== (p.work_id || null)) {
      body.work = !draft.work ? '' : draft.work.create ? {create: draft.work.create} : draft.work.tag ? {tag: draft.work.tag} : draft.work.id;
    }
    if (Object.keys(body).length === 1) { close(); return; }
    q('#pfSave').disabled = true;
    try {
      const r = await post('/api/character/profile', body);
      close(); setLatest(r.undo); toast(tb(r.undo.label), true);
      state.localLinks = null; await refreshMeta(); renderHeading();
    } catch (err) { q('#pfSave').disabled = false; toast(err.message); }
  };
  d.showModal(); (q('#pfTag') && !p.tag ? q('#pfTag') : q('#pfName')).focus();
}
async function openLocalLinks() {
  const items = state.localLinks || [];
  const d = document.createElement('dialog'); d.className = 'palette profile-dialog';
  const links = items.filter(x => x.kind === 'link'), merges = items.filter(x => x.kind === 'merge');
  d.innerHTML = `<form method="dialog"><div class="dialog-top"><h2>${t('关联自建角色')}</h2><button type="button" class="icon" data-close aria-label="${t('关闭')}">×</button></div>
    ${links.length ? `<p class="muted-note">${t('关联后自动补中文名、别名和所属作品，原来的名字留作别名，可以一步撤销。名字和 tag 完全一致的已勾选；标「可能是」的是按名字搜到的最像的一个，请确认后再勾。')}</p>
      <div class="link-list">${links.map(x => `<label class="link-row"><input type="checkbox" data-link="${esc(x.id)}"${x.guess ? '' : ' checked'}><span><b>${esc(x.name)}${x.guess ? ` <i class="guess">${t('可能是')}</i>` : ''}</b><small>${esc(x.tag)}${x.work_name ? ' · ' + esc(x.work_name) : ''} · ${t('D 站 {n} 张', {n: number(x.posts)})}</small></span><em>${t('{n} 张', {n: number(x.count)})}</em></label>`).join('')}</div>` : ''}
    ${merges.length ? `<p class="muted-note">${t('下面的自建角色和图库里已有的角色像是同一个，合并后图和人工结论都会过去（可撤销）：')}</p>
      <div class="link-list">${merges.map(x => `<div class="link-row"><span><b>${esc(x.name)}${x.guess ? ` <i class="guess">${t('可能是')}</i>` : ''}</b><small>${t('{n} 张', {n: number(x.count)})} → ${esc(x.target.name)} · ${esc(x.tag)}</small></span><button type="button" class="quiet" data-merge="${esc(x.id)}">${t('合并到「{name}」', {name: esc(x.target.name)})}</button></div>`).join('')}</div>` : ''}
    <div class="dialog-actions"><button type="button" class="quiet" data-close>${t('关闭')}</button>${links.length ? '<button class="primary" id="llApply"></button>' : ''}</div></form>`;
  document.body.append(d);
  const close = () => { d.close(); d.remove(); };
  for (const b of d.querySelectorAll('[data-close]')) b.onclick = close;
  d.addEventListener('close', () => d.remove());
  const count = () => { const n = d.querySelectorAll('[data-link]:checked').length; if ($('llApply')) { $('llApply').textContent = t('关联选中的 {n} 个', {n: number(n)}); $('llApply').disabled = !n; } };
  d.addEventListener('change', count);
  count();
  for (const b of d.querySelectorAll('[data-merge]')) b.onclick = async () => {
    const x = merges.find(m => m.id === b.dataset.merge); b.disabled = true;
    try { const r = await post('/api/merge', {source: x.id, target: x.target.id}); setLatest(r.undo); toast(tb(r.undo.label), true); b.closest('.link-row').remove(); await refreshMeta(); refreshLocalLinks(); }
    catch (e) { b.disabled = false; toast(e.message); }
  };
  d.querySelector('form').onsubmit = async e => {
    e.preventDefault();
    const chosen = links.filter(x => d.querySelector(`[data-link="${CSS.escape(x.id)}"]`)?.checked).map(x => ({id: x.id, tag: x.tag}));
    if (!chosen.length) return;
    $('llApply').disabled = true; $('llApply').textContent = t('正在关联…');
    try { const r = await post('/api/local-links', {items: chosen}); close(); setLatest(r.undo); toast(tb(r.undo.label), true); await refreshMeta(); refreshLocalLinks(); }
    catch (err) { count(); toast(err.message); }
  };
  d.showModal();
}
async function toggleOc(c) {
  try {
    const r = await post('/api/kind', {entity: c.id, kind: c.kind === 'oc' ? '' : 'oc'});
    Object.assign(c, r); toast(r.kind === 'oc' ? t('{name} 已标为原创角色', {name: c.name}) : t('{name} 不再标为原创角色', {name: c.name}));
    await refreshMeta(); renderHeading();
  } catch (err) { toast(err.message); }
}
async function toggleMascot(c) {
  try {
    const r = await post('/api/character/mascot', {entity: c.id, mascot: !c.mascot});
    c.mascot = r.mascot; toast(r.mascot ? t('{name} 不再算角色，只当标签', {name: c.name}) : t('{name} 已恢复成角色', {name: c.name}));
    const [fresh] = await Promise.all([fetchChar(c.id), refreshMeta(), reload()]); if (fresh) Object.assign(c, fresh); renderHeading();
  } catch (err) { toast(err.message); }
}
// ---------- how selected images relate (web/relate.py) ----------
async function openRelate(ids) {
  const d = $('relateDialog');
  $('relateTitle').textContent = t('{n} 张图的关系', {n: number(ids.length)});
  $('relateBody').innerHTML = `<p class="muted-note">${t('正在逐对比较…只在本机计算')}</p>`;
  d.showModal();
  let r; try { r = await post('/api/relate', {ids}); } catch (e) { $('relateBody').innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  const no = u => ids.indexOf(u) + 1;
  const figure = (u, boxes) => `<figure class="rel-fig"><div class="rel-img"><img src="/image?id=${encodeURIComponent(u)}&size=medium" alt="">${boxes.map(b =>
    `<i class="${b.match ? 'match' : 'diff'}" style="left:${(b.x * 100).toFixed(2)}%;top:${(b.y * 100).toFixed(2)}%;width:${(b.w * 100).toFixed(2)}%;height:${(b.h * 100).toFixed(2)}%"></i>`).join('')}<span class="rel-no">${no(u)}</span></div>
    <figcaption>${r.sizes[u] ? `${number(r.sizes[u].width)} × ${number(r.sizes[u].height)}` : ''}</figcaption></figure>`;
  const shown = r.pairs.filter(p => p.kind !== 'unrelated'), rest = r.pairs.length - shown.length;
  const summary = [...r.same.map(g => t('第 {list} 张是同一张图', {list: g.map(no).join(t('、'))})), ...(r.missing.length ? [t('{n} 张读不到原图（可能还在 iCloud），没有比较', {n: number(r.missing.length)})] : [])];
  $('relateBody').innerHTML = `${summary.length ? `<p class="rel-summary">${summary.map(esc).join(t('；'))}</p>` : ''}
    ${shown.map(p => `<section class="rel-pair"><div class="rel-head"><span class="rel-kind k-${p.kind}">${esc(tb(p.label))}</span><span>${esc(tb(p.note || ''))}</span></div>
      <div class="rel-figs">${figure(p.a, p.boxes_a)}${figure(p.b, p.boxes_b)}</div></section>`).join('')}
    ${rest ? `<p class="muted-note">${shown.length ? t('另外 {n} 对是不同的图', {n: number(rest)}) : t('{n} 对是不同的图', {n: number(rest)})}</p>` : ''}
    <p class="dialog-note">${t('红框：两张不一样的地方；蓝色虚线：小图在大图里的位置。逐像素比较得出，只在本机计算，结果不保存。')}</p>`;
}
$('relateClose').onclick = () => $('relateDialog').close();

function jumpToCharacter() { openPicker({title: t('跳到角色'), jump: true, sentence: e => t('打开 {name}', {name: e.name}), pick: e => go(charRoute(e.id))}); }
$('jumpButton').onclick = jumpToCharacter;

// ---------- inspector ----------
function closeInspector() { detailGen++; if (state.viewer < 0) kept.hold = null; state.focused = null; state.detail = null; if (state.viewer < 0) state.question = null; $('inspector').hidden = true; markHomeFocus(); syncCards(); requestAnimationFrame(() => renderGrid()); }
function identityChips(d, compact = false) {
  const r = state.route;
  const chips = d.memberships.map(m => `<div class="identity"><span class="dot ${m.status}" title="${m.status === 'machine' ? t('机器识别') : m.origin === 'album' ? t('相簿') : t('人工确认')} · ${esc(evidence(m.evidence))}"></span><a class="identity-name" href="${charHash(m.id)}" title="${esc(m.name)}">${esc(m.name)}</a><span class="identity-actions"><button class="icon small" data-swap="${esc(m.id)}" title="${t('换成…（C）')}">⇄</button><button class="icon small" data-not="${esc(m.id)}" title="${t('不是 TA（⌫）')}">×</button></span>
    ${compact ? '' : `<div class="variant-row">${(m.variants || []).map(v => `<span class="variant-chip">${esc(v.name)}<button data-unvariant="${esc(v.id)}" title="${t('去掉子标签')}">×</button></span>`).join('')}${(state.charMap.get(m.id)?.variants || []).some(v => !(m.variants || []).some(x => x.id === v.id)) ? `<button class="variant-add" data-addvariant="${esc(m.id)}">＋ ${t('子标签')}</button>` : ''}</div>`}</div>`).join('');
  const alts = (d.alternatives || []).slice(0, 9).map((a, i) => `<button class="alt${a.weak ? ' weak' : ''}" data-alt="${i}" title="${a.weak ? t('弱线索（中档相似，不算归属）') + ' · ' : ''}${t('{evidence}。点击：换成这个；⌥ 点击：也是', {evidence: esc(evidence(a.evidence))})}"><kbd>${i + 1}</kbd>${esc(a.name)}</button>`).join('');
  return `<div class="identities">${chips || `<p class="none-yet">${d.unknown ? t('已跳过，还没有角色') : t('还没有角色')}</p>`}<button class="add-identity" data-also="1">＋ ${d.memberships.length ? t('也是…') : t('设为角色…')}</button></div>` +
    (alts ? `<div class="alts"><h3>${t('也可能是')}</h3><div class="alt-row">${alts}</div></div>` : '');
}
// PROTO = prototypes learned from the user's own verdicts (web/learn.py).
const SOURCE_NAMES = {CCIP: t('外部索引'), PROTO: t('你的标注')};
const evidence = e => !e ? '' : [e.wd != null ? `WD ${e.wd.toFixed(2)}` : '', e.ccip != null ? `${e.sources?.includes('PROTO') && !e.sources.includes('CCIP') ? SOURCE_NAMES.PROTO : SOURCE_NAMES.CCIP} ${e.ccip.toFixed(3)}` : '', !('wd' in e) && e.sources?.length ? e.sources.map(s => SOURCE_NAMES[s] || s).join(' + ') : ''].filter(Boolean).join(' · ');
const INSP_ICONS = {
  prev: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m12 4.5-5.5 5.5 5.5 5.5"/></svg>',
  next: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m8 4.5 5.5 5.5L8 15.5"/></svg>',
  expand: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="M12 3.5h4.5V8M8 16.5H3.5V12M16.5 3.5 11 9M3.5 16.5 9 11"/></svg>',
  close: '<svg viewBox="0 0 20 20" aria-hidden="true"><path d="m5 5 10 10M15 5 5 15"/></svg>',
};
// Collection rhythm of the image's character: cached per character until the next write or R18 change.
const statsCache = new Map();
async function loadCharStats(id, ident, gen) {
  const box = $('charStats'); if (!box || (!viewOpts.chart && !viewOpts.same)) return;
  const key = `${id}|${state.nsfw}|${state.readEpoch}`;
  let st = statsCache.get(key);
  if (!st) { try { st = await api('/api/character/stats?' + new URLSearchParams({id, nsfw: state.nsfw, exclude: ident})); } catch { return; } statsCache.set(key, st); }
  if (gen !== detailGen || !$('charStats')) return;
  const months = st.months || [], max = Math.max(1, ...months.map(m => m.count)), now = months.at(-1)?.count || 0;
  const recent = (st.recent || []).filter(u => u !== ident).slice(0, 4);
  $('charStats').innerHTML = (viewOpts.chart ? `<div class="cc-line"><span>${t('收藏 {n} 张', {n: `<b>${number(st.count)}</b>`})}${st.first ? ' · ' + t('始于 {date}', {date: st.first.replace('-', '.')}) : ''}</span>${now ? `<em>${t('本月 +{n}', {n: number(now)})}</em>` : ''}</div>
    <div class="cc-bars">${months.map((m, i) => `<i style="height:${Math.max(8, m.count / max * 100)}%${i === months.length - 1 ? '' : ''}" class="${i === months.length - 1 ? 'now' : ''}" title="${monthText(m.month.slice(0, 4), m.month.slice(5, 7)) + ' · ' + t('{n} 张', {n: number(m.count)})}"></i>`).join('')}</div>` : '') +
    (viewOpts.same && recent.length ? `<div class="cc-same"><h5>${t('同一角色')}<a href="${charHash(id)}">${t('全部')} ›</a></h5><div>${recent.map(u => `<button data-copy="${esc(u)}" title="${t('查看这张')}"><img src="/image?id=${encodeURIComponent(u)}&size=thumb" alt="" loading="lazy"></button>`).join('')}</div></div>` : '');
  for (const b of $('charStats').querySelectorAll('[data-copy]')) b.onclick = () => inspect(b.dataset.copy);
}
async function inspect(ident, quiet = false) {
  const gen = ++detailGen; state.focused = ident; $('inspector').hidden = false; markHomeFocus();
  if (!quiet) $('inspector').innerHTML = `<div class="loading-line">${t('读取图片详情…')}</div>`;
  syncCards(); requestAnimationFrame(() => renderGrid());
  try {
    const d = await api('/api/detail?id=' + encodeURIComponent(ident)); if (gen !== detailGen) return;
    state.detail = d;
    const primary = primaryOf(d), pc = primary && (state.charMap.get(primary.id) || primary);
    const original = pc?.tag && pc.tag !== pc.name ? pc.tag.replaceAll('_', ' ') : '';
    const path = pc ? [pc.kind === 'oc' ? t('原创角色') : pc.work_name || workName(pc.work || ''), ...(primary.variants || []).map(v => v.name)].filter(Boolean) : [];
    const curate = d.excluded ? `<button class="btn" data-curate="include" title="${t('已排除 · {reason}', {reason: esc(REASONS[d.excluded.reason] || '')})}${d.excluded.origin === 'rule' ? t('（自动规则）') : ''}">${t('恢复收录')}</button>`
      : d.pending ? `<button class="btn" data-curate="exclude">${t('排除')} <kbd>X</kbd></button><button class="btn" data-curate="include">${t('保留')}</button>`
      : `<button class="btn" data-curate="exclude" title="${t('截图、照片等不是插画的图')}">${t('排除')} <kbd>X</kbd></button>`;
    const pendingNote = d.excluded ? `<div class="cc-note">${t('已排除 · {reason}', {reason: esc(REASONS[d.excluded.reason] || '')})}</div>` : d.pending ? `<div class="cc-note">${t('待确认：像截图')}</div>` : '';
    $('inspector').innerHTML = `<div class="inspector-head"><button class="icon" data-nav="-1" aria-label="${t('上一张')}" title="${t('上一张（←）')}">${INSP_ICONS.prev}</button><button class="icon" data-nav="1" aria-label="${t('下一张')}" title="${t('下一张（→）')}">${INSP_ICONS.next}</button><div class="spacer"></div><button class="icon" id="expand" title="${t('大图（Space）')}" aria-label="${t('大图')}">${INSP_ICONS.expand}</button><button class="icon" id="closeInspector" aria-label="${t('关闭详情')}" title="${t('关闭（Esc）')}">${INSP_ICONS.close}</button></div>
      <button class="preview" id="previewButton" title="${t('大图查看')}"><img src="/image?id=${encodeURIComponent(ident)}&size=preview" alt="${t('插画预览')}"></button>
      ${shouldAsk(d, ident) ? askHTML(d.question) : ''}
      ${pc ? `<section class="inspector-section cc-card"><a class="cc-name" href="${charHash(pc.id)}" title="${t('在图库里只看 TA')}">${esc(pc.name)}</a>${original ? `<div class="cc-orig">${esc(original)}</div>` : ''}${path.length ? `<div class="cc-path">${path.map(esc).join(' · ')}</div>` : ''}${pendingNote}
        <div id="charStats" class="cc-stats"></div></section>`
        : `<section class="inspector-section cc-card"><div class="cc-name muted">${d.unknown ? t('已跳过') : t('还没认出是谁')}</div>${pendingNote}</section>`}
      <section class="inspector-section"><h3 class="section-title">${t('角色')}</h3>${identityChips(d)}${(d.tag_identities || []).length ? `<div class="tag-ids"><span>${t('标签')}</span>${d.tag_identities.map(x => `<a class="variant-chip" href="${charHash(x.id)}" title="${t('只当标签、不算角色的身份；点开可以改回角色')}">${esc(x.name)}</a>`).join('')}</div>` : ''}</section>
      ${artistSection(d, ident)}
      <section class="inspector-section"><h3 class="section-title">${t('我的标签')}</h3><div class="user-tags">${(d.user_tags || []).map(tag => `<span class="variant-chip"><a href="#/?utag=${encodeURIComponent(tag)}"># ${esc(tag)}</a><button data-untag="${esc(tag)}" title="${t('去掉标签')}">×</button></span>`).join('')}<button class="variant-add" id="addTag">＋ ${t('标签')} <kbd>G</kbd></button></div></section>
      ${d.duplicates?.length ? `<section class="inspector-section"><h3 class="section-title">${t('重复副本 · 共 {n} 份', {n: d.duplicates.length + 1})}</h3><div class="dup-list">${d.duplicates.map(x => `<button class="dup-copy" data-copy="${esc(x.uuid)}" title="${t('查看这份副本')}"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=thumb" alt="" loading="lazy"><span><b>${x.pos}/${x.size}</b> ${esc((x.added_at || '').slice(0, 10))}<br><small>${x.names.length ? x.names.map(esc).join(' · ') : t('未识别')}</small></span></button>`).join('')}</div></section>` : ''}
      ${viewOpts.meta ? `<section class="inspector-section"><dl class="facts"><div><dt>${t('尺寸')}</dt><dd class="mono">${number(d.width)} × ${number(d.height)}</dd></div><div><dt>${t('加入')}</dt><dd class="mono">${esc((d.added_at || '').slice(0, 16).replace('T', ' '))}</dd></div><div><dt>${t('文件')}</dt><dd title="${esc(d.filename)}">${esc(d.filename)}</dd></div><div><dt>${t('文件形态')}</dt><dd>${d.src_kind === 'deriv' ? t('本地副本') : t('原图')}</dd></div></dl></section>
      <details class="inspector-section"><summary>${t('画面标签')}</summary><div class="tags">${d.tags.map(x => `<button class="tag" data-tag="${esc(x.value)}" title="${t('按此标签筛选图库 · WD {conf}', {conf: x.conf.toFixed(2)})}">${esc(x.value.replaceAll('_', ' '))}</button>`).join('') || `<p>${t('暂无')}</p>`}</div></details>` : ''}
      <div class="inspector-actions">${d.memberships.length ? `<button class="btn pri" id="fixChar">${t('更正角色')} <kbd>C</kbd></button>` : `<button class="btn pri" data-also="1">${t('设为角色…')} <kbd>C</kbd></button>`}${state.route.page === 'unmatched' && !d.memberships.length ? `<button class="btn" id="skipOne">${t('跳过')} <kbd>S</kbd></button>` : ''}${d.unknown ? `<button class="btn" id="resumeOne">${t('恢复到未识别')}</button>` : ''}${curate}</div>`;
    if (pc) loadCharStats(pc.id, ident, gen);
    $('fixChar')?.addEventListener('click', () => swapOne(d, ident, primary.id));
    $('inspector').querySelector('.preview img').addEventListener('error', e => { e.target.parentElement.innerHTML = `<div class="missing">${t('图片暂不可读，请检查 Photos 下载状态与访问权限')}</div>`; }, {once: true});
    $('closeInspector').onclick = closeInspector;
    $('expand').onclick = $('previewButton').onclick = () => openViewer(indexOf(ident));
    for (const b of $('inspector').querySelectorAll('[data-nav]')) b.onclick = () => step(Number(b.dataset.nav));
    for (const b of $('inspector').querySelectorAll('[data-tag]')) b.onclick = () => go({page: 'library', tag: b.dataset.tag});
    for (const b of $('inspector').querySelectorAll('[data-copy]')) b.onclick = () => inspect(b.dataset.copy);
    for (const b of $('inspector').querySelectorAll('[data-curate]')) b.onclick = () => mutate('/api/curate', {ids: [ident], action: b.dataset.curate}, b.dataset.curate === 'exclude' || ['excluded', 'pending'].includes(state.route.page));
    $('skipOne')?.addEventListener('click', () => singleAction('skip'));
    $('addTag').onclick = () => singleAction('tag');
    for (const b of $('inspector').querySelectorAll('[data-artist-verdict]')) b.onclick = () => {
      const v = b.dataset.artistVerdict || null;
      mutate('/api/artist/verdict', {ids: [ident], artist: b.dataset.artist, verdict: v, name: b.dataset.name}, v === 'no' && state.route.artist === b.dataset.artist);
    };
    $('inspector').querySelector('[data-artist-find]')?.addEventListener('click', () => findArtists([ident], () => inspect(ident, true)));
    for (const b of $('inspector').querySelectorAll('[data-artist-more]')) b.onclick = () => findArtistWorks(b.dataset.artistMore, b.dataset.name, () => inspect(ident, true));
    for (const b of $('inspector').querySelectorAll('[data-untag]')) b.onclick = () => mutate('/api/tag', {ids: [ident], tag: b.dataset.untag, add: false}, state.route.utag === b.dataset.untag);
    $('resumeOne')?.addEventListener('click', () => mutate('/api/change', {ids: [ident], action: 'resume'}, state.route.page === 'skipped'));
    bindIdentity($('inspector'), d, ident); bindAsk($('inspector'), d, ident);
  } catch (e) { if (gen === detailGen) { $('inspector').innerHTML = `<div class="loading-line">${esc(e.message)}<br><button class="quiet" id="closeInspector">${t('关闭')}</button></div>`; $('closeInspector').onclick = closeInspector; } }
}
function bindIdentity(root, d, ident) {
  const leaves = id => state.route.char === id;
  for (const b of root.querySelectorAll('[data-not]')) b.onclick = () => mutate('/api/change', {ids: [ident], action: 'rejected', entity: b.dataset.not}, leaves(b.dataset.not));
  for (const b of root.querySelectorAll('[data-swap]')) b.onclick = () => swapOne(d, ident, b.dataset.swap);
  for (const b of root.querySelectorAll('[data-unvariant]')) b.onclick = () => mutate('/api/change', {ids: [ident], action: 'remove_variant', entity: b.dataset.unvariant}, !!state.route.variant);
  for (const b of root.querySelectorAll('[data-addvariant]')) b.onclick = async () => {
    const epoch = state.readEpoch, mode = state.nsfw;
    const c = await fetchChar(b.dataset.addvariant);
    if (!c || epoch !== state.readEpoch || mode !== state.nsfw || (state.focused !== ident && state.viewerItem?.uuid !== ident)) return;
    openPicker({title: t('加子标签'), items: c.variants.map(v => ({id: v.id, name: v.name, hint: t('{n} 张', {n: number(v.count)}), counted: true})), placeholder: t('筛选子标签'), sentence: e => t('给这张加上 {name}', {name: e.name}), pick: e => mutate('/api/change', {ids: [ident], action: 'add_variant', entity: e.id}, false)});
  };
  for (const b of root.querySelectorAll('[data-alt]')) b.onclick = e => pickAlternative(d, ident, Number(b.dataset.alt), e.altKey);
  for (const b of root.querySelectorAll('[data-also]')) b.onclick = () => openPicker({title: d.memberships.length ? t('这张也是…') : t('这张是…'), sentence: e => d.memberships.length ? t('给这张加上 {name}', {name: e.name}) : t('把这张设为 {name}', {name: e.name}), exclude: d.memberships.map(m => m.id), suggest: {ids: [ident]}, multi: true,
    pick: list => mutate('/api/change', {ids: [ident], action: 'confirmed', ...targets(list)}, ['unmatched', 'skipped'].includes(state.route.page))});
}
// ---------- 主动提问：偶尔问一句「是 TA 吗？」 ----------
// While browsing, at most one question every few images, and none for the rest of the visit after three are passed by;
// cleanup pages (a character, 未识别…) always show it. Answers go to /api/ask/answer and feed the learner.
const ask = {cool: 0, ignored: 0, off: false, pending: null};
function shouldAsk(d, ident) {
  if (!viewOpts.ask || !d.question) return false;
  if (ask.pending === ident) return true;
  if (ask.pending) { ask.ignored++; ask.pending = null; if (ask.ignored >= 3) ask.off = true; }
  if (!cleanup()) {
    if (ask.off) return false;
    if (ask.cool > 0) { ask.cool--; return false; }
    ask.cool = 3;
  }
  ask.pending = ident; return true;
}
function askHTML(q, dark = false) {
  const why = q.kind === 'audit' ? t('随机抽查') : q.kind === 'hint' ? t('和你确认过的 TA 很像') + (q.current_name ? ' · ' + t('现在算作 {name}', {name: esc(q.current_name)}) : '') : q.kind === 'ambiguous' ? t('和 {name} 分不太清', {name: esc(q.rival_name)}) : q.confirmed < 5 ? t('再确认 {n} 张就能学会认 TA', {n: 5 - q.confirmed}) : '';
  return `<div class="ask-bar${dark ? ' on-dark' : ''}"><div class="ask-q">${t('是 {name} 吗？', {name: `<b>${esc(q.name)}</b>`})}${why ? `<small>${why}</small>` : ''}</div>
    <div class="ask-btns"><button class="ask-b yes" data-ask="yes">${t('是')} <kbd>Y</kbd></button><button class="ask-b" data-ask="no">${t('不是')} <kbd>N</kbd></button>${q.rival ? `<button class="ask-b" data-ask="switch" title="${t('换成 {name}', {name: esc(q.rival_name)})}">${t('是 {name}', {name: esc(q.rival_name)})}</button>` : ''}</div></div>`;
}
function bindAsk(root, d, ident) {
  state.question = root.querySelector('.ask-bar') ? {q: d.question, ident} : null;
  for (const b of root.querySelectorAll('[data-ask]')) b.onclick = () => answerAsk(b.dataset.ask);
}
function answerAsk(how) {
  const cur = state.question; if (!cur) return;
  const {q, ident} = cur; ask.pending = null; ask.ignored = 0;
  // 随机抽查: the answered image leaves the list, so the inspector moves on to the next one.
  if (state.route.page === 'audit' && state.viewer < 0 && state.focused === ident) { const i = indexOf(ident); state.focused = state.items[i + 1]?.uuid || state.items[i - 1]?.uuid || null; }
  const body = {ids: [ident], entity: q.entity, kind: q.kind, ...(how === 'yes' ? {yes: [ident]} : {no: [ident]}), ...(how === 'switch' ? {switch: q.rival} : {})};
  mutate('/api/ask/answer', body, how !== 'yes' && state.route.char === q.entity, {advance: state.route.page === 'audit'});
}
// The identity an action replaces: the current character page if it applies, else the first one.
const primaryOf = d => d.memberships.find(m => m.id === state.route.char) || d.memberships[0];
function swapOne(d, ident, from) {
  const old = d.memberships.find(m => m.id === from);
  openPicker({title: t('换成…'), sentence: e => t('把这张从 {from} 换成 {name}', {from: old.name, name: e.name}), exclude: d.memberships.map(m => m.id), suggest: {ids: [ident]}, multi: true,
    pick: list => mutate('/api/reassign', {ids: [ident], old: from, new: newIds(list)}, state.route.char === from)});
}
function pickAlternative(d, ident, i, also = false) {
  const alt = d.alternatives?.[i]; if (!alt) return;
  const from = primaryOf(d);
  if (from && !also) mutate('/api/reassign', {ids: [ident], old: from.id, new: alt.id}, state.route.char === from.id);
  else mutate('/api/change', {ids: [ident], action: 'confirmed', entity: alt.id}, ['unmatched', 'skipped'].includes(state.route.page));
}
// Single-image shortcuts act on the focused/cursor image when nothing is selected.
function singleTarget() { return currentItem(); }
async function singleAction(kind) {
  const item = singleTarget(); if (!item) return;
  const r = state.route;
  if (kind === 'remove') {
    if (r.char) mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: r.char}, true);
    else if (item.memberships.length === 1) mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: item.memberships[0].id}, false);
    else if (item.memberships.length) openPicker({title: t('这张不是…'), items: item.memberships, sentence: e => t('把 {name} 从这张移出', {name: e.name}), pick: e => mutate('/api/change', {ids: [item.uuid], action: 'rejected', entity: e.id}, false)});
  } else if (kind === 'tag') {
    const d = state.detail?.uuid === item.uuid ? state.detail : null;
    openTagPicker(() => ({ids: [item.uuid]}), 1, [{...item, user_tags: d?.user_tags || item.user_tags}], () => false);
  } else if (kind === 'skip' && r.page === 'unmatched') mutate('/api/change', {ids: [item.uuid], action: 'unknown'}, true);
  else if (kind === 'exclude' || kind === 'include') mutate('/api/curate', {ids: [item.uuid], action: kind}, true);
  else if (kind === 'swap' || kind === 'also') {
    const d = state.detail?.uuid === item.uuid ? state.detail : await api('/api/detail?id=' + encodeURIComponent(item.uuid));
    const from = primaryOf(d);
    if (kind === 'swap' && from) swapOne(d, item.uuid, from.id);
    else openPicker({title: from ? t('这张也是…') : t('这张是…'), sentence: e => from ? t('给这张加上 {name}', {name: e.name}) : t('把这张设为 {name}', {name: e.name}), exclude: d.memberships.map(m => m.id), suggest: {ids: [item.uuid]},
      pick: e => mutate('/api/change', {ids: [item.uuid], action: 'confirmed', entity: e.id}, ['unmatched', 'skipped'].includes(r.page))});
  }
}
const indexOf = uuid => state.items.findIndex(x => x?.uuid === uuid);
async function step(delta) {
  const cur = state.viewer >= 0 ? state.viewerItem?.uuid : state.focused;
  if (kept.hold && kept.hold.uuid === cur && Math.abs(delta) === 1) {
    const target = indexOf(delta > 0 ? kept.hold.next : kept.hold.prev); kept.hold = null;
    if (target >= 0) { state.cursor = target; ensureVisible(target); if (state.viewer >= 0) showViewer(target); else inspect(state.items[target].uuid); return; }
  }
  kept.hold = null;
  const at = cur ? indexOf(cur) : -1;
  const from = at >= 0 ? at : state.viewer >= 0 ? state.viewer : state.cursor;
  const n = Math.max(0, Math.min(state.total - 1, from + delta)); if (n === from || n < 0) return;
  if (!state.items[n]) await loadPage(Math.floor(n / PAGE) * PAGE);
  if (!state.items[n]) return;
  state.cursor = n; ensureVisible(n);
  if (state.viewer >= 0) showViewer(n); else inspect(state.items[n].uuid);
}
function ensureVisible(i) {
  const v = $('viewport'), row = rowOf(i), top = rowTop(row), height = rowHeight(row);
  if (top < v.scrollTop) v.scrollTop = top; else if (top + height > v.scrollTop + v.clientHeight) v.scrollTop = top + height - v.clientHeight;
  syncCards();
}

// ---------- full-screen viewer ----------
const view = {scale: 1, x: 0, y: 0, fit: 1};
function openViewer(i) { if (i < 0 || !state.items[i]) return; state.viewer = i; $('viewer').hidden = false; document.body.classList.add('viewing'); showViewer(i); }
function closeViewer() { if (state.viewer < 0) return; kept.hold = null; state.viewerItem = null; state.cursor = state.viewer; $('viewer').hidden = true; document.body.classList.remove('viewing'); state.viewer = -1; ensureVisible(state.cursor); }
async function showViewer(i, held = null) {
  if (i < 0 && !held) { closeViewer(); return; }
  if (!held && !state.items[i]) await loadPage(Math.floor(i / PAGE) * PAGE);
  const item = held || state.items[i]; if (!item) { closeViewer(); return; }
  const same = state.viewerItem?.uuid === item.uuid && !$('viewer').hidden;
  state.viewer = Math.max(0, i); state.cursor = state.viewer; state.viewerItem = item;
  if (same) { $('viewerCount').textContent = indexOf(item.uuid) >= 0 ? `${number(indexOf(item.uuid) + 1)} / ${number(state.total)}` : t('已移出当前列表 · ← → 继续'); return refreshViewerBar(item, i); }
  const img = $('viewerImg'); img.style.transform = ''; img.hidden = false; $('viewerMissing').hidden = true; Object.assign(view, {scale: 1, x: 0, y: 0});
  img.onerror = () => { if (state.viewerItem?.uuid === item.uuid) { img.hidden = true; $('viewerMissing').hidden = false; } };
  img.src = `/image?id=${encodeURIComponent(item.uuid)}&size=preview`;
  const full = new Image(); full.onload = () => { if (state.viewerItem?.uuid === item.uuid) { img.src = full.src; $('viewerSize').textContent = t('本地副本 {w} × {h}', {w: full.naturalWidth, h: full.naturalHeight}); } }; full.src = `/image?id=${encodeURIComponent(item.uuid)}&size=full`;
  $('viewerSize').textContent = '';
  $('viewerCount').textContent = `${number(i + 1)} / ${number(state.total)}`;
  for (const d of [-2, -1, 1, 2]) { const x = state.items[i + d]; if (x) new Image().src = `/image?id=${encodeURIComponent(x.uuid)}&size=preview`; }
  return refreshViewerBar(item, i);
}
async function refreshViewerBar(item, i) {
  let d; try { d = await api('/api/detail?id=' + encodeURIComponent(item.uuid)); } catch (e) { if (state.viewerItem?.uuid === item.uuid) readFailure($('viewerBar'), e); return; }
  if (!d || state.viewerItem?.uuid !== item.uuid) return;
  state.detail = d; $('viewerBar').innerHTML = (shouldAsk(d, item.uuid) ? askHTML(d.question, true) : '') + identityChips(d, true);
  bindIdentity($('viewerBar'), d, item.uuid); bindAsk($('viewerBar'), d, item.uuid);
}
function applyView() { $('viewerImg').style.transform = `translate(${view.x}px,${view.y}px) scale(${view.scale})`; $('viewerStage').classList.toggle('zoomed', view.scale > 1.01); }
function zoomView(factor, cx, cy) {
  const stage = $('viewerStage').getBoundingClientRect(), px = cx - stage.left - stage.width / 2, py = cy - stage.top - stage.height / 2;
  const img = $('viewerImg'), limit = Math.max(2, (img.naturalWidth / img.clientWidth) * 2 || 2);
  const next = Math.max(1, Math.min(limit, view.scale * factor)), k = next / view.scale;
  view.x = px - (px - view.x) * k; view.y = py - (py - view.y) * k; view.scale = next;
  if (next === 1) { view.x = 0; view.y = 0; } applyView();
}
$('viewerStage').addEventListener('wheel', e => {
  e.preventDefault();
  if (e.ctrlKey || e.metaKey) zoomView(Math.exp(-e.deltaY / 200), e.clientX, e.clientY);
  else if (view.scale > 1) { view.x -= e.deltaX; view.y -= e.deltaY; applyView(); }
}, {passive: false});
let viewGesture = 1;
$('viewerStage').addEventListener('gesturestart', e => { e.preventDefault(); viewGesture = 1; });
$('viewerStage').addEventListener('gesturechange', e => { e.preventDefault(); zoomView(e.scale / viewGesture, e.clientX, e.clientY); viewGesture = e.scale; });
$('viewerStage').addEventListener('dblclick', e => {
  const img = $('viewerImg'), actual = img.naturalWidth / img.clientWidth;
  if (view.scale > 1.01) { Object.assign(view, {scale: 1, x: 0, y: 0}); applyView(); } else zoomView(Math.max(2, actual), e.clientX, e.clientY);
});
let pan = null;
$('viewerStage').addEventListener('pointerdown', e => { if (view.scale > 1) { pan = {x: e.clientX - view.x, y: e.clientY - view.y}; $('viewerStage').setPointerCapture(e.pointerId); } });
$('viewerStage').addEventListener('pointermove', e => { if (pan) { view.x = e.clientX - pan.x; view.y = e.clientY - pan.y; applyView(); } });
$('viewerStage').addEventListener('pointerup', () => { pan = null; });
$('viewerStage').addEventListener('click', e => { if (e.target === $('viewerStage') && view.scale <= 1) closeViewer(); });
$('viewerClose').onclick = closeViewer; $('viewerPrev').onclick = () => step(-1); $('viewerNext').onclick = () => step(1);

// ---------- 首页: newest images grouped by the day they were added ----------
const dayLabel = day => { const [y, m, d] = day.split('-').map(Number); const date = new Date(y, m - 1, d), now = new Date();
  const diff = Math.round((new Date(now.getFullYear(), now.getMonth(), now.getDate()) - date) / 864e5);
  return diff === 0 ? t('今天') : diff === 1 ? t('昨天') : dateText(y, m, d, y !== now.getFullYear()); };
const weekday = day => { const [y, m, d] = day.split('-').map(Number); return new Intl.DateTimeFormat(I18N.locale, {weekday: 'short'}).format(new Date(y, m - 1, d)); };
const home = {days: [], next: '', gen: 0, loading: false, ask: null, askSkip: [], askOff: new Set()};
let homeObserver = null, homeResize = null, homeWidth = 0;
async function renderHome(keep = false) {
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = true; $('homePage').hidden = false;
  $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = t('点图片看详情 · 点角色名只看 TA');
  home.loading = false; homeObserver?.disconnect(); dayLoading.clear();
  const gen = ++home.gen, scroll = keep ? $('viewport').scrollTop : 0;
  if (!keep) { home.days = []; home.next = ''; $('viewport').scrollTop = 0; $('homePage').innerHTML = `<div class="loading-line">${t('读取中…')}</div>`; }
  try {
    const r = await api('/api/home?' + new URLSearchParams({nsfw: state.nsfw, days: Math.min(20, Math.max(5, home.days.length))}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    home.days = r.days; home.next = r.next;
    if (!keep) { home.askSkip = []; home.askOff = new Set(); home.ask = await nextAsk(); if (gen !== home.gen || state.route.page !== 'home') return; }
    drawHome();
    if (keep) $('viewport').scrollTop = scroll;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') $('homePage').innerHTML = `<p class="empty">${esc(readError(e))}</p>`; }
}
async function moreHome() {
  if (home.loading || !home.next || state.route.page !== 'home') return;
  home.loading = true; const gen = home.gen; let appended = false;
  $('homeSentinel').textContent = t('正在读取更早的图…');
  try {
    const r = await api('/api/home?' + new URLSearchParams({nsfw: state.nsfw, before: home.next}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    const known = new Set(home.days.map(g => g.day)), days = r.days.filter(g => !known.has(g.day) && known.add(g.day));
    home.days.push(...days); home.next = r.next;
    const sentinel = $('homeSentinel');
    sentinel.insertAdjacentHTML('beforebegin', days.map(g => homeDay(g, homeWidth)).join(''));
    for (const day of days) for (const el of $('homePage').querySelectorAll(`.home-day[data-day="${day.day}"] .home-strip`)) syncStrip(el);
    state.items.push(...days.flatMap(g => g.items)); state.total = state.items.length;
    if (!home.next) { homeObserver?.disconnect(); sentinel.textContent = t('已经到底了'); }
    appended = true;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') {
    $('homeSentinel').innerHTML = `<button class="home-retry" data-home-retry>${t('读取失败，重试')}</button>`; toast(e.message);
  } }
  finally {
    if (gen === home.gen) {
      home.loading = false;
      // Re-observe after every append so a sentinel still within the margin loads again.
      if (appended && home.next && state.route.page === 'home' && $('homeSentinel')) { homeObserver?.unobserve($('homeSentinel')); homeObserver?.observe($('homeSentinel')); }
    }
  }
}
const ratio = x => x.width && x.height ? Math.min(2.4, Math.max(.4, x.width / x.height)) : .75;
function homeTile(x, style, big = false) {
  const names = x.memberships.map(m => m.name), ids = x.memberships.map(m => m.id).join(' ');
  const badge = viewOpts.marks && x.dup ? `<span class="home-badge" title="${t('共 {n} 份副本', {n: x.dup.size})}">×${x.dup.size}</span>` : '';
  return `<button class="home-j${state.focused === x.uuid ? ' on' : ''}" data-id="${esc(x.uuid)}" data-chars="${esc(ids)}" style="${style}"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=${big ? 'medium' : 'thumb'}" alt="" loading="lazy" decoding="async" draggable="false">${badge}${big ? '' : `<span class="home-ov">${names.length ? names.map(esc).join(' · ') : x.unknown ? t('已跳过') : t('未识别')}</span>`}</button>`;
}
function homeRows(items, width, target) {
  const G = 6, rows = []; let row = [], sum = 0;
  for (const x of items) { row.push(x); sum += ratio(x); if (sum * target + G * (row.length - 1) >= width) { rows.push(row); row = []; sum = 0; } }
  if (row.length) {
    const fill = row.reduce((a, x) => a + ratio(x), 0) * target / width;
    if (fill < .7 && rows.length) { const both = rows.pop().concat(row), tot = both.reduce((a, x) => a + ratio(x), 0); let acc = 0, cut = 0;
      for (; cut < both.length - 1; cut++) { acc += ratio(both[cut]); if (acc >= tot / 2) break; }
      rows.push(both.slice(0, cut + 1), both.slice(cut + 1)); }
    else rows.push(row);
  }
  return rows.filter(r => r.length).map(r => { const s = r.reduce((a, x) => a + ratio(x), 0), fit = (width - G * (r.length - 1)) / s, h = r.length > 2 ? fit : Math.min(target * 1.8, fit);
    return `<div class="home-row" style="height:${Math.round(h)}px">${r.map(x => homeTile(x, `flex:${ratio(x)} 1 0`)).join('')}</div>`; }).join('');
}
function heroTile(hero, w, h) {
  const top = hero.memberships[0];
  return `<button class="home-j home-hero${state.focused === hero.uuid ? ' on' : ''}" data-id="${esc(hero.uuid)}" data-chars="${esc(hero.memberships.map(m => m.id).join(' '))}" style="width:${w}px;height:${h}px"><img src="/image?id=${encodeURIComponent(hero.uuid)}&size=medium" alt="" loading="lazy" decoding="async" draggable="false">${top ? `<span class="home-cap"><b>${esc(top.name)}</b>${top.tag && top.tag !== top.name ? `<small>${esc(top.tag.replaceAll('_', ' '))}</small>` : ''}</span>` : ''}</button>`;
}
// A day that does not fit the width scrolls sideways: the hero, then two rows filled shortest-first.
function stripRows(items, rowH) {
  const rows = [[], []], w = [0, 0];
  for (const x of items) { const i = w[0] <= w[1] ? 0 : 1; rows[i].push(x); w[i] += ratio(x) * rowH + 6; }
  return rows.map(r => `<div class="home-row strip-row" style="height:${rowH}px">${r.map(x => homeTile(x, `flex:none;width:${Math.round(ratio(x) * rowH)}px`)).join('')}</div>`).join('');
}
function homeSizes() {
  const i = viewOpts.homeSize in [0, 1, 2] ? viewOpts.homeSize : 1;
  return {target: [150, 190, 240][i], heroW: [200, 240, 280][i]};
}
function homeDay(g, width) {
  const {target, heroW} = homeSizes();
  const hero = viewOpts.hero && g.items.length >= 4 && width > heroW * 2.4 ? g.items.find(x => x.uuid === g.hero) || g.items[0] : null;
  const rest = hero ? g.items.filter(x => x !== hero) : g.items;
  const rowH = hero ? (Math.round(heroW / .72) - 6) / 2 : target;
  // Natural width of the two-row strip; when it overflows (or more pictures are waiting) the day scrolls sideways.
  const natural = (hero ? heroW + 6 : 0) + rest.reduce((a, x) => a + ratio(x) * rowH + 6, 0) / 2;
  const strip = g.count > g.items.length || natural > width * 1.5;
  let body;
  if (strip) {
    const heroH = Math.round(rowH * 2 + 6);
    body = `<div class="home-strip-wrap"><button class="strip-nav prev" data-strip="-1" aria-label="${t('向左')}" hidden>${INSP_ICONS.prev}</button>
      <div class="home-strip" data-day="${g.day}">${hero ? heroTile(hero, heroW, heroH) : ''}<div class="strip-rows">${stripRows(rest, Math.round(rowH))}</div>
        ${g.count > g.items.length ? `<a class="strip-more" href="${hashFor({page: 'library', day: g.day})}" style="height:${heroH}px">${t('{n} 张没显示', {n: `<b>${number(g.count - g.items.length)}</b>`})}<small>${t('继续往右滑自动加载 · 或在图库里看全部')}</small></a>` : ''}</div>
      <button class="strip-nav next" data-strip="1" aria-label="${t('向右')}">${INSP_ICONS.next}</button></div>`;
  } else if (hero) {
    const heroH = Math.round(heroW / .72), sideW = width - heroW - 6, side = [[], []];
    let r = 0, sum = 0, used = 0;
    for (const x of rest) { if (r > 1) break; side[r].push(x); sum += ratio(x); used++; if (sum * rowH + 6 * (side[r].length - 1) >= sideW) { r++; sum = 0; } }
    body = `<div class="home-top">${heroTile(hero, heroW, heroH)}<div class="home-side">${side.filter(x => x.length).map(rw => `<div class="home-row">${rw.map(x => homeTile(x, `flex:${ratio(x)} 1 0`)).join('')}</div>`).join('')}</div></div>${homeRows(rest.slice(used), width, target)}`;
  } else body = homeRows(g.items, width, target);
  const more = g.people - g.characters.length;
  const chips = g.characters.map(c => { const cover = state.charMap.get(c.id)?.cover;
    return `<a class="home-cc" href="${charHash(c.id)}" data-char="${esc(c.id)}" title="${t('在图库里只看 TA')}">${cover ? `<img src="/image?id=${encodeURIComponent(cover)}&size=thumb" alt="" loading="lazy">` : '<i></i>'}${esc(c.name)}<small>${number(c.count)}</small></a>`; }).join('');
  return `<section class="home-day" data-day="${g.day}"><div class="home-dh"><b>${esc(dayLabel(g.day))}</b><span>${weekday(g.day)} · ${t('{n} 张', {n: number(g.count)})}${viewOpts.people && g.people ? ' · ' + t('{n} 位角色', {n: number(g.people)}) : ''}</span><a class="home-all" href="${hashFor({page: 'library', day: g.day})}">${t('在图库里看')} ›</a></div>
    ${chips ? `<div class="home-chars">${chips}${more > 0 ? `<span class="home-more">${t('另外 {n} 位', {n: number(more)})}</span>` : ''}</div>` : ''}${body}</section>`;
}
// Sideways scrolling: arrows show only when there is more that way; nearing the end loads the next pictures of that day.
function syncStrip(el) {
  const wrap = el.parentElement, max = el.scrollWidth - el.clientWidth - 2;
  wrap.querySelector('.prev').hidden = el.scrollLeft <= 2; wrap.querySelector('.next').hidden = el.scrollLeft >= max;
  if (el.scrollLeft + el.clientWidth > el.scrollWidth - 600) moreOfDay(el.dataset.day);
}
const dayLoading = new Set();
async function moreOfDay(day) {
  const g = home.days.find(x => x.day === day); if (!g || g.items.length >= g.count || dayLoading.has(day)) return;
  dayLoading.add(day); const gen = home.gen;
  try {
    const r = await api('/api/assets?' + new URLSearchParams({day, nsfw: state.nsfw, order: 'newest', offset: g.items.length, limit: 30, count: 0}));
    if (gen !== home.gen || state.route.page !== 'home') return;
    const known = new Set(g.items.map(x => x.uuid)); g.items.push(...r.items.filter(x => !known.has(x.uuid)));
    if (!r.items.length) g.count = g.items.length;
    const el = $('homePage').querySelector(`.home-strip[data-day="${day}"]`), left = el?.scrollLeft || 0;
    const sec = el?.closest('.home-day'); if (!sec) return;
    const tmp = document.createElement('div'); tmp.innerHTML = homeDay(g, homeWidth); sec.replaceWith(tmp.firstElementChild);
    const fresh = $('homePage').querySelector(`.home-strip[data-day="${day}"]`); if (fresh) { fresh.scrollLeft = left; syncStrip(fresh); }
    state.items = home.days.flatMap(x => x.items); state.total = state.items.length;
  } catch (e) { if (gen === home.gen && state.route.page === 'home') toast(e.message); } finally { if (gen === home.gen) dayLoading.delete(day); }
}
function drawHome() {
  if (state.route.page !== 'home') return;
  const page = $('homePage'); homeWidth = page.clientWidth;
  if (!home.days.length) { page.innerHTML = `<p class="empty">${t('没有符合当前 R18 设置的图片。')}</p>`; state.items = []; state.total = 0; return; }
  page.innerHTML = home.days.map((g, i) => homeDay(g, homeWidth) + (i === 0 ? askCardHTML() : '')).join('') + `<div class="home-more-line" id="homeSentinel">${home.next ? t('正在读取更早的图…') : t('已经到底了')}</div>`;
  state.items = home.days.flatMap(g => g.items); state.total = state.items.length;
  for (const el of page.querySelectorAll('.home-strip')) syncStrip(el);
  homeObserver?.disconnect();
  if (home.next) { homeObserver = new IntersectionObserver(es => { if (es.some(e => e.isIntersecting)) moreHome(); }, {root: $('viewport'), rootMargin: '600px'}); homeObserver.observe($('homeSentinel')); }
}
function markHomeFocus() { if (state.route.page === 'home') for (const b of $('homePage').querySelectorAll('.home-j')) b.classList.toggle('on', b.dataset.id === state.focused); }
// 首页「认一认」: one character, a few of its machine-recognised images; tap the wrong ones, then confirm the rest.
async function nextAsk() {
  if (!viewOpts.ask) return null;
  try { return (await api('/api/ask?' + new URLSearchParams({nsfw: state.nsfw, skip: home.askSkip.join(',')}))).batch; } catch { return null; }
}
function askCardHTML() {
  const b = home.ask; if (!b || !viewOpts.ask || !b.items.length) return '';
  const off = home.askOff.size, why = b.kind === 'hint' ? t('和你确认过的 TA 很像 · 点对了就能认出更多') : b.kind === 'seed' ? t('再确认 {n} 张就能学会认 TA', {n: Math.max(1, 5 - b.confirmed)}) : t('抽查一下认得准不准');
  return `<section class="ask-card" id="askCard"><div class="ask-head"><b>${t('认一认')}</b><span>${t('这些是 {name} 吗？', {name: `<a href="${charHash(b.entity)}">${esc(b.name)}</a>`})}</span><small>${why}</small></div>
    <div class="ask-row">${b.items.map(x => `<button class="ask-img${home.askOff.has(x.uuid) ? ' off' : ''}" data-askimg="${esc(x.uuid)}" title="${home.askOff.has(x.uuid) ? t('点一下：改回「是」') : t('点一下：不是 TA')}" style="aspect-ratio:${ratio(x)}"><img src="/image?id=${encodeURIComponent(x.uuid)}&size=thumb" alt="" loading="lazy"><i aria-hidden="true">✕</i></button>`).join('')}</div>
    <div class="ask-foot"><span>${off ? t('{n} 张不是 TA', {n: off}) : t('点掉不是 TA 的')}</span><button class="btn" data-askcard="later">${t('以后再说')}</button><button class="btn pri" data-askcard="ok">${off === b.items.length ? t('都不是') : off ? t('其余 {n} 张是', {n: b.items.length - off}) : t('都对')}</button></div></section>`;
}
function redrawAskCard() { const el = $('askCard'), html = askCardHTML(); if (!el) return drawHome(); if (!html) { el.remove(); return; } const t = document.createElement('div'); t.innerHTML = html; el.replaceWith(t.firstElementChild); }
async function submitAskCard(how) {
  const b = home.ask; if (!b || edits.busy) return;
  const ids = b.items.map(x => x.uuid), no = ids.filter(u => home.askOff.has(u)), yes = ids.filter(u => !home.askOff.has(u));
  home.askSkip.push(b.entity); home.askOff = new Set();
  if (how === 'later') {
    home.ask = null; redrawAskCard();
    post('/api/ask/answer', {entity: b.entity, kind: b.kind, later: ids}).catch(e => toast(e.message));
    return;
  }
  home.ask = await nextAsk();   // the next character is ready before the page redraws
  await mutate('/api/ask/answer', {entity: b.entity, kind: b.kind, yes, no}, false);
}
$('homePage').addEventListener('click', e => {
  if (e.target.closest('[data-home-retry]')) { moreHome(); return; }
  const ai = e.target.closest('[data-askimg]');
  if (ai) { const u = ai.dataset.askimg; home.askOff.has(u) ? home.askOff.delete(u) : home.askOff.add(u); redrawAskCard(); return; }
  const ac = e.target.closest('[data-askcard]'); if (ac) { submitAskCard(ac.dataset.askcard); return; }
  const n = e.target.closest('[data-strip]');
  if (n) { const el = n.parentElement.querySelector('.home-strip'); el.scrollBy({left: Number(n.dataset.strip) * el.clientWidth * .8, behavior: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth'}); return; }
  const b = e.target.closest('.home-j'); if (b) inspect(b.dataset.id);
});
$('homePage').addEventListener('scroll', e => { if (e.target.classList?.contains('home-strip')) syncStrip(e.target); }, true);
$('homePage').addEventListener('dblclick', e => { const b = e.target.closest('.home-j'); if (b) openViewer(indexOf(b.dataset.id)); });
$('homePage').addEventListener('mouseover', e => {
  const c = e.target.closest('.home-cc'), day = c?.closest('.home-day');
  for (const d of $('homePage').querySelectorAll('.home-day.focus')) if (d !== day) d.classList.remove('focus');
  if (!c) return;
  day.classList.add('focus'); for (const b of day.querySelectorAll('.home-j')) b.classList.toggle('hit', b.dataset.chars.split(' ').includes(c.dataset.char));
});
$('homePage').addEventListener('mouseleave', () => { for (const d of $('homePage').querySelectorAll('.home-day.focus')) d.classList.remove('focus'); });
new ResizeObserver(() => { if (state.route.page !== 'home' || !home.days.length || Math.abs($('homePage').clientWidth - homeWidth) < 8) return; clearTimeout(homeResize); homeResize = setTimeout(drawHome, 120); }).observe($('homePage'));

// ---------- look-alike groups of unmatched images ----------
// 'look': close centres; 'trait': farther, but the same distinctive WD traits (weaker hint).
const nearText = g => g.near_kind === 'trait' ? t('外观相似：第 {n} 组', {n: g.near}) : t('可能和第 {n} 组是同一人', {n: g.near});
// 未识别, group by group: CCIP puts images of one person together reliably (99%) even when it cannot name them (45%).
// A group whose leads mostly agree is asked「是 TA 吗？」as a whole; the others get 设为… / 原创角色 / 跳过.
// Tap a picture to leave it out of the group's answer. Answered groups leave the page in place (no reload jump).
const groups = {list: [], shown: 40, off: new Map(), revision: 0};
async function renderGroups(keep = false) {
  if (state.route.page !== 'groups') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = t('点一下图片：不算在这组里 · 组头的按钮对整组生效，都可以撤销');
  const scroll = $('viewport').scrollTop, revision = ++groups.revision, nsfw = state.nsfw;
  let result; try { result = await api('/api/clusters?nsfw=' + state.nsfw); } catch (e) { if (state.route.page === 'groups' && revision === groups.revision && nsfw === state.nsfw) readFailure($('groupsPage'), e); return; }
  const {clusters, learner} = result;
  if (state.route.page !== 'groups' || revision !== groups.revision || nsfw !== state.nsfw) return;
  // Groups that can be confirmed in one click come first (biggest first), then the ones still to be named.
  groups.list = [...clusters].sort((a, b) => !!b.lead - !!a.lead || b.size - a.size || a.cid - b.cid); groups.learner = learner;
  if (!keep) { groups.shown = 40; groups.off = new Map(); }
  drawGroups();
  if (keep) $('viewport').scrollTop = scroll;
}
function drawGroups() {
  const list = groups.list, busy = ['learning', 'grouping', 'waiting'].includes(groups.learner?.status), images = list.reduce((s, g) => s + g.size, 0);
  const rest = Math.max(0, (state.pools.unmatched || 0) - images), named = list.filter(g => g.lead).length;
  if (!$('groupsPage').querySelector('.gsecs')) $('groupsPage').innerHTML = '<div class="groups-controls"></div><div class="gsecs"></div><div class="groups-more"></div>';
  $('groupsPage').querySelector('.groups-controls').innerHTML = `<div class="groups-bar"><span class="muted-note">${t('{n} 组 · 共 {images} 张', {n: number(list.length), images: number(images)})}${named ? ' · ' + t('{n} 组能直接确认是谁', {n: number(named)}) : ''}${list[0] ? ' · ' + t('分组于 {time}', {time: esc(stamp(list[0].built_at))}) : ''}${busy ? ' · ' + esc(groups.learner.status === 'waiting' ? groups.learner.message || '' : t('正在学习…')) : ''}</span><div class="spacer"></div>${rest ? `<a class="quiet link" href="#/unmatched">${t('没成组的 {n} 张', {n: number(rest)})} →</a>` : ''}<button class="quiet" id="regroup" title="${t('用当前的未识别图重新分组')}">${t('重新分组')}</button></div>`;
  reconcileSections($('groupsPage').querySelector('.gsecs'), list.slice(0, groups.shown).map(g => [String(g.cid), groupSection(g)]), t('没有成组的未识别图。可以点「重新分组」。'));
  $('groupsPage').querySelector('.groups-more').innerHTML = `${list.length > groups.shown ? `<div class="gsec-more"><button class="quiet" id="moreGroups">${t('再显示 {n} 组', {n: number(Math.min(40, list.length - groups.shown))})}</button></div>` : ''}`;
  $('regroup').onclick = async () => { try { await post('/api/regroup', {}); toast(t('正在重新分组…')); setTimeout(() => renderGroups(), 3000); } catch (e) { toast(e.message); } };
  $('moreGroups')?.addEventListener('click', () => { groups.shown += 40; drawGroups(); });
}
function groupSection(g) {
  const off = groups.off.get(g.cid) || new Set(), n = g.members.length - off.size, max = 16;
  const head = g.lead
    ? `<span class="gsec-q">${t('是 {name} 吗？', {name: `<a href="${charHash(g.lead.id)}">${esc(g.lead.name)}</a>`})}<small>${t('{n} 张的线索都指向 TA', {n: number(g.lead.count)})}</small></span>
       <button class="btn pri" data-gact="yes"${n ? '' : ' disabled'}>${off.size ? t('其余 {n} 张是', {n: number(n)}) : t('都是')} ✓</button><button class="btn" data-gact="no">${t('都不是')} ✗</button><button class="quiet" data-gact="assign">${t('是别人…')}</button>`
    : `<span class="gsec-q muted">${t('还不知道是谁')}</span><button class="btn pri" data-gact="assign"${n ? '' : ' disabled'}>${t('设为…')}</button><button class="quiet" data-gact="oc" title="${t('画师自己的原创角色')}">${t('原创角色')}</button><button class="quiet" data-gact="skip">${t('跳过')}</button>`;
  const thumbs = g.members.slice(0, max).map(u => `<button class="gsec-img${off.has(u) ? ' off' : ''}" data-gimg="${esc(u)}" title="${off.has(u) ? t('点一下：算回这组') : t('点一下：不算在这组里')}"><img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt=""><i aria-hidden="true">✕</i></button>`).join('');
  return `<section class="gsec" data-cid="${g.cid}"><div class="gsec-head"><b>${t('第 {id} 组', {id: g.cid})}</b><span class="muted-note">${t('{n} 张', {n: number(g.size)})}${g.traits?.length ? ' · ' + g.traits.slice(0, 2).map(esc).join(' · ') : ''}</span>${head}<a class="quiet link gsec-open" href="#/g/${g.cid}">${t('逐张看')} →</a></div>
    <div class="gsec-imgs">${thumbs}${g.members.length > max ? `<a class="gsec-rest" href="#/g/${g.cid}">+${number(g.members.length - max)}</a>` : ''}</div>${g.near ? `<div class="group-near">${nearText(g)}</div>` : ''}</section>`;
}
$('groupsPage').addEventListener('click', async e => {
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
  else if (act === 'oc') { await markOc({ids}, false); renderGroups(true); }
  else if (act === 'assign') openPicker({title: t('这 {n} 张是…', {n: number(n)}), sentence: x => t('把这 {n} 张设为 {name}', {n: number(n), name: x.name}), suggest: {ids}, exclude: g.lead ? [g.lead.id] : [],
    pick: async x => done(await mutate('/api/change', {ids, action: 'confirmed', entity: x.id}, false))});
});

// ---------- look-alike images across the library (web/similar.py) ----------
let similarTimer = 0;
async function renderSimilar() {
  if (state.route.page !== 'similar') return;
  clearTimeout(similarTimer);
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = t('点一组进去看');
  const kind = state.route.kind || '';
  let result; try { result = await api('/api/similar?' + new URLSearchParams({kind, nsfw: state.nsfw})); } catch (e) { if (state.route.page === 'similar' && kind === (state.route.kind || '')) readFailure($('groupsPage'), e); return; }
  const {groups, state: s} = result;
  if (state.route.page !== 'similar') return;
  const busy = SIMILAR_ACTIVE.includes(s.status);
  $('groupsPage').innerHTML = `<div class="similar-status"><span class="local-dot${busy ? ' busy' : ''}"></span><span class="muted-note">${t('{n} 组 · 共 {images} 张', {n: number(groups.length), images: number(groups.reduce((n, g) => n + g.size, 0))})}${similarLine(s) ? ' · ' + esc(similarLine(s)) : ''}</span></div>
    <div class="groups-bar"><div class="segmented">${RELATION_KINDS.map(([k, label]) => `<button class="${k === kind ? 'on' : ''}" data-kind="${k}">${label}</button>`).join('')}</div>
      <div class="spacer"></div><button class="quiet" id="similarScan" ${busy || s.status === 'off' ? 'disabled' : ''} title="${t('给新加入的图算签名，比较还没比较过的候选')}">${t('重新检查')}</button></div>
    ${busy && s.total ? `<div class="progress"><span style="width:${Math.round((s.done || 0) / s.total * 100)}%"></span></div>` : ''}
    <div class="group-grid">${groups.map(g => `<a class="group-card" href="#/s/${g.gid}"><div class="group-covers">${g.covers.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="">`).join('')}</div>
      <div class="group-meta"><b>${t('第 {id} 组', {id: g.gid})}</b><span>${t('{n} 张', {n: number(g.size)})}</span></div>
      <div class="group-traits">${Object.entries(g.kinds).sort((a, b) => b[1] - a[1]).map(([k, n]) => `<span>${RELATION_NAMES[k] || k}${n > 1 ? ' ×' + n : ''}</span>`).join('')}</div></a>`).join('')
      || `<p class="empty">${busy ? t('还在检查，找到的组会陆续出现。') : t('没有找到相似的图。')}</p>`}</div>`;
  for (const b of $('groupsPage').querySelectorAll('[data-kind]')) b.onclick = () => go({page: 'similar', kind: b.dataset.kind});
  $('similarScan').onclick = async () => { try { await post('/api/similar', {}); setTimeout(renderSimilar, 800); } catch (e) { toast(e.message); } };
  if (busy) similarTimer = setTimeout(renderSimilar, 3000);
}

// ---------- identity review: pairs of look-alike characters (web/identity.py, Store.identity_queue) ----------
// One click decides a pair; every decision is an undoable relation, raw records never move.
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
  else if (h?.kind === 'form_of') parts.push(t('{source}：「{subject}」是「{object}」的一种形态', {source: esc(h.source || t('外部资料')), subject: esc(name(h.subject)), object: esc(name(h.object))}));
  if (x.distance !== null && x.distance !== undefined) parts.push(t('参考图外观距离 {d}', {d: x.distance.toFixed(3)}) + (x.distance <= 0.01 ? t('（几乎分不开）') : ''));
  return parts.join(' · ') || t('参考图外观很接近');
}
function identitySide(s, label) {
  return `<div class="pair-side"><div class="pair-name"><span class="pair-label">${label}</span><a href="${charHash(s.id)}" title="${esc(s.tag || s.name)}">${esc(s.name)}</a><small>${t('{n} 张', {n: number(s.count)})}</small></div>
    <div class="pair-thumbs">${s.samples.map(u => `<img src="/image?id=${encodeURIComponent(u)}&size=thumb" loading="lazy" alt="">`).join('') || `<span class="muted-note">${t('没有可显示的图')}</span>`}</div></div>`;
}
function identityCard(x, i) {
  const s = identitySuggestion(x), btn = (act, text, title) => `<button class="${s === act ? 'primary' : 'quiet'}" data-act="${act}" data-i="${i}" title="${esc(title)}">${text}</button>`;
  return `<article class="pair-card" data-i="${i}">
    <div class="pair-body">${identitySide(x.a, 'A')}${identitySide(x.b, 'B')}</div>
    <p class="pair-evidence">${identityEvidence(x)}</p>
    <div class="pair-actions">
      ${btn('same', t('同一角色'), t('其实是同一个人：合并显示，图和人工结论都不搬动，可撤销'))}
      ${btn('b_of_a', t('B 是 A 的一种形态'), t('B 挂到 A 下面作为子标签（服装、异格、另一形态）'))}
      ${btn('a_of_b', t('A 是 B 的一种形态'), t('A 挂到 B 下面作为子标签'))}
      ${btn('different', t('不同角色'), t('只是长得像、经常同框，或是 TA 的吉祥物、宠物：记下来，以后不再问'))}
      <span class="spacer"></span>${btn('skip', t('暂不判断'), t('先放一边，队列里不再出现（可撤销）'))}
    </div></article>`;
}
async function renderIdentity(more = false) {
  if (state.route.page !== 'identity') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('hint').textContent = t('点角色名看全部图 · 高亮的是外部资料建议的做法');
  if (!more) { identity.items = []; identity.offset = 0; }
  let page;
  try { page = await api('/api/identity/queue?' + new URLSearchParams({limit: IDENTITY_PAGE, offset: identity.items.length, nsfw: state.nsfw})); }
  catch (e) { $('groupsPage').innerHTML = `<p class="empty">${esc(readError(e))}</p>`; return; }
  if (state.route.page !== 'identity') return;
  identity.items.push(...page.items); identity.total = page.total; state.identityCount = page.total; navCount('countIdentity', page.total);
  $('resultCount').textContent = t('{n} 对待判断', {n: number(page.total)});
  renderHeading();
  $('groupsPage').innerHTML = `<div class="pair-list">${identity.items.map(identityCard).join('') || `<p class="empty">${t('没有需要判断的角色了。')}</p>`}</div>
    ${identity.items.length < identity.total ? `<div class="pair-more"><button class="quiet" id="identityMore">${t('再显示一些')}</button></div>` : ''}`;
  $('identityMore')?.addEventListener('click', () => renderIdentity(true));
  for (const b of $('groupsPage').querySelectorAll('[data-act]')) b.onclick = () => decideIdentity(Number(b.dataset.i), b.dataset.act);
}
async function decideIdentity(i, act) {
  const x = identity.items[i]; if (!x || identity.busy.has(i) || edits.busy) return;
  const card = $('groupsPage').querySelector(`.pair-card[data-i="${i}"]`);
  identity.busy.add(i); card?.classList.add('working');
  // A duplicate is shown under the side with more images.
  const [small, big] = x.a.count <= x.b.count ? [x.a, x.b] : [x.b, x.a];
  const body = act === 'skip' ? null : {status: 'accepted', reason: '身份整理',
    ...{same: {a: small.id, b: big.id, kind: 'duplicate_of'}, b_of_a: {a: x.b.id, b: x.a.id, kind: 'form_of'},
        a_of_b: {a: x.a.id, b: x.b.id, kind: 'form_of'}, different: {a: x.a.id, b: x.b.id, kind: 'different_from'}}[act]};
  try {
    const r = act === 'skip' ? await post('/api/identity/skip', {a: x.a.id, b: x.b.id}) : await post('/api/identity/decision', body);
    setLatest(r.undo); toast(act === 'skip' ? t('已暂不判断') : {same: t('已把 {a} 并入 {b}', {a: small.name, b: big.name}), b_of_a: t('{a} 已作为 {b} 的一种形态', {a: x.b.name, b: x.a.name}),
      a_of_b: t('{a} 已作为 {b} 的一种形态', {a: x.a.name, b: x.b.name}), different: t('已记下 {a} 和 {b} 是不同角色', {a: x.a.name, b: x.b.name})}[act], true);
    card?.classList.add('leaving');
    await new Promise(done => setTimeout(done, 170));
    await renderIdentity();
    refreshMeta().catch(e => error(readError(e)));
  } catch (e) { toast(e.message); card?.classList.remove('working'); }
  finally { identity.busy.delete(i); }
}

// ---------- misc wiring ----------
wireGrid();
wireShell();
wireSettings();
wireCatalog();
wireTasks({confirm: confirmAction, indexAdd: () => openIndexAdd(),
  tick: open => open && !state.libraries ? [loadLibraries()] : [], ticked: () => updatesTick()});
$('undoButton').onclick = undo; $('helpButton').onclick = () => $('help').showModal();
$('sidebarToggle').onclick = () => { if (innerWidth <= 760) $('sidebar').classList.toggle('mobile-open'); else $('sidebar').hidden = !$('sidebar').hidden; requestAnimationFrame(() => renderGrid()); };
for (const dialog of document.querySelectorAll('dialog')) dialog.addEventListener('click', e => { if (e.target === dialog) { const r = dialog.getBoundingClientRect(); if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) dialog.close(); } });

// ---------- 画师抽查: acceptance of 找回画师 (docs/PROVENANCE.md section 8) ----------
// A fixed random sample (the server orders by a hash of the id), answered with the same 是 TA / 不是 as the detail
// panel, so every answer is in the undo history and fixes the library at the same time.
const AUDIT_METHODS = {pixel_hash: [t('像素哈希'), t('和 Danbooru 上的帖子是同一个文件，画师取自帖子的画师标签')],
  gallery_match: [t('本地比对'), t('和 TA 公开作品的小图逐张比对一致')], ocr: [t('签名识别'), t('图上的签名或水印')]};
const auditMethod = a => a.methods?.includes('pixel_hash') ? 'pixel_hash' : a.methods?.[0] || '';
async function renderArtistAudit() {
  if (state.route.page !== 'artist-audit') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('settingsPage').hidden = true; $('groupsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = '';
  $('hint').textContent = t('点原帖或画师主页对照 · 看不出来就先留着 · 都可以撤销');
  const n = Number(prefs.get('artistAuditN', 50)) || 50, scroll = $('viewport').scrollTop;
  let d;
  try { d = await api('/api/artist-audit?n=' + n); } catch (e) { $('groupsPage').innerHTML = `<p class="muted-note">${esc(readError(e))}</p>`; return; }
  if (state.route.page !== 'artist-audit') return;
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
    return `<div class="aa-card${state_}"><a class="aa-img" href="/image?id=${encodeURIComponent(it.uuid)}&size=full" target="_blank" title="${t('新标签页打开大图')}"><img src="/image?id=${encodeURIComponent(it.uuid)}&size=medium" alt="" loading="lazy"><span class="aa-no">${i + 1}</span></a>
      <div class="aa-body">${it.artists.map(a => artistRow(it, a)).join('')}${it.posts?.length ? `<div class="aa-links">${t('原帖')} ${it.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</div></div>`;
  };
  $('groupsPage').innerHTML = `<div class="aa-summary">
      <div class="prov-stat"><b>${acc === null ? '—' : (acc * 100).toFixed(1) + '%'}</b><span>${t('准确率')}<small>${t('目标 ≥ 95%')}</small></span></div>
      <div class="prov-stat"><b>${number(tl.done)}<em> / ${number(tl.all)}</em></b><span>${t('已判断')}<small>${t('对 {yes} · 不对 {no}', {yes: number(tl.yes), no: number(tl.no)})}</small></span></div>
      ${byMethod.map(([m, x]) => `<div class="prov-stat"><b>${x.done ? (x.yes / x.done * 100).toFixed(0) + '%' : '—'}</b><span>${esc(AUDIT_METHODS[m][0])}<small>${t('{done} / {all} 已判', {done: number(x.done), all: number(x.all)})}</small></span></div>`).join('')}
    </div>
    <p class="muted-note">${t('从 {pool} 张有画师的图里按固定的随机顺序取前 {n} 张，每次打开都是同一批。判断会写进图库（「不对」的画师这张图就不再显示），和详情面板里的「是 TA / 不是」一样可以撤销。', {pool: number(d.pool), n: number(d.items.length)})}</p>
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

// ---------- artists (provenance/, read-only here): shown only when found, never as an empty field ----------
const artistNames = {};
// ---------- 按需查找画师 (docs/PROVENANCE.md section 9): the click authorises this one lookup ----------
const LOOKUP_NOTE = t('查找画师会把所选图片的像素哈希和 IQDB 签名（一串文本，不是图片）发给 Danbooru，按结果显示画师和原帖。\n「找 TA 的其他图」会下载这位画师公开作品的小缩略图，只在本机比对，比完不留图。\n\n以后点这两个按钮不再提示。继续吗？');
let lookupTimer = null;
async function lookupAllowed() {
  try { if (localStorage.getItem('artistLookupOk')) return true; } catch {}
  if (!await confirmAction(LOOKUP_NOTE, t('查找画师'))) return false;
  try { localStorage.setItem('artistLookupOk', '1'); } catch {}
  return true;
}
async function artistLookup(body, label, after) {
  const token = session.token;
  if (!await lookupAllowed() || token !== session.token) return;
  try { await api('/api/provenance', {method: 'POST', body: JSON.stringify(body), headers: {'X-CSRF-Token': token}}); } catch (err) { toast(err.message); return; }
  toast(label);
  clearTimeout(lookupTimer);
  const poll = async () => {
    let p;
    try { p = await api('/api/provenance'); } catch { lookupTimer = setTimeout(poll, 2000); return; }
    const s = p.state || {};
    if (['running', 'stopping'].includes(s.status)) { lookupTimer = setTimeout(poll, 1500); return; }
    const found = s.found || 0;
    toast(['done', 'stopped'].includes(s.status) ? (found ? t('找到 {n} 张图的画师', {n: number(found)}) : t('没有找到新的画师'))
      : t('查找没有完成：{reason}', {reason: tb(s.reason) || s.status}));
    after?.(found);
  };
  lookupTimer = setTimeout(poll, 1500);
}
const findArtists = (ids, after) => artistLookup({action: 'lookup', ids}, ids.length > 1 ? t('正在查找 {n} 张图的画师…', {n: number(ids.length)}) : t('正在查找画师…'), after);
const findArtistWorks = (artist, name, after) => artistLookup({action: 'artist', artist}, t('正在找 {name} 的其他图…', {name: name || t('这位画师')}), after);
function artistName(id) {
  if (!(id in artistNames)) {
    artistNames[id] = '…';
    api('/api/artist?' + new URLSearchParams({id})).then(r => { artistNames[id] = r.name || t('未知画师'); if (state.route.artist === id) renderHeading(); }).catch(e => error(readError(e)));
  }
  return artistNames[id];
}
function artistSection(d, ident) {
  const find = `<div class="artist-empty"><span class="muted-note">${d.maybe?.length ? t('还没确定画师') : t('还不知道画师')}</span><button class="quiet" data-artist-find title="${t('用像素哈希和 IQDB 查这张图的画师，几秒钟（只发哈希文本，不发图片）')}">${t('查找画师')}</button></div>`;
  if (!d.artists?.length && !d.posts?.length && !d.maybe?.length) return `<section class="inspector-section"><h3 class="section-title">${t('画师')}</h3>${find}</section>`;
  const link = (url, label, title = '') => `<a class="link" href="${esc(url)}" target="_blank" rel="noreferrer noopener"${title ? ` title="${esc(title)}"` : ''}>${esc(label)}</a>`;
  const judge = a => a.confirmed ? `<span class="artist-judge"><span class="artist-ok" title="${t('你确认过')}">✓ ${t('已确认')}</span><button data-artist-verdict="" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('撤回确认')}">${t('撤回')}</button></span>`
    : `<span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('确认是 TA 画的')}">${t('是 TA')}</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('不是 TA 画的：这张不再显示这位画师')}">${t('不是')}</button></span>`;
  const artist = a => `<div class="artist-row"><div class="artist-top"><a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="${t('在图库里只看 TA 的图')}">${esc(a.name)}</a>${judge(a)}</div>
      ${a.count > 1 ? `<a class="artist-more" href="${hashFor({page: 'library', artist: a.id})}">${t('库里还有 {n} 张', {n: number(a.count - 1)})}</a>` : ''}
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, t('在 {site} 打开 TA 的主页', {site: h.site}))).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('下载 TA 公开作品的小缩略图，在本机比对出库里 TA 的其他图（一两分钟）')}">${t('找 TA 的其他图')}</button></div>`;
  // 「也许是 TA」: a group recommendation or a signature read on the picture (native/terms/src/http_artists.rs); 是 TA settles it.
  const maybe = a => `<div class="artist-row artist-maybe"><div class="artist-top"><span class="artist-maybe-name">${t('也许是 {name}', {name: `<a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="${t('在图库里只看 TA 的图')}">${esc(a.name)}</a>`})}</span>
      <span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('确认是 TA 画的')}">${t('是 TA')}</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('不是 TA 画的：不再推荐')}">${t('不是')}</button></span></div>
      <div class="artist-reason">${esc(tb(a.reason || ''))}</div>
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, t('在 {site} 打开 TA 的主页', {site: h.site}))).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('下载 TA 公开作品的小缩略图，在本机比对：是同一张图就能确定')}">${t('找 TA 的其他图')}</button></div>`;
  return `<section class="inspector-section"><h3 class="section-title">${t('画师')}</h3>${(d.artists || []).map(artist).join('')}${(d.maybe || []).map(maybe).join('')}${!d.artists?.length ? find : ''}
    ${d.posts?.length ? `<div class="artist-posts"><span>${t('原帖')}</span>${d.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</section>`;
}
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
async function loadGuide(works = false) {
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
async function guideOnOpen() {
  try { await loadGuide(); } catch (e) { return; }
  if (state.guide?.guide.status === 'new') await guideAction({action: 'step', step: 'welcome'});
}
const splashGone = () => new Promise(done => {
  const started = Date.now(), wait = () => (!document.getElementById('splash') || Date.now() - started > 8000) ? done() : setTimeout(wait, 150);
  wait();
});
async function guideAfterBoot() {
  if (!guideOn()) return;
  await splashGone();
  if (guideActive()) showTour(Math.max(0, TOUR.indexOf(state.guide.guide.step)));
  drawGuide();
}
// Something about the library changed (a sync step, a mutation): read the guide again, at most every few seconds.
async function guideRefresh(force = false) {
  if (!guideOn() || (!force && Date.now() - guideLoadedAt < 4000)) return;
  try { await loadGuide(); } catch (e) { return; }
  drawGuide();
}
function drawGuide() {
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
function drawGuideNotice() {
  const el = $('guideNotice'), g = state.guide; if (!el) return;
  const c = g?.capability, check = c?.check || {};
  if (guideOn() && c?.recognition && check.status === 'idle' && !guideChecked) { guideChecked = true; guideAction({action: 'check'}).then(() => setTimeout(() => guideRefresh(true), 1500)); }
  if (guideOn() && check.status === 'checking') setTimeout(() => guideRefresh(true), 1500);
  const problem = !guideOn() ? '' : !c.recognition ? t('这个安装没有识别组件：图片会加入图库、可以手动整理，但不会自动认出角色。安装 Illustash 完整版后就能自动识别。')
    : check.status === 'failed' ? t('识别组件没能启动（{reason}）。图片仍会加入图库；可以重新检查，或重新安装完整版。', {reason: esc(tb(check.message || ''))}) : '';
  const trouble = guideOn() && !problem ? syncTrouble() : '';
  el.hidden = !problem && !trouble;
  el.innerHTML = problem ? `<span>${problem}</span>${c.recognition ? `<button class="quiet small" data-guide-check>${t('重新检查')}</button>` : ''}`
    : trouble ? `<span>${trouble}</span><button class="quiet small" data-guide-sync>${t('再同步一次')}</button>` : '';
}
$('guideNotice').addEventListener('click', async e => {
  if (e.target.closest('[data-guide-check]')) { await guideAction({action: 'check'}); drawGuide(); setTimeout(() => guideRefresh(true), 1500); }
  if (e.target.closest('[data-guide-sync]')) { try { await startSync(); pollSync(); } catch (err) { toast(err.message); } drawGuide(); }
});

// ----- the tour: one real element lit up at a time -----
function tourStop(id) {
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
$('tour').addEventListener('click', async e => {
  const act = e.target.closest('[data-tour]')?.dataset.tour; if (!act) return;
  if (act === 'skip' || act === 'done') { closeTour(); await guideAction({action: act === 'done' ? 'done' : 'skip'}); drawGuide(); return; }
  tourAt = Math.max(0, Math.min(TOUR.length - 1, tourAt + (act === 'next' ? 1 : -1)));
  placeTour(true); guideAction({action: 'step', step: TOUR[tourAt]});
});
addEventListener('resize', () => { if (tourAt >= 0) requestAnimationFrame(() => placeTour(false)); });
document.addEventListener('keydown', e => { if (tourAt >= 0 && e.key === 'Escape') { e.stopImmediatePropagation(); closeTour(); guideAction({action: 'skip'}); } }, true);

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
function drawNudge() {
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

// ----- the checklist: ticks itself as things get done -----
function guideTick(item) {
  const g = state.guide; if (!guideOn() || g.guide.ticked.includes(item)) return;
  g.guide.ticked = [...g.guide.ticked, item]; drawChecklist(item);
  guideAction({action: 'tick', item}).then(() => drawChecklist());
}
// Called after every successful change: which checklist item it shows was learned.
function guideAfterPost(path, body) {
  if (!guideOn()) return;
  if (path === '/api/undo') guideTick('undo');
  else if (path === '/api/change' && body?.action === 'confirmed' && ['groups', 'group'].includes(state.route.page)) guideTick('group');
  else if (path === '/api/change' || path === '/api/reassign') guideTick('correct');
  else if (path === '/api/index' && body?.action === 'watch' && body.works?.length) guideTick('follow');
  if (Date.now() - guideLoadedAt > 4000) guideRefresh();
}
function drawChecklist(just = '') {
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
$('checklist').addEventListener('click', async e => {
  const act = e.target.closest('[data-cl]')?.dataset.cl;
  if (act === 'toggle') { checklistOpen = !checklistOpen; drawChecklist(); }
  else if (act === 'hide') { await guideAction({action: 'hide'}); drawChecklist(); }
  else if (e.target.closest('a[href]')) { checklistOpen = false; drawChecklist(); }
});
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
async function replayGuide() {
  if (!await guideAction({action: 'reopen'})) return;
  if (state.route.page === 'settings') go({page: 'home'});
  await new Promise(done => setTimeout(done, 300));
  showTour(0);
}

// What the ui/ modules still reach in this file (#199); filled here, once every function below exists, before the page
// starts.
Object.assign(app, {go, charRoute, charHash, renderHeading, closeInspector, refreshMeta, setNsfw, replayGuide, drawGuide, drawNudge, artistName, clearSelection, dayLabel, editProfile, filters, guideTick, markOc, mergeCharacter, nearText, openLocalLinks, openRelate, reload, renderArtistAudit, renderGroups, renderHome, renderIdentity, renderSimilar, selectAll, short, splitOc, sweep, toggleFit, toggleMascot, toggleOc, toggleSelecting, answerAsk, closeViewer, drawHome, ensureVisible, findArtists, indexOf, inspect, jumpToCharacter, newIds, openPicker, openViewer, pickAlternative, ratio, showViewer, singleAction, step, targets, zoomView});
(async () => {
  try {
    const bootNsfw = state.nsfw;
    const boot = await api('/api/bootstrap?' + new URLSearchParams({nsfw: bootNsfw})); adoptToken(boot.token); setLatest(boot.undo);
    if (boot.read_only) toast(boot.read_only);   // the library could not be switched to the terms core: read-only
    navCount('countLibrary', boot.total);
    window.splash?.status(t('正在整理 {n} 张图…', {n: number(boot.total)}));
    const meta = refreshMeta({boot: bootNsfw === state.nsfw ? boot : null});
    if (parseHash().char || parseHash().page === 'characters') await meta;
    await guideOnOpen();
    await applyRoute();
    await meta;
    window.splash?.ready();
    taskTick();
    // The library opened although it could not be listed among the recent ones (#176): say so once.
    api('/api/libraries').then(x => {
      if (x?.unlisted) error(t('没能把这个图库记进最近图库：{why}。图库照常可用，只是下次启动时可能不会自动打开它。', {why: x.unlisted}));
      // Once per upgrade in this browser: the backup outlives the session that made it (#193).
      if (x?.upgraded && prefs.get('upgradeNoticed', '') !== x.upgraded.backup) {
        prefs.set('upgradeNoticed', x.upgraded.backup);
        confirmAction(t('这个图库已升级到新格式，旧版本的 Illustash 打不开它了。升级前的备份在：\n{backup}\n\n要回到旧版本，先装回旧版，再用这份备份恢复。', {backup: x.upgraded.backup}), t('图库已升级'), t('知道了'), false);
      }
    }).catch(() => {});
    guideAfterBoot();
  } catch (e) { window.splash?.ready(); error(t('启动失败：{reason}。请检查启动终端，然后刷新页面。', {reason: e.message})); }
})();

