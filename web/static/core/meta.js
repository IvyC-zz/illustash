// 图库的元数据（#199）：角色、复核池和计数、用户的标签以及最近的撤销步骤，一起读取；期间的写入不会被更早的读取覆盖。
// 不碰 DOM：onMeta(detail) 在每次被采用的读取之后通知（{total, directory, queue}），或通知一次读取失败（{error}）。
import {api, adoptToken} from './session.js';
import {setLatest} from './edits.js';
import {pageOf} from './pages.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onMeta = changed.on;
const failed = error => changed.emit({error});
export const fetchChar = async id => (await api('/api/characters?' + new URLSearchParams({id, nsfw: state.nsfw})))[0];
let metaPending = null, metaRevision = 0, metaFull = false;
// 后端接受了一次写入：仍在进行中的更早的读取不能覆盖它（标签只需要重读标签）。
export function metaWritten(tagsOnly) { if (metaPending) { metaRevision++; metaFull ||= !tagsOnly; } }
// 编辑之后，侧栏计数和角色列表（服务端约 0.6 s 的工作）可以等编辑停下再刷新：连续回答 Y / N 只刷新一次，而不是每个回答
// 刷新一次。
let metaTimer = null, metaTagsOnly = true;
export function metaSoon(tagsOnly) {
  metaTagsOnly &&= tagsOnly; clearTimeout(metaTimer);
  metaTimer = setTimeout(() => { const t = metaTagsOnly; metaTagsOnly = true; refreshMeta({tagsOnly: t}).catch(failed); }, 700);
}
export function refreshMeta({tagsOnly = false, boot = null} = {}) {
  metaRevision++; metaFull ||= !tagsOnly;
  const bootEpoch = state.readEpoch, bootMode = state.nsfw;
  if (!metaPending) metaPending = Promise.resolve().then(async () => {
    try {
      for (;;) {
        const revision = metaRevision, full = metaFull; metaFull = false;
        try { await refreshMetaOnce(full, revision, bootEpoch === state.readEpoch && bootMode === state.nsfw ? boot : null); }
        catch (e) { if (revision === metaRevision) throw e; }
        boot = null;
        if (revision === metaRevision) return;
        // 读取进行中发生了写入，需要随后再读一次新的，不能用它过时的结果。
        metaFull ||= full;
      }
    } finally { metaPending = null; }
  });
  return metaPending;
}
async function refreshMetaOnce(full, revision, bootSeed) {
  if (!full) {
    const tags = await api('/api/user-tags');
    if (revision === metaRevision) state.userTags = tags;
    return;
  }
  const nsfw = new URLSearchParams({nsfw: state.nsfw});
  const charId = state.char?.id;
  const directory = !!pageOf(state.route).directory;
  const [boot, pools, chars, tags, queue, detail] = await Promise.all([bootSeed || api('/api/bootstrap?' + nsfw), api('/api/review-pools?' + nsfw), api('/api/characters?min=1&' + nsfw + (directory ? '' : '&summary=1')), api('/api/user-tags'), api('/api/identity/count?' + nsfw).catch(e => { failed(e); return null; }), charId && !directory ? fetchChar(charId) : null]);
  const currentChar = charId && (detail || (directory && (chars.find(c => c.id === charId) || await fetchChar(charId))));
  if (revision !== metaRevision || nsfw.get('nsfw') !== state.nsfw || !adoptToken(boot.token)) return;
  state.userTags = tags;
  setLatest(boot.undo);
  state.pools = Object.fromEntries(pools.map(p => [p.kind, p.count]));
  state.chars = chars; state.charMap = new Map(chars.map(c => [c.id, c]));
  state.workNames = new Map(chars.filter(c => c.work && c.work_name).map(c => [c.work, c.work_name]));
  state.dupInfo = pools.find(p => p.kind === 'duplicates') || state.dupInfo;
  state.exclInfo = pools.find(p => p.kind === 'excluded') || state.exclInfo;
  state.pendingCount = state.pools.pending || 0;
  state.recentCount = state.pools.recent || 0;
  if (queue) state.identityCount = queue.total;
  if (state.char) state.char = (state.char.id === charId && currentChar) || state.char;
  await changed.emit({total: boot.total, directory, queue: !!queue});
  // 读摘要行期间进入了目录：它需要完整的行。
  if (pageOf(state.route).directory && !directory) refreshMeta().catch(failed);
}

