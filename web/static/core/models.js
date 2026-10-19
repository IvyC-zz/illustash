// The recognition models an installer without them downloads on first use (#168, native/terms/src/app_models.rs).
// App-wide like the update check: the same state in every library and window, and a download goes on across a
// library switch. No DOM. onModels(before) tells after each read or change, with the state before it.
import {api} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
const changed = listeners();
export const onModels = changed.on;

let timer = 0, reading = null;
const adopt = async value => {
  const before = state.models;
  state.models = value;
  // While a download or import runs, its progress is read every second; it ends on its own.
  clearTimeout(timer);
  if (value?.job?.status === 'running') timer = setTimeout(() => loadModels().catch(() => {}), 1000);
  await changed.emit(before);
  return value;
};
export function loadModels() {
  reading ||= api('/api/models').then(adopt).finally(() => { reading = null; });
  return reading;
}
// download, cancel, discard, mirror; import without paths opens the native chooser first ({cancelled} if closed).
export async function modelsPost(body) {
  const value = await api('/api/models', {method: 'POST', body: JSON.stringify(body)});
  return value.cancelled ? value : adopt(value);
}
// This install downloads its models and they are not in place yet: recognition waits.
export const modelsWaiting = (m = state.models) => !!m?.on_demand && m.status !== 'ready';
// A job that has just finished, and how: once, when the state read says it stopped running.
export const modelsFinished = (before, after = state.models) => before?.job?.status === 'running' && after?.job?.status !== 'running' ? after.job : null;
