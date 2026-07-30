import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import "./styles.css";

interface Maintainer {
  name: string;
  id: string;
  username: string;
  profileUrl: string;
}

interface ExtensionLink {
  label: string;
  url: string;
}

type SiteLinkKind = "api" | "checkin" | "benefit" | "status" | "extension";

interface AddressItem {
  label: string;
  url: string;
  note?: string;
}

interface SiteRecord {
  id: string;
  name: string;
  description: string;
  registrationLimit: number;
  icon: string;
  apiBaseUrl: string;
  tags: string[];
  supportsImmersiveTranslation: boolean;
  supportsLdc: boolean;
  supportsCheckin: boolean;
  supportsNsfw: boolean;
  checkinUrl: string;
  checkinNote: string;
  benefitUrl: string;
  maintainers: Maintainer[];
  rateLimit: string;
  statusUrl: string;
  extensionLinks: ExtensionLink[];
  isOnlyMaintainerVisible: boolean;
  requiresInviteCode: boolean;
  isRunaway: boolean;
  isFakeCharity: boolean;
  hasPendingReport: boolean;
  isPersonal: boolean;
  favorite: boolean;
  hidden: boolean;
  updatedAt: string;
}

interface LibraryData {
  sites: SiteRecord[];
  suggestedTags: string[];
}

const icon = (content: string, className = "") => `<svg class="${className}" viewBox="0 0 24 24" aria-hidden="true">${content}</svg>`;
const icons = {
  search: icon('<circle cx="11" cy="11" r="7"></circle><path d="m20 20-3.6-3.6"></path>'),
  plus: icon('<path d="M12 5v14M5 12h14"></path>'),
  chevron: icon('<path d="m7 9 5 5 5-5"></path>'),
  eye: icon('<path d="M2.5 12s3.5-6 9.5-6 9.5 6 9.5 6-3.5 6-9.5 6-9.5-6-9.5-6Z"></path><circle cx="12" cy="12" r="2.6"></circle>'),
  eyeOff: icon('<path d="m3 3 18 18M10.6 6.2A10.6 10.6 0 0 1 12 6c6 0 9.5 6 9.5 6a17 17 0 0 1-2.2 2.9M6.2 6.2C3.8 8 2.5 12 2.5 12s3.5 6 9.5 6c1.5 0 2.8-.4 4-1"></path>'),
  grid: icon('<rect x="4" y="4" width="6" height="6" rx="1"></rect><rect x="14" y="4" width="6" height="6" rx="1"></rect><rect x="4" y="14" width="6" height="6" rx="1"></rect><rect x="14" y="14" width="6" height="6" rx="1"></rect>'),
  rows: icon('<path d="M8 6h12M8 12h12M8 18h12"></path><circle cx="4" cy="6" r="1"></circle><circle cx="4" cy="12" r="1"></circle><circle cx="4" cy="18" r="1"></circle>'),
  edit: icon('<path d="m14 5 5 5M4 20l3.5-.7L19 7.8a2 2 0 0 0-2.8-2.8L4.7 16.5 4 20Z"></path>'),
  external: icon('<path d="M14 4h6v6M20 4l-9 9"></path><path d="M18 13v6a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h6"></path>'),
  star: icon('<path d="m12 3 2.8 5.7 6.2.9-4.5 4.4 1.1 6.2-5.6-3-5.6 3 1.1-6.2L3 9.6l6.2-.9L12 3Z"></path>'),
  copy: icon('<rect x="9" y="9" width="10" height="10" rx="2"></rect><path d="M15 9V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v7a2 2 0 0 0 2 2h3"></path>'),
  calendar: icon('<rect x="3" y="5" width="18" height="16" rx="2"></rect><path d="M8 3v4M16 3v4M3 10h18M9 15l2 2 4-4"></path>'),
  gift: icon('<rect x="3" y="9" width="18" height="12" rx="2"></rect><path d="M12 9v12M3 13h18M12 9H7.5A2.5 2.5 0 1 1 10 6.5L12 9Zm0 0h4.5A2.5 2.5 0 1 0 14 6.5L12 9Z"></path>'),
  pulse: icon('<path d="M3 12h4l2-7 4 14 2-7h6"></path>'),
  more: icon('<circle cx="5" cy="12" r="1"></circle><circle cx="12" cy="12" r="1"></circle><circle cx="19" cy="12" r="1"></circle>'),
  translate: icon('<path d="M4 5h10M9 3v2m-4 8c3-2 5-5 6-8m-5 3c1 2 3 4 6 5M14 21l4-10 4 10m-6.5-4h5"></path>'),
  card: icon('<rect x="3" y="5" width="18" height="14" rx="2"></rect><path d="M3 10h18M7 15h4"></path>'),
  close: icon('<path d="m6 6 12 12M18 6 6 18"></path>'),
  info: icon('<circle cx="12" cy="12" r="9"></circle><path d="M12 11v6M12 7h.01"></path>'),
  settings: icon('<circle cx="12" cy="12" r="3"></circle><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H3v-4h.1a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1A1.7 1.7 0 0 0 9 4.6a1.7 1.7 0 0 0 1-1.6V3h4v.1a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z"></path>'),
  users: icon('<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8ZM22 21v-2a4 4 0 0 0-3-3.9M16 3.1a4 4 0 0 1 0 7.8"></path>'),
  link: icon('<path d="M10 13a5 5 0 0 0 7.1.1l2-2a5 5 0 0 0-7.1-7.1l-1.1 1.1M14 11a5 5 0 0 0-7.1-.1l-2 2A5 5 0 0 0 12 20l1.1-1.1"></path>'),
  trash: icon('<path d="M4 7h16M9 7V4h6v3M7 7l1 13h8l1-13M10 11v5M14 11v5"></path>'),
  flag: icon('<path d="M5 21V4m0 1h10l-1.5 3L15 11H5"></path>'),
  restore: icon('<path d="M4 9V4m0 0h5M4.5 4.5A9 9 0 1 1 3 13"></path>'),
  sidebarClose: icon('<path d="M15 4h4v16h-4M11 8l-4 4 4 4"></path>'),
  sidebarOpen: icon('<path d="M9 4H5v16h4m4-12 4 4-4 4"></path>'),
  monitor: icon('<rect x="3" y="4" width="18" height="13" rx="2"></rect><path d="M8 21h8M12 17v4"></path>'),
  database: icon('<ellipse cx="12" cy="5" rx="8" ry="3"></ellipse><path d="M4 5v7c0 1.7 3.6 3 8 3s8-1.3 8-3V5M4 12v7c0 1.7 3.6 3 8 3s8-1.3 8-3v-7"></path>'),
};

const isTauri = "__TAURI_INTERNALS__" in window;

type ThemePreference = "system" | "light" | "dark";
interface AppPreferences {
  theme: ThemePreference;
  defaultStatus: "active" | "runaway";
  defaultView: "cards" | "list";
  showHiddenOnStartup: boolean;
  sidebarCollapsed: boolean;
}

const PREFERENCES_KEY = "ldoh:preferences";
const defaultPreferences: AppPreferences = {
  theme: "system",
  defaultStatus: "active",
  defaultView: "cards",
  showHiddenOnStartup: false,
  sidebarCollapsed: false,
};

function loadPreferences(): AppPreferences {
  try {
    const saved = JSON.parse(localStorage.getItem(PREFERENCES_KEY) ?? "{}") as Partial<AppPreferences>;
    const legacyTheme = localStorage.getItem("ldoh:theme");
    return {
      theme: ["system", "light", "dark"].includes(String(saved.theme)) ? saved.theme! : legacyTheme === "dark" ? "dark" : legacyTheme === "light" ? "light" : defaultPreferences.theme,
      defaultStatus: saved.defaultStatus === "runaway" ? "runaway" : "active",
      defaultView: saved.defaultView === "list" ? "list" : "cards",
      showHiddenOnStartup: Boolean(saved.showHiddenOnStartup),
      sidebarCollapsed: Boolean(saved.sidebarCollapsed),
    };
  } catch {
    return { ...defaultPreferences };
  }
}

let preferences = loadPreferences();

function savePreferences() {
  localStorage.setItem(PREFERENCES_KEY, JSON.stringify(preferences));
}

const state = {
  sites: [] as SiteRecord[],
  suggestedTags: [] as string[],
  query: "",
  tag: "all",
  level: "all",
  feature: "all",
  status: preferences.defaultStatus as "active" | "runaway",
  showHidden: preferences.showHiddenOnStartup,
  compact: preferences.defaultView === "list",
  page: "library" as "library" | "settings",
  editingId: null as string | null,
  activeTab: "basic" as "basic" | "features" | "maintenance",
};
let browserData: LibraryData | null = null;

const emptySite = (): SiteRecord => ({
  id: "",
  name: "",
  description: "",
  registrationLimit: 0,
  icon: "",
  apiBaseUrl: "",
  tags: [],
  supportsImmersiveTranslation: false,
  supportsLdc: false,
  supportsCheckin: false,
  supportsNsfw: false,
  checkinUrl: "",
  checkinNote: "",
  benefitUrl: "",
  maintainers: [],
  rateLimit: "",
  statusUrl: "",
  extensionLinks: [],
  isOnlyMaintainerVisible: false,
  requiresInviteCode: false,
  isRunaway: false,
  isFakeCharity: false,
  hasPendingReport: false,
  isPersonal: false,
  favorite: false,
  hidden: false,
  updatedAt: "",
});

async function runCommand<T>(command: string, args: Record<string, unknown> = {}): Promise<T> {
  if (isTauri) return invoke<T>(command, args);
  if (!browserData) {
    const response = await fetch("/sites.json");
    const raw = await response.json() as { sites: SiteRecord[]; tags: string[] };
    browserData = { sites: raw.sites.map((site) => ({ ...emptySite(), ...site })), suggestedTags: raw.tags };
  }
  if (command === "list_library") return browserData as T;
  if (command === "create_site") {
    const site = { ...(args.input as SiteRecord), id: `local-${Date.now()}`, updatedAt: new Date().toISOString() };
    browserData.sites.unshift(site);
    return site as T;
  }
  if (command === "update_site") {
    const id = String(args.id);
    const index = browserData.sites.findIndex((site) => site.id === id);
    browserData.sites[index] = { ...(args.input as SiteRecord), id, updatedAt: new Date().toISOString() };
    return browserData.sites[index] as T;
  }
  if (command === "delete_site") {
    browserData.sites = browserData.sites.filter((site) => site.id !== args.id);
    return undefined as T;
  }
  if (command === "toggle_favorite" || command === "toggle_hidden") {
    const site = browserData.sites.find((item) => item.id === args.id)!;
    if (command === "toggle_favorite") site.favorite = !site.favorite;
    else site.hidden = !site.hidden;
    return site as T;
  }
  if (command === "toggle_runaway") {
    const site = browserData.sites.find((item) => item.id === args.id)!;
    site.isRunaway = !site.isRunaway;
    site.updatedAt = new Date().toISOString();
    return site as T;
  }
  throw new Error(`Unsupported command: ${command}`);
}

const root = document.querySelector<HTMLDivElement>("#app")!;
root.innerHTML = `
  <div class="app-layout">
    <aside class="app-sidebar">
      <div class="brand">
        <img src="/icon.png" width="40" height="40" alt="" />
        <span><strong>OpenHub</strong><small id="library-count">本地站点资料库</small></span>
      </div>

      <section class="toolbar" aria-label="站点筛选">
        <div class="toolbar-heading"><span>站点筛选</span><i></i></div>
        <div class="status-segment" role="group" aria-label="站点状态">
          <button class="status-option active" id="status-active" type="button"><i></i><span>存活</span><b id="active-count">0</b></button>
          <button class="status-option" id="status-runaway" type="button"><i></i><span>跑路</span><b id="runaway-count">0</b></button>
          <button class="status-option" id="status-personal" type="button"><i></i><span>自用</span><b id="personal-count">0</b></button>
        </div>
        <div class="select-box tag-select" data-custom-select><select id="tag-filter" tabindex="-1" aria-hidden="true"><option value="all">全部标签</option></select><button class="select-trigger" type="button" aria-haspopup="listbox" aria-expanded="false"><span>全部标签</span>${icons.chevron}</button><div class="select-menu" role="listbox" hidden></div></div>
        <div class="select-box" data-custom-select><select id="level-filter" tabindex="-1" aria-hidden="true"><option value="all">全部等级</option><option value="0">LV0</option><option value="1">LV1</option><option value="2">LV2</option><option value="3">LV3</option></select><button class="select-trigger" type="button" aria-haspopup="listbox" aria-expanded="false"><span>全部等级</span>${icons.chevron}</button><div class="select-menu" role="listbox" hidden></div></div>
        <div class="select-box" data-custom-select><select id="feature-filter" tabindex="-1" aria-hidden="true"><option value="all">全部功能</option><option value="checkin">支持签到</option><option value="translation">沉浸式翻译</option><option value="ldc">支持 LDC</option><option value="nsfw">支持 NSFW</option><option value="invite">需要邀请码</option></select><button class="select-trigger" type="button" aria-haspopup="listbox" aria-expanded="false"><span>全部功能</span>${icons.chevron}</button><div class="select-menu" role="listbox" hidden></div></div>
        <div class="toolbar-actions">
          <button class="tool-button" id="hidden-toggle" title="显示隐藏站点">${icons.eye}</button>
          <button class="tool-button" id="view-toggle" title="切换卡片与列表视图">${icons.rows}</button>
        </div>
      </section>

      <div class="sidebar-footer">
        <button class="icon-button sidebar-collapse" id="sidebar-collapse" type="button" aria-label="收起侧栏">${icons.sidebarClose}</button>
        <button class="icon-button" id="settings-toggle" aria-label="打开设置">${icons.settings}</button>
      </div>
    </aside>

    <div class="app-workspace">
      <header class="app-header">
        <div class="header-inner">
          <label class="search-box">${icons.search}<input id="search-input" type="search" placeholder="搜索站点、API 地址或标签…" autocomplete="off" /><kbd>⌘ K</kbd></label>
          <div class="header-actions">
            <button class="primary-button" id="add-site">${icons.plus}<span>添加站点</span></button>
          </div>
        </div>
      </header>

      <main class="main-content">
        <div class="result-bar"><span id="result-count">正在读取本地资料库…</span><button id="clear-filter" hidden>清除筛选</button></div>
        <section class="site-grid" id="site-grid"></section>
        <section class="empty-state" id="empty-state" hidden>
          <div>${icons.search}</div><h2>没有匹配的站点</h2><p>尝试修改搜索词或清除筛选条件。</p><button class="secondary-button" id="empty-clear">清除筛选</button>
        </section>
      </main>

      <section class="settings-page" id="settings-page" hidden>
        <div class="settings-dialog" role="dialog" aria-modal="true" aria-labelledby="settings-title">
        <header class="settings-header">
          <div><h1 id="settings-title">设置</h1><p>应用偏好与本地数据</p></div>
          <button class="close-button" id="close-settings" type="button" aria-label="关闭设置">${icons.close}</button>
        </header>
        <div class="settings-scroll">
          <div class="settings-content">
            <section class="settings-section">
              <div class="settings-section-title"><span>${icons.monitor}</span><div><h2>外观</h2><p>界面主题与显示方式</p></div></div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div><strong>主题模式</strong><small>跟随系统会随 macOS 外观自动切换</small></div>
                  <div class="preference-segment" id="theme-preference" role="group" aria-label="主题模式">
                    <button type="button" data-theme-choice="system">跟随系统</button><button type="button" data-theme-choice="light">明亮</button><button type="button" data-theme-choice="dark">暗黑</button>
                  </div>
                </div>
                <div class="settings-row">
                  <div><strong>默认视图</strong><small>设置启动时使用的站点布局</small></div>
                  <div class="preference-segment" id="view-preference" role="group" aria-label="默认视图">
                    <button type="button" data-view-choice="cards">${icons.grid}<span>卡片</span></button><button type="button" data-view-choice="list">${icons.rows}<span>列表</span></button>
                  </div>
                </div>
              </div>
            </section>

            <section class="settings-section">
              <div class="settings-section-title"><span>${icons.settings}</span><div><h2>浏览偏好</h2><p>启动状态与可见范围</p></div></div>
              <div class="settings-rows">
                <div class="settings-row">
                  <div><strong>默认站点状态</strong><small>打开应用时首先显示的列表</small></div>
                  <div class="preference-segment" id="status-preference" role="group" aria-label="默认站点状态">
                    <button type="button" data-status-choice="active">存活</button><button type="button" data-status-choice="runaway">跑路</button><button type="button" data-status-choice="personal">自用</button>
                  </div>
                </div>
                <label class="settings-row settings-toggle-row">
                  <div><strong>启动时显示隐藏站点</strong><small>隐藏记录仍会保留在本地数据库</small></div>
                  <input id="startup-hidden" type="checkbox" /><i aria-hidden="true"></i>
                </label>
              </div>
            </section>

            <section class="settings-section">
              <div class="settings-section-title"><span>${icons.database}</span><div><h2>本地数据</h2><p>SQLite 资料库统计</p></div></div>
              <dl class="data-summary">
                <div><dt>全部站点</dt><dd id="settings-total">0</dd></div>
                <div><dt>在用</dt><dd id="settings-active">0</dd></div>
                <div><dt>跑路</dt><dd id="settings-runaway">0</dd></div>
                <div><dt>收藏</dt><dd id="settings-favorite">0</dd></div>
                <div><dt>隐藏</dt><dd id="settings-hidden">0</dd></div>
              </dl>
            </section>

            <section class="settings-section settings-about">
              <div class="settings-section-title"><span>${icons.info}</span><div><h2>关于</h2><p>OpenHub</p></div></div>
              <div class="about-line"><span>版本</span><strong>0.3.0</strong></div>
            </section>
          </div>
        </div>
        </div>
      </section>
    </div>
  </div>

  <div class="modal-backdrop" id="site-modal" hidden>
    <section class="site-modal" role="dialog" aria-modal="true" aria-labelledby="modal-title">
      <header class="modal-header">
        <div><h2 id="modal-title">新增站点</h2><p>帮助完善站点信息，带 * 为必填项</p></div>
        <button class="close-button" type="button" data-close-modal>${icons.close}</button>
      </header>
      <nav class="modal-tabs">
        <button class="tab-button active" type="button" data-tab="basic">${icons.info}<span>基础信息</span></button>
        <button class="tab-button" type="button" data-tab="features">${icons.settings}<span>功能配置</span></button>
        <button class="tab-button" type="button" data-tab="maintenance">${icons.users}<span>维护与扩展</span></button>
      </nav>
      <form id="site-form">
        <div class="modal-scroll">
          <section class="tab-panel active" data-panel="basic">
            <div class="form-grid two-cols">
              <label class="field"><span>站点名称 <b>*</b></span><input name="name" required maxlength="100" placeholder="例如：My AI Service" /></label>
              <label class="field"><span>API BASE URL <b>*</b></span><input name="apiBaseUrl" type="url" required placeholder="https://api.example.com" /></label>
              <label class="field field-wide"><span>站点描述</span><textarea name="description" rows="4" maxlength="800" placeholder="简要介绍站点的特色…"></textarea></label>
              <label class="field"><span>等级限制（LV）</span><select name="registrationLimit"><option value="0">0</option><option value="1">1</option><option value="2">2</option><option value="3">3</option></select><small>等级限制范围为 0–3</small></label>
              <label class="field"><span>速率限制</span><input name="rateLimit" placeholder="例如：10/min、500/20min、无限制" /></label>
              <label class="check-card field-wide"><input name="requiresInviteCode" type="checkbox" /><i></i><span><strong>注册时是否需要邀请码</strong><small>标记该站点注册时需要邀请码</small></span></label>
              <label class="field field-wide"><span>TAGS（支持的模型/功能）</span><input name="tags" id="tags-input" placeholder="输入标签，用逗号分隔…" /></label>
              <div class="suggested-tags field-wide"><span>推荐标签（点击添加）：</span><div id="suggested-tags"></div></div>
            </div>
          </section>

          <section class="tab-panel" data-panel="features">
            <h3 class="section-title">${icons.settings} 功能开关</h3>
            <div class="feature-switches">
              <label class="check-card"><input name="supportsCheckin" type="checkbox" /><i></i><span><strong>支持签到</strong><small>是否支持每日签到</small></span></label>
              <label class="check-card"><input name="supportsImmersiveTranslation" type="checkbox" /><i></i><span><strong>支持沉浸式翻译</strong><small>是否可用于沉浸式翻译插件</small></span></label>
              <label class="check-card"><input name="supportsLdc" type="checkbox" /><i></i><span><strong>支持 LDC</strong><small>是否支持 Linux Do Credit</small></span></label>
              <label class="check-card"><input name="supportsNsfw" type="checkbox" /><i></i><span><strong>支持 NSFW</strong><small>是否支持 NSFW</small></span></label>
            </div>
            <h3 class="section-title section-spaced">${icons.link} 相关链接</h3>
            <div class="form-grid two-cols">
              <label class="field"><span>签到页 URL</span><input name="checkinUrl" type="url" placeholder="默认 APIBaseUrl + /console/personal" /></label>
              <label class="field"><span>福利站 URL</span><input name="benefitUrl" type="url" placeholder="https://…" /></label>
              <label class="field"><span>签到说明</span><input name="checkinNote" placeholder="例如：每日签到送 10 刀" /></label>
              <label class="field"><span>状态页 URL</span><input name="statusUrl" type="url" placeholder="https://status…" /></label>
            </div>
          </section>

          <section class="tab-panel" data-panel="maintenance">
            <h3 class="section-title">站点状态</h3>
            <label class="check-card"><input name="isPersonal" type="checkbox" /><i></i><span><strong>个人自用</strong><small>自建或非公开的私有站点</small></span></label>
            <label class="check-card runaway-check"><input name="isRunaway" type="checkbox" /><i></i><span><strong>标记为已跑路</strong><small>保存后将站点归入“跑路”分组</small></span></label>
            <div class="section-heading section-spaced"><h3>维护者信息</h3><button class="secondary-button" id="add-maintainer" type="button">${icons.plus} 添加维护者</button></div>
            <div class="dynamic-list" id="maintainer-list"></div>
            <div class="section-heading section-spaced"><h3>更多扩展链接</h3><button class="secondary-button" id="add-extension" type="button">${icons.plus} 添加链接</button></div>
            <div class="dynamic-list" id="extension-list"></div>
          </section>
        </div>
        <p class="form-error" id="form-error"></p>
        <footer class="modal-footer"><button class="secondary-button" type="button" data-close-modal>取消</button><button class="save-button" id="save-site" type="submit">保存</button></footer>
      </form>
    </section>
  </div>

  <div class="link-dialog-backdrop" id="link-dialog" hidden>
    <section class="link-dialog" role="dialog" aria-modal="true" aria-labelledby="link-dialog-title">
      <header class="link-dialog-header">
        <div><h2 id="link-dialog-title">地址列表</h2><p id="link-dialog-subtitle"></p></div>
        <button class="close-button" id="close-link-dialog" type="button" aria-label="关闭地址列表">${icons.close}</button>
      </header>
      <div class="address-list" id="address-list"></div>
    </section>
  </div>

  <div class="preview-dialog-backdrop" id="preview-dialog" hidden>
    <section class="preview-dialog" role="dialog" aria-modal="true" aria-labelledby="preview-title">
      <header class="preview-header">
        <div class="preview-heading" id="preview-heading"></div>
        <button class="close-button" id="close-preview" type="button" aria-label="关闭站点预览">${icons.close}</button>
      </header>
      <div class="preview-scroll" id="preview-content"></div>
    </section>
  </div>

  <div class="ui-tooltip" id="ui-tooltip" role="tooltip" hidden></div>
  <div class="toast" id="toast" role="status"></div>
`;

const $ = <T extends Element>(selector: string) => document.querySelector<T>(selector)!;
const elements = {
  grid: $("#site-grid") as HTMLElement,
  empty: $("#empty-state") as HTMLElement,
  search: $("#search-input") as HTMLInputElement,
  tag: $("#tag-filter") as HTMLSelectElement,
  level: $("#level-filter") as HTMLSelectElement,
  feature: $("#feature-filter") as HTMLSelectElement,
  hidden: $("#hidden-toggle") as HTMLButtonElement,
  view: $("#view-toggle") as HTMLButtonElement,
  clear: $("#clear-filter") as HTMLButtonElement,
  count: $("#result-count") as HTMLElement,
  modal: $("#site-modal") as HTMLElement,
  linkDialog: $("#link-dialog") as HTMLElement,
  linkDialogTitle: $("#link-dialog-title") as HTMLElement,
  linkDialogSubtitle: $("#link-dialog-subtitle") as HTMLElement,
  addressList: $("#address-list") as HTMLElement,
  previewDialog: $("#preview-dialog") as HTMLElement,
  previewHeading: $("#preview-heading") as HTMLElement,
  previewContent: $("#preview-content") as HTMLElement,
  tooltip: $("#ui-tooltip") as HTMLElement,
  form: $("#site-form") as HTMLFormElement,
  error: $("#form-error") as HTMLElement,
  tagsInput: $("#tags-input") as HTMLInputElement,
  suggestedTags: $("#suggested-tags") as HTMLElement,
  maintainerList: $("#maintainer-list") as HTMLElement,
  extensionList: $("#extension-list") as HTMLElement,
  toast: $("#toast") as HTMLElement,
  settings: $("#settings-toggle") as HTMLButtonElement,
  sidebarCollapse: $("#sidebar-collapse") as HTMLButtonElement,
  appLayout: $(".app-layout") as HTMLElement,
  settingsPage: $("#settings-page") as HTMLElement,
  sidebar: $(".app-sidebar") as HTMLElement,
  appHeader: $(".app-header") as HTMLElement,
  mainContent: $(".main-content") as HTMLElement,
  startupHidden: $("#startup-hidden") as HTMLInputElement,
  statusActive: $("#status-active") as HTMLButtonElement,
  statusRunaway: $("#status-runaway") as HTMLButtonElement,
  statusPersonal: $("#status-personal") as HTMLButtonElement,
};

function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character]!);
}

function syncCustomSelect(select: HTMLSelectElement) {
  const root = select.closest<HTMLElement>("[data-custom-select]");
  if (!root) return;
  const trigger = root.querySelector<HTMLButtonElement>(".select-trigger")!;
  const menu = root.querySelector<HTMLElement>(".select-menu")!;
  const options = [...select.options];
  const signature = options.map((option) => `${option.value}\u0000${option.text}`).join("\u0001");

  if (menu.dataset.signature !== signature) {
    menu.innerHTML = options.map((option) => `<button class="select-option" type="button" role="option" data-select-value="${escapeHtml(option.value)}">${escapeHtml(option.text)}</button>`).join("");
    menu.dataset.signature = signature;
  }

  const selected = select.selectedOptions[0] ?? options[0];
  trigger.querySelector("span")!.textContent = selected?.text ?? "";
  menu.querySelectorAll<HTMLButtonElement>(".select-option").forEach((option) => {
    const active = option.dataset.selectValue === select.value;
    option.classList.toggle("selected", active);
    option.setAttribute("aria-selected", String(active));
  });
}

function closeCustomSelects(except?: HTMLElement) {
  document.querySelectorAll<HTMLElement>("[data-custom-select].open").forEach((root) => {
    if (root === except) return;
    root.classList.remove("open");
    root.querySelector<HTMLElement>(".select-menu")!.hidden = true;
    root.querySelector<HTMLButtonElement>(".select-trigger")!.setAttribute("aria-expanded", "false");
  });
}

function openCustomSelect(root: HTMLElement) {
  closeCustomSelects(root);
  root.classList.add("open");
  root.querySelector<HTMLElement>(".select-menu")!.hidden = false;
  root.querySelector<HTMLButtonElement>(".select-trigger")!.setAttribute("aria-expanded", "true");
}

function formatDate(value: string): string {
  if (!value) return "未知";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit" }).format(date).replace(/\//g, "-");
}

function formatRateLimit(value: string): string {
  let formatted = value.trim().replace(/\s+/g, " ");
  if (!formatted) return "";
  const compact = formatted.toLocaleLowerCase().replace(/\s+/g, "");
  if (["unknown", "未知"].includes(compact)) return "";
  if (["0", "∞", "无", "无限制", "不限制", "不限制rpm", "不限", "不限速", "unlimit", "unlimited"].includes(compact)) return "不限速";

  const rateNumber = (raw: string) => {
    const numeric = Number(raw.replace(/,/g, ""));
    return Number.isFinite(numeric) ? new Intl.NumberFormat("zh-CN", { maximumFractionDigits: 2 }).format(numeric) : raw;
  };
  const duration = (amount: string | undefined, unit: string) => {
    const count = amount ? Number(amount) : 1;
    const normalizedUnit = unit.toLocaleLowerCase();
    const label = /^(?:s|sec|secs|second|seconds|秒)$/.test(normalizedUnit) ? "秒"
      : /^(?:h|hr|hrs|hour|hours|时|小时)$/.test(normalizedUnit) ? "小时"
        : /^(?:d|day|days|天)$/.test(normalizedUnit) ? "天" : "分钟";
    return count === 1 ? label : `${rateNumber(String(count))}${label}`;
  };

  formatted = formatted
    .replace(/\brpm\s*(\d[\d,]*(?:\.\d+)?)\b/gi, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(/(\d[\d,]*(?:\.\d+)?)\s*rpm\b/gi, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(/(\d[\d,]*(?:\.\d+)?)\s*(?:次)?\s*\/\s*一分(?:钟)?/g, (_, count: string) => `${rateNumber(count)}次/分钟`)
    .replace(/(\d[\d,]*(?:\.\d+)?)\s*(?:次)?\s*\/\s*(?:(\d+(?:\.\d+)?)\s*)?(seconds?|secs?|sec|s|minutes?|mins?|min|m|hours?|hrs?|hr|h|days?|day|d|秒|分钟|分|小时|时|天)/gi,
      (_, count: string, amount: string | undefined, unit: string) => `${rateNumber(count)}次/${duration(amount, unit)}`)
    .replace(/\bgpt\b/gi, "GPT")
    .replace(/:/g, "：")
    .replace(/(?:默认|翻译)\s*(?=\d[\d,]*(?:\.\d+)?次\/)/g, (label) => `${label.trim()}：`);
  if (/^\d[\d,]*(?:\.\d+)?$/.test(formatted)) return `${rateNumber(formatted)}次/分钟`;
  return formatted;
}

function hostname(url: string): string {
  try { return new URL(url).hostname; } catch { return url; }
}

function logoText(site: SiteRecord): string {
  const host = hostname(site.apiBaseUrl).replace(/^www\./, "");
  return (host.split(".")[0] || site.name).slice(0, 6);
}

function matchesFeature(site: SiteRecord): boolean {
  switch (state.feature) {
    case "checkin": return site.supportsCheckin;
    case "translation": return site.supportsImmersiveTranslation;
    case "ldc": return site.supportsLdc;
    case "nsfw": return site.supportsNsfw;
    case "invite": return site.requiresInviteCode;
    default: return true;
  }
}

function filteredSites(): SiteRecord[] {
  const query = state.query.trim().toLocaleLowerCase("zh-CN");
  return state.sites.filter((site) => {
    if (state.status === "active" && (site.isRunaway || site.isPersonal)) return false;
    if (state.status === "runaway" && (!site.isRunaway || site.isPersonal)) return false;
    if (state.status === "personal" && !site.isPersonal) return false;
    if (!state.showHidden && site.hidden) return false;
    if (state.tag !== "all" && !site.tags.includes(state.tag)) return false;
    if (state.level !== "all" && site.registrationLimit !== Number(state.level)) return false;
    if (!matchesFeature(site)) return false;
    const content = [site.name, site.apiBaseUrl, site.description, site.rateLimit, ...site.tags, ...site.maintainers.map((item) => item.name)].join(" ").toLocaleLowerCase("zh-CN");
    return !query || content.includes(query);
  }).sort((a, b) => Number(b.favorite) - Number(a.favorite) || new Date(b.updatedAt).getTime() - new Date(a.updatedAt).getTime());
}

function featureButton(iconSvg: string, title: string): string {
  return `<button class="round-feature active" type="button" title="${escapeHtml(title)}">${iconSvg}</button>`;
}

function capabilityActions(site: SiteRecord): string {
  const actions = [
    site.supportsImmersiveTranslation ? featureButton(icons.translate, "沉浸式翻译") : "",
    site.supportsLdc ? featureButton(icons.card, "LDC") : "",
    site.supportsNsfw ? '<span class="age-chip active" title="NSFW">18+</span>' : "",
  ].filter(Boolean).join("");
  return actions ? `<div class="capability-actions">${actions}</div>` : "";
}

function configuredLinkButton(iconSvg: string, title: string, details: string[], kind: SiteLinkKind, id: string): string {
  const configured = details.map((detail) => detail.trim()).filter(Boolean);
  if (!configured.length) return "";
  const tooltip = [title, ...configured].join("\n");
  return `<button class="round-feature link-feature active" type="button" aria-label="${escapeHtml(title)}" data-tooltip="${escapeHtml(tooltip)}" data-link-list="${kind}" data-site-id="${escapeHtml(id)}">${iconSvg}</button>`;
}

function runawayActionButton(site: SiteRecord): string {
  const title = site.isRunaway ? "恢复在用" : "标记为跑路";
  return `<button class="runaway-toggle ${site.isRunaway ? "is-runaway" : ""}" type="button" data-runaway="${escapeHtml(site.id)}" title="${title}" aria-label="${title}">${site.isRunaway ? icons.restore : icons.flag}</button>`;
}

function tagList(site: SiteRecord): string {
  const validTags = site.tags.filter((t) => t.trim().toUpperCase() !== "UNKNOWN" && t.trim() !== "未知");
  const personalTag = site.isPersonal ? `<span class="tag-chip tag-personal">自用</span>` : "";
  return `<div class="tag-list">${personalTag}${validTags.map((tag) => `<span class="tag-chip">${escapeHtml(tag)}</span>`).join("")}<span class="tag-overflow" tabindex="0" aria-label="查看隐藏标签" hidden></span></div>`;
}

function siteCard(site: SiteRecord): string {
  const extensionDetails = site.extensionLinks.filter((link) => link.url.trim()).map((link) => `${link.label.trim() || "扩展链接"}：${link.url.trim()}`);
  const rateLimit = formatRateLimit(site.rateLimit);
  return `
    <article class="site-card ${site.hidden ? "is-hidden" : ""} ${site.isRunaway ? "is-runaway" : ""} ${site.isPersonal ? "is-personal" : ""}" data-id="${escapeHtml(site.id)}">
      <div class="card-top">
        <div class="site-avatar">${escapeHtml(logoText(site))}</div>
        <div class="site-main">
          <div class="title-row">
            <h2 title="${escapeHtml(site.name)}">${escapeHtml(site.name)}</h2>
            <div class="card-actions">
              <button type="button" data-preview="${escapeHtml(site.id)}" title="查看详情" aria-label="查看站点详情">${icons.info}</button>
              <button type="button" data-edit="${escapeHtml(site.id)}" title="编辑">${icons.edit}</button>
              <button class="${site.favorite ? "active" : ""}" type="button" data-favorite="${escapeHtml(site.id)}" title="收藏">${icons.star}</button>
              <button class="${site.hidden ? "active" : ""}" type="button" data-hidden="${escapeHtml(site.id)}" title="隐藏">${site.hidden ? icons.eye : icons.eyeOff}</button>
              ${runawayActionButton(site)}
            </div>
          </div>
          <div class="meta-chips"><span class="level-chip">LV${site.registrationLimit}</span>${site.requiresInviteCode ? '<span class="invite-chip">邀请码</span>' : ""}${rateLimit ? `<span class="rate-chip" title="速率限制：${escapeHtml(rateLimit)}">${escapeHtml(rateLimit)}</span>` : ""}</div>
        </div>
      </div>
      ${tagList(site)}
      <p class="description ${site.description ? "" : "muted"}">${escapeHtml(site.description || "暂无描述，稍后可以补充站点说明。")}</p>
      <div class="card-bottom">
        <div class="feature-actions">
          ${configuredLinkButton(icons.link, "API 地址", [site.apiBaseUrl], "api", site.id)}
          ${configuredLinkButton(icons.calendar, "签到地址", site.checkinUrl ? [site.checkinNote, site.checkinUrl] : [], "checkin", site.id)}
          ${configuredLinkButton(icons.gift, "福利站地址", [site.benefitUrl], "benefit", site.id)}
          ${configuredLinkButton(icons.pulse, "状态页地址", [site.statusUrl], "status", site.id)}
          ${configuredLinkButton(icons.more, "扩展链接", extensionDetails, "extension", site.id)}
        </div>
        ${capabilityActions(site)}
      </div>
      <div class="updated-row"><span>更新时间：${escapeHtml(formatDate(site.updatedAt))}</span><button type="button" data-delete="${escapeHtml(site.id)}" title="删除">${icons.trash}</button></div>
    </article>`;
}

function siteRow(site: SiteRecord): string {
  const extensionDetails = site.extensionLinks.filter((link) => link.url.trim()).map((link) => `${link.label.trim() || "扩展链接"}：${link.url.trim()}`);
  const rateLimit = formatRateLimit(site.rateLimit);
  return `
    <article class="site-row ${site.hidden ? "is-hidden" : ""} ${site.isRunaway ? "is-runaway" : ""} ${site.isPersonal ? "is-personal" : ""}" data-id="${escapeHtml(site.id)}">
      <div class="site-row-avatar"><div class="site-avatar">${escapeHtml(logoText(site))}</div></div>
      <span class="site-status-dot" title="本地记录"></span>
      <div class="site-row-content">
        <div class="site-row-identity">
          <h2 title="${escapeHtml(site.name)}">${escapeHtml(site.name)}</h2>
          <div class="meta-chips">
            <span class="level-chip">LV${site.registrationLimit}</span>
            ${site.requiresInviteCode ? '<span class="invite-chip">邀请码</span>' : ""}
            ${rateLimit ? `<span class="rate-chip" title="速率限制：${escapeHtml(rateLimit)}">${escapeHtml(rateLimit)}</span>` : ""}
          </div>
        </div>
        ${tagList(site)}
      </div>
      <div class="site-row-tools">
        <div class="feature-actions">
          ${configuredLinkButton(icons.link, "API 地址", [site.apiBaseUrl], "api", site.id)}
          ${configuredLinkButton(icons.calendar, "签到地址", site.checkinUrl ? [site.checkinNote, site.checkinUrl] : [], "checkin", site.id)}
          ${configuredLinkButton(icons.gift, "福利站地址", [site.benefitUrl], "benefit", site.id)}
          ${configuredLinkButton(icons.pulse, "状态页地址", [site.statusUrl], "status", site.id)}
          ${configuredLinkButton(icons.more, "扩展链接", extensionDetails, "extension", site.id)}
        </div>
        ${capabilityActions(site)}
      </div>
      <div class="site-row-actions card-actions">
        <button type="button" data-preview="${escapeHtml(site.id)}" title="查看详情" aria-label="查看站点详情">${icons.info}</button>
        <button type="button" data-edit="${escapeHtml(site.id)}" title="编辑">${icons.edit}</button>
        <button class="${site.favorite ? "active" : ""}" type="button" data-favorite="${escapeHtml(site.id)}" title="收藏">${icons.star}</button>
        <button class="${site.hidden ? "active" : ""}" type="button" data-hidden="${escapeHtml(site.id)}" title="隐藏">${site.hidden ? icons.eye : icons.eyeOff}</button>
        ${runawayActionButton(site)}
      </div>
    </article>`;
}

function renderFilters() {
  const tags = [...new Set([...state.suggestedTags, ...state.sites.flatMap((site) => site.tags)])];
  const signature = tags.join("\u0000");
  if (elements.tag.dataset.signature !== signature) {
    elements.tag.innerHTML = `<option value="all">全部标签</option>${tags.map((tag) => `<option value="${escapeHtml(tag)}">${escapeHtml(tag)}</option>`).join("")}`;
    elements.tag.dataset.signature = signature;
  }
  elements.tag.value = state.tag;
  elements.level.value = state.level;
  elements.feature.value = state.feature;
  syncCustomSelect(elements.tag);
  syncCustomSelect(elements.level);
  syncCustomSelect(elements.feature);
}

function hasFilters(): boolean {
  return Boolean(state.query || state.tag !== "all" || state.level !== "all" || state.feature !== "all" || state.status !== "active" || state.showHidden);
}

let tagLayoutFrame = 0;

function layoutTagList(list: HTMLElement) {
  const chips = [...list.querySelectorAll<HTMLElement>(".tag-chip")];
  const overflow = list.querySelector<HTMLElement>(".tag-overflow");
  if (!overflow || !chips.length || list.clientWidth <= 0) return;

  chips.forEach((chip) => { chip.hidden = false; });
  overflow.hidden = true;
  const gap = Number.parseFloat(getComputedStyle(list).columnGap) || 0;
  const widths = chips.map((chip) => chip.getBoundingClientRect().width);
  let visibleCount = chips.length;

  for (let count = chips.length; count >= 0; count -= 1) {
    const hiddenCount = chips.length - count;
    let overflowWidth = 0;
    if (hiddenCount > 0) {
      overflow.textContent = `+${hiddenCount}`;
      overflow.hidden = false;
      overflowWidth = overflow.getBoundingClientRect().width;
    } else {
      overflow.hidden = true;
    }
    const itemCount = count + (hiddenCount > 0 ? 1 : 0);
    const totalWidth = widths.slice(0, count).reduce((sum, width) => sum + width, 0) + overflowWidth + Math.max(0, itemCount - 1) * gap;
    if (totalWidth <= list.clientWidth || count === 0) {
      visibleCount = count;
      break;
    }
  }

  chips.forEach((chip, index) => { chip.hidden = index >= visibleCount; });
  const hiddenTags = chips.slice(visibleCount).map((chip) => chip.textContent?.trim() ?? "").filter(Boolean);
  overflow.hidden = hiddenTags.length === 0;
  if (hiddenTags.length) {
    overflow.textContent = `+${hiddenTags.length}`;
    overflow.dataset.hiddenTags = hiddenTags.join("、");
    overflow.setAttribute("aria-label", `隐藏标签：${hiddenTags.join("、")}`);
  } else {
    delete overflow.dataset.hiddenTags;
  }
}

function scheduleTagLayout() {
  window.cancelAnimationFrame(tagLayoutFrame);
  tagLayoutFrame = window.requestAnimationFrame(() => {
    hideTooltip();
    elements.grid.querySelectorAll<HTMLElement>(".tag-list").forEach(layoutTagList);
  });
}

const tagListResizeObserver = new ResizeObserver(scheduleTagLayout);

let tooltipTarget: HTMLElement | null = null;

function tooltipText(target: HTMLElement): string {
  if (target.matches(".tag-overflow") && target.dataset.hiddenTags) return `已隐藏：${target.dataset.hiddenTags}`;
  if (target.dataset.tooltip) return target.dataset.tooltip;
  if (target.dataset.uiTooltip) return target.dataset.uiTooltip;
  const title = target.getAttribute("title")?.trim();
  if (title) {
    target.dataset.uiTooltip = title;
    target.removeAttribute("title");
    if (target.matches("button") && !target.getAttribute("aria-label")) target.setAttribute("aria-label", title);
    return title;
  }
  if (target.matches('button[aria-label],[role="button"][aria-label]')) return target.getAttribute("aria-label")?.trim() ?? "";
  return "";
}

function tooltipElement(origin: EventTarget | null): HTMLElement | null {
  if (!(origin instanceof Element)) return null;
  return origin.closest<HTMLElement>('.tag-overflow,[data-tooltip],[data-ui-tooltip],[title],button[aria-label],[role="button"][aria-label]');
}

function showTooltip(target: HTMLElement) {
  const content = tooltipText(target);
  if (!content) return;
  tooltipTarget = target;
  elements.tooltip.textContent = content;
  elements.tooltip.classList.remove("is-below");
  elements.tooltip.hidden = false;
  const targetRect = target.getBoundingClientRect();
  const tooltipRect = elements.tooltip.getBoundingClientRect();
  const left = Math.min(Math.max(10, targetRect.left + targetRect.width / 2 - tooltipRect.width / 2), window.innerWidth - tooltipRect.width - 10);
  const above = targetRect.top - tooltipRect.height - 9;
  const showBelow = above < 10;
  const arrowLeft = Math.min(Math.max(9, targetRect.left + targetRect.width / 2 - left), tooltipRect.width - 9);
  elements.tooltip.style.left = `${left}px`;
  elements.tooltip.style.top = `${showBelow ? targetRect.bottom + 9 : above}px`;
  elements.tooltip.style.setProperty("--tooltip-arrow-left", `${arrowLeft}px`);
  elements.tooltip.classList.toggle("is-below", showBelow);
}

function hideTooltip(target?: HTMLElement | null) {
  if (target && tooltipTarget !== target) return;
  elements.tooltip.hidden = true;
  tooltipTarget = null;
}

function renderSites() {
  const visible = filteredSites();
  elements.grid.classList.toggle("compact", state.compact);
  elements.grid.innerHTML = visible.map(state.compact ? siteRow : siteCard).join("");
  elements.grid.hidden = visible.length === 0;
  elements.empty.hidden = visible.length !== 0;
  elements.count.textContent = `显示 ${visible.length} / ${state.sites.length} 个本地站点`;
  $("#library-count").textContent = `${state.sites.length} 个本地站点`;
  elements.clear.hidden = !hasFilters();
  elements.hidden.classList.toggle("active", state.showHidden);
  elements.hidden.innerHTML = state.showHidden ? icons.eyeOff : icons.eye;
  elements.view.innerHTML = state.compact ? icons.grid : icons.rows;
  elements.statusActive.classList.toggle("active", state.status === "active");
  elements.statusRunaway.classList.toggle("active", state.status === "runaway");
  elements.statusPersonal.classList.toggle("active", state.status === "personal");
  elements.statusActive.setAttribute("aria-pressed", String(state.status === "active"));
  elements.statusRunaway.setAttribute("aria-pressed", String(state.status === "runaway"));
  elements.statusPersonal.setAttribute("aria-pressed", String(state.status === "personal"));
  $("#active-count").textContent = String(state.sites.filter((site) => !site.isRunaway && !site.isPersonal).length);
  $("#runaway-count").textContent = String(state.sites.filter((site) => site.isRunaway && !site.isPersonal).length);
  $("#personal-count").textContent = String(state.sites.filter((site) => site.isPersonal).length);
  tagListResizeObserver.disconnect();
  elements.grid.querySelectorAll<HTMLElement>(".tag-list").forEach((list) => tagListResizeObserver.observe(list));
  scheduleTagLayout();
}

function renderSettings() {
  document.querySelectorAll<HTMLButtonElement>("[data-theme-choice]").forEach((button) => button.classList.toggle("active", button.dataset.themeChoice === preferences.theme));
  document.querySelectorAll<HTMLButtonElement>("[data-view-choice]").forEach((button) => button.classList.toggle("active", button.dataset.viewChoice === preferences.defaultView));
  document.querySelectorAll<HTMLButtonElement>("[data-status-choice]").forEach((button) => button.classList.toggle("active", button.dataset.statusChoice === preferences.defaultStatus));
  elements.startupHidden.checked = preferences.showHiddenOnStartup;
  $("#settings-total").textContent = String(state.sites.length);
  $("#settings-active").textContent = String(state.sites.filter((site) => !site.isRunaway && !site.isPersonal).length);
  $("#settings-runaway").textContent = String(state.sites.filter((site) => site.isRunaway && !site.isPersonal).length);
  $("#settings-personal").textContent = String(state.sites.filter((site) => site.isPersonal).length);
  $("#settings-favorite").textContent = String(state.sites.filter((site) => site.favorite).length);
  $("#settings-hidden").textContent = String(state.sites.filter((site) => site.hidden).length);
}

function renderSidebar() {
  elements.appLayout.classList.toggle("sidebar-collapsed", preferences.sidebarCollapsed);
  elements.sidebarCollapse.innerHTML = preferences.sidebarCollapsed ? icons.sidebarOpen : icons.sidebarClose;
  const label = preferences.sidebarCollapsed ? "展开侧栏" : "收起侧栏";
  elements.sidebarCollapse.setAttribute("aria-label", label);
  elements.sidebarCollapse.title = label;
}

function render() {
  renderFilters();
  renderSites();
  renderSettings();
  renderSidebar();
}

async function loadLibrary() {
  try {
    const data = await runCommand<LibraryData>("list_library");
    state.sites = data.sites.map((site) => ({ ...emptySite(), ...site }));
    state.suggestedTags = data.suggestedTags;
    render();
  } catch (error) {
    showToast(`本地数据库读取失败：${String(error)}`, true);
  }
}

function clearFilters() {
  state.query = "";
  state.tag = "all";
  state.level = "all";
  state.feature = "all";
  state.status = preferences.defaultStatus;
  state.showHidden = preferences.showHiddenOnStartup;
  elements.search.value = "";
  elements.level.value = "all";
  elements.feature.value = "all";
  render();
}

function openSettings() {
  closeCustomSelects();
  state.page = "settings";
  elements.settingsPage.hidden = false;
  elements.settings.classList.add("active");
  renderSettings();
  ($("#close-settings") as HTMLButtonElement).focus();
  [elements.sidebar, elements.appHeader, elements.mainContent].forEach((element) => {
    element.inert = true;
    element.setAttribute("aria-hidden", "true");
  });
}

function closeSettings() {
  state.page = "library";
  elements.settingsPage.hidden = true;
  elements.settings.classList.remove("active");
  [elements.sidebar, elements.appHeader, elements.mainContent].forEach((element) => {
    element.inert = false;
    element.removeAttribute("aria-hidden");
  });
  elements.settings.focus();
}

function updatePreferences(update: Partial<AppPreferences>) {
  preferences = { ...preferences, ...update };
  savePreferences();
  renderSettings();
}

function setTab(tab: typeof state.activeTab) {
  state.activeTab = tab;
  document.querySelectorAll<HTMLElement>("[data-tab]").forEach((button) => button.classList.toggle("active", button.dataset.tab === tab));
  document.querySelectorAll<HTMLElement>("[data-panel]").forEach((panel) => panel.classList.toggle("active", panel.dataset.panel === tab));
}

function maintainerRow(item: Maintainer = { name: "", id: "", username: "", profileUrl: "" }): string {
  return `<div class="dynamic-row maintainer-row">
    <label class="input-with-icon">${icons.link}<input data-maintainer-url type="url" value="${escapeHtml(item.profileUrl)}" placeholder="LD 个人主页：https://linux.do/u/xxx/summary" /></label>
    <label class="input-with-icon">${icons.users}<input data-maintainer-name value="${escapeHtml(item.name)}" placeholder="显示名称" /></label>
    <button class="remove-row" type="button" title="删除">${icons.trash}</button>
  </div>`;
}

function extensionRow(item: ExtensionLink = { label: "", url: "" }): string {
  return `<div class="dynamic-row extension-row">
    <input data-extension-label value="${escapeHtml(item.label)}" placeholder="链接名称" />
    <label class="input-with-icon">${icons.link}<input data-extension-url type="url" value="${escapeHtml(item.url)}" placeholder="https://…" /></label>
    <button class="remove-row" type="button" title="删除">${icons.trash}</button>
  </div>`;
}

function renderSuggestedTags() {
  const selected = new Set(elements.tagsInput.value.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean));
  elements.suggestedTags.innerHTML = state.suggestedTags.map((tag) => `<button class="suggest-tag ${selected.has(tag) ? "selected" : ""}" type="button" data-suggest-tag="${escapeHtml(tag)}">+ ${escapeHtml(tag)}</button>`).join("");
}

function setFormValue(name: string, value: string | number | boolean) {
  const control = elements.form.elements.namedItem(name) as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | null;
  if (!control) return;
  if (control instanceof HTMLInputElement && control.type === "checkbox") control.checked = Boolean(value);
  else control.value = String(value ?? "");
}

function openModal(site?: SiteRecord) {
  const value = site ?? emptySite();
  state.editingId = site?.id ?? null;
  $("#modal-title").textContent = site ? "编辑站点" : "新增站点";
  elements.form.reset();
  elements.error.textContent = "";
  setFormValue("name", value.name);
  setFormValue("apiBaseUrl", value.apiBaseUrl);
  setFormValue("description", value.description);
  setFormValue("registrationLimit", value.registrationLimit);
  setFormValue("rateLimit", value.rateLimit);
  setFormValue("requiresInviteCode", value.requiresInviteCode);
  setFormValue("tags", value.tags.join(", "));
  setFormValue("supportsCheckin", value.supportsCheckin);
  setFormValue("supportsImmersiveTranslation", value.supportsImmersiveTranslation);
  setFormValue("supportsLdc", value.supportsLdc);
  setFormValue("supportsNsfw", value.supportsNsfw);
  setFormValue("isPersonal", value.isPersonal);
  setFormValue("isRunaway", value.isRunaway);
  setFormValue("checkinUrl", value.checkinUrl);
  setFormValue("benefitUrl", value.benefitUrl);
  setFormValue("checkinNote", value.checkinNote);
  setFormValue("statusUrl", value.statusUrl);
  elements.maintainerList.innerHTML = (value.maintainers.length ? value.maintainers : [{ name: "", id: "", username: "", profileUrl: "" }]).map(maintainerRow).join("");
  elements.extensionList.innerHTML = (value.extensionLinks.length ? value.extensionLinks : [{ label: "", url: "" }]).map(extensionRow).join("");
  renderSuggestedTags();
  setTab("basic");
  elements.modal.hidden = false;
  document.body.classList.add("modal-open");
  setTimeout(() => (elements.form.elements.namedItem("name") as HTMLInputElement)?.focus(), 40);
}

function closeModal() {
  elements.modal.hidden = true;
  document.body.classList.remove("modal-open");
  state.editingId = null;
}

function formBoolean(name: string): boolean {
  return (elements.form.elements.namedItem(name) as HTMLInputElement).checked;
}

function formString(name: string): string {
  return String((elements.form.elements.namedItem(name) as HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement).value ?? "").trim();
}

function formSite(): SiteRecord {
  const existing = state.sites.find((site) => site.id === state.editingId) ?? emptySite();
  const maintainers = [...elements.maintainerList.querySelectorAll<HTMLElement>(".maintainer-row")].map((row) => ({
    name: row.querySelector<HTMLInputElement>("[data-maintainer-name]")!.value.trim(),
    id: "",
    username: "",
    profileUrl: row.querySelector<HTMLInputElement>("[data-maintainer-url]")!.value.trim(),
  })).filter((item) => item.name || item.profileUrl);
  const extensionLinks = [...elements.extensionList.querySelectorAll<HTMLElement>(".extension-row")].map((row) => ({
    label: row.querySelector<HTMLInputElement>("[data-extension-label]")!.value.trim(),
    url: row.querySelector<HTMLInputElement>("[data-extension-url]")!.value.trim(),
  })).filter((item) => item.label || item.url);
  return {
    ...existing,
    name: formString("name"),
    apiBaseUrl: formString("apiBaseUrl"),
    description: formString("description"),
    registrationLimit: Number(formString("registrationLimit")),
    rateLimit: formString("rateLimit"),
    requiresInviteCode: formBoolean("requiresInviteCode"),
    tags: formString("tags").split(/[,，]/).map((tag) => tag.trim()).filter(Boolean),
    supportsCheckin: formBoolean("supportsCheckin"),
    supportsImmersiveTranslation: formBoolean("supportsImmersiveTranslation"),
    supportsLdc: formBoolean("supportsLdc"),
    supportsNsfw: formBoolean("supportsNsfw"),
    isPersonal: formBoolean("isPersonal"),
    isRunaway: formBoolean("isRunaway"),
    checkinUrl: formString("checkinUrl"),
    benefitUrl: formString("benefitUrl"),
    checkinNote: formString("checkinNote"),
    statusUrl: formString("statusUrl"),
    maintainers,
    extensionLinks,
  };
}

async function saveSite(event: SubmitEvent) {
  event.preventDefault();
  const input = formSite();
  elements.error.textContent = "";
  if (!input.name) { elements.error.textContent = "请输入站点名称"; setTab("basic"); return; }
  try {
    const url = new URL(input.apiBaseUrl);
    if (!["http:", "https:"].includes(url.protocol)) throw new Error();
  } catch { elements.error.textContent = "请输入完整的 API BASE URL"; setTab("basic"); return; }
  const button = $("#save-site") as HTMLButtonElement;
  const editingId = state.editingId;
  button.disabled = true;
  button.textContent = "正在保存…";
  try {
    if (editingId) await runCommand<SiteRecord>("update_site", { id: editingId, input });
    else await runCommand<SiteRecord>("create_site", { input });
    closeModal();
    await loadLibrary();
    showToast(editingId ? "站点已更新" : "站点已添加");
  } catch (error) {
    elements.error.textContent = String(error);
  } finally {
    button.disabled = false;
    button.textContent = "保存";
  }
}

function siteById(id: string | undefined): SiteRecord | undefined {
  return state.sites.find((site) => site.id === id);
}

async function openExternal(url: string) {
  if (!url) return;
  try { if (isTauri) await openUrl(url); else window.open(url, "_blank", "noopener"); }
  catch (error) { showToast(`无法打开链接：${String(error)}`, true); }
}

function addressItems(site: SiteRecord, kind: SiteLinkKind): AddressItem[] {
  if (kind === "api") return [{ label: "API 地址", url: site.apiBaseUrl }].filter((item) => item.url.trim());
  if (kind === "checkin") return [{ label: "签到地址", url: site.checkinUrl, note: site.checkinNote }].filter((item) => item.url.trim());
  if (kind === "benefit") return [{ label: "福利站地址", url: site.benefitUrl }].filter((item) => item.url.trim());
  if (kind === "status") return [{ label: "状态页地址", url: site.statusUrl }].filter((item) => item.url.trim());
  return site.extensionLinks
    .filter((item) => item.url.trim())
    .map((item) => ({ label: item.label.trim() || "扩展链接", url: item.url.trim() }));
}

function allAddressItems(site: SiteRecord): AddressItem[] {
  return (["api", "checkin", "benefit", "status", "extension"] as SiteLinkKind[]).flatMap((kind) => addressItems(site, kind));
}

const linkDialogTitles: Record<SiteLinkKind, string> = {
  api: "API 地址",
  checkin: "签到地址",
  benefit: "福利站地址",
  status: "状态页地址",
  extension: "扩展链接",
};

let visibleAddressItems: AddressItem[] = [];
let linkDialogTrigger: HTMLElement | null = null;

function openLinkDialog(site: SiteRecord, kind: SiteLinkKind, trigger: HTMLElement) {
  visibleAddressItems = addressItems(site, kind);
  if (!visibleAddressItems.length) return;
  linkDialogTrigger = trigger;
  elements.linkDialogTitle.textContent = linkDialogTitles[kind];
  elements.linkDialogSubtitle.textContent = `${site.name} · ${visibleAddressItems.length} 个地址`;
  elements.addressList.innerHTML = visibleAddressItems.map((item, index) => `
    <div class="address-row">
      <div class="address-details">
        <strong>${escapeHtml(item.label)}</strong>
        ${item.note?.trim() ? `<small>${escapeHtml(item.note.trim())}</small>` : ""}
        <button class="address-value" type="button" data-open-address="${index}" title="打开地址">${escapeHtml(item.url)}</button>
      </div>
      <div class="address-actions">
        <button class="open-address" type="button" data-open-address="${index}" aria-label="打开${escapeHtml(item.label)}" title="打开地址">${icons.external}</button>
        <button class="copy-address" type="button" data-copy-address="${index}" aria-label="复制${escapeHtml(item.label)}" title="复制地址">${icons.copy}</button>
      </div>
    </div>`).join("");
  elements.linkDialog.hidden = false;
  document.body.classList.add("modal-open");
  $<HTMLButtonElement>("#close-link-dialog").focus();
}

function closeLinkDialog() {
  if (elements.linkDialog.hidden) return;
  elements.linkDialog.hidden = true;
  visibleAddressItems = [];
  document.body.classList.remove("modal-open");
  linkDialogTrigger?.focus();
  linkDialogTrigger = null;
}

async function copyAddress(item: AddressItem) {
  try { await navigator.clipboard.writeText(item.url); showToast(`${item.label}已复制`); }
  catch { showToast("复制失败，请手动复制", true); }
}

let previewAddressItems: AddressItem[] = [];
let previewSite: SiteRecord | null = null;
let previewTrigger: HTMLElement | null = null;

function previewFact(label: string, value: string): string {
  return `<div><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value || "未配置")}</dd></div>`;
}

function previewFeatureTag(label: string): string {
  return `<span class="preview-feature-tag" title="支持${escapeHtml(label)}"><i></i>${escapeHtml(label)}</span>`;
}

function openPreview(site: SiteRecord, trigger: HTMLElement) {
  previewTrigger = trigger;
  previewSite = site;
  previewAddressItems = allAddressItems(site);
  let statusText = site.isRunaway ? "已跑路" : "存活";
  if (site.isPersonal) statusText = site.isRunaway ? "自用 (已跑路)" : "自用 (存活)";
  const supportedFeatures = [
    ["每日签到", site.supportsCheckin],
    ["沉浸式翻译", site.supportsImmersiveTranslation],
    ["LDC", site.supportsLdc],
    ["NSFW", site.supportsNsfw],
    ["邀请码", site.requiresInviteCode],
  ].filter(([, supported]) => supported).map(([label]) => previewFeatureTag(String(label))).join("");
  elements.previewHeading.innerHTML = `
    <div class="preview-avatar">${escapeHtml(logoText(site))}</div>
    <div>
      <p>站点详情</p>
      <h2 id="preview-title">${escapeHtml(site.name)}</h2>
      <div class="preview-heading-meta"><span class="${site.isRunaway ? "danger" : "success"}">${statusText}</span><span>LV${site.registrationLimit}</span></div>
      ${supportedFeatures ? `<div class="preview-heading-features">${supportedFeatures}</div>` : ""}
    </div>`;

  const maintainers = site.maintainers.length
    ? site.maintainers.map((item, index) => `
      <div class="preview-maintainer-row">
        <div><strong>${escapeHtml(item.name || item.username || "未命名维护者")}</strong><small>${escapeHtml([item.username ? `@${item.username}` : "", item.id ? `ID：${item.id}` : ""].filter(Boolean).join(" · ") || "未配置账号信息")}</small></div>
        ${item.profileUrl ? `<button type="button" data-preview-profile="${index}" title="打开维护者主页">${icons.external}</button>` : ""}
      </div>`).join("")
    : '<p class="preview-empty">未配置维护者信息</p>';

  const links = previewAddressItems.length
    ? previewAddressItems.map((item, index) => `
      <div class="preview-link-row">
        <div><strong>${escapeHtml(item.label)}</strong>${item.note?.trim() ? `<small>${escapeHtml(item.note.trim())}</small>` : ""}</div>
        <button class="preview-link-value" type="button" data-preview-open="${index}" title="打开地址">${escapeHtml(item.url)}</button>
        <div class="preview-link-actions"><button type="button" data-preview-open="${index}" title="打开地址">${icons.external}</button><button type="button" data-preview-copy="${index}" title="复制地址">${icons.copy}</button></div>
      </div>`).join("")
    : '<p class="preview-empty">未配置相关链接</p>';

  elements.previewContent.innerHTML = `
    <section class="preview-section preview-summary"><h3>站点概览</h3><p>${escapeHtml(site.description || "暂无站点描述")}</p></section>
    <section class="preview-section"><h3>基础信息</h3><dl class="preview-facts">
      ${previewFact("API BASE URL", site.apiBaseUrl)}
      ${previewFact("注册等级", `LV${site.registrationLimit}`)}
      ${previewFact("速率限制", formatRateLimit(site.rateLimit))}
      ${previewFact("更新时间", formatDate(site.updatedAt))}
    </dl></section>
    <section class="preview-section"><h3>标签</h3><div class="preview-tags">${site.tags.length ? site.tags.map((tag) => `<span>${escapeHtml(tag)}</span>`).join("") : '<span class="muted">未配置标签</span>'}</div></section>
    <section class="preview-section"><h3>相关链接 <span>${previewAddressItems.length}</span></h3><div class="preview-link-list">${links}</div></section>
    <section class="preview-section"><h3>维护者 <span>${site.maintainers.length}</span></h3><div class="preview-maintainers">${maintainers}</div></section>
    <section class="preview-section"><h3>本地状态</h3><dl class="preview-facts">
      ${previewFact("运行状态", statusText)}
      ${previewFact("收藏状态", site.favorite ? "已收藏" : "未收藏")}
      ${previewFact("显示状态", site.hidden ? "已隐藏" : "正常显示")}
      ${previewFact("信息可见范围", site.isOnlyMaintainerVisible ? "仅维护者可见" : "公开")}
      ${previewFact("公益属性", site.isFakeCharity ? "疑似伪公益" : "正常")}
      ${previewFact("待核实标记", site.hasPendingReport ? "有" : "无")}
    </dl></section>`;
  elements.previewDialog.hidden = false;
  document.body.classList.add("modal-open");
  $<HTMLButtonElement>("#close-preview").focus();
}

function closePreview() {
  if (elements.previewDialog.hidden) return;
  elements.previewDialog.hidden = true;
  previewSite = null;
  previewAddressItems = [];
  document.body.classList.remove("modal-open");
  previewTrigger?.focus();
  previewTrigger = null;
}

async function deleteSite(site: SiteRecord) {
  if (!window.confirm(`确定删除“${site.name}”吗？此操作会永久移除本地记录。`)) return;
  try { await runCommand<void>("delete_site", { id: site.id }); await loadLibrary(); showToast("站点已删除"); }
  catch (error) { showToast(String(error), true); }
}

async function toggleRunaway(site: SiteRecord) {
  const wasRunaway = site.isRunaway;
  try {
    await runCommand<SiteRecord>("toggle_runaway", { id: site.id });
    await loadLibrary();
    showToast(wasRunaway ? "已恢复为在用站点" : "已移入跑路列表");
  } catch (error) {
    showToast(`状态更新失败：${String(error)}`, true);
  }
}

let toastTimer: number | undefined;
function showToast(message: string, error = false) {
  elements.toast.textContent = message;
  elements.toast.classList.toggle("error", error);
  elements.toast.classList.add("visible");
  window.clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => elements.toast.classList.remove("visible"), 2300);
}

$("#add-site").addEventListener("click", () => openModal());
elements.settings.addEventListener("click", openSettings);
elements.sidebarCollapse.addEventListener("click", () => {
  updatePreferences({ sidebarCollapsed: !preferences.sidebarCollapsed });
  renderSidebar();
});
$("#close-settings").addEventListener("click", closeSettings);
elements.settingsPage.addEventListener("click", (event) => { if (event.target === elements.settingsPage) closeSettings(); });
document.querySelectorAll<HTMLButtonElement>("[data-theme-choice]").forEach((button) => button.addEventListener("click", () => setThemePreference(button.dataset.themeChoice as ThemePreference)));
document.querySelectorAll<HTMLButtonElement>("[data-view-choice]").forEach((button) => button.addEventListener("click", () => {
  const view = button.dataset.viewChoice === "list" ? "list" : "cards";
  updatePreferences({ defaultView: view });
  state.compact = view === "list";
  renderSites();
}));
document.querySelectorAll<HTMLButtonElement>("[data-status-choice]").forEach((button) => button.addEventListener("click", () => {
  const status = button.dataset.statusChoice === "runaway" ? "runaway" : (button.dataset.statusChoice === "personal" ? "personal" : "active");
  updatePreferences({ defaultStatus: status });
  state.status = status;
  renderSites();
}));
elements.startupHidden.addEventListener("change", () => {
  updatePreferences({ showHiddenOnStartup: elements.startupHidden.checked });
  state.showHidden = elements.startupHidden.checked;
  renderSites();
});
$("#empty-clear").addEventListener("click", clearFilters);
elements.clear.addEventListener("click", clearFilters);
elements.search.addEventListener("input", () => { state.query = elements.search.value; renderSites(); });
elements.tag.addEventListener("change", () => { state.tag = elements.tag.value; syncCustomSelect(elements.tag); renderSites(); });
elements.level.addEventListener("change", () => { state.level = elements.level.value; syncCustomSelect(elements.level); renderSites(); });
elements.feature.addEventListener("change", () => { state.feature = elements.feature.value; syncCustomSelect(elements.feature); renderSites(); });
elements.statusActive.addEventListener("click", () => { state.status = "active"; renderSites(); });
elements.statusRunaway.addEventListener("click", () => { state.status = "runaway"; renderSites(); });
elements.statusPersonal.addEventListener("click", () => { state.status = "personal"; renderSites(); });
elements.hidden.addEventListener("click", () => { state.showHidden = !state.showHidden; renderSites(); });
elements.view.addEventListener("click", () => {
  state.compact = !state.compact;
  updatePreferences({ defaultView: state.compact ? "list" : "cards" });
  renderSites();
});

document.querySelectorAll<HTMLElement>("[data-custom-select]").forEach((root) => {
  const select = root.querySelector<HTMLSelectElement>("select")!;
  const trigger = root.querySelector<HTMLButtonElement>(".select-trigger")!;
  const menu = root.querySelector<HTMLElement>(".select-menu")!;
  syncCustomSelect(select);

  trigger.addEventListener("click", () => {
    if (root.classList.contains("open")) closeCustomSelects();
    else openCustomSelect(root);
  });

  menu.addEventListener("click", (event) => {
    const option = (event.target as HTMLElement).closest<HTMLButtonElement>(".select-option");
    if (!option) return;
    select.value = option.dataset.selectValue ?? "";
    closeCustomSelects();
    trigger.focus();
    select.dispatchEvent(new Event("change", { bubbles: true }));
  });

  root.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      closeCustomSelects();
      trigger.focus();
      return;
    }
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    event.preventDefault();
    if (!root.classList.contains("open")) openCustomSelect(root);
    const options = [...menu.querySelectorAll<HTMLButtonElement>(".select-option")];
    const current = options.indexOf(document.activeElement as HTMLButtonElement);
    const next = event.key === "ArrowDown" ? Math.min(current + 1, options.length - 1) : Math.max(current < 0 ? options.length - 1 : current - 1, 0);
    options[next]?.focus();
  });
});

document.addEventListener("click", (event) => {
  if (!(event.target as HTMLElement).closest("[data-custom-select]")) closeCustomSelects();
});

document.querySelectorAll<HTMLElement>("[data-close-modal]").forEach((button) => button.addEventListener("click", closeModal));
document.querySelectorAll<HTMLElement>("[data-tab]").forEach((button) => button.addEventListener("click", () => setTab(button.dataset.tab as typeof state.activeTab)));
elements.modal.addEventListener("click", (event) => { if (event.target === elements.modal) closeModal(); });
$("#close-link-dialog").addEventListener("click", closeLinkDialog);
elements.linkDialog.addEventListener("click", async (event) => {
  if (event.target === elements.linkDialog) { closeLinkDialog(); return; }
  const target = event.target as HTMLElement;
  const copyButton = target.closest<HTMLButtonElement>("[data-copy-address]");
  if (copyButton) {
    const item = visibleAddressItems[Number(copyButton.dataset.copyAddress)];
    if (item) await copyAddress(item);
    return;
  }
  const addressButton = target.closest<HTMLButtonElement>("[data-open-address]");
  if (addressButton) {
    const item = visibleAddressItems[Number(addressButton.dataset.openAddress)];
    if (item) await openExternal(item.url);
  }
});
$("#close-preview").addEventListener("click", closePreview);
elements.previewDialog.addEventListener("click", async (event) => {
  if (event.target === elements.previewDialog) { closePreview(); return; }
  const target = event.target as HTMLElement;
  const copyButton = target.closest<HTMLButtonElement>("[data-preview-copy]");
  if (copyButton) {
    const item = previewAddressItems[Number(copyButton.dataset.previewCopy)];
    if (item) await copyAddress(item);
    return;
  }
  const openButton = target.closest<HTMLButtonElement>("[data-preview-open]");
  if (openButton) {
    const item = previewAddressItems[Number(openButton.dataset.previewOpen)];
    if (item) await openExternal(item.url);
    return;
  }
  const profileButton = target.closest<HTMLButtonElement>("[data-preview-profile]");
  if (profileButton) {
    const profileUrl = previewSite?.maintainers[Number(profileButton.dataset.previewProfile)]?.profileUrl ?? "";
    await openExternal(profileUrl);
  }
});
elements.form.addEventListener("submit", saveSite);
elements.tagsInput.addEventListener("input", renderSuggestedTags);
elements.suggestedTags.addEventListener("click", (event) => {
  const button = (event.target as HTMLElement).closest<HTMLButtonElement>("[data-suggest-tag]");
  if (!button) return;
  const tags = elements.tagsInput.value.split(/[,，]/).map((tag) => tag.trim()).filter(Boolean);
  const tag = button.dataset.suggestTag!;
  if (!tags.includes(tag)) tags.push(tag); else tags.splice(tags.indexOf(tag), 1);
  elements.tagsInput.value = tags.join(", ");
  renderSuggestedTags();
});

$("#add-maintainer").addEventListener("click", () => elements.maintainerList.insertAdjacentHTML("beforeend", maintainerRow()));
$("#add-extension").addEventListener("click", () => elements.extensionList.insertAdjacentHTML("beforeend", extensionRow()));
document.querySelector(".site-modal")!.addEventListener("click", (event) => {
  const remove = (event.target as HTMLElement).closest<HTMLButtonElement>(".remove-row");
  if (remove) remove.closest(".dynamic-row")?.remove();
});

elements.grid.addEventListener("click", async (event) => {
  const target = event.target as HTMLElement;
  const action = target.closest<HTMLElement>("button[data-preview],button[data-edit],button[data-favorite],button[data-hidden],button[data-runaway],button[data-delete],button[data-link-list]");
  if (!action) return;
  const id = action.dataset.siteId ?? action.dataset.preview ?? action.dataset.edit ?? action.dataset.favorite ?? action.dataset.hidden ?? action.dataset.runaway ?? action.dataset.delete;
  const site = siteById(id);
  if (!site) return;
  if (action.dataset.preview) openPreview(site, action);
  else if (action.dataset.edit) openModal(site);
  else if (action.dataset.favorite) { await runCommand<SiteRecord>("toggle_favorite", { id: site.id }); await loadLibrary(); }
  else if (action.dataset.hidden) { await runCommand<SiteRecord>("toggle_hidden", { id: site.id }); await loadLibrary(); }
  else if (action.dataset.runaway) await toggleRunaway(site);
  else if (action.dataset.delete) await deleteSite(site);
  else if (action.dataset.linkList) openLinkDialog(site, action.dataset.linkList as SiteLinkKind, action);
});

document.addEventListener("pointerover", (event) => {
  const target = tooltipElement(event.target);
  if (target && !target.contains(event.relatedTarget as Node | null)) showTooltip(target);
});
document.addEventListener("pointerout", (event) => {
  const target = tooltipElement(event.target);
  if (target && !target.contains(event.relatedTarget as Node | null)) hideTooltip(target);
});
document.addEventListener("focusin", (event) => {
  const target = tooltipElement(event.target);
  if (target) showTooltip(target);
});
document.addEventListener("focusout", (event) => {
  const target = tooltipElement(event.target);
  if (target) hideTooltip(target);
});
document.addEventListener("pointerdown", () => hideTooltip());
document.addEventListener("scroll", () => hideTooltip(), { capture: true, passive: true });
window.addEventListener("resize", () => hideTooltip(), { passive: true });
new ResizeObserver(scheduleTagLayout).observe(elements.grid);

document.addEventListener("keydown", (event) => {
  if (event.key === "Tab" && state.page === "settings") {
    const focusable = [...elements.settingsPage.querySelectorAll<HTMLElement>('button:not([disabled]),input:not([disabled]),[tabindex]:not([tabindex="-1"])')].filter((element) => !element.hidden);
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
  }
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k" && state.page === "library") { event.preventDefault(); elements.search.focus(); elements.search.select(); }
  if (event.key === "Escape" && !elements.previewDialog.hidden) closePreview();
  else if (event.key === "Escape" && !elements.linkDialog.hidden) closeLinkDialog();
  else if (event.key === "Escape" && !elements.modal.hidden) closeModal();
  else if (event.key === "Escape" && state.page === "settings") closeSettings();
});

const themeMedia = window.matchMedia("(prefers-color-scheme: dark)");
function applyTheme() {
  const theme = preferences.theme === "system" ? (themeMedia.matches ? "dark" : "light") : preferences.theme;
  document.documentElement.dataset.theme = theme;
  renderSettings();
}
function setThemePreference(theme: ThemePreference) {
  updatePreferences({ theme });
  localStorage.removeItem("ldoh:theme");
  applyTheme();
}
applyTheme();
renderSidebar();
themeMedia.addEventListener("change", () => { if (preferences.theme === "system") applyTheme(); });

void loadLibrary();
