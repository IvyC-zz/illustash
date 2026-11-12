// 设置各页（桌面布局，#199）：每页的卡片、它们的表单和按钮。它们读取和更改的内容在 core/settings.js（任务自己的卡片在
// core/jobs.js）。
import {t, tb, said, I18N} from '../core/i18n.js';
import {esc, isMac, number, bytes, stamp, localStamp, pct, localTime} from '../core/format.js';
import {prefs} from '../core/prefs.js';
import {diagnosticReport, diagnosticText, diagnosticLine, downloadDiagnostic} from '../core/diagnostics.js';
import {session, shared, readError} from '../core/session.js';
import {state} from '../core/state.js';
import {REASONS, FOLD_OPTIONS} from '../core/labels.js';
import {SYNC_ACTIVE, INDEX_ACTIVE, NAMES_ACTIVE, PROV_ACTIVE, syncLine, indexLine, namesLine, importLine, importActive, pollSync, pollIndex,
  pollNamesSoon, loadProvenance, importStarted, pollImports, cancelImport, indexAction} from '../core/jobs.js';
import {onSettings, allowNames, loadSettingsData, saveStartup, saveFold, backupNow, clearCache, saveLanguage, loadAbout, saveModelApi, testModelApi,
  loadLibraries, libraryRequest, pollExport, chooseFolders, openLibraryRequest, libraryOpened, loadAccounts, accountRequest,
  waitPixivLogin, stopPixivLogin, checkUpdates, updatesPost, updatesTick} from '../core/settings.js';
import {$, toast, error, readFailure, fold, wireFolds} from './dom.js';
import {confirmAction} from './dialog.js';
import {openTasks} from './tasks.js';
import {charHash} from '../core/route.js';
import {refreshMeta} from '../core/meta.js';
import {closeInspector} from './detail.js';
import {setNsfw} from './grid.js';
import {drawGuide, drawNudge, replayGuide} from './guide.js';
import {renderIntegrityLine} from './integrity.js';
import {renderModelsCard} from './models.js';

// ---------- 名称：这个图库联网查询名称的授权 ----------
// 名称的文字查询（只有 tag 和作品名）需要这个图库授权：授权之前，后端以「显示名联网尚未授权」拒绝。其他失败都是真正的
// 网络或服务错误。
const NAMES_NOTE = t('允许这个图库联网查询名称吗？\n\n会把你输入的角色名、作品名，以及图库里角色的 Danbooru tag 和作品名发给 Danbooru 和 Bangumi，用来搜索角色、作品和补全中文名。不会发送图片。\n\n允许后对这个图库一直有效，可以在「设置 › 网络与隐私」里随时撤销。');
export const namesAllowButton = `<button type="button" class="link-button" data-names-allow>${t('允许查询…')}</button>`;
export async function setNamesNetwork(allow) {
  if (allow && !await confirmAction(NAMES_NOTE, t('允许联网查询名称'))) return false;
  try {
    await allowNames(allow);
  } catch (err) { toast(err.message); return false; }
  toast(allow ? t('已允许这个图库联网查询名称') : t('已撤销名称联网查询'));
  return true;
}

// ---------- 外部索引的新角色（native/terms/src/app_index.rs） ----------
let indexArmed = null;
export async function renderIndexCard(fresh = true) {
  const route = state.route, token = session.token;
  if (!$('indexCard') || state.route.page !== 'settings') return;
  if (fresh || !state.index) { try { const value = await shared('/api/index'); if (route !== state.route || token !== session.token) return; state.index = value; } catch (e) { if (route === state.route && token === session.token) readFailure($('indexCard'), e); return; } }
  const card = $('indexCard'), x = state.index; if (!x || !card) return;
  const s = x.state, active = INDEX_ACTIVE.includes(s.status), watched = x.watch.works.length;
  // 关注哪些作品在「资料库 · 作品」里逐个作品决定；这里只管检查怎么运行。
  if (!setSettingsHTML(card, cardHead(t('检查新角色'), `${t('定期在 Danbooru 上查关注的作品有没有本地还不认识的角色，只发送作品名。')}<span class="hint-more" title="${t('添加角色时每个角色下载约 20 张公开参考图（约 {mb} MB），在本机算好特征后重新扫描图库。', {mb: x.mb_per_character})}">${t('了解详情')}</span>`, 'library') + `
    ${taskLink(indexLine(s, x.check), active ? 'busy' : s.status === 'error' ? 'warn' : s.status === 'off' ? 'off' : '')}
    <a class="setting-line set-link" href="#/characters/works"><span>${t('关注的作品')}<small class="line-hint">${t('在资料库的「作品」里逐部开关')}</small></span><span>${watched ? t('{n} 部', {n: number(watched)}) : t('还没有')} ›</span></a>
    <div class="setting-line"><span>${t('只提示图片较多的角色')}<small class="line-hint">${t('Danbooru 上图片少于这个数的角色不提示')}</small></span><span class="num-field"><input id="watchMin" type="number" min="20" max="5000" step="10" value="${x.watch.min_posts}"> ${t('张')}</span></div>
    <p class="set-foot">${t('每次检查前确认联网，只允许本次运行。')}</p>
    ${x.history.length ? fold('indexHistory', t('最近添加的角色（{n} 个）', {n: number(x.history.reduce((n, h) => n + h.characters.length, 0))}), `<table class="settings-table">${x.history.flatMap(h => h.characters.map(c => `<tr><td>${esc(stamp(h.at))}</td>
      <td>${c.entity && c.status === 'added' ? `<a class="link" href="${charHash(c.entity.id)}">${esc(c.entity.name)}</a>` : esc(c.tag || c.short_tag)}</td>
      <td>${c.status === 'added' ? t('匹配 {n} 张', {n: number(c.entity?.count ?? c.claims)}) : c.status === 'dropped' ? t('已移除') : t('未添加：{reason}', {reason: esc(tb(c.reason || ''))})}</td>
      <td>${c.status === 'added' && c.entity ? `<button class="quiet small${indexArmed === c.short_tag ? ' armed' : ''}" data-drop="${esc(c.short_tag)}" ${active ? 'disabled' : ''} title="${t('从索引中移除这个角色：重新扫描后它匹配的图恢复原状，以后检查不再提示')}">${indexArmed === c.short_tag ? t('再点一次确认移除') : t('移除')}</button>` : ''}</td></tr>`)).join('')}</table>`) : ''}`, x.works)) return;
  wireFolds(card);
  const act = async body => { try { await indexAction(body); renderIndexCard(false); pollIndex(); } catch (e) { toast(e.message); } };
  $('watchMin').onchange = () => act({action: 'watch', works: x.watch.works, min_posts: Number($('watchMin').value)});
  for (const b of card.querySelectorAll('[data-drop]')) b.onclick = () => {
    if (indexArmed !== b.dataset.drop) { indexArmed = b.dataset.drop; renderIndexCard(false); setTimeout(() => { if (indexArmed === b.dataset.drop) { indexArmed = null; renderIndexCard(false); } }, 4000); return; }
    indexArmed = null; act({action: 'drop', char: b.dataset.drop, reason: '在设置中手动移除'});
  };
}

// ---------- 显示名 ----------
// 从 Bangumi 取的中文 / 日文名字（只有文字查询）；配置好的模型 API 是用户选择开启的最后手段。
export async function renderNamesCard(fresh = true) {
  const route = state.route, token = session.token;
  if (!$('namesCard') || state.route.page !== 'settings') return;
  if (fresh || !state.names) { try { const value = await shared('/api/names'); if (route !== state.route || token !== session.token) return; state.names = value; } catch (e) { if (route === state.route && token === session.token) readFailure($('namesCard'), e); return; } }
  // 等待期间页面可能已经重新渲染：总是对现在屏幕上的那张卡片操作。
  const card = $('namesCard'), x = state.names; if (!card) return; if (!x) { card.hidden = true; return; }
  card.hidden = false;
  const s = x.state || {}, active = NAMES_ACTIVE.includes(s.status);
  const row = (label, c) => c ? `<tr><td>${label}</td><td>${number(c.total)}</td><td>${number(c.zh || 0)}</td><td>${number(c.ja || 0)}</td><td>${number(c.readable || 0)}</td></tr>` : '';
  const cc = x.counts?.characters;
  card.innerHTML = cardHead(t('角色名称'), t('从 Bangumi 取角色的中文名，只发送 tag 和作品名。没有中文名时显示日文名，再没有就显示英文名；原始 tag 在悬停提示里。'), 'library') + `
    ${taskLink(namesLine(s) + (s.error ? t('：{reason}', {reason: tb(s.error)}) : ''), active ? 'busy' : s.status === 'error' ? 'warn' : ['off', 'unauthorized'].includes(s.status) ? 'off' : '')}
    ${startupSwitch('names', t('启动时获取'), t('只查新增的角色'))}
    ${x.counts ? fold('namesStats', t('覆盖率') + (active ? t('（完成后更新）') : '') + (cc ? t('：{p}% 的角色有中文名', {p: Math.round(cc.zh / Math.max(1, cc.total) * 100)}) : ''), `<table class="settings-table"><tr><th></th><th>${t('共')}</th><th>${t('中文名')}</th><th>${t('日文名')}</th><th>${t('英文名')}</th></tr>${row(t('角色'), x.counts.characters)}${row(t('作品'), x.counts.works)}${row(t('皮肤'), x.counts.skins)}</table>`) : ''}`;
  wireFolds(card);
  renderModelCard();
  if (fresh && active) pollNamesSoon();
}
// 可选的 OpenAI 兼容模型，只用来翻译 Bangumi 没有的名字。
const modelHost = m => { try { return new URL(m?.base || 'https://api.deepseek.com').host; } catch (e) { return '—'; } };
export function renderModelCard() {
  const card = $('modelCard'), m = state.names?.model_api; if (!card || !m) return;
  if (!setSettingsHTML(card, cardHead(t('名称翻译（可选）'), t('Bangumi 没有中文名时，用兼容 OpenAI 接口的大模型翻译，只发送 tag、日文名和作品名。译名可能不准，悬停提示里会标「机器翻译」。API key 只存在这个图库的数据库里。'), 'library') + `
    <div class="form-grid">
      <label for="modelBase">${t('接口地址')}</label><input id="modelBase" value="${esc(m.base || '')}" placeholder="https://api.deepseek.com" spellcheck="false" autocomplete="off">
      <label for="modelName">${t('模型')}</label><input id="modelName" value="${esc(m.model || '')}" placeholder="deepseek-chat" spellcheck="false" autocomplete="off">
      <label for="modelKey">API key</label><input id="modelKey" type="password" placeholder="${m.configured ? t('已配置（尾号 {hint}），留空则不修改', {hint: esc(m.hint || '')}) : t('未配置')}" spellcheck="false" autocomplete="off">
    </div>
    <div class="sync-now form-actions"><span class="muted-note">${m.configured ? t('已配置') : t('未配置，不会使用')}</span><div class="spacer"></div>
      ${m.configured ? `<button class="quiet small" id="modelClear">${t('清除 key')}</button><button class="quiet small" id="modelTest">${t('测试连接')}</button>` : ''}<button class="primary" id="modelSave">${t('保存')}</button></div>`)) return;
  const save = async (body, done) => { try { await saveModelApi(body); toast(typeof done === 'function' ? done() : done); renderSettings(); } catch (e) { toast(e.message); } };
  $('modelSave').onclick = () => {
    const body = {base: $('modelBase').value.trim() || 'https://api.deepseek.com', model: $('modelName').value.trim() || 'deepseek-chat'};
    if ($('modelKey').value.trim()) body.key = $('modelKey').value.trim();
    // 后端只为输入 key 时的那个地址保留 key：换了新地址而不带 key 会把它清掉。
    save(body, () => m.configured && !state.names?.model_api?.configured ? t('接口地址已更改，原来的 API key 已清除，请重新填写') : t('设置已保存'));
  };
  for (const id of ['modelBase', 'modelName', 'modelKey']) $(id).onkeydown = e => { if (e.key === 'Enter' && !e.isComposing) $('modelSave').click(); };
  if ($('modelClear')) $('modelClear').onclick = () => save({key: null}, t('API key 已清除'));
  if ($('modelTest')) $('modelTest').onclick = async () => {
    $('modelTest').disabled = true;
    try { const r = await testModelApi(); toast(r.ok === false ? t('连接失败：{reason}', {reason: tb(r.error) || t('未知错误')}) : t('连接成功')); } catch (e) { toast(t('连接失败：{reason}', {reason: e.message})); }
    if ($('modelTest')) $('modelTest').disabled = false;
  };
}
const settingsMarkup = new WeakMap();
export function setSettingsHTML(card, html, version = null) {
  const previous = settingsMarkup.get(card);
  if (previous?.html === html && previous.version === version) return false;
  // 后台的元数据更新不能抹掉正在编辑的表单。
  if (card.contains(document.activeElement) && document.activeElement?.matches('input,textarea,select')) return false;
  card.innerHTML = html;
  settingsMarkup.set(card, {html, version});
  return true;
}
function patchSettingsPage(html, section) {
  const page = $('settingsPage');
  // 另一节就是另一页：整个替换。同一节则逐张卡片修补，保留正在填的表单。
  if (page.querySelector('.settings-main')?.dataset.section !== section) {
    page.innerHTML = html;
    for (const card of page.querySelectorAll('.settings-card')) settingsMarkup.set(card, {html: card.innerHTML, version: null});
    $('viewport').scrollTop = 0;
    return;
  }
  const template = document.createElement('template'); template.innerHTML = html;
  for (const card of template.content.querySelectorAll('.settings-card')) {
    // 这些卡片自己负责渲染、表单和异步生命周期。
    if (OWN_CARDS.includes(card.id)) continue;
    setSettingsHTML($(card.id), card.innerHTML);
  }
}
// 用户带来的每个问题一页：看起来怎样、图片从哪来、图里是谁、谁画的、什么会离开本机、数据存在哪。每页列出它显示的
// 卡片（#/settings/<id>）。
const SETTINGS_SECTIONS = [
  ['general', t('通用'), t('语言、外观、图片网格与默认排序'), ['setDisplay', 'setLanguage']],
  ['library', t('图库'), t('当前图库、来源、同步与排除'), ['setLibraries', 'setSync', 'setExclude', 'setFold']],
  ['characters', t('角色资料'), t('新角色检查、角色名称与翻译'), ['indexCard', 'namesCard', 'modelCard']],
  ['artists', t('画师'), t('找回画师与绑定的账号'), ['setProvenance', 'setAccounts']],
  ['network', t('网络与隐私'), t('所有会访问网络的功能，以及它们发送什么'), ['setNetwork']],
  ['data', t('数据'), t('备份、缓存、识别模型与存储位置'), ['setData', 'setModels']],
  ['about', t('关于'), t('版本、致谢与诊断信息'), ['setAbout']]];
const OWN_CARDS = ['setLibraries', 'setProvenance', 'setAccounts', 'indexCard', 'namesCard', 'modelCard', 'setAbout', 'setModels'];
export const settingsSection = () => SETTINGS_SECTIONS.find(s => s[0] === state.route.section) || SETTINGS_SECTIONS[0];
// 只给开发者的入口（测试图库、粘贴 Pixiv cookie）要带 ?dev 打开过一次页面后才显示。
export const devMode = (() => { try { if (new URLSearchParams(location.search).has('dev')) prefs.set('dev', true); } catch {} return !!prefs.get('dev', false); })();
// 设置保存在哪里，让用户知道哪些设置会跟着他们跨图库、跨浏览器。
const SCOPE = {browser: [t('仅本浏览器'), t('保存在这个浏览器里，换浏览器或清除网站数据会恢复默认')], library: [t('本图库'), t('每个图库各自保存')], shared: [t('所有图库共用'), t('保存在这台电脑上，所有图库共用')]};
const scope = key => `<span class="set-scope" title="${SCOPE[key][1]}">${SCOPE[key][0]}</span>`;
export const cardHead = (title, note = '', key = '') => `<h2>${title}${key ? scope(key) : ''}</h2>${note ? `<p class="settings-note">${note}</p>` : ''}`;
// 启动时的开关；本次启动仍以 --no-* 参数为准。
function startupSwitch(key, name, hint) {
  const info = state.settings; if (!info?.startup) return '';
  return `<label class="switch-line"><span><b>${name}</b><small>${hint} · ${t('下次启动时生效')}</small>${(info.forced_off || []).includes(key) ? `<em>${t('这次启动已用命令行参数关闭')}</em>` : ''}</span><input type="checkbox" class="switch" data-startup="${key}" ${info.startup[key] ? 'checked' : ''}></label>`;
}
// 控件在任务面板里的状态行。
const taskLink = (line, dot = '') => `<div class="setting-line set-status"><span><span class="local-dot${dot ? ' ' + dot : ''}"></span>${esc(line)}</span><button class="quiet small" data-open-tasks>${t('在任务面板查看')}</button></div>`;
const CREDITS = [
  ['WD SwinV2 Tagger v3', 'SmilingWolf', t('图片标签与角色识别'), 'https://huggingface.co/SmilingWolf/wd-swinv2-tagger-v3'],
  ['CCIP', 'DeepGHS', t('角色特征比对'), 'https://huggingface.co/deepghs/ccip'],
  ['Character Index', 'DeepGHS', t('角色索引数据'), 'https://huggingface.co/datasets/deepghs/character_index'],
  [t('imgutils 检测与分级模型'), 'DeepGHS', t('人物检测、打码检测、内容分级'), 'https://github.com/deepghs/imgutils'],
  ['Anime Censorship Tagger', 'Maltox', t('打码类型识别'), 'https://huggingface.co/Maltox/anime-censorship-tagger-mnv3-384'],
  ['Danbooru', '', t('标签与角色资料'), 'https://danbooru.donmai.us'],
  [t('Bangumi 番组计划'), '', t('角色中文名称'), 'https://bgm.tv'],
  ['Pillow · NumPy · ONNX Runtime', '', t('图像处理与模型推理'), 'https://onnxruntime.ai']];
async function renderAbout() {
  const route = state.route, token = session.token;
  if (!state.about) { try { await loadAbout(); if (route !== state.route || token !== session.token) return; } catch (e) { if (route === state.route && token === session.token) readFailure($('setAbout'), e); return; } }
  const card = $('setAbout'), a = state.about; if (!card || !a) return;
  let report;
  try { report = await diagnosticReport(); } catch { report = {available: false, records: []}; }
  if (route !== state.route || token !== session.token) return;
  const version = `${a.version}${a.channel ? ' ' + tb(a.channel) : ''}`;
  const diag = [[t('应用'), a.name], [t('版本'), version], [t('构建'), a.build || '—'], [t('数据库架构'), a.schema], [a.runtime ? t('运行时') : 'Python', a.runtime || a.python], [t('系统'), a.platform],
    ...Object.entries(a.libraries || {}).map(([k, v]) => [tb(k), tb(v) || t('未安装')])];
  card.innerHTML = `<div class="about-head"><img src="/favicon.svg" alt="" width="44" height="44"><div><h2 class="about-name">${esc(a.name)}</h2><p class="muted-note">${t('版本 {v}', {v: esc(version)})}${a.build ? ' · ' + t('构建 {build}', {build: esc(a.build)}) : ''}</p></div></div>
    <p class="settings-note">${t('在本机识别、整理收藏的插画和里面的角色。图片和识别数据都只在这台电脑上。')}</p>
    <div class="setting-line" id="aboutUpdates">${updatesLine()}</div>
    <div class="about-actions"><button class="quiet" id="aboutGuide">${t('新手引导')}</button><button class="quiet" id="aboutKeys">${t('键盘快捷键')}</button><button class="quiet" id="aboutCopy">${t('复制诊断信息')}</button><button class="quiet" id="aboutExport">${t('导出诊断信息')}</button></div>
    ${fold('credits', t('致谢与第三方组件'), `<table class="settings-table credits">${CREDITS.map(([n, by, use, url]) => `<tr><td><a class="link" href="${url}" target="_blank" rel="noreferrer noopener">${esc(n)}</a>${by ? `<small>${esc(by)}</small>` : ''}</td><td>${esc(use)}</td></tr>`).join('')}</table><p class="settings-note net-foot">${t('识别用的 CCIP 模型（随安装包附带，或第一次使用时下载）依据 {license} 许可提供给你，使用和再分发均受该许可第 5 条约束：不得用于其附件 A 列出的用途，再分发时须把这些限制写进分发协议并附上许可。许可原文和各模型的来源见应用资源目录里的 resources/licenses/models/。其余组件的许可条款以其项目主页为准。', {license: '<a class="link" href="https://bigscience.huggingface.co/blog/bigscience-openrail-m" target="_blank" rel="noreferrer noopener">BigScience Open RAIL-M</a>'})}</p>`)}
    ${fold('diagnostics', t('诊断信息'), `<p class="settings-note">${t('运行错误仅保存在本机，最多 128 条、保留 14 天；不会自动上传，不含图片、文件名、路径或账号凭据。')}</p>
      <p class="settings-note">${report.available ? t('最近的错误记录') : t('诊断记录暂不可读')}</p>
      ${(report.records || []).slice(0, 8).map(r => `<p class="muted-note">${esc(new Date(r.at * 1000).toLocaleString())} · ${esc(diagnosticLine(r))}</p>`).join('')}
      <table class="settings-table">${diag.map(([k, v]) => `<tr><td>${esc(k)}</td><td class="mono">${esc(v)}</td></tr>`).join('')}</table>`)}`;
  wireFolds(card);
  $('aboutKeys').onclick = () => $('help').showModal();
  $('aboutGuide').onclick = () => replayGuide();
  $('aboutCopy').onclick = async () => {
    try { await navigator.clipboard.writeText(diagnosticText({...await diagnosticReport(), environment: Object.fromEntries(diag)})); toast(t('诊断信息已复制')); }
    catch (e) { toast(t('无法访问剪贴板，请导出诊断信息')); }
  };
  $('aboutExport').onclick = async () => {
    try { downloadDiagnostic({...await diagnosticReport(), environment: Object.fromEntries(diag)}); } catch (e) { error(readError(e)); }
  };
}
// 每个会联网的功能、它发送什么、是否开启：集中在一处查看（#200）。
function networkCard() {
  const up = state.settings?.startup || {}, p = state.provenance, acct = state.accounts, model = state.names?.model_api;
  const net = state.names?.network, names = !net ? '' : net.granted_at ? t('已允许（{time} 起）', {time: esc(net.granted_at)}) : net.this_run ? t('本次启动已允许（命令行）') : t('未允许，不联网');
  const row = (section, name, to, what, now) => `<tr><td><a class="link-button strong" href="#/settings/${section}">${name}</a></td><td>${to}</td><td>${what}</td><td>${now}</td></tr>`;
  return cardHead(t('会访问网络的功能'), t('图片本身和由图片算出的特征，任何时候都不会发出去。下面是全部会联网的功能和它们发送的内容。')) +
    `<table class="settings-table net-table"><tr><th>${t('功能')}</th><th>${t('服务')}</th><th>${t('发送的内容')}</th><th>${t('现在')}</th></tr>
      ${row('characters', t('检查新角色'), 'Danbooru', t('关注作品的名称；添加角色时下载约 20 张公开参考图'), t('每次运行前确认'))}
      ${row('characters', t('角色名称'), t('Bangumi、Danbooru'), t('tag、作品名、日文名'), net && !net.authorized ? names : up.names ? t('启动时自动（只查新角色）') : t('手动'))}
      ${row('characters', t('名称翻译'), model?.configured ? esc(modelHost(model)) : t('你配置的模型接口'), t('tag、日文名、作品名'), model?.configured ? t('已配置') : t('未配置，不使用'))}
      ${row('artists', t('找回画师'), t('Danbooru（含 IQDB）'), t('像素哈希和 IQDB 签名（一串文本，不是图片）、画师名和账号'), p?.auto_network ? t('同步后自动，每次有流量上限') : t('每次手动授权'))}
      ${row('artists', t('读取 Pixiv 关注'), 'Pixiv', t('用你在 app 里登录的状态读取关注列表（只读）'), acct?.pixiv?.bound ? t('已登录') : t('未登录，不使用'))}
      ${row('artists', t('查找画师、找 TA 的其他图'), t('Danbooru、画师主页'), t('所选图片的哈希文本；下载画师公开作品的小图，只在本机比对'), t('只在你点按钮时'))}
      ${row('characters', t('在 Danbooru 上搜角色和作品'), 'Danbooru', t('你输入的文字；自建角色的名字（核对能否对上 Danbooru）；「设为…」里 Danbooru 候选的 tag（取一张全年龄小图）'), net && !net.authorized ? names : t('只在你搜索或打开资料库时'))}
      ${row('artists', t('以图搜图'), 'SauceNAO', '—', acct?.saucenao?.bound ? t('已保存 key，还没有功能使用它') : t('未使用'))}
      ${state.models?.on_demand ? row('data', t('下载识别模型'), esc(state.models.sources.join(t('、'))), t('一次普通的 HTTPS 下载，取回模型文件；不发送图库里的任何信息'), t('只在你点「下载」时')) : ''}
      ${row('about', t('检查更新'), 'GitHub', t('一次普通的 HTTPS 请求，读取公开的版本列表；不发送图库里的任何信息'), state.updates?.auto ? t('启动时和之后每天自动') : t('只在你点「检查更新」时'))}
    </table>
    ${state.updates ? `<label class="switch-line"><span><b>${t('自动检查更新')}</b><small>${t('启动时和之后每天查一次 GitHub 上有没有新版本，有新版本时提示你，不会自己下载或安装')} · ${SCOPE.shared[0]}</small></span><input type="checkbox" class="switch" data-updates-auto ${state.updates.auto ? 'checked' : ''}></label>` : ''}
    ${net ? `<div class="setting-line"><span><b>${t('名称联网查询')}</b><small>${t('角色名称和 Danbooru 搜索共用这一项授权，只发送文字')} · ${names}</small></span>
      ${net.granted_at ? `<button class="quiet small" data-names-network="0">${t('撤销')}</button>` : `<button class="quiet small" data-names-network="1">${t('允许…')}</button>`}</div>` : ''}
    <p class="set-foot">${t('同步、图片识别、相似图片检测、学习与分组都只在本机进行。X 的关注通过导入官方数据归档获得，不联网。')}</p>`;
}
export async function renderSettings(fetchMissing = true, refresh = false) {
  if (state.route.page !== 'settings') return;
  $('grid').hidden = true; $('empty').hidden = true; $('charIndex').hidden = true; $('groupsPage').hidden = true; $('settingsPage').hidden = false;
  closeInspector(); $('selectionBar').hidden = true; $('resultCount').textContent = ''; $('hint').textContent = '';
  if (fetchMissing) loadSettingsData(refresh);
  const [section, , , cards] = settingsSection();
  if (fetchMissing && ['network', 'about'].includes(section)) updatesTick();
  const runs = state.syncHistory, info = state.settings;
  const reasons = state.exclInfo.reasons || {};
  const sourcesOf = info?.package?.sources || state.libraries?.info?.sources || [];
  const photos = sourcesOf.some(x => x.kind === 'photos');
  const segment = (name, options, current) => `<div class="segmented" data-setting="${name}">${options.map(([v, label]) => `<button class="${current === v ? 'on' : ''}" data-value="${v}">${label}</button>`).join('')}</div>`;
  // 按 App 分的计数只有照片来源才有（X 或 Pixiv 保存的图片）；文件夹图库没有。
  const apps = Object.entries(runs.find(r => r.sources)?.sources || {}).sort((a, b) => b[1].photos - a[1].photos);
  const appRow = ([, v]) => `<tr><td title="${esc(v.name)}">${esc(tb(v.name))}</td><td>${number(v.photos)}</td><td>${v.collected ? number(v.collected) : '—'}</td></tr>`;
  const sort = prefs.get('sortLibrary', 'newest'), csort = prefs.get('sortCharacter', 'suspicious');
  const s = state.sync;
  const body = {
    setDisplay: () => cardHead(t('显示'), '', 'browser') + `
      <div class="setting-line"><span>${t('外观')}</span>${segment('theme', [['', t('跟随系统')], ['light', t('浅色')], ['dark', t('深色')]], prefs.get('theme', ''))}</div>
      <div class="setting-line"><span>${t('图片网格')}</span>${segment('fit', [['0', t('方形裁切')], ['1', t('原比例')]], state.fit ? '1' : '0')}</div>
      <div class="setting-line"><span>${t('R18 内容')}</span>${segment('nsfw', [['', t('显示')], ['hide', t('隐藏')], ['only', t('只看 R18')]], state.nsfw)}</div>
      <div class="setting-line"><span>${t('图库默认排序')}</span>${segment('sortLibrary', [['newest', t('最新加入')], ['oldest', t('最早加入')]], sort)}</div>
      <div class="setting-line"><span>${t('按角色筛选时的默认排序')}</span>${segment('sortCharacter', [['suspicious', t('可疑优先')], ['newest', t('最新加入')], ['oldest', t('最早加入')]], csort)}</div>
      <p class="set-foot">${t('缩略图大小、图上显示什么、详情面板的内容，在顶栏的「视图选项」里调。')}</p>`,
    // 全应用共用（preferences.json），所以桌面外壳和每个图库、每个浏览器都共用它（#173）。可选项是 web/static/locales
    // 里的表（#182），每个用它自己的语言命名。
    setLanguage: () => cardHead(t('语言'), '', 'shared') + `
      <div class="setting-line"><span>${t('界面语言')}<small class="line-hint">${t('切换后页面会重新载入')}</small></span>${segment('language', [['system', t('跟随系统')], ...I18N.languages.map(([code, name]) => [code, esc(name)])], I18N.preference)}</div>`,
    setSync: () => cardHead(t('同步'), t('只读读取来源里的图片，新增的导入并在本机识别；来源里删掉的图会从图库移除，人工整理记录保留。同步不联网。'), 'library') + `
      ${taskLink(s.status === 'off' ? t('这次启动没有开同步') : syncLine(s), SYNC_ACTIVE.includes(s.status) ? 'busy' : ['error', 'needs_recognition'].includes(s.status) ? 'warn' : s.status === 'off' ? 'off' : '')}
      ${startupSwitch('sync', t('启动时同步'), t('检查来源里新增和删除的图'))}
      ${photos && apps.length ? fold('sourcesApps', t('iCloud 照片按存图的 App 统计（{n} 个 App）', {n: number(apps.length)}), `<p class="muted-note">${t('iCloud 照片里只导入 X 和 Pixiv 存下的图。')}</p><table class="settings-table"><tr><th>App</th><th>${t('照片里')}</th><th>${t('已导入')}</th></tr>${apps.map(appRow).join('')}</table>`) : ''}`,
    setExclude: () => cardHead(t('排除规则'), photos ? t('照片、截图、文字图这类不是插画的图不算进图库。只是做标记，不改动照片图库；你的判断优先于自动规则。') : t('照片、截图、文字图这类不是插画的图不算进图库。只是做标记，不改动来源文件；你的判断优先于自动规则。'), 'library') + `
      <div class="rule-row">${Object.entries(REASONS).map(([k, label]) => `<a class="rule" href="#/excluded?reason=${k}"><b>${number(reasons[k] || 0)}</b><span>${label}</span></a>`).join('')}<a class="rule" href="#/pending"><b>${number(state.pendingCount)}</b><span>${t('待确认')}</span></a></div>`,
    setFold: () => cardHead(t('重复与相似'), `${t('勾选的关系会合并显示：图库里只留分辨率最高的一张，整组在「重复」页查看。')}<span class="hint-more" title="${t('改动组里任一张会同步到整组。组里角色不一致时不合并，留在「重复」页等你处理。字节完全相同的副本总是合并。')}">${t('了解详情')}</span>`, 'library') + `
      ${startupSwitch('similar', t('启动时检测相似图片'), t('只比较新导入的图'))}
      <div class="fold-row">${FOLD_OPTIONS.map(([k, label, hint]) => `<label class="fold-option" title="${hint}"><input type="checkbox" data-fold="${k}" ${(state.fold || []).includes(k) ? 'checked' : ''} ${!state.fold ? 'disabled' : ''}><span>${label}</span><small>${hint}</small></label>`).join('')}</div>`,
    setNetwork: networkCard,
    setData: () => cardHead(t('备份与存储'), t('备份的是应用数据库里的人工整理记录；数据库升级前会自动备份一次。恢复备份前先退出 app。') + ' ' + t('每次打开图库后会在后台检查识别结果数据库，整理记录数据库每 7 天检查一次；上次没有正常关闭时，打开前先检查两个库。')) + `
      <div class="setting-line"><span>${info ? t('共 {n} 份备份', {n: number(info.backup_count)}) + (info.backups?.[0] ? `<small class="line-hint">${t('最近一次 {time}', {time: esc(localStamp(info.backups[0].time))})}</small>` : '') : '…'}</span><button class="quiet small" id="backupNow" ${!info ? 'disabled' : ''}>${t('立即备份')}</button></div>
      <div class="setting-line" id="integrityLine"></div>
      <div class="setting-line"><span>${t('缩略图缓存')}<small class="line-hint">${info ? t('{n} 个文件，{size}；清除后浏览时会重新生成', {n: number(info.thumbnails.files), size: bytes(info.thumbnails.bytes)}) : ''}</small></span><button class="quiet small" id="clearCache" ${info?.thumbnails.files ? '' : 'disabled'}>${t('清除')}</button></div>
      ${info?.backups?.length ? fold('backupList', t('最近的备份'), `<table class="settings-table">${info.backups.slice(0, 5).map(b => `<tr><td class="mono">${esc(b.name)}</td><td>${bytes(b.bytes)}</td></tr>`).join('')}</table>`) : ''}
      ${info ? fold('dataPaths', t('存储位置'), `<table class="settings-table">${[[t('源数据库（识别结果，可以重建）'), info.source], [t('应用数据库（人工整理，不要删）'), info.library], [t('缩略图缓存（可以放心删）'), info.thumbnails]].map(([label, d]) => `<tr><td>${label}</td><td class="mono">${esc(d.path)}</td><td>${bytes(d.bytes)}</td></tr>`).join('')}</table>`) : ''}`,
  };
  patchSettingsPage(`<nav class="settings-nav" aria-label="${t('设置')}">${SETTINGS_SECTIONS.map(([id, title]) => `<a href="#/settings/${id}" class="${id === section ? 'on' : ''}"${id === section ? ' aria-current="page"' : ''}>${title}</a>`).join('')}</nav>
    <div class="settings-main" data-section="${section}">${cards.map(id => `<section class="settings-card" id="${id}">${body[id] ? body[id]() : ''}</section>`).join('')}</div>`, section);
  const page = $('settingsPage');
  renderAbout();
  renderModelsCard();
  if ($('setLibraries')) { if (!$('setLibraries').innerHTML) loadLibraries(); else renderLibrariesCard(); }
  if ($('setProvenance')) { if ($('setProvenance').innerHTML) renderProvenanceCard(); loadProvenance(); }   // 先立即显示缓存的状态，再取新的
  if ($('setAccounts') || section === 'network') { if (!state.accounts || !$('setAccounts')?.innerHTML) loadAccounts(); else renderAccountsCard(); }
  if (state.index?.works) renderIndexCard(false);
  if (state.names) renderNamesCard(false);
  wireFolds(page);
  for (const box of page.querySelectorAll('[data-fold]')) box.onchange = async () => {
    const kinds = [...page.querySelectorAll('[data-fold]:checked')].map(b => b.dataset.fold);
    try { await saveFold(kinds); await refreshMeta(); toast(t('已更新，「重复」页现在有 {n} 组', {n: number(state.dupInfo.groups)})); }
    catch (e) { toast(e.message); box.checked = !box.checked; }
  };
  const startup = async (body, done) => { try { await saveStartup(body); toast(done); } catch (e) { toast(e.message); } renderSettings(); };
  page.onchange = e => { const box = e.target.closest('[data-startup]'); if (box) startup({[box.dataset.startup]: box.checked}, t('已保存，下次启动时生效')); };
  if ($('backupNow')) $('backupNow').onclick = async () => { try { const r = await backupNow(); toast(t('备份完成：{name}', {name: r.path.split('/').pop()})); renderSettings(); } catch (e) { toast(e.message); } };
  renderIntegrityLine();
  if ($('clearCache')) $('clearCache').onclick = async () => { try { const r = await clearCache(); toast(t('已清除 {n} 个缩略图', {n: number(r.removed)})); renderSettings(); } catch (e) { toast(e.message); } };
  for (const g of page.querySelectorAll('[data-setting]')) for (const b of g.querySelectorAll('button')) b.onclick = () => {
    const key = g.dataset.setting, v = b.dataset.value;
    if (key === 'language') { setLanguage(v); return; }
    if (key === 'fit' && state.fit !== (v === '1')) { state.fit = !state.fit; prefs.set('fit', state.fit); }
    if (key === 'nsfw') setNsfw(v);
    if (key === 'theme') { prefs.set('theme', v); window.applyTheme?.(); }
    if (key === 'sortLibrary' || key === 'sortCharacter') prefs.set(key, v);
    renderSettings();
  };
}
// 整个页面用新语言重画；先保存设置，这样每个窗口和图库都跟着变。
async function setLanguage(language) {
  if (language === I18N.preference) return;
  try { await saveLanguage(language); location.reload(); } catch (e) { toast(e.message); }
}
// ---------- 图库（docs/LIBRARIES.md）：在设置页管理；打开一个图库会让服务在它上面重启 ----------
// 新图库和当前图库的来源按出处分组（#107）：每个文件夹都写明它的图片从哪来；不从文件夹名猜测。文件夹包含它的子文件夹，
// 不能互相嵌套。
const ORIGINS = [['pixiv', 'Pixiv'], ['x', 'X'], ['other', t('其他')]];
const IS_MAC = /Mac/.test(navigator.platform);
// created：已发布但没有登记到最近图库里的图库（#97），提供它而不是再新建一个。
export const libForm = {open: false, name: '', folders: {}, photos: false, created: null};
// 后端列出它知道的出处（#111）；后备值覆盖没有图库信息的旧布局。
const origins = () => { const list = state.libraries?.info?.origins; return list?.length ? list.map(o => [o.id, tb(o.label)]) : ORIGINS; };
export function renderLibrariesCard() {
  const card = $('setLibraries'); if (!card || state.route.page !== 'settings') return;
  const data = state.libraries;
  if (!data) { card.innerHTML = `<h2>${t('图库')}</h2><p class="muted-note">${t('正在读取…')}</p>`; return; }
  const home = p => p.replace(/^\/Users\/[^/]+/, '~'), cur = data.info, list = origins();
  const seg = (attr, value, options) => `<div class="segmented">${options.map(([v, label]) => `<button class="${v === value ? 'on' : ''}" ${attr}="${v}">${label}</button>`).join('')}</div>`;
  const editable = cur && !cur.test, last = cur?.sources.length <= 1;
  const remove = s => editable ? `<button class="quiet small" data-lib-remove="${esc(s.id)}" ${last ? `disabled title="${t('图库至少要保留一个来源')}"` : ''}>${t('移除')}</button>` : '';
  // #293：一个来源列表，每个文件夹的出处在它自己那一行设置，再加一行用来添加更多来源的按钮。
  const originOf = s => list.some(o => o[0] === s.preset) ? s.preset : 'other';
  const folders = !cur ? [] : cur.sources.filter(s => s.kind === 'folder').sort((a, b) => list.findIndex(o => o[0] === originOf(a)) - list.findIndex(o => o[0] === originOf(b)));
  const folderRow = s => `<div class="setting-line lib-folder"><span class="lib-path" title="${esc(s.path)}"><bdi>${esc(home(s.path))}</bdi></span>
      <span class="lib-src">${editable ? seg(`data-lib-preset="${esc(s.id)}" data-preset`, s.preset, list) : `<span class="muted-note">${esc(list.find(o => o[0] === originOf(s))[1])}</span>`}${remove(s)}</span></div>`;
  const photos = cur?.sources.find(s => s.kind === 'photos');
  const sources = !cur ? '' : `<h3 class="set-sub">${t('来源')}</h3>
    ${folders.map(folderRow).join('')}${photos ? `<div class="setting-line lib-folder"><span><b>${t('iCloud 照片')}</b><small>${t('本机「照片」图库，按存图的 App 区分 X 和 Pixiv')}</small></span><span class="lib-src">${remove(photos)}</span></div>` : ''}
    ${folders.length || photos ? '' : `<p class="muted-note set-empty">${t('还没有来源')}</p>`}
    ${editable ? `<div class="set-actions"><span class="set-actions-label">${t('添加文件夹：')}</span>${list.map(([id, label]) => `<button class="quiet small" data-lib-add-source="${id}" title="${t('图片来自 {origin} 的文件夹，子文件夹会一起读取', {origin: esc(label)})}">${esc(label)}</button>`).join('')}${!photos && IS_MAC ? `<button class="quiet small" data-lib-add-photos>${t('添加 iCloud 照片')}</button>` : ''}</div>` : ''}`;
  // 测试图库是由开发者工具（tools/test_libraries.py，#186 已删除）生成的，所以它的标记只在开发模式下显示。
  const dev = devMode && data.libraries.some(l => l.test);
  const others = data.libraries.filter(l => l.path !== data.current);
  const row = l => `<div class="setting-line"><span><b>${esc(l.name)}${dev && l.test ? ` <span class="set-scope">${esc(l.test)}</span>` : ''}</b><small class="lib-path-hint">${esc(home(l.path))}</small></span>
    <span class="lib-src"><button class="quiet small" data-open-library="${esc(l.path)}" data-name="${esc(l.name)}">${t('打开')}</button><button class="quiet small" data-forget-library="${esc(l.path)}" title="${t('只从列表里移除，不删除图库')}">${t('移出列表')}</button></span></div>`;
  const ex = data.export || {};
  const exportLine = ex.status === 'running' ? `<p class="muted-note">${t('正在导出到 {path}…', {path: esc(home(ex.path))})}</p>`
    : ex.status === 'done' ? `<p class="muted-note">${t('已导出：{path}（{mb} MB）。', {path: esc(home(ex.path)), mb: (ex.bytes / 1048576).toFixed(0)})}${EXPORT_LEAVES_OUT}</p>`
    : ex.status === 'error' ? `<p class="muted-note">${t('导出失败：{reason}', {reason: esc(tb(ex.error))})}</p>` : '';
  const f = libForm;
  const form = !f.open ? '' : `<div class="lib-form">
      <p class="muted-note">${t('按图片来自哪里分别添加文件夹，每类可以有多个。子文件夹会一起读取；图片保留在原位置，不复制。选 X 或 Pixiv 的文件夹才能用以图搜图、画师回查。')}</p>
      <div class="lib-columns">${list.map(([id, label]) => `<div class="lib-col"><h3>${esc(label)}</h3>
        ${(f.folders[id] || []).map((p, i) => `<div class="lib-picked"><span class="lib-path" title="${esc(p)}"><bdi>${esc(home(p))}</bdi></span><button class="quiet" data-lib-drop="${id}" data-index="${i}" title="${t('不添加这个文件夹')}">×</button></div>`).join('')}
        <button class="quiet" data-lib-pick="${id}">${t('添加文件夹…')}</button></div>`).join('')}</div>
      ${IS_MAC ? `<label class="lib-photos"><input type="checkbox" id="libPhotos" ${f.photos ? 'checked' : ''}> ${t('同时读取 iCloud 照片（本机「照片」图库，按存图的 App 区分 X 和 Pixiv）')}</label>` : ''}
      <div class="setting-line"><span>${t('名字')}<small class="line-hint">${t('保存在 {path}', {path: esc(home(data.home))})}</small></span><input class="lib-input" id="libName" placeholder="${t('新图库')}" value="${esc(f.name)}"></div>
      ${f.created ? `<div class="lib-created"><p>${t('{done}，保存在 {path}，但没能加入最近图库列表：{reason}', {done: `<b>${t('图库已经建好')}</b>`, path: `<bdi>${esc(home(f.created.path))}</bdi>`, reason: esc(tb(f.created.error))})}</p>
        <p class="muted-note">${t('打开它会再试一次加入列表；再点创建会另建一个带编号的图库。')}</p>
        <p><button class="primary" data-open-library="${esc(f.created.path)}" data-name="${esc(f.created.name)}">${t('打开这个图库')}</button> <button class="quiet" data-lib-again>${t('仍然新建一个')}</button></p></div>`
        : `<p><button class="primary" data-lib-create>${t('创建并打开')}</button> <button class="quiet" data-lib-cancel>${t('取消')}</button></p>`}</div>`;
  setSettingsHTML(card, `${cardHead(t('图库'), t('每个图库各自保存识别结果和人工整理记录，图片本身留在原处、不复制。切换图库时后台会重启，几秒后自动刷新。'))}
    ${cur ? `<div class="setting-line"><span><b>${esc(cur.name)}</b><small class="lib-path-hint">${esc(home(cur.path))}</small></span><span class="tag-on">${t('当前图库')}</span></div>
    ${sources}` : `<p class="muted-note">${t('当前是旧的数据布局（不是图库包）。')}</p>`}
    <h3 class="set-sub">${t('图库管理')}</h3>
    ${others.length ? `<p class="set-sub-note">${t('这台电脑上的其他图库')}</p>${others.map(row).join('')}` : ''}
    <div class="set-actions"><button class="quiet small" data-lib-new>${t('新建图库')}</button><button class="quiet small" data-lib-choose="library">${t('打开其他图库…')}</button><button class="quiet small" data-lib-choose="zip" ${importActive() ? 'disabled' : ''}>${t('导入…')}</button>${cur ? `<button class="quiet small" data-lib-export ${ex.status === 'running' ? 'disabled' : ''}>${t('导出当前图库')}</button>` : ''}</div>
    ${form}${exportLine}${importBlock(state.importTask)}
    <p class="set-foot">${t('导出的 zip 包含识别结果、人工整理记录和来源配置，不含模型连接设置或联网授权，可以换电脑或发给别人。导入后需重新设置模型接口，并自行确认名称查询的联网授权；旧版导出包也会清除这些设置。个人备份请保留完整图库目录。')}${ex.status === 'done' ? '' : EXPORT_LEAVES_OUT}</p>`);
}
// #88：导入作为后台任务运行（core/jobs.js）。卡片显示它的状态行、运行期间的取消按钮，以及它是怎样结束的。
function importBlock(job) {
  const line = importLine(job); if (!line) return '';
  const r = job.result || {};
  const button = job.status === 'running' ? ` <button class="quiet" data-import-cancel>${t('取消导入')}</button>`
    : job.status === 'done' && r.listed === false ? ` <button class="primary" data-open-library="${esc(r.path)}" data-name="${esc(r.name)}">${t('打开这个图库')}</button>` : '';
  return `<p class="muted-note import-line">${esc(line)}${button}</p>`;
}
// #41：可分享的导出不包含个人输入；照实说明，而不要暗示这是一次完整的搬家。
const EXPORT_LEAVES_OUT = t('不含图片、画师出处缓存和导入过的 X 关注列表；在新电脑上要回查画师，需要重新导入 X 归档。');
// 系统的文件夹选择框（可一次选多个）。没有选择框的地方，用输入的路径代替。
async function pickFolders() {
  try { return await chooseFolders(); }
  catch (err) {
    if (!/Unsupported/.test(err.message)) { toast(err.message); return []; }
    const typed = prompt(t('这台电脑上没有文件夹选择窗口，请输入文件夹路径：'));
    return typed?.trim() ? [typed.trim()] : [];
  }
}
const removingSources = new Set();
async function libraryAction(action, body = {}, token = session.token) {
  try { return await libraryRequest(action, body, token); } catch (err) { toast(err.message); return null; }
}
export async function startImport(path) {
  const r = await libraryAction('import', {path});
  if (r?.task) importStarted(r.task);
}
// 旧格式的图库要用户同意升级后才能打开（#193）：后端这样说明，页面用后端自己的解释询问，然后带上 upgrade 再请求一次。
// 新页面会说明备份在哪。检查发现 source.sqlite 损坏的图库以同样的方式提供抢救（#228）；抢救在图库打开之前运行，大图库
// 上可能要几分钟。
export async function openLibrary(path, name, agreed = null) {
  try { await openLibraryRequest(path, agreed); }
  catch (err) {
    if (!agreed && said(err.said, '这个图库需要升级到新格式才能打开。升级后旧版本的 Illustash 打不开它；升级前会自动备份，升级没有完成时图库保持原样。')) {
      if (await confirmAction(err.message, t('「{name}」需要升级', {name}), t('升级并打开'))) return openLibrary(path, name, 'upgrade');
      return;
    }
    if (!agreed && said(err.said, '这个图库的识别结果数据库（source.sqlite）已损坏，为保护数据已停止写入；人工整理记录在 library.sqlite 里，没有受影响。可以点「抢救重建」：还读得出的识别结果和每张图的 ID 都会保留，读不出的部分重新扫描来源文件夹、重新识别；损坏的文件移到图库的 backups 文件夹里留存。抢救前请确认来源文件夹都能访问。')) {
      if (await confirmAction(err.message, t('「{name}」的识别结果数据库已损坏', {name}), t('抢救重建'))) {
        toast(t('正在抢救「{name}」的识别结果数据库，可能要几分钟…', {name}));
        return openLibrary(path, name, 'rescue');
      }
      return;
    }
    toast(err.message); return;
  }
  // 这个页面的状态属于它离开的那个图库：在下面重新载入之前不再读写。
  toast(t('正在打开「{name}」…', {name}));
  if (await libraryOpened(path)) { location.hash = '#/home'; location.reload(); return; }
  $('errorBanner').textContent = t('切换图库超时：请检查启动终端，然后刷新页面。'); $('errorBanner').hidden = false;
}

// ---------- 绑定的账号（docs/PROVENANCE.md 第 8 节）：所有图库共用；密钥存在钥匙串里 ----------
export function renderAccountsCard() {
  const card = $('setAccounts'); if (!card || state.route.page !== 'settings') return;
  const a = state.accounts;
  if (!a) { card.innerHTML = `<h2>${t('账号')}</h2><p class="muted-note">${t('正在读取…')}</p>`; return; }
  const p = a.pixiv, x = a.x, s = a.saucenao;
  card.innerHTML = `${cardHead(t('账号'), t('找回画师从你的关注出发：拉他们公开作品的小图，在本机逐张比对。只读，不点赞、不关注、不发任何内容。'))}
    <h3 class="acct-h">Pixiv${scope('shared')}</h3>
    ${pixivAccount(a)}
    <h3 class="acct-h">X${scope('library')}</h3>
    <p class="muted-note">${t('用 X 的官方数据归档导入关注列表：X 设置 → 你的账号 → 下载你的数据，收到邮件后下载 zip 导入。只读取其中的关注（数字账号 ID），私信、推文都不会打开。')}</p>
    <div class="setting-line"><span>${x.imported ? `${t('已导入 {n} 个关注', {n: number(x.imported)})}<small class="line-hint">${t('已对应到画师 {resolved} 位 · 待查 {pending} 个', {resolved: number(x.resolved || 0), pending: number(x.pending || 0)})}${x.imported_at ? ' · ' + t('导入于 {time}', {time: esc(String(x.imported_at).slice(0, 16))}) : ''}</small>` : t('尚未导入')}</span>
      <span class="lib-pick"><input class="lib-input" id="acctXPath" placeholder="${t('或粘贴 zip / 解压后文件夹的路径')}"><button class="quiet" data-acct-x>${t('导入归档…')}</button></span></div>
    <h3 class="acct-h">${t('SauceNAO（以图搜图）')}${scope('shared')}</h3>
    ${s.bound ? `<div class="setting-line"><span>${t('已保存 API key')}<small class="line-hint">${t('以图搜图还没有上线，现在不会使用')}</small></span><span><button class="quiet" data-acct-unbind="saucenao">${t('移除')}</button></span></div>`
      : `<div class="setting-line"><span>API key<small class="line-hint">${t('在 saucenao.com 账号页的 search-api 里；以图搜图上线前不会使用')}</small></span><span class="lib-pick"><input class="lib-input" id="acctSauce" type="password" autocomplete="off" placeholder="${t('40 位十六进制')}"><button class="quiet" data-acct-bind="saucenao">${t('保存')}</button></span></div>`}`;
}
// Pixiv：在桌面 App 里，用户在它的 Pixiv 窗口里自己登录（docs/PROVENANCE.md 第 8 节）；粘贴 PHPSESSID 作为普通网页版的
// 开发入口保留。
function pixivAccount(a) {
  const p = a.pixiv, desk = a.desktop, inApp = p.via === 'app';
  const paste = label => `<div class="setting-line"><span>PHPSESSID<small class="line-hint">${t('开发用入口')}</small></span><span class="lib-pick"><input class="lib-input" id="acctPixiv" type="password" autocomplete="off" placeholder="12345678_…"><button class="quiet" data-acct-bind="pixiv">${label}</button></span></div>`;
  // 开发模式之外，普通网页版只指向桌面 App 的登录窗口。
  const pasteHint = devMode ? `<p class="muted-note">${t('网页版（开发用）：在浏览器里登录 pixiv.net，打开开发者工具（{keys}）→ Application（Safari 为「存储空间」）→ Cookie → pixiv.net，复制 {cookie} 的值粘贴到这里。', {keys: isMac ? '⌥⌘I' : 'Ctrl+Shift+I', cookie: '<code>PHPSESSID</code>'})}</p>` : `<p class="muted-note">${t('请从 Illustash 桌面 app 打开设置，在它的 Pixiv 窗口里登录。')}</p>`;
  const login = label => state.pixivWaiting ? `<span class="muted-note">${t('正在等你在 Pixiv 窗口里登录…')}</span>` : `<button class="quiet" data-acct-login>${label}</button>`;
  if (!p.bound) {
    return desk ? `<p class="muted-note">${t('打开 Pixiv 官方登录页，由你亲手登录（出现人机验证请自己完成）；登录状态只留在 app 的 Pixiv 窗口里。登录后才能看到 R18 作品。Pixiv 没有给第三方开放接口，这里用的是网页自己的接口，低频只读。')}</p>
      <div class="setting-line"><span>${t('尚未登录')}</span><span>${login(t('登录 Pixiv…'))}</span></div>` : pasteHint + (devMode ? paste(t('绑定')) : '');
  }
  const line = `<div class="setting-line"><span>${p.expired ? `<span class="acct-expired">${t('登录已过期')}</span>` : t('已绑定')}<small class="line-hint">${esc(p.name || '')}${p.user_id ? ` · ID ${esc(p.user_id)}` : ''}${p.following != null && !p.expired ? ' · ' + t('关注 {n} 位', {n: number(p.following)}) : ''}${p.checked_at ? ' · ' + t('检查于 {time}', {time: esc(p.checked_at)}) : ''}</small></span><span>${desk && !p.expired ? login(inApp ? t('打开 Pixiv 窗口') : t('改用窗口登录…')) : ''}<button class="quiet" data-acct-check="pixiv">${t('重新检查')}</button><button class="quiet" data-acct-unbind="pixiv">${t('解绑')}</button></span></div>`;
  if (!p.expired) return line;
  const why = t('找回画师会暂停查 Pixiv，免得在没登录的状态下漏掉 R18 作品。');
  if (desk) return line + `<p class="muted-note">${t('Pixiv 不再接受这个登录（退出过，或者过期了）。')}${why}${t('在 Pixiv 窗口里重新登录一次就能恢复。')}${p.window_logged_in ? t('窗口里已经重新登录了，点「重新检查」确认。') : ''}</p>
      <div class="setting-line"><span>${t('重新登录')}</span><span>${login(t('重新登录 Pixiv…'))}</span></div>`;
  if (inApp) return line + `<p class="muted-note">${t('Pixiv 登录在桌面 app 的 Pixiv 窗口里。')}${why}${t('请从 Illustash 桌面 app 打开，再重新登录。')}</p>`;
  if (!devMode) return line + `<p class="muted-note">${t('Pixiv 不再接受这个登录状态。')}${why}${t('请从 Illustash 桌面 app 重新登录。')}</p>`;
  return line + `<p class="muted-note">${t('Pixiv 不再接受这个登录状态（在浏览器里退出过，或者过期了）。')}${why}${t('重新复制一次 {cookie} 粘贴到下面就能恢复。', {cookie: '<code>PHPSESSID</code>'})}</p>` + paste(t('重新绑定'));
}
async function accountAction(action, body = {}) {
  try { return await accountRequest(action, body); } catch (err) { toast(err.message); return null; }
}

// ---------- 找回画师（docs/PROVENANCE.md 第 8 节）：设置卡片；运行的轮询在 core/jobs.js ----------
const provenanceDrawn = new WeakMap();
export function renderProvenanceCard() {
  const card = $('setProvenance'); if (!card || state.route.page !== 'settings') return;
  const p = state.provenance;
  if (!p) { card.innerHTML = `<h2>${t('找回画师')}</h2><p class="muted-note">${t('正在读取…')}</p>`; return; }
  if (!p.available) { card.innerHTML = `<h2>${t('找回画师')}</h2><p class="muted-note">${t('只有图库包能找回画师。')}</p>`; return; }
  const s = p.state || {}, busy = PROV_ACTIVE.includes(s.status), cov = p.coverage;
  const covLine = (label, key) => cov?.pictures?.[key] ? `<div class="prov-stat"><b>${pct(cov.with_artist?.[key] || 0, cov.pictures[key])}</b><span>${label}<small>${t('{done}/{total} 张', {done: number(cov.with_artist?.[key] || 0), total: number(cov.pictures[key])})}</small></span></div>` : '';
  const total = cov ? Object.values(cov.pictures || {}).reduce((a, b) => a + b, 0) : 0;
  const html = cardHead(t('找回画师'), t('逐张查图的画师：用像素哈希和 IQDB 签名（只发哈希文本，不发图片）在 Danbooru 查原帖，并比对你关注的画师。结果在详情面板的「画师」里，可以点「不是 TA」纠正。')) + `
    ${cov ? `<div class="prov-stats">${covLine(t('X 的图有画师'), 'x')}${covLine(t('Pixiv 的图有画师'), 'pixiv')}${covLine(t('其他来源有画师'), 'other')}<div class="prov-stat"><b>${pct(cov.with_original_post || 0, total)}</b><span>${t('能对到原帖')}<small>${t('{n} 张', {n: number(cov.with_original_post || 0)})}</small></span></div></div>` : ''}
    <div class="setting-line set-status"><span><span class="local-dot${busy ? ' busy' : s.status === 'failed' ? ' warn' : ''}"></span>${busy ? t('正在查找…') : s.status === 'failed' && s.reason ? t('上次出错：{reason}', {reason: esc(tb(s.reason))}) : p.runs?.[0] ? t('上次 {time} · 新找到 {n}', {time: esc(localTime(p.runs[0].started_at)), n: number(p.runs[0].found || 0)}) : t('还没有查过')}</span>${busy ? `<button class="quiet small" data-open-tasks>${t('在任务面板查看')}</button>` : `<button class="quiet small" data-open-tasks="provenance">${t('查找画师…')}</button>`}</div>
    <label class="switch-line"><span><b>${t('同步后自动查新图')}${scope('library')}</b><small>${p.auto_network ? t('新加入的图做签名识别、查像素哈希、比对新找到的画师；每次最多 {mb} MB', {mb: number(p.budgets?.new || 50)}) : t('新加入的图只在本机做签名识别；要联网查，在任务面板里每次授权')}${p.frozen ? ' · ' + t('测试图库不同步，这个开关不起作用') : ''}</small></span><input type="checkbox" class="switch" id="provAuto" ${p.auto ? 'checked' : ''}></label>
    ${cov ? `<a class="setting-line set-link" href="#/artist-audit"><span>${t('画师抽查')}<small class="line-hint">${t('随机抽 50 张，逐张判断找到的画师对不对')}</small></span><span>›</span></a>` : ''}
    ${state.accounts && !state.accounts.pixiv?.bound ? `<p class="settings-note net-foot">${t('登录 Pixiv 后才能比对 R18 作品、查 Pixiv 上的相关画师（见下方「账号」）。')}</p>` : ''}`;
  // 运行期间会轮询：只在有变化时重画，这样点击绝不会因为按钮被替换而丢失。
  if (provenanceDrawn.get(card) === html) return;
  provenanceDrawn.set(card, html); card.innerHTML = html;
  wireFolds(card);
}

// ---------- 检查更新（#191，native/terms/src/app_updates.rs） ----------
// 用户允许之前关闭：新图库的向导问一次，「设置 › 网络与隐私」里有开关。允许之后，后端在每次启动时检查，App 一直开着时
// 每天检查一次；页面只询问是否该检查了。有新版本时在底部提示，有任务运行时绝不提示，点开是在浏览器里打开 Release 页面：
// 这里不下载也不安装任何东西。「设置 › 关于」里总有手动检查。
// 设置各页上次画出的更新状态：只在它变了时重画，正在输入时绝不重画。
let updatesDrawn = 'null';
export function updatesLine() {
  const u = state.updates; if (!u) return `<span>${t('检查更新')}</span>`;
  const when = u.checked_at ? t('上次检查 {time}', {time: esc(localStamp(Date.parse(u.checked_at) / 1000))}) : t('还没有检查过');
  const status = u.newer ? t('有新版本 {version}', {version: esc(u.latest)}) : u.checked_at && !u.error ? t('已是最新版本') : '';
  return `<span>${status || t('检查更新')}<small class="line-hint">${when}${u.error ? ' · ' + esc(tb(u.error)) : ''}${u.auto ? '' : ' · ' + t('自动检查已关闭，可在「设置 › 网络与隐私」里打开')}</small></span>
    <span class="update-acts">${u.newer ? `<button class="primary small" data-updates-open>${t('打开下载页')}</button>` : ''}<button class="quiet small" data-updates-check>${t('检查更新')}</button></span>`;
}
export const openRelease = () => { if (state.updates?.url) window.open(state.updates.url, '_blank', 'noopener'); };


// 新图库表单里输入的内容随输入保存，这样卡片在后台刷新时不会丢失任何东西。
export function onLibraryInput(e) {
  if (e.target.id === 'libName') libForm.name = e.target.value;
  else if (e.target.id === 'libPhotos') libForm.photos = e.target.checked;
}
// 图库卡片的按钮。
export async function onLibrariesClick(e) {
  const btn = e.target.closest('button'); if (!btn || !$('setLibraries')?.contains(btn)) return;
  const d = btn.dataset;
  const keep = () => { libForm.name = $('libName')?.value ?? libForm.name; libForm.photos = $('libPhotos')?.checked ?? libForm.photos; };
  if ('libNew' in d) { Object.assign(libForm, {open: true, name: '', folders: {}, photos: false, created: null}); renderLibrariesCard(); }
  else if ('libAgain' in d) { libForm.created = null; renderLibrariesCard(); }
  else if ('libCancel' in d) { libForm.open = false; renderLibrariesCard(); }
  else if (d.libPick) {
    keep();
    const picked = await pickFolders(), column = libForm.folders[d.libPick] ||= [];
    for (const p of picked) if (!Object.values(libForm.folders).some(c => c.includes(p))) column.push(p);
    renderLibrariesCard();
  }
  else if (d.libDrop) { keep(); libForm.folders[d.libDrop]?.splice(+d.index, 1); renderLibrariesCard(); }
  else if (d.libChoose) {
    const r = await libraryAction('choose', {kind: d.libChoose}); if (!r?.path) return;
    if (d.libChoose === 'library') openLibrary(r.path, r.path.split('/').pop().replace(/\.pilib$/, ''));
    else await startImport(r.path);
  }
  else if ('libCreate' in d) {
    keep();
    const sources = Object.entries(libForm.folders).flatMap(([preset, paths]) => paths.map(path => ({kind: 'folder', path, preset})));
    if (libForm.photos) sources.push({kind: 'photos'});
    if (!sources.length) { toast(t('请至少添加一个文件夹')); return; }
    const lib = await libraryAction('new', {name: libForm.name, sources});
    if (lib?.listed === false) { libForm.created = lib; renderLibrariesCard(); }
    else if (lib) { libForm.open = false; openLibrary(lib.path, lib.name); }
  }
  else if (d.libAddSource) {
    const paths = await pickFolders(); if (!paths.length) return;
    const r = await libraryAction('sources/add', {kind: 'folder', preset: d.libAddSource, paths});
    await loadLibraries();
    if (r) { toast(t('已添加 {n} 个文件夹', {n: r.added?.length || 1}) + (r.sync ? t('，正在同步') : '')); if (r.sync) pollSync(); }
  }
  else if ('libAddPhotos' in d) {
    const r = await libraryAction('sources/add', {kind: 'photos'});
    await loadLibraries();
    if (r) { toast(t('已添加 iCloud 照片') + (r.sync ? t('，正在同步') : '')); if (r.sync) pollSync(); }
  }
  else if (d.libRemove) {
    if (removingSources.has(d.libRemove)) return;
    removingSources.add(d.libRemove);
    const token = session.token, route = state.route;
    try {
      const ask = await libraryAction('sources/remove', {source: d.libRemove}, token); if (!ask) return;
      if (token !== session.token || route !== state.route) return;
      if (!await confirmAction(t('这个来源的 {n} 张图会从图库移除。人工整理记录会保留，重新添加同一个文件夹可以恢复。确定移除吗？', {n: number(ask.count)}), t('移除来源'))) return;
      const r = await libraryAction('sources/remove', {source: d.libRemove, confirm: 1}, token);
      await loadLibraries();
      if (r) { toast(t('已移除来源（{n} 张图）', {n: number(r.removed)}) + (r.sync ? t('，正在同步') : '')); if (r.sync) pollSync(); }
    } finally { removingSources.delete(d.libRemove); }
  }
  else if (d.libPreset !== undefined && d.preset) {
    if (await libraryAction('preset', {source: d.libPreset, preset: d.preset})) { toast(t('出处已更新')); loadLibraries(); }
  }
  else if (d.forgetLibrary) { if (await libraryAction('forget', {path: d.forgetLibrary})) loadLibraries(); }
  else if ('libExport' in d) { if (await libraryAction('export')) { await loadLibraries(); pollExport(); } }
  else if ('importCancel' in d) { btn.disabled = true; try { await cancelImport(); } catch (err) { toast(err.message); } }
}

// 设置各页的处理函数，以及读取或更改它们的数据之后要做的事。页面启动时调用一次。
export function wireSettings() {
  // 设置 › 网络与隐私：授权或撤销，然后重画卡片。
  document.addEventListener('click', async e => {
    const b = e.target.closest('[data-names-network]');
    if (b && await setNamesNetwork(b.dataset.namesNetwork === '1')) { renderSettings(false); drawGuide(); }
  });
  // 搜索下面的「允许查询…」按钮：一旦允许，就把输入的内容再搜一次。
  document.addEventListener('click', async e => {
    const b = e.target.closest('[data-names-allow]');
    if (!b) return;
    e.preventDefault();
    const input = b.closest('dialog, .combo, .watch-add, #picker')?.querySelector('input');
    if (await setNamesNetwork(true)) { input?.focus(); input?.dispatchEvent(new Event('input', {bubbles: true})); }
  });
  onSettings((what, detail) => {
    if (what === 'data') {
      if (detail.error) error(readError(detail.error));
      if (state.route.page === 'settings') renderSettings(false);
    } else if (what === 'libraries') {
      const cur = state.libraries?.info;
      $('brandLib').textContent = cur?.test ? t('测试·') + (cur.name.split(' · ').pop()) : '';
      $('brand').title = cur ? `Illustash · ${cur.name}` : 'Illustash';
      renderLibrariesCard();
    } else if (what === 'accounts') renderAccountsCard();
    else if (what === 'pixiv') toast(detail.error ? detail.error.message : t('已绑定 Pixiv：{name}', {name: detail.bound.name || detail.bound.user_id}));
    else if (what === 'updates') {
      drawNudge();
      // 显示它的设置页只在有变化时重画，绝不在用户输入时重画。
      const now = JSON.stringify(state.updates ?? null);
      if (now !== updatesDrawn) { updatesDrawn = now; if (state.route.page === 'settings' && ['network', 'about'].includes(settingsSection()[0])) renderSettings(false); }
    }
  });
  // 设置页上的状态行转到任务面板，运行在那里启动、停止和确认。
  $('settingsPage').addEventListener('click', e => {
    const b = e.target.closest('[data-open-tasks]'); if (!b) return;
    e.stopPropagation();
    openTasks({armProvenance: b.dataset.openTasks === 'provenance'});
  });
  $('settingsPage').addEventListener('input', onLibraryInput);
  $('settingsPage').addEventListener('click', onLibrariesClick);
  $('settingsPage').addEventListener('click', e => {
    const b = e.target.closest('[data-open-library]');
    if (b) openLibrary(b.dataset.openLibrary, b.dataset.name);
  });
  $('settingsPage').addEventListener('click', async e => {
    const btn = e.target.closest('button'); if (!btn || !$('setAccounts')?.contains(btn)) return;
    const d = btn.dataset;
    if (d.acctBind) {
      const input = $(d.acctBind === 'pixiv' ? 'acctPixiv' : 'acctSauce'); btn.disabled = true;
      const r = await accountAction(d.acctBind, {value: input.value}); btn.disabled = false;
      if (r) { toast(d.acctBind === 'pixiv' ? t('已绑定 Pixiv：{name}', {name: r.name || r.user_id}) : t('已保存 SauceNAO API key')); loadAccounts(); }
    } else if ('acctLogin' in d) {
      btn.disabled = true;
      const r = await accountAction('pixiv/login'); btn.disabled = false;
      if (r) waitPixivLogin();
    } else if (d.acctCheck) {
      btn.disabled = true;
      const r = await accountAction('pixiv/check'); btn.disabled = false;
      if (r) { toast(r.ok ? t('Pixiv 登录有效：{name}', {name: r.name || r.user_id}) : t('Pixiv 登录已过期，请重新登录')); loadAccounts(); }
    } else if (d.acctUnbind) {
      if (d.acctUnbind === 'pixiv') stopPixivLogin();
      if (await accountAction(d.acctUnbind + '/unbind')) { toast(t('已解绑')); loadAccounts(); }
    } else if ('acctX' in d) {
      const path = $('acctXPath').value.trim();
      const r = await accountAction('x/import', path ? {path} : {});
      if (r && !r.cancelled) { toast(t('新导入 {n} 个关注', {n: number(r.new)})); loadAccounts(); }
    }
  });
  $('settingsPage').addEventListener('change', async e => {
    if (e.target.id !== 'provAuto') return;
    try { await saveStartup({provenance: e.target.checked}); toast(e.target.checked ? t('同步后会自动查新图的画师') : t('同步后不再自动查画师')); }
    catch (err) { toast(err.message); e.target.checked = !e.target.checked; }
    loadProvenance();
  });
  document.addEventListener('click', async e => {
    if (e.target.closest('[data-updates-open]')) { openRelease(); return; }
    const b = e.target.closest('[data-updates-check]'); if (!b) return;
    b.disabled = true; b.textContent = t('正在检查…');
    try { const u = await checkUpdates(); toast(u.error ? tb(u.error) : u.newer ? t('有新版本 {version}', {version: u.latest}) : t('已是最新版本')); }
    catch (err) { toast(err.message); }
    if ($('aboutUpdates')) $('aboutUpdates').innerHTML = updatesLine();
    drawNudge();
  });
  document.addEventListener('change', async e => {
    const box = e.target.closest('[data-updates-auto]'); if (!box) return;
    try { await updatesPost({action: 'auto', allow: box.checked}); toast(box.checked ? t('已打开自动检查更新') : t('已关闭自动检查更新')); }
    catch (err) { toast(err.message); box.checked = !box.checked; }
    if (state.route.page === 'settings') renderSettings(false);
    updatesTick();
  });
  loadLibraries();
  pollImports();
}
