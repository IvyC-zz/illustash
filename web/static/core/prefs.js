// Per-browser preferences (#199): view size, filters, theme and the like, kept in localStorage under "pi.". A browser
// that refuses storage gets the defaults. No DOM.
export const prefs = {
  get(key, fallback) { try { const v = localStorage.getItem('pi.' + key); return v === null ? fallback : JSON.parse(v); } catch { return fallback; } },
  set(key, value) { try { localStorage.setItem('pi.' + key, JSON.stringify(value)); } catch {} },
};
