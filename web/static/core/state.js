// The page's state that is not yet split by domain (#199). It moved out of app.js so that ui/ modules can read it
// without importing app.js; each later step of #199 moves its fields to their own module, with one writer each.
// Already elsewhere: the session (session.js), edits and undo (edits.js). No DOM.
import {prefs} from './prefs.js';
export const state = {
  route: {page: 'library'},
  chars: [], charMap: new Map(), pins: [], char: null, pools: {unmatched: 0, skipped: 0}, dupInfo: {groups: 0, mixed: 0}, exclInfo: {count: 0, reasons: {}}, pendingCount: 0, sync: {status: 'off'}, syncHistory: [], settings: null, recentCount: 0, maybeCount: 0, userTags: [],
  items: [], total: 0, readEpoch: 0, pages: new Set(), pending: new Set(), generation: 0, dataVersion: 0, gridKey: '',
  // size is set by app.js, which knows the zoom range.
  cols: 1, cellW: 180, rowH: 250, gap: 12, size: 185, fit: prefs.get('fit', false),
  selecting: false, sel: {all: false, ids: new Set(), exclude: new Set()}, anchor: -1, cursor: -1,
  focused: null, detail: null, viewer: -1, armed: null, nsfw: ['', 'hide', 'only'].includes(prefs.get('nsfw', '')) ? prefs.get('nsfw', '') : '',
};
