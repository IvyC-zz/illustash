// Database checks (#212): the background check after opening, 「立即检查」 in settings, and damage a check found. The
// service then refuses every write and says what to do. state.integrity is GET /api/integrity: running {database,
// done} or null, checked {file: seconds or null}, damaged {database, message} or null, error.
import {session, api, post} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
// fn() after each answer that was applied.
export const onIntegrity = changed.on;
let timer, latest = 0;
export async function pollIntegrity() {
  clearTimeout(timer);
  // Polls overlap (boot, the settings card, after 立即检查): only the latest answer counts.
  const poll = ++latest, token = session.token;
  try {
    const answer = await api('/api/integrity');
    if (poll !== latest) return;
    // Asked before the page adopted its session token (the settings page opened at boot): the answer is this session's.
    if (token && token !== session.token) return;
    state.integrity = answer;
  } catch { if (poll === latest && token === session.token) timer = setTimeout(pollIntegrity, 10000); return; }
  await changed.emit();
  if (state.integrity.running) timer = setTimeout(pollIntegrity, 2000);
}
export async function checkNow() {
  state.integrity = await post('/api/integrity', {});
  await changed.emit();
  pollIntegrity();
}
