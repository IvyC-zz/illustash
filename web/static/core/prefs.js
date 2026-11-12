// 按浏览器保存的偏好（#199）：视图大小、筛选、主题之类，存在 localStorage 里，键以「pi.」开头。拒绝存储的浏览器
// 用默认值。不碰 DOM。
export const prefs = {
  get(key, fallback) { try { const v = localStorage.getItem('pi.' + key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem('pi.' + key, JSON.stringify(value)); } catch {} },
};
