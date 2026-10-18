// What a write or a background job's answer changes elsewhere on the page (desktop layout, #199): the library's
// counts and pictures, the settings cards that show a job, the guide. app.js hands these to onPost() and onJob().
import {SYNC_ACTIVE, importLine} from '../core/jobs.js';
import {metaWritten, refreshMeta} from '../core/meta.js';
import {panelPage} from '../core/route.js';
import {loadLibraries} from '../core/settings.js';
import {readError} from '../core/session.js';
import {state} from '../core/state.js';
import {drawShelf} from './catalog.js';
import {error, toast} from './dom.js';
import {reload, selCount} from './grid.js';
import {guideAfterPost, guideRefresh} from './guide.js';
import {renderHome} from './home.js';
import {renderIndexCard, renderLibrariesCard, renderNamesCard, renderProvenanceCard} from './settings.js';

// A write the backend accepted: the counts and reads that may have changed, and the guide's next step.
export function afterWrite(path, body) {
  guideAfterPost(path, body);
  state.countDirty = true; state.readEpoch++;
  // Prevent an older metadata read from overwriting the just-completed mutation.
  metaWritten(path === '/api/tag');
}
// What a background job's answer changes outside the task panel: the library's counts and pictures, the settings
// cards that show the job, and the guide.
export async function afterJob(job, change) {
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
}
