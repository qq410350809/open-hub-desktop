import type { SiteModelHealth, SiteModelItem } from "../../types";

export interface SiteModelsResult {
  models: SiteModelItem[];
  source: string;
  keys: string[];
  keyGroups?: Record<string, string>;
  keyModels?: Record<string, SiteModelItem[]>;
  modelHealth?: Record<string, SiteModelHealth>;
  errors?: string[];
  profileId?: string;
}

export interface SyncAccount {
  profileId: string;
  profileName: string;
  accountName: string;
  username: string;
}

export interface AccountKeySyncOutcome {
  status: "success" | "partial" | "error";
  keyCount: number;
  modelCount: number;
  error: string;
}

type Command = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
type SyncSite = { id: string; apiBaseUrl: string };

/** One account is the unit of replacement. Sync never deletes another account's cache. */
export function createSiteKeySync(command: Command) {
  const pending = new Map<string, Promise<AccountKeySyncOutcome>>();

  async function sync(site: SyncSite, account?: SyncAccount): Promise<AccountKeySyncOutcome> {
    let result: SiteModelsResult | null = null;
    let error = "";
    try {
      result = await command<SiteModelsResult>("fetch_site_models_json", {
        url: site.apiBaseUrl.trim(), siteId: site.id,
        ...(account?.profileId ? { profileId: account.profileId } : {}),
      });
      if (account && result.profileId && result.profileId !== account.profileId) {
        throw new Error("返回数据的账号与请求账号不一致，已丢弃本次结果");
      }
      error = result.errors?.filter(Boolean).join("\n") || "";
      if (!result.keys?.length) error ||= "同步未返回 Key，已保留原有缓存";
    } catch (cause) {
      result = null;
      error = cause instanceof Error ? cause.message : String(cause);
    }

    // Without an explicit account, only the backend-confirmed owner may receive data.
    const profileId = account?.profileId || result?.profileId;
    if (!profileId) {
      return { status: "error", keyCount: 0, modelCount: 0,
        error: error || "未取得 Key 的账号归属，已保留原有缓存" };
    }
    const hasKeys = Boolean(result?.keys?.length);
    await command("save_site_model_cache_for_account", {
      siteId: site.id,
      account: {
        profileId, profileName: account?.profileName ?? "", accountName: account?.accountName ?? "",
        username: account?.username ?? "", keys: result?.keys ?? [], keyGroups: result?.keyGroups ?? {},
        keyModels: result?.keyModels ?? {}, error,
      },
      // Pass health independently even when key discovery failed; preserveKeys protects data.
      result, preserveKeys: !hasKeys,
    });
    return { status: !hasKeys ? "error" : error ? "partial" : "success",
      keyCount: result?.keys?.length ?? 0, modelCount: result?.models?.length ?? 0, error };
  }

  return (site: SyncSite, account?: SyncAccount): Promise<AccountKeySyncOutcome> => {
    const id = JSON.stringify([site.id, site.apiBaseUrl.trim(), account?.profileId ?? ""]);
    const existing = pending.get(id);
    if (existing) return existing;
    const task = sync(site, account).finally(() => { if (pending.get(id) === task) pending.delete(id); });
    pending.set(id, task);
    return task;
  };
}
