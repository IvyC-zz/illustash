// Illustash front end: plain JS, no build step. Machine identities count by default;
// the user only removes what is wrong. Every write is undoable.
// Native ES modules (#199): core/ holds what any layout can reuse and never touches the DOM (requests and the session,
// edits and undo, background jobs, settings, the catalog, routing, metadata, formatting); ui/ is this desktop layout.
// This file is the entry: it connects the modules (what a write or a job's answer changes elsewhere on the page),
// wires them in order and starts the page. docs/WEB_V2.md「前端模块」 maps the modules.
import {t} from './core/i18n.js';
import {number} from './core/format.js';
import {prefs} from './core/prefs.js';
import {state} from './core/state.js';
import {adoptToken, onPost, api} from './core/session.js';
import {setLatest} from './core/edits.js';
import {onJob} from './core/jobs.js';
import {$, platformHints, toast, wireToast, error, wireDom} from './ui/dom.js';
import {wireTasks, taskTick} from './ui/tasks.js';
import {confirmAction} from './ui/dialog.js';
import {wireSettings} from './ui/settings.js';
import {wireCatalog} from './ui/catalog.js';
import {refreshMeta} from './core/meta.js';
import {parseHash} from './core/route.js';
import {applyRoute, navCount, wireShell} from './ui/shell.js';
import {renderGrid, undo, wireGrid} from './ui/grid.js';
import {wirePalette} from './ui/palette.js';
import {wireDetail} from './ui/detail.js';
import {wireHome} from './ui/home.js';
import {wireReview} from './ui/review.js';
import {guideAfterBoot, guideOnOpen, wireGuide} from './ui/guide.js';
import {afterWrite, afterJob} from './ui/effects.js';
import {pollIntegrity} from './core/integrity.js';
import {wireModels} from './ui/models.js';
import {wireIntegrity} from './ui/integrity.js';
platformHints(document.body);
wireDom();
onPost(afterWrite);
onJob(afterJob);
wireToast(() => undo());
// In the order the page once registered them: handlers of the same event run in this order (the guide's Escape last).
wireShell();
wireGrid();
wirePalette();
wireDetail();
wireHome();
wireReview();
wireSettings();
wireCatalog();
wireTasks();
wireGuide();
wireIntegrity();
wireModels();
$('undoButton').onclick = undo; $('helpButton').onclick = () => $('help').showModal();
$('sidebarToggle').onclick = () => { if (innerWidth <= 760) $('sidebar').classList.toggle('mobile-open'); else $('sidebar').hidden = !$('sidebar').hidden; requestAnimationFrame(() => renderGrid()); };
for (const dialog of document.querySelectorAll('dialog')) dialog.addEventListener('click', e => { if (e.target === dialog) { const r = dialog.getBoundingClientRect(); if (e.clientX < r.left || e.clientX > r.right || e.clientY < r.top || e.clientY > r.bottom) dialog.close(); } });

// Start: this session's token and undo step, the counts, then the page the address names.
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
    pollIntegrity();
    // The library opened although it could not be listed among the recent ones (#176): say so once.
    api('/api/libraries').then(x => {
      if (x?.unlisted) error(t('没能把这个图库记进最近图库：{why}。图库照常可用，只是下次启动时可能不会自动打开它。', {why: x.unlisted}));
      // Once per upgrade in this browser: the backup outlives the session that made it (#193).
      if (x?.upgraded && prefs.get('upgradeNoticed', '') !== x.upgraded.backup) {
        prefs.set('upgradeNoticed', x.upgraded.backup);
        confirmAction(t('这个图库已升级到新格式，旧版本的 Illustash 打不开它了。升级前的备份在：\n{backup}\n\n要回到旧版本，先装回旧版，再用这份备份恢复。', {backup: x.upgraded.backup}), t('图库已升级'), t('知道了'), false);
      }
      // Once per rescue in this browser (#228): what it kept, what is recognized again, where the damaged file went.
      const r = x?.rescued;
      if (r?.damaged_copy && prefs.get('rescueNoticed', '') !== r.damaged_copy) {
        prefs.set('rescueNoticed', r.damaged_copy);
        const lines = [t('识别结果数据库已经重建。从损坏的文件里读回了 {salvaged} 张图的记录，按整理记录找回了 {kept} 张图的 ID。', {salvaged: number(r.salvaged), kept: number(r.kept)})];
        if (r.new) lines.push(t('{n} 张图没有找到原来的 ID，按新图处理。', {n: number(r.new)}));
        if (r.pending) lines.push(t('{n} 张图需要重新识别，会在同步时进行。', {n: number(r.pending)}));
        lines.push(t('损坏的文件留在：\n{path}\n确认图库一切正常后可以删掉它。', {path: r.damaged_copy}));
        confirmAction(lines.join('\n\n'), t('抢救完成'), t('知道了'), false);
      }
    }).catch(() => {});
    guideAfterBoot();
  } catch (e) { window.splash?.ready(); error(t('启动失败：{reason}。请检查启动终端，然后刷新页面。', {reason: e.message})); }
})();

