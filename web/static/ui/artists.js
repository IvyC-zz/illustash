// 详情面板里的画师（桌面布局，#199）：只在找到时显示，以及点击即授权的查找。
import {esc, number} from '../core/format.js';
import {t, tb} from '../core/i18n.js';
import {poller} from '../core/poll.js';
import {hashFor} from '../core/route.js';
import {api, session} from '../core/session.js';
import {state} from '../core/state.js';
import {confirmAction} from './dialog.js';
import {toast} from './dom.js';

// ---------- 画师（provenance/，这里只读）：只在找到时显示，从不显示成空栏 ----------
// ---------- 按需查找画师（docs/PROVENANCE.md 第 9 节）：点击即授权这一次查找 ----------
const LOOKUP_NOTE = t('查找画师会把所选图片的像素哈希和 IQDB 签名（一串文本，不是图片）发给 Danbooru，按结果显示画师和原帖。\n「找 TA 的其他图」会下载这位画师公开作品的小缩略图，只在本机比对；缩略图比完就删，只留比对用的签名。\n\n以后点这两个按钮不再提示。继续吗？');
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
  lookupAfter = after; lookupPoll.later(1500);
}
// 等这次查找结束（1.5 s 一次，读失败 2 s 后再试），然后说结果、交给 after。新的查找替换旧的（只等最后一次）。
let lookupAfter = null;
const lookupPoll = poller(async fresh => {
  const p = await api('/api/provenance');
  if (!fresh()) return 0;
  const s = p.state || {};
  if (['running', 'stopping'].includes(s.status)) return 1500;
  const found = s.found || 0;
  toast(['done', 'stopped'].includes(s.status) ? (found ? t('找到 {n} 张图的画师', {n: number(found)}) : t('没有找到新的画师'))
    : t('查找没有完成：{reason}', {reason: tb(s.reason) || s.status}));
  lookupAfter?.(found);
  return 0;
}, {retry: 2000});
export const findArtists = (ids, after) => artistLookup({action: 'lookup', ids}, ids.length > 1 ? t('正在查找 {n} 张图的画师…', {n: number(ids.length)}) : t('正在查找画师…'), after);
export const findArtistWorks = (artist, name, after) => artistLookup({action: 'artist', artist}, t('正在找 {name} 的其他图…', {name: name || t('这位画师')}), after);
export function artistSection(d) {
  const find = `<div class="artist-empty"><span class="muted-note">${d.maybe?.length ? t('还没确定画师') : t('还不知道画师')}</span><button class="quiet" data-artist-find title="${t('用像素哈希和 IQDB 查这张图的画师，几秒钟（只发哈希文本，不发图片）')}">${t('查找画师')}</button></div>`;
  if (!d.artists?.length && !d.posts?.length && !d.maybe?.length) return `<section class="inspector-section"><h3 class="section-title">${t('画师')}</h3>${find}</section>`;
  const link = (url, label, title = '') => `<a class="link" href="${esc(url)}" target="_blank" rel="noreferrer noopener"${title ? ` title="${esc(title)}"` : ''}>${esc(label)}</a>`;
  const judge = a => a.confirmed ? `<span class="artist-judge"><span class="artist-ok" title="${t('你确认过')}">✓ ${t('已确认')}</span><button data-artist-verdict="" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('撤回确认')}">${t('撤回')}</button></span>`
    : `<span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('确认是 TA 画的')}">${t('是 TA')}</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('不是 TA 画的：这张不再显示这位画师')}">${t('不是')}</button></span>`;
  const artist = a => `<div class="artist-row"><div class="artist-top"><a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="${t('在图库里只看 TA 的图')}">${esc(a.name)}</a>${judge(a)}</div>
      ${a.count > 1 ? `<a class="artist-more" href="${hashFor({page: 'library', artist: a.id})}">${t('库里还有 {n} 张', {n: number(a.count - 1)})}</a>` : ''}
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, t('在 {site} 打开 TA 的主页', {site: h.site}))).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('下载 TA 公开作品的小缩略图，在本机比对出库里 TA 的其他图（一两分钟）')}">${t('找 TA 的其他图')}</button></div>`;
  // 「也许是 TA」：分组推荐，或在图上读到的签名（native/terms/src/http_artists.rs）；「是 TA」就定下来。
  const maybe = a => `<div class="artist-row artist-maybe"><div class="artist-top"><span class="artist-maybe-name">${t('也许是 {name}', {name: `<a class="artist-name" href="${hashFor({page: 'library', artist: a.id})}" title="${t('在图库里只看 TA 的图')}">${esc(a.name)}</a>`})}</span>
      <span class="artist-judge"><button data-artist-verdict="yes" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('确认是 TA 画的')}">${t('是 TA')}</button><button data-artist-verdict="no" data-artist="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('不是 TA 画的：不再推荐')}">${t('不是')}</button></span></div>
      <div class="artist-reason">${esc(tb(a.reason || ''))}</div>
      ${a.homes.length ? `<div class="artist-homes">${a.homes.map(h => link(h.url, h.label, t('在 {site} 打开 TA 的主页', {site: h.site}))).join(' · ')}</div>` : ''}
      <button class="quiet artist-find-more" data-artist-more="${esc(a.id)}" data-name="${esc(a.name)}" title="${t('下载 TA 公开作品的小缩略图，在本机比对：是同一张图就能确定')}">${t('找 TA 的其他图')}</button></div>`;
  return `<section class="inspector-section"><h3 class="section-title">${t('画师')}</h3>${(d.artists || []).map(artist).join('')}${(d.maybe || []).map(maybe).join('')}${!d.artists?.length ? find : ''}
    ${d.posts?.length ? `<div class="artist-posts"><span>${t('原帖')}</span>${d.posts.map(p => link(p.url, p.site)).join(' · ')}</div>` : ''}</section>`;
}
