// 一次写入或一个后台任务的回应会改动页面别处的什么（桌面布局，#199）：图库的计数和图片、显示任务的设置卡片、向导。
// app.js 把这些交给 onPost() 和 onJob()。
import {SYNC_ACTIVE, importLine} from '../core/jobs.js';
import {metaWritten, refreshMeta} from '../core/meta.js';
import {pageOf} from '../core/pages.js';
import {loadLibraries} from '../core/settings.js';
import {readError} from '../core/session.js';
import {state} from '../core/state.js';
import {drawShelf} from './catalog.js';
import {error, toast} from './dom.js';
import {reload, selCount} from './grid.js';
import {guideAfterPost, guideRefresh} from './guide.js';
import {renderIndexCard, renderLibrariesCard, renderNamesCard, renderProvenanceCard} from './settings.js';

// 后端接受了一次写入：可能变了的计数和读取，以及向导的下一步。
export function afterWrite(path, body) {
  guideAfterPost(path, body);
  state.countDirty = true; state.readEpoch++;
  // 防止更早的元数据读取覆盖刚完成的改动。
  metaWritten(path === '/api/tag');
}
// 后台任务的回应在任务面板之外改动什么：图库的计数和图片、显示这个任务的设置卡片，以及向导。
export async function afterJob(job, change) {
  if (job === 'sync') {
    if (change.arrived) {
      state.countDirty = true; state.readEpoch++;
      await refreshMeta().catch(e => error(readError(e)));
      // 面板页自己重画（首页的按天分组和计数来自它自己的读取，同步落定后重画一次）。
      if (!pageOf(state.route).panel && !selCount()) reload();
      else if (!SYNC_ACTIVE.includes(state.sync.status)) pageOf(state.route).refresh?.('sync');
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
