// 按浏览器保存的偏好（#199）：视图大小、筛选、主题之类，存在 localStorage 里，键以「pi.」开头。拒绝存储的浏览器
// 用默认值。不碰 DOM。
export const prefs = {
  get(key, fallback) { try { const v = localStorage.getItem('pi.' + key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem('pi.' + key, JSON.stringify(value)); } catch {} },
  drop(key) { try { localStorage.removeItem('pi.' + key); } catch {} },
};
// 只属于一个图库的偏好（#523），如最近用过的标签和角色：键后面带图库 ID，换了图库各存各的。没有 ID（取不到当前图库）
// 时读到默认值，写入不保存，不退回到所有图库共用的键。
export const libraryPrefs = id => ({
  get: (key, fallback) => id ? prefs.get(`${key}@${id}`, fallback) : fallback,
  set: (key, value) => { if (id) prefs.set(`${key}@${id}`, value); },
});
