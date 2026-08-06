import { useLibrary } from "./useLibrary";
import { usePreferences } from "./usePreferences";
import { useToast } from "./useToast";
import { useFilterState } from "./useFilterState";
import { useUIState } from "./useUIState";
import { useChromeSession } from "./useChromeSession";
import { useSyncState } from "./useSyncState";
import { useSiteActions } from "./useSiteActions";
import { useCharityMonitor } from "./useCharityMonitor";
import { useProxyPool } from "./useProxyPool";

export function useStore() {
  const { sites, suggestedTags, loading, loadLibrary } = useLibrary();
  const { preferences, updatePreferences } = usePreferences();
  const { showToast } = useToast();

  const filter = useFilterState();
  const ui = useUIState();
  const chrome = useChromeSession();
  const sync = useSyncState();
  const actions = useSiteActions();
  const charity = useCharityMonitor();
  const proxy = useProxyPool();

  return {
    // 数据
    sites,
    suggestedTags,
    loading,
    preferences,
    // 过滤状态
    ...filter,
    // UI 状态
    ...ui,
    // Chrome 会话
    ...chrome,
    // 同步逻辑
    ...sync,
    // 站点操作
    ...actions,
    // 公益推广监听
    ...charity,
    // Clash 代理池
    ...proxy,
    // 通用
    loadLibrary,
    updatePreferences,
    showToast,
  };
}
