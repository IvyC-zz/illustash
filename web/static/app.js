// Illustash 前端：纯 JS，没有构建步骤。机器给出的身份默认计入，用户只去掉错的。每次写入都可以撤销。
// 原生 ES 模块（#199）：core/ 放任何布局都能复用、从不碰 DOM 的部分（请求与会话、编辑与撤销、后台任务、设置、资料库、
// 路由、元数据、格式化）；ui/ 是这套桌面布局。
// 本文件是入口：把各模块连起来（一次写入或一个任务的回应会改动页面别处的什么），按顺序挂好，然后启动页面。
// 模块地图见 docs/WEB_V2.md「前端模块」。
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
import {pageOf} from './core/pages.js';
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
// 按页面原来注册它们的顺序：同一事件的处理函数按这个顺序运行（向导的 Escape 排最后）。
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

// 启动：本会话的 token 和撤销步骤、各项计数，然后是地址指向的页面。
(async () => {
  try {
    const bootNsfw = state.nsfw;
    const boot = await api('/api/bootstrap?' + new URLSearchParams({nsfw: bootNsfw})); adoptToken(boot.token); setLatest(boot.undo);
    if (boot.read_only) toast(boot.read_only);   // 图库没能切换到词条核心：只读
    navCount('countLibrary', boot.total);
    window.splash?.status(t('正在整理 {n} 张图…', {n: number(boot.total)}));
    const meta = refreshMeta({boot: bootNsfw === state.nsfw ? boot : null});
    const first = parseHash();
    if (first.char || pageOf(first).directory) await meta;
    await guideOnOpen();
    await applyRoute();
    await meta;
    window.splash?.ready();
    taskTick();
    pollIntegrity();
    // 图库打开了，但没能记进最近图库（#176）：提示一次。
    api('/api/libraries').then(x => {
      if (x?.unlisted) error(t('没能把这个图库记进最近图库：{why}。图库照常可用，只是下次启动时可能不会自动打开它。', {why: x.unlisted}));
      // 在这个浏览器里每次升级提示一次：备份比生成它的会话活得久（#193）。
      if (x?.upgraded && prefs.get('upgradeNoticed', '') !== x.upgraded.backup) {
        prefs.set('upgradeNoticed', x.upgraded.backup);
        confirmAction(t('这个图库已升级到新格式，旧版本的 Illustash 打不开它了。升级前的备份在：\n{backup}\n\n要回到旧版本，先装回旧版，再用这份备份恢复。', {backup: x.upgraded.backup}), t('图库已升级'), t('知道了'), false);
      }
      // 在这个浏览器里每次抢救提示一次（#228）：保留了什么、哪些会重新识别、损坏的文件放到了哪里。
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

