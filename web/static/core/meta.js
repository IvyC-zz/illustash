// The library's metadata (#199): characters, review pools and counts, the user's tags and the latest undo step, read
// together; writes in between are never overwritten by an older read. No DOM: onMeta(detail) tells after each read that
// was applied ({total, directory, queue}), or that one failed ({error}).
import {api, adoptToken} from './session.js';
import {setLatest} from './edits.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onMeta = changed.on;
const failed = error => changed.emit({error});
export const fetchChar = async id => (await api('/api/characters?' + new URLSearchParams({id, nsfw: state.nsfw})))[0];
let metaPending = null, metaRevision = 0, metaFull = false;
// A write the backend accepted: an older read still in flight must not overwrite it (a tag needs only the tags again).
export function metaWritten(tagsOnly) { if (metaPending) { metaRevision++; metaFull ||= !tagsOnly; } }
// After an edit the sidebar counts and character list (~0.6 s of server work) can wait until the edits stop:
// answering Y / N in a row then costs one refresh, not one per answer.
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
        // A write during an in-flight read needs a trailing fresh read, not its stale result.
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
  const directory = state.route.page === 'characters';
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
  // Moved into the directory while the summary rows were read: it needs the full rows.
  if (state.route.page === 'characters' && !directory) refreshMeta().catch(failed);
}

