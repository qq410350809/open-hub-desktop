/** Run a small shared queue; stop requests prevent new work, while active tasks finish. */
export async function runModelSyncQueue<T>(
  items: readonly T[],
  run: (item: T, index: number) => Promise<void>,
  shouldStop: () => boolean,
  concurrency = 3,
): Promise<void> {
  let next = 0;
  const worker = async () => {
    while (!shouldStop() && next < items.length) {
      const index = next++;
      await run(items[index], index);
    }
  };
  await Promise.all(Array.from({ length: Math.min(Math.max(1, concurrency), items.length) }, worker));
}

/** The backend bounds network work; this also bounds a lost IPC/RPC response. */
export async function withModelSyncDeadline<T>(
  task: Promise<T>,
  timeoutMs = 75_000,
): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      task,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error("同步响应超时，请重试；已有模型缓存已保留")), timeoutMs);
      }),
    ]);
  } finally {
    if (timer !== undefined) clearTimeout(timer);
  }
}
