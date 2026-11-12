// 编辑和撤销（#199）。每次写入都可以撤销：后端对每次写入都回应它能撤销的最近一步；同一时间只进行一项改动——一次写入，
// 或撤销一次写入。不碰 DOM：布局按 onLatest() 画撤销控件。
import {t, tb, I18N} from './i18n.js';
import {post} from './session.js';
import {listeners} from './events.js';
// latest：撤销会还原的那一步的 {id, label}，或 null。busy：正在进行一项改动或撤销。
export const edits = {latest: null, busy: false};
const changed = listeners();
// 每次改动后调用 fn(edits)；返回一个再把这个监听去掉的函数。
export const onLatest = changed.on;
// busy 改变后也会调用，让控件跟着变。
export function setLatest(step) {
  edits.latest = step;
  changed.emit(edits);
}
// 还原最近一步；解析为它的 label（照发送时的样子），下一步已经设好。
export async function undoLatest() {
  const {id, label} = edits.latest, result = await post('/api/undo', {id});
  setLatest(result.undo);
  return label;
}
// 撤销带这个 label 的步骤后，toast 显示的文字。
export const undoneText = label => t('已撤销 · {label}', {label: I18N.language === 'zh-CN' ? label.replace(/^已/, '') : tb(label)});
