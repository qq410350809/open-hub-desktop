/** A view owns one loader. Only its latest read, while the view still matches, may commit. */
export function createScopedLoader() {
  let revision = 0;
  return {
    invalidate() { revision++; },
    async load<T>(
      read: () => Promise<T>,
      current: () => boolean,
      commit: (value: T) => void,
      pending?: (value: boolean) => void,
    ): Promise<boolean> {
      const request = ++revision;
      const active = () => request === revision && current();
      if (!current()) return false;
      pending?.(true);
      try {
        const value = await read();
        if (!active()) return false;
        commit(value);
        return true;
      } catch (error) {
        if (active()) throw error;
        return false;
      } finally {
        if (active()) pending?.(false);
      }
    },
  };
}
