// 页面共享的状态（#199）：路由、图库的列表和选择、各项计数以及每个任务最近一次的回应，由需要它们的模块读写；不按领域
// 拆分。放在别处、各自只有一个写入方的：会话（session.js），编辑和撤销（edits.js）。不碰 DOM。
import {prefs} from './prefs.js';
export const state = {
  route: {page: 'library'},
  chars: [], charMap: new Map(), pins: [], char: null, pools: {unmatched: 0, skipped: 0}, dupInfo: {groups: 0, mixed: 0}, exclInfo: {count: 0, reasons: {}}, pendingCount: 0, sync: {status: 'off'}, syncHistory: [], settings: null, recentCount: 0, maybeCount: 0, userTags: [],
  items: [], total: 0, readEpoch: 0, pages: new Set(), pending: new Set(), generation: 0, dataVersion: 0, gridKey: '',
  // size 由知道缩放范围的 ui/grid.js 设置。
  cols: 1, cellW: 180, rowH: 250, gap: 12, size: 185, fit: prefs.get('fit', false),
  selecting: false, sel: {all: false, ids: new Set(), exclude: new Set()}, anchor: -1, cursor: -1,
  focused: null, detail: null, viewer: -1, armed: null, nsfw: ['', 'hide', 'only'].includes(prefs.get('nsfw', '')) ? prefs.get('nsfw', '') : '',
};
