// 自己排下一次的轮询（#432 ③-5）。几次轮询会重叠（任务面板、定时器、启动之后、某个操作之后各叫一次）：每次调用都先
// 取消排着的下一次、领一个新序号；更早发出的读取最后才回来时 fresh() 为假，它的回应丢掉，不让状态倒退，也不再排下一次。
// 不碰 DOM。
//   step(fresh)  读一次并采用回应。每个 await 之后先问 fresh()，为假就什么也别写、返回 0。返回多久后再读（毫秒），0 不再读。
//   retry        step 抛出时多久后再试（毫秒），默认 10 s；0 不再试。要按情况区分的，在 step 里自己接住错误、返回间隔。
// 返回的函数就是「现在读一次」（返回 Promise，读完、采用完才结束）；它的 later(ms) 是「ms 后再读」，替换排着的那次。
export function poller(step, {retry = 10000} = {}) {
  let timer = 0, seq = 0;
  const later = ms => { clearTimeout(timer); timer = ms ? setTimeout(run, ms) : 0; };
  async function run() {
    clearTimeout(timer);
    const mine = ++seq, fresh = () => mine === seq;
    let next;
    try { next = await step(fresh); } catch { if (fresh()) later(retry); return; }
    if (fresh()) later(next);
  }
  run.later = later;
  return run;
}
