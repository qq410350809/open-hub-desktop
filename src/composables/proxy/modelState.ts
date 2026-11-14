import type { ChannelModelFetchError } from "./types";

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown> : undefined;
}
function text(value: unknown): string { return typeof value === "string" ? value.trim() : ""; }

/** Normalize current tuple replies and older flat/snake_case caches at the IPC boundary. */
export function parseModelSyncResult(value: unknown): {
  models: Record<string, string[]>;
  errors: ChannelModelFetchError[];
} {
  if (!Array.isArray(value)) throw new Error("模型列表响应格式无效");
  const tuple = Array.isArray(value[0]);
  const items: unknown[] = tuple ? value[0] : value;
  const failures: unknown[] = tuple && Array.isArray(value[1]) ? value[1] : [];
  const models: Record<string, string[]> = Object.create(null);
  for (const item of items) {
    const row = record(item);
    if (!row) continue;
    const id = text(row.channelId ?? row.channel_id);
    if (!id || !Array.isArray(row.models)) continue;
    models[id] = [...new Set(row.models.map(text).filter(Boolean))];
  }
  const errors: ChannelModelFetchError[] = [];
  for (const failure of failures) {
    const row = record(failure);
    if (!row) continue;
    const channelId = text(row.channelId ?? row.channel_id);
    if (!channelId) continue;
    errors.push({ channelId, channelName: text(row.channelName ?? row.channel_name),
      alias: text(row.alias), error: text(row.error) || "上游模型拉取失败" });
  }
  return { models, errors };
}

/** Superseded reads may finish, but only the latest read can commit state or clear loading. */
export function createLatestLoader() {
  let revision = 0;
  return async <T>(load: () => Promise<T>, commit: (value: T) => void, pending?: (value: boolean) => void) => {
    const request = ++revision;
    pending?.(true);
    try {
      const value = await load();
      if (request === revision) commit(value);
    } catch (error) {
      if (request === revision) throw error;
    } finally {
      if (request === revision) pending?.(false);
    }
  };
}

export const proxyLogSortColumns = {
  timestamp: "timestamp", status: "status_code", tokens: "total_tokens", duration: "duration_ms",
} as const;
