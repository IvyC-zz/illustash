// 诊断导出只取原生白名单和当前错误回执；不从 DOM、路径、请求或异常原文拼接。
import {t} from './i18n.js';
import {api, lastDiagnostic} from './session.js';
const commits = () => ({not_applicable: t('不涉及提交'), not_committed: t('尚未提交'), committed: t('已经提交'), may_be_partial: t('可能已有部分提交'), unknown: t('提交状态需核对')});
const retries = () => ({retry: t('可以重试'), after_fix: t('处理原因后重试'), check_state: t('核对结果后再重试'), restart: t('重启后核对结果')});
export function diagnosticLine(d) {
  if (!d?.id) return '';
  return [t('错误编号：{id}', {id: d.id}), commits()[d.publication], retries()[d.retry], d.stored === false ? t('诊断记录未保存') : ''].filter(Boolean).join(' · ');
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
