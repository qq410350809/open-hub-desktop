import { useLibrary } from "../site/useLibrary";
import { usePreferences } from "./usePreferences";
import { useToast } from "./useToast";
import { useFilterState } from "../site/useFilterState";
import { useUIState } from "../ui/useUIState";
import { useChromeSession } from "../site/useChromeSession";
import { useSyncState } from "../site/useSyncState";
import { useSiteActions } from "../site/useSiteActions";
import { useCharityMonitor } from "../charity/useCharityMonitor";
import { useProxyPool } from "../proxy/useProxyPool";
import { useTokenStats } from "../token/useTokenStats";
import { useModelCatalog } from "../model/useModelCatalog";

export function useStore() {
  const { sites, suggestedTags, loading, loadLibrary, startDailyRefresh, stopDailyRefresh } = useLibrary();
  const { preferences, updatePreferences } = usePreferences();
  const { showToast } = useToast();

  const filter = useFilterState();
  const ui = useUIState();
  const chrome = useChromeSession();
  const sync = useSyncState();
  const actions = useSiteActions();
  const charity = useCharityMonitor();
  const proxy = useProxyPool();
  const tokenStats = useTokenStats();
  const modelCatalog = useModelCatalog();

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
    // Token 统计
    ...tokenStats,
    // 模型参数
    ...modelCatalog,
    // 通用
    loadLibrary,
    startDailyRefresh,
    stopDailyRefresh,
    updatePreferences,
    showToast,
  };
}
