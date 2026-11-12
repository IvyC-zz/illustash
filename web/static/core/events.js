// core 模块发出的通知的监听器（#199）：on(fn) 添加一个，返回去掉它的函数。emit() 按添加顺序调用它们：其中一个抛错就
// 不再调用其余的，和直接调用一样；返回 promise 的都落定后它才解析，失败的记进日志。
export function listeners() {
  const set = new Set();
  return {
    on: fn => { set.add(fn); return () => set.delete(fn); },
    emit: async (...args) => {
      for (const result of await Promise.allSettled([...set].map(fn => fn(...args)))) if (result.status === 'rejected') console.error(result.reason);
    },
  };
}
