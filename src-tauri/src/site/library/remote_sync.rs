use crate::context::{home_dir, spawn_blocking, AppContext, EventBus, Managed};
use crate::db::*;
use crate::models::*;
use crate::site::library::*;
use crate::site::sync;
use std::sync::Arc;
use std::{collections::HashSet, time::Duration};

pub(crate) fn remote_user_string(value: &serde_json::Value, paths: &[&str]) -> String {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn remote_user_name(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/displayName",
            "/user/display_name",
            "/user/nickname",
            "/user/name",
            "/user/username",
            "/data/displayName",
            "/data/display_name",
            "/data/nickname",
            "/data/name",
            "/data/username",
            "/displayName",
            "/display_name",
            "/nickname",
            "/name",
            "/username",
            "/user/login",
            "/data/login",
            "/login",
        ],
    )
}

pub(crate) fn remote_user_username(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/username",
            "/user/login",
            "/data/username",
            "/data/login",
            "/username",
            "/login",
        ],
    )
}

pub(crate) fn remote_user_avatar(value: &serde_json::Value) -> String {
    remote_user_string(
        value,
        &[
            "/user/avatarUrl",
            "/user/avatar_url",
            "/user/avatar",
            "/data/avatarUrl",
            "/data/avatar_url",
            "/data/avatar",
            "/avatarUrl",
            "/avatar_url",
            "/avatar",
        ],
    )
}

pub(crate) fn remote_sites_from_json(value: serde_json::Value) -> Result<Vec<SiteRecord>, String> {
    let mut sites = if value.is_array() {
        value
    } else if let Some(sites) = value.get("sites") {
        sites.clone()
    } else if let Some(sites) = value.pointer("/data/sites") {
        sites.clone()
    } else if let Some(data) = value.get("data").filter(|data| data.is_array()) {
        data.clone()
    } else {
        return Err("远端站点接口返回格式不正确：缺少 sites 列表".into());
    };

    if let Some(items) = sites.as_array_mut() {
        for item in items {
            if let Some(site) = item.as_object_mut() {
                let system_type = infer_remote_system_type(site);
                if !system_type.is_empty() {
                    site.insert("systemType".into(), serde_json::Value::String(system_type));
                }
            }
        }
    }

    serde_json::from_value(sites).map_err(|error| format!("远端站点数据格式不正确：{error}"))
}

pub(crate) async fn authenticated_remote_session(
    database: &Database,
) -> Result<(sync::ChromeCookieSession, serde_json::Value), String> {
    let home_dir = home_dir().ok_or("无法定位用户目录")?;
    let sessions = spawn_blocking(move || {
        sync::read_chrome_cookie_sessions_from_home(
            &home_dir,
            REMOTE_ROOT_URL,
            REMOTE_SESSION_COOKIE,
        )
    })
    .await
    .map_err(|error| format!("读取 Chrome 登录会话任务失败：{error}"))??;

    let client = build_http_client(database, Duration::from_secs(20), 5, "同步请求")?;

    for session in sessions {
        let cookie = reqwest::header::HeaderValue::from_str(&session.cookie_header)
            .map_err(|_| "Chrome 登录 Cookie 格式无效".to_string())?;
        let response = client
            .get(REMOTE_USER_URL)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
            .header(reqwest::header::COOKIE, cookie)
            .send()
            .await
            .map_err(|error| format!("无法连接用户信息接口：{error}"))?;

        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            continue;
        }
        if !response.status().is_success() {
            return Err(format!(
                "用户信息接口请求失败（HTTP {}）",
                response.status().as_u16()
            ));
        }
        let user = response
            .json::<serde_json::Value>()
            .await
            .map_err(|error| format!("用户信息接口返回格式不正确：{error}"))?;
        return Ok((session, user));
    }

    Err("Chrome 中找到的 ldoh 登录会话均已失效，请先在 Chrome 登录后重试".into())
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn get_remote_user(ctx: Managed<'_, Arc<AppContext>>) -> Result<RemoteUserInfo, String> {
    let (session, user) = authenticated_remote_session(&ctx.database).await?;
    let username = remote_user_username(&user);
    let name = {
        let value = remote_user_name(&user);
        if value.is_empty() {
            if !username.is_empty() {
                username.clone()
            } else if !session.account_name.is_empty() {
                session.account_name.clone()
            } else {
                "已登录用户".to_string()
            }
        } else {
            value
        }
    };

    Ok(RemoteUserInfo {
        name,
        username,
        avatar_url: remote_user_avatar(&user),
        profile_name: session.profile_name,
        account_name: session.account_name,
    })
}


#[derive(Clone)]
struct ExistingLocalSite {
    id: String,
    favorite: bool,
    hidden: bool,
    is_personal: bool,
    is_pending: bool,
    use_system_proxy: bool,
    use_proxy_pool: bool,
    system_type: String,
    /// 本地已保存的辅助站点地址。公共库上游不下发该字段，
    /// 合并时用它保留用户手工登记的备用域名，防止同步洗掉。
    aux_base_url: String,
}

/// 一次合并的落库结果，供不同公共库来源各自上报。
#[derive(Debug, Default)]
pub(crate) struct MergeOutcome {
    pub(crate) added: usize,
    pub(crate) updated: usize,
    /// 仅新增站点 id：站点类型检测只针对新增站点，更新站点的类型保持冻结。
    pub(crate) added_ids: Vec<String>,
}

/// 站点地址的比对键：用于「同一站点被不同来源描述」时的去重。
///
/// 不区分协议、忽略 `www.` 前缀、且把常见的接口前缀（`/v1`、`/api` 等）折掉——
/// 同一个站点在 ldoh 里往往记裸域名，在别的目录里记 `https://xxx/v1`，
/// 按字面比对会重复插入。
pub(crate) fn site_address_key(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return trimmed.trim_end_matches('/').to_ascii_lowercase();
    };
    let host = parsed
        .host_str()
        .unwrap_or("")
        .to_lowercase()
        .trim_start_matches("www.")
        .to_string();
    if host.is_empty() {
        return String::new();
    }
    let port = match parsed.port() {
        Some(port) if !matches!(port, 80 | 443) => format!(":{port}"),
        _ => String::new(),
    };
    let mut segments: Vec<&str> = parsed
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    // 折掉末尾的接口前缀段，让 `/v1`、`/anthropic`、`/v1beta` 与裸域名同键。
    while let Some(last) = segments.last().copied() {
        if matches!(
            last.to_ascii_lowercase().as_str(),
            "v1" | "v2" | "v3" | "v1beta" | "anthropic" | "openai" | "api"
        ) {
            segments.pop();
        } else {
            break;
        }
    }
    let path = segments.join("/");
    if path.is_empty() {
        format!("{host}{port}")
    } else {
        format!("{host}{port}/{path}")
    }
}

/// 把一批公共库站点并入本地站点库。
///
/// 匹配顺序：先按站点 id（ldoh 的站点 id 稳定）、再按 [`site_address_key`]。
/// 命中已有记录时保留本地归类（收藏 / 隐藏 / 在用 / 待定 / 代理偏好）与已冻结的类型；
/// 未命中则新建。`Use a transaction` 由本函数自行持有，调用方不要再包事务。
pub(crate) fn merge_remote_sites(
    database: &Database,
    remote_sites: Vec<SiteRecord>,
) -> Result<MergeOutcome, String> {
    if remote_sites.is_empty() {
        return Ok(MergeOutcome::default());
    }
    let mut connection = database.lock_conn()?;
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;

    let mut existing_map: std::collections::HashMap<String, ExistingLocalSite> =
        std::collections::HashMap::new();
    let mut existing_by_id: std::collections::HashMap<String, ExistingLocalSite> =
        std::collections::HashMap::new();
    let mut existing_ids: HashSet<String> = HashSet::new();

    {
        let mut statement = transaction
            .prepare(
                "SELECT id, api_base_url, favorite, hidden, is_personal, is_pending, use_system_proxy, use_proxy_pool, system_type, aux_base_url FROM directory_sites",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)? != 0,
                    row.get::<_, i64>(5)? != 0,
                    row.get::<_, i64>(6)? != 0,
                    row.get::<_, i64>(7)? != 0,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            })
            .map_err(|error| error.to_string())?;

        for item in rows {
            let (
                id,
                api_base_url,
                favorite,
                hidden,
                is_personal,
                is_pending,
                use_system_proxy,
                use_proxy_pool,
                system_type,
                aux_base_url,
            ) = item.map_err(|error| error.to_string())?;
            let existing = ExistingLocalSite {
                id: id.clone(),
                favorite,
                hidden,
                is_personal,
                is_pending,
                use_system_proxy,
                use_proxy_pool,
                system_type,
                aux_base_url,
            };
            existing_ids.insert(id.clone());
            existing_by_id.insert(id, existing.clone());
            let key = site_address_key(&api_base_url);
            if !key.is_empty() {
                existing_map.insert(key, existing);
            }
        }
    }

    let mut outcome = MergeOutcome::default();
    let mut synced_ids = HashSet::new();
    let mut processed_url_keys = HashSet::new();

    for mut site in remote_sites {
        let url_key = site_address_key(&site.api_base_url);
        if url_key.is_empty() && site.id.trim().is_empty() {
            continue;
        }
        if !url_key.is_empty() && !processed_url_keys.insert(url_key.clone()) {
            // 同一批次中重复出现的站点地址，跳过后续重复项
            continue;
        }

        // 先按站点 id 命中（ldoh 下发的 id 稳定），再退回按地址命中。
        let trimmed_id = site.id.trim().to_string();
        let existing_match = if !trimmed_id.is_empty() {
            existing_by_id.get(&trimmed_id).cloned()
        } else {
            None
        }
        .or_else(|| {
            if !url_key.is_empty() {
                existing_map.get(&url_key).cloned()
            } else {
                None
            }
        });

        let mut is_new_site = false;
        if let Some(existing) = existing_match {
            site.id = existing.id.clone();
            site.favorite = existing.favorite;
            site.hidden = existing.hidden;
            site.is_personal = existing.is_personal;
            site.is_pending = existing.is_pending && !existing.is_personal;
            site.use_system_proxy = existing.use_system_proxy;
            site.use_proxy_pool = existing.use_proxy_pool;
            // 已存在站点的类型一律冻结保留：本地类型可能来自用户手工调整或既有证据，
            // 全量同步只允许为“新增站点”提供类型；本地非空时绝不改写，
            // 本地为空（历史遗留缺类型）时才用远端值补齐。
            if !existing.system_type.trim().is_empty() {
                site.system_type = existing.system_type.clone();
            }
            // 用户设置的辅助地址本地优先；远端只能补齐尚未配置的地址。
            if !existing.aux_base_url.trim().is_empty() {
                site.aux_base_url = existing.aux_base_url.clone();
            }
            outcome.updated += 1;
        } else {
            site.id = trimmed_id;
            if site.id.is_empty() || existing_ids.contains(&site.id) {
                site.id = generated_id();
            }
            existing_ids.insert(site.id.clone());
            site.favorite = false;
            site.hidden = false;
            site.is_personal = false;
            site.is_pending = false;
            site.use_system_proxy = false;
            site.use_proxy_pool = false;
            outcome.added += 1;
            is_new_site = true;
        }

        if !synced_ids.insert(site.id.clone()) {
            continue;
        }
        if is_new_site {
            outcome.added_ids.push(site.id.clone());
        }

        let site_name = site.name.clone();
        let site = normalize_remote_site(site)
            .map_err(|error| format!("同步站点「{site_name}」失败：{error}"))?;
        insert_site_transaction(&transaction, &site)?;
    }

    transaction.commit().map_err(|error| error.to_string())?;
    Ok(outcome)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_remote_sites(
    ctx: Managed<'_, Arc<AppContext>>,
    _runaway: Option<bool>,
    run_id: u64,
) -> Result<SyncSitesResult, String> {
    sync_remote_sites_inner(&ctx, run_id).await
}

/// ldoh 同步的实际实现：不依赖 `Managed` 包装，方便被「同步公共库」多来源入口复用。
pub(crate) async fn sync_remote_sites_inner(
    ctx: &AppContext,
    run_id: u64,
) -> Result<SyncSitesResult, String> {
    let database = &*ctx.database;
    let bus: EventBus = ctx.event_bus.clone();
    emit_sync_progress(
        &bus,
        run_id,
        "session",
        "running",
        "正在读取并验证 Chrome 登录会话".into(),
    );
    let (session, user) = authenticated_remote_session(database).await?;
    emit_sync_progress(
        &bus,
        run_id,
        "session",
        "success",
        format!("Chrome {} 登录会话验证完成", session.profile_name),
    );
    let client = build_http_client(&database, Duration::from_secs(30), 5, "同步请求")?;
    let cookie = reqwest::header::HeaderValue::from_str(&session.cookie_header)
        .map_err(|_| "Chrome 登录 Cookie 格式无效".to_string())?;

    emit_sync_progress(
        &bus,
        run_id,
        "download",
        "running",
        "正在同时请求存活站点与跑路站点列表".into(),
    );

    let alive_request = client
        .get(REMOTE_SITES_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .header(reqwest::header::COOKIE, cookie.clone())
        .send();

    let runaway_url = format!("{REMOTE_SITES_URL}?mode=runaway");
    let runaway_request = client
        .get(runaway_url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .header(reqwest::header::COOKIE, cookie)
        .send();

    let (alive_response, runaway_response) = tokio::try_join!(alive_request, runaway_request)
        .map_err(|error| format!("无法连接站点同步接口：{error}"))?;

    if matches!(
        alive_response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) || matches!(
        runaway_response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err("Chrome 登录会话已失效，请重新登录后重试".into());
    }

    if !alive_response.status().is_success() {
        return Err(format!(
            "存活站点同步接口请求失败（HTTP {}）",
            alive_response.status().as_u16()
        ));
    }
    if !runaway_response.status().is_success() {
        return Err(format!(
            "跑路站点同步接口请求失败（HTTP {}）",
            runaway_response.status().as_u16()
        ));
    }

    emit_sync_progress(
        &bus,
        run_id,
        "download",
        "success",
        "存活与跑路站点接口响应正常".into(),
    );
    emit_sync_progress(
        &bus,
        run_id,
        "parse",
        "running",
        "正在解析并校验远端存活与跑路站点数据".into(),
    );

    let (alive_json, runaway_json) = tokio::try_join!(
        async {
            alive_response
                .json::<serde_json::Value>()
                .await
                .map_err(|error| format!("存活站点接口返回格式不正确：{error}"))
        },
        async {
            runaway_response
                .json::<serde_json::Value>()
                .await
                .map_err(|error| format!("跑路站点接口返回格式不正确：{error}"))
        }
    )?;

    let mut alive_sites = remote_sites_from_json(alive_json)?;
    for site in &mut alive_sites {
        site.is_runaway = false;
    }

    let mut runaway_sites = remote_sites_from_json(runaway_json)?;
    for site in &mut runaway_sites {
        site.is_runaway = true;
    }

    let alive_count = alive_sites.len();
    let runaway_count = runaway_sites.len();
    let mut all_remote_sites = Vec::with_capacity(alive_count + runaway_count);
    all_remote_sites.extend(alive_sites);
    all_remote_sites.extend(runaway_sites);

    emit_sync_progress(
        &bus,
        run_id,
        "parse",
        "success",
        format!(
            "已解析 {} 条远端站点记录（存活 {} 条，跑路 {} 条）",
            all_remote_sites.len(),
            alive_count,
            runaway_count
        ),
    );
    emit_sync_progress(
        &bus,
        run_id,
        "save",
        "running",
        "正在写入本地数据库并保留本地类型与在用状态".into(),
    );

    let outcome = merge_remote_sites(database, all_remote_sites)?;
    let (added, updated) = (outcome.added, outcome.updated);
    emit_sync_progress(
        &bus,
        run_id,
        "save",
        "success",
        format!("本地写入完成：新增 {added}，更新 {updated}"),
    );

    let user_name = {
        let api_name = remote_user_name(&user);
        if !api_name.is_empty() {
            api_name
        } else if !session.account_name.trim().is_empty() {
            session.account_name.clone()
        } else {
            session.profile_name.clone()
        }
    };
    let site_ids = outcome.added_ids;
    Ok(SyncSitesResult {
        added,
        updated,
        total: added + updated,
        profile_name: session.profile_name,
        account_name: session.account_name,
        user_name,
        runaway: false,
        site_ids,
        sources: vec![SOURCE_LDOH.to_string()],
    })
}

/// 公共库来源标识：ldoh（社区维护的公益站目录，需 Chrome 登录）。
pub(crate) const SOURCE_LDOH: &str = "ldoh";
/// 公共库来源标识：白嫖公益站目录（公开可抓，无需登录）。
pub(crate) const SOURCE_BAIPIAO: &str = "baipiao";

pub(crate) fn public_source_label(source: &str) -> &'static str {
    match source {
        SOURCE_BAIPIAO => "白嫖公益站目录",
        _ => "ldoh 公益站目录",
    }
}

/// 「同步公共库」的统一入口：按顺序跑完所选来源，单个来源失败不影响其它来源。
///
/// - 每个来源的成败都以 [`PublicSourceResult`] 逐条返回，前端据 `ok` 分别提示；
/// - 只要至少一个来源成功，命令本身即成功（否则返回合并后的错误）；
/// - `sources` 为空时退化为只同步 ldoh，保持旧调用方的行为不变。
#[cfg_attr(feature = "desktop", tauri::command)]
pub async fn sync_public_library(
    ctx: Managed<'_, Arc<AppContext>>,
    sources: Option<Vec<String>>,
    run_id: u64,
) -> Result<Vec<PublicSourceResult>, String> {
    let ctx: &AppContext = &ctx;
    let database = &*ctx.database;
    let bus: EventBus = ctx.event_bus.clone();

    let mut selected: Vec<String> = Vec::new();
    for source in sources.unwrap_or_default() {
        let source = source.trim().to_ascii_lowercase();
        if source.is_empty() || selected.contains(&source) {
            continue;
        }
        if !matches!(source.as_str(), SOURCE_LDOH | SOURCE_BAIPIAO) {
            return Err(format!("未知的公共库来源：{source}"));
        }
        selected.push(source);
    }
    if selected.is_empty() {
        selected.push(SOURCE_LDOH.to_string());
    }

    let mut results = Vec::with_capacity(selected.len());
    for source in selected {
        let label = public_source_label(&source);
        emit_sync_progress(
            &bus,
            run_id,
            &format!("{source}-start"),
            "info",
            format!("开始同步{label}"),
        );
        let outcome = match source.as_str() {
            SOURCE_BAIPIAO => sync_baipiao_sites(database, &bus, run_id).await,
            _ => sync_remote_sites_inner(ctx, run_id).await,
        };
        results.push(match outcome {
            Ok(result) => PublicSourceResult {
                source: source.clone(),
                label: label.to_string(),
                ok: true,
                added: result.added,
                updated: result.updated,
                total: result.total,
                error: String::new(),
                site_ids: result.site_ids,
            },
            Err(error) => {
                emit_sync_progress(
                    &bus,
                    run_id,
                    &format!("{source}-failed"),
                    "error",
                    format!("{label}同步失败：{error}"),
                );
                PublicSourceResult {
                    source: source.clone(),
                    label: label.to_string(),
                    ok: false,
                    added: 0,
                    updated: 0,
                    total: 0,
                    error,
                    site_ids: Vec::new(),
                }
            }
        });
    }

    if results.iter().all(|result| !result.ok) {
        let errors: Vec<String> = results
            .iter()
            .map(|result| format!("{}：{}", result.label, result.error))
            .collect();
        return Err(errors.join("；"));
    }
    Ok(results)
}
