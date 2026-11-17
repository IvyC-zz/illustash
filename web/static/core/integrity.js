// 数据库检查（#212）：打开后的后台检查、设置里的「立即检查」，以及检查发现的损坏。之后服务拒绝一切写入，并说明该
// 怎么办。state.integrity 即 GET /api/integrity：running 为 {database, done} 或 null，checked 为 {file: 秒数或 null}，
// damaged 为 {database, message} 或 null，error，以及 rescue 为 {running 为 {stage, done} 或 null, ready, error}：
// 在后台重建损坏的 source.sqlite（#228）。
import {session, api, post} from './session.js';
import {state} from './state.js';
import {listeners} from './events.js';
import {poller} from './poll.js';
const changed = listeners();
// 每个被采用的回应之后调用 fn()。
export const onIntegrity = changed.on;
// 几次轮询会重叠（启动、设置卡片、立即检查之后）：只有最新的回应算数（core/poll.js）。
export const pollIntegrity = poller(async fresh => {
  const token = session.token;
  let answer; try { answer = await api('/api/integrity'); } catch { return fresh() && token === session.token ? 10000 : 0; }
  // 在页面采用会话 token 之前发出的请求（启动时打开的设置页）：回应属于这个会话。
  if (!fresh() || (token && token !== session.token)) return 0;
  state.integrity = answer;
  await changed.emit();
  return state.integrity.running ? 2000 : state.integrity.rescue?.running ? 1000 : 0;
});
export async function checkNow() {
  state.integrity = await post('/api/integrity', {});
  await changed.emit();
  pollIntegrity();
}
// 「抢救重建」（#228）：新的 source.sqlite 构建期间会话保持停止；ready 表示可以把它换上了。
export async function rescueNow() {
  state.integrity = await post('/api/integrity/rescue', {});
  await changed.emit();
  pollIntegrity();
}
