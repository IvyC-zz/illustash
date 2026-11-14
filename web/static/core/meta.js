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
// 当前角色页的角色（不在角色页时为 null）：换页时由 ui/shell.js 经这里换上，元数据重读后这里换成新读到的那份。
export function setChar(c) { state.char = c; }
// 身份整理还剩几对：元数据读取带回一次，身份整理页翻页时也会带回最新的总数。
export function setIdentityCount(n) { state.identityCount = n; }
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
// 后台学习收尾（#400）：确认或撤销角色之后，服务端在下一次恢复检查（约每秒一次，native/terms/src/app_http.rs）时
// 才开始学习（app_research.rs），学到的结果发布之后，未识别计数和分组才是最终的样子；写入请求返回时还没到这一步。
// 能等的信号是 /api/clusters 带回的 learner 状态：idle（带上次学完的时间 at）、learning / importing / grouping /
// waiting（进行中）、error、off（没有学习 worker，写入时已同步处理完，不用等）。
export const learnerState = () => api('/api/clusters?nsfw=' + state.nsfw).then(r => r.learner || null);
const LEARNER_BUSY = ['learning', 'importing', 'grouping', 'waiting'];
// 一直空闲这么久就认为这次写入没有触发学习（撤销不涉及角色时就不会）；学习本身最多等 LEARN_LIMIT，之后由下一次
// 写入、同步结束或换页再读。按累计的等待时间算，不读时钟。
export const LEARN_QUIET = 3000, LEARN_LIMIT = 60000;
let learnRun = 0;
// before：写入发出前读到的 learner 状态（读不到为 null）。学到的结果已经发布时解析为 true；没有学习、学习失败、
// 超时，或更新的一次写入接手了等待（每次调用都会让之前的等待作废）时解析为 false。
export async function learningSettled(before) {
  const run = ++learnRun;
  let busySeen = LEARNER_BUSY.includes(before?.status), baseAt = before?.status === 'idle' ? before.at ?? null : undefined;
  if (before?.status === 'off') return false;
  for (let waited = 0, step = 400; waited < LEARN_LIMIT; step = Math.min(2000, Math.round(step * 1.5))) {
    await new Promise(done => setTimeout(done, step)); waited += step;
    if (run !== learnRun) return false;
    let s; try { s = await learnerState(); } catch { return false; }
    if (run !== learnRun || !s || s.status === 'off' || s.status === 'error') return false;
    if (LEARNER_BUSY.includes(s.status)) { busySeen = true; continue; }
    if (s.status !== 'idle') return false;
    // 空闲：学过一轮（看到过进行中，或学完的时间变了）就是落定了；写入前的状态没读到时，第一次读到的空闲作为基准。
    if (busySeen || (baseAt !== undefined && (s.at ?? null) !== baseAt)) return true;
    if (baseAt === undefined) baseAt = s.at ?? null;
    if (waited >= LEARN_QUIET) return false;
  }
  return false;
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

