// Edits and their undo (#199). Every write is undoable: the backend answers each one with the latest step it can undo,
// and one change runs at a time — a write, or the undo of one. No DOM: a layout draws the undo control from onLatest().
import {t, tb, I18N} from './i18n.js';
import {post} from './session.js';
import {listeners} from './events.js';
// latest: {id, label} of the step undo would revert, or null. busy: a change or an undo is running.
export const edits = {latest: null, busy: false};
const changed = listeners();
// fn(edits) after each change; returns a function that removes the listener again.
export const onLatest = changed.on;
// Also called after busy changes, so the controls follow it.
export function setLatest(step) {
  edits.latest = step;
  changed.emit(edits);
}
// Reverts the latest step; resolves to its label (as sent), with the next step already set.
export async function undoLatest() {
  const {id, label} = edits.latest, result = await post('/api/undo', {id});
  setLatest(result.undo);
  return label;
}
// What a toast says after undoing the step with this label.
export const undoneText = label => t('已撤销 · {label}', {label: I18N.language === 'zh-CN' ? label.replace(/^已/, '') : tb(label)});
