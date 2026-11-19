// 诊断导出只取原生白名单和当前错误回执；不从 DOM、路径、请求或异常原文拼接。
import {t, sentences} from './i18n.js';
import {api, lastDiagnostic} from './session.js';
const changes = () => ({
  not_committed: t('图库没有改动'),
  committed: t('图库已更新'),
  may_be_partial: t('图库可能已改动一部分'),
  unknown: t('图库是否改动尚不确定'),
});
const retries = () => ({retry: t('可以重试'), after_fix: t('处理原因后重试'), check_state: t('核对结果后再重试'), restart: t('重启后核对结果')});
export function diagnosticLine(d) {
  if (!d?.id) return '';
  return sentences(t('错误编号：{id}', {id: d.id}), changes()[d.publication], retries()[d.retry], d.stored === false && t('诊断记录未保存'));
}
export async function diagnosticReport() {
  const report = await api('/api/diagnostics');
  return {...report, ...(lastDiagnostic ? {current: lastDiagnostic} : {})};
}
export const diagnosticText = report => JSON.stringify(report, null, 2);
export const openDiagnosticLogs = () => api('/api/diagnostics/open-logs', {method: 'POST', body: '{}'});
export function downloadDiagnostic(report) {
  const url = URL.createObjectURL(new Blob([diagnosticText(report)], {type: 'application/json'}));
  const a = document.createElement('a'); a.href = url; a.download = 'illustash-diagnostics.json'; a.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
