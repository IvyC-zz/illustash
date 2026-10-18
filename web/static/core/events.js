// Listeners for what a core module announces (#199): on(fn) adds one and returns the function that removes it. emit()
// calls them in the order they were added: one that throws stops the rest, as a direct call would; it resolves once
// those that return a promise have settled, and logs any that failed.
export function listeners() {
  const set = new Set();
  return {
    on: fn => { set.add(fn); return () => set.delete(fn); },
    emit: async (...args) => {
      for (const result of await Promise.allSettled([...set].map(fn => fn(...args)))) if (result.status === 'rejected') console.error(result.reason);
    },
  };
}
