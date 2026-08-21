use crate::chrome_sync;
use crate::models::*;
use crate::site_library;
use serde_json;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri;
use url::Url;

pub(crate) fn validate_url(value: &str, label: &str, required: bool) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return if required {
            Err(format!("{label}不能为空"))
        } else {
            Ok(String::new())
        };
    }
    let parsed =
        Url::parse(value).map_err(|_| format!("{label}必须是完整的 http:// 或 https:// 地址"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!("{label}仅支持 http:// 或 https:// 地址"));
    }
    Ok(parsed.to_string())
}

pub(crate) fn unique_trimmed(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let value = value.trim().to_string();
            if value.is_empty() || !seen.insert(value.clone()) {
                None
            } else {
                Some(value)
            }
        })
        .collect()
}

pub(crate) fn normalize_site(mut site: SiteRecord) -> Result<SiteRecord, String> {
    site.name = site.name.trim().to_string();
    if site.name.is_empty() {
        return Err("站点名称不能为空".into());
    }
    site.api_base_url = validate_url(&site.api_base_url, "API BASE URL", true)?;
    site.system_type = canonical_system_type(&site.system_type);
    site.checkin_url = validate_url(&site.checkin_url, "签到页 URL", false)?;
    site.benefit_url = validate_url(&site.benefit_url, "福利站 URL", false)?;
    site.status_url = validate_url(&site.status_url, "状态页 URL", false)?;
    site.description = site.description.trim().to_string();
    site.rate_limit = site.rate_limit.trim().to_string();
    site.checkin_note = site.checkin_note.trim().to_string();
    site.registration_limit = site.registration_limit.clamp(0, 3);
    site.tags = unique_trimmed(site.tags);

    site.maintainers = site
        .maintainers
        .into_iter()
        .filter_map(|mut maintainer| {
            maintainer.name = maintainer.name.trim().to_string();
            maintainer.id = maintainer.id.trim().to_string();
            maintainer.username = maintainer.username.trim().to_string();
            maintainer.profile_url = maintainer.profile_url.trim().to_string();
            if maintainer.name.is_empty() && maintainer.profile_url.is_empty() {
                None
            } else {
                Some(maintainer)
            }
        })
        .collect();

    for maintainer in &mut site.maintainers {
        maintainer.profile_url = validate_url(&maintainer.profile_url, "维护者主页", false)?;
    }

    site.extension_links = site
        .extension_links
        .into_iter()
        .filter_map(|mut link| {
            link.label = link.label.trim().to_string();
            link.url = link.url.trim().to_string();
            if link.label.is_empty() && link.url.is_empty() {
                None
            } else {
                Some(link)
            }
        })
        .collect();

    for link in &mut site.extension_links {
        link.url = validate_url(&link.url, "扩展链接", true)?;
        if link.label.is_empty() {
            link.label = "扩展链接".into();
        }
    }

    // 在用优先：同时勾选时保留在用，清除待定。
    if site.is_personal {
        site.is_pending = false;
    }

    Ok(site)
}

pub(crate) fn canonical_system_type(value: &str) -> String {
    site_library::canonical_platform(value)
}

/// 仅基于 URL 中明确出现的产品名提供高置信度提示。
/// 不把通用的 `api`、`new-api` 等模糊命名当成类型依据，避免误判。
pub(crate) fn system_type_hint_from_url(value: &str) -> Option<&'static str> {
    site_library::detect_platform_by_url_hint(value)
        .or_else(|| site_library::low_priority_url_hint(value))
}

pub(crate) fn infer_remote_system_type(
    site: &serde_json::Map<String, serde_json::Value>,
) -> String {
    for key in [
        "systemType",
        "system_type",
        "siteType",
        "site_type",
        "apiType",
        "api_type",
        "platform",
        "system",
        "type",
    ] {
        if let Some(value) = site.get(key).and_then(serde_json::Value::as_str) {
            let system_type = canonical_system_type(value);
            if !system_type.is_empty() {
                return system_type;
            }
        }
    }
    // 刷新令牌形态（newapi2）优先于 Cookie 形态（new-api）：
    // 站点同时下发两个标记时，以更具体的认证形态为准。
    for (key, system_type) in [
        ("isNewApi2", "newapi2"),
        ("is_newapi2", "newapi2"),
        ("isSub2Api", "Sub2API"),
        ("is_sub2api", "Sub2API"),
        ("isNewApi", "NewAPI"),
        ("is_newapi", "NewAPI"),
    ] {
        if site
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return system_type.into();
        }
    }
    if let Some(tags) = site.get("tags").and_then(serde_json::Value::as_array) {
        for tag in tags.iter().filter_map(serde_json::Value::as_str) {
            let system_type = canonical_system_type(tag);
            if !system_type.is_empty() {
                return system_type;
            }
        }
    }

    let urls = ["checkinUrl", "checkin_url", "apiBaseUrl", "api_base_url"]
        .iter()
        .filter_map(|key| site.get(*key).and_then(serde_json::Value::as_str))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    // 路径特征（/console、/profile、/dashboard）优先，域名产品名仅作兜底。
    if urls.iter().any(|url| url.contains("/console/")) {
        "new-api".into()
    } else if urls
        .iter()
        .any(|url| url.contains("/profile") || url.contains("/dashboard"))
    {
        "sub2api".into()
    } else if let Some(system_type) = urls.iter().find_map(|url| system_type_hint_from_url(url)) {
        system_type.into()
    } else {
        String::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EndpointProbe {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) is_json: bool,
    pub(crate) is_challenge: bool,
}

#[derive(Debug)]
pub(crate) struct DiscoveryResponse {
    pub(crate) content_type: String,
    pub(crate) body: String,
}

impl DiscoveryResponse {
    pub(crate) fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_str(&self.body).ok()
    }
}

pub(crate) fn normalize_import_base_url(value: &str) -> Result<Url, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| "站点 URL 必须是完整的 http:// 或 https:// 地址".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("站点 URL 必须是完整的 http:// 或 https:// 地址".into());
    }
    url.set_username("")
        .map_err(|_| "站点 URL 不能包含登录凭据".to_string())?;
    url.set_password(None)
        .map_err(|_| "站点 URL 不能包含登录凭据".to_string())?;
    url.set_query(None);
    url.set_fragment(None);
    url.set_path("/");
    Ok(url)
}

pub(crate) async fn fetch_discovery_resource(
    client: reqwest::Client,
    url: Url,
    accept: &'static str,
) -> Option<DiscoveryResponse> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .header(reqwest::header::USER_AGENT, "OpenHub-Desktop/0.3")
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?;
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let bytes = response.bytes().await.ok()?;
    let body = String::from_utf8_lossy(&bytes[..bytes.len().min(1_048_576)]).into_owned();
    Some(DiscoveryResponse { content_type, body })
}

pub(crate) fn json_data_object(
    value: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    value
        .get("data")
        .and_then(serde_json::Value::as_object)
        .or_else(|| value.as_object())
}

pub(crate) fn discovered_json_string(value: &serde_json::Value, keys: &[&str]) -> String {
    json_data_object(value)
        .and_then(|object| {
            keys.iter()
                .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
        .to_string()
}

pub(crate) fn discovered_json_bool(value: &serde_json::Value, keys: &[&str]) -> bool {
    json_data_object(value).is_some_and(|object| {
        keys.iter().any(|key| {
            object
                .get(*key)
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
        })
    })
}

pub(crate) fn decode_basic_html_entities(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .trim()
        .to_string()
}

pub(crate) fn html_attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    for quote in ['\"', '\''] {
        let marker = format!("{name}={quote}");
        if let Some(marker_start) = lower.find(&marker) {
            let start = marker_start + marker.len();
            let end = tag[start..].find(quote)? + start;
            return Some(decode_basic_html_entities(&tag[start..end]));
        }
    }
    None
}

pub(crate) fn html_title(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let Some(open_start) = lower.find("<title") else {
        return String::new();
    };
    let Some(open_end) = lower[open_start..].find('>') else {
        return String::new();
    };
    let content_start = open_start + open_end + 1;
    let Some(content_end) = lower[content_start..].find("</title>") else {
        return String::new();
    };
    decode_basic_html_entities(&html[content_start..content_start + content_end])
}

pub(crate) fn html_meta_description(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(start) = lower[offset..].find("<meta") {
        let start = offset + start;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + end];
        let is_description = html_attribute(tag, "name")
            .or_else(|| html_attribute(tag, "property"))
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "description" | "og:description"
                )
            });
        if is_description {
            if let Some(content) = html_attribute(tag, "content") {
                return content;
            }
        }
        offset = start + end + 1;
        if offset >= lower.len() {
            break;
        }
    }
    String::new()
}

pub(crate) fn html_icon_href(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut offset = 0;
    while let Some(start) = lower[offset..].find("<link") {
        let start = offset + start;
        let Some(end) = lower[start..].find('>') else {
            break;
        };
        let tag = &html[start..=start + end];
        if html_attribute(tag, "rel")
            .is_some_and(|value| value.to_ascii_lowercase().contains("icon"))
        {
            if let Some(href) = html_attribute(tag, "href") {
                return href;
            }
        }
        offset = start + end + 1;
        if offset >= lower.len() {
            break;
        }
    }
    String::new()
}

pub(crate) fn resolve_discovered_url(base_url: &Url, value: &str) -> String {
    Url::parse(value.trim())
        .ok()
        .or_else(|| base_url.join(value.trim()).ok())
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
        .unwrap_or_default()
}

pub(crate) fn endpoint_probe_exists(probe: EndpointProbe) -> bool {
    probe.status == reqwest::StatusCode::UNAUTHORIZED
        || (probe.is_json
            && (probe.status.is_success() || probe.status == reqwest::StatusCode::FORBIDDEN))
}

pub(crate) fn shield_page_response(
    status: reqwest::StatusCode,
    content_type: &str,
    security_gateway_header: bool,
    body: &[u8],
) -> bool {
    if serde_json::from_slice::<serde_json::Value>(body).is_ok() {
        return false;
    }
    let first = body
        .iter()
        .copied()
        .find(|byte| !byte.is_ascii_whitespace());
    let looks_html = content_type.contains("text/html") || first == Some(b'<');
    if !looks_html && !security_gateway_header {
        return false;
    }
    let lower = String::from_utf8_lossy(&body[..body.len().min(200_000)]).to_ascii_lowercase();
    security_gateway_header
        || matches!(status.as_u16(), 403 | 429 | 503)
        || [
            "cf-chl-",
            "challenge-platform",
            "cloudflare ray id",
            "just a moment",
            "attention required",
            "acw_sc__v2",
            "acw_tc",
            "cdn_sec_tc",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
}

pub(crate) fn system_type_from_probes(
    newapi_probe: Option<EndpointProbe>,
    sub2api_probe: Option<EndpointProbe>,
) -> Option<&'static str> {
    if newapi_probe.is_some_and(endpoint_probe_exists) {
        Some("new-api")
    } else if sub2api_probe.is_some_and(endpoint_probe_exists) {
        Some("sub2api")
    } else if newapi_probe.is_some_and(|probe| probe.status == reqwest::StatusCode::NOT_FOUND)
        && sub2api_probe.is_some_and(|probe| probe.status == reqwest::StatusCode::NOT_FOUND)
    {
        Some("")
    } else {
        None
    }
}

pub(crate) async fn probe_site_system_type_details(
    client: &reqwest::Client,
    base_url: &str,
) -> (Option<String>, bool) {
    // 移植自 metapi 的 detectPlatform 流水线：URL 提示 → title 提示 → 端点探测。
    let detection = site_library::detect_platform(client, base_url).await;
    (detection.platform, detection.challenge)
}

pub(crate) async fn probe_site_system_type(
    client: &reqwest::Client,
    base_url: &str,
) -> Option<String> {
    probe_site_system_type_details(client, base_url).await.0
}

pub(crate) fn chrome_system_probe_script(marker: &str) -> String {
    let marker = serde_json::to_string(marker).unwrap_or_else(|_| "\"\"".into());
    r#"(() => {
  const token = __OPENHUB_MARKER__;
  const pending = "__OPENHUB_PENDING__";
  if (!/^https?:$/.test(window.location.protocol)) return pending;
  const previous = window.__openHubSystemProbe;
  if (previous && previous.token === token) {
    return previous.result ? JSON.stringify(previous.result) : pending;
  }
  const bridge = { token, result: null };
  window.__openHubSystemProbe = bridge;
  const probe = async (path) => {
    try {
      const response = await fetch(path, {
        method: "GET",
        credentials: "include",
        cache: "no-store",
        headers: { Accept: "application/json" },
        signal: AbortSignal.timeout(12000)
      });
      const text = await response.text();
      let isJson = false;
      try { JSON.parse(text); isJson = true; } catch (_) {}
      return { status: response.status, isJson };
    } catch (_) {
      return null;
    }
  };
  Promise.all([probe("/api/status"), probe("/setup/status")])
    .then(([newapi, sub2api]) => { bridge.result = { ok: true, newapi, sub2api }; })
    .catch((error) => { bridge.result = { ok: false, error: String(error) }; });
  return pending;
})()"#
        .replace("__OPENHUB_MARKER__", &marker)
}

pub(crate) fn parse_chrome_system_probe(value: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(value).ok()?;
    if value.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let parse = |name: &str| {
        let value = value.get(name)?;
        Some(EndpointProbe {
            status: reqwest::StatusCode::from_u16(value.get("status")?.as_u64()?.try_into().ok()?)
                .ok()?,
            is_json: value.get("isJson")?.as_bool()?,
            is_challenge: false,
        })
    };
    system_type_from_probes(parse("newapi"), parse("sub2api")).map(str::to_string)
}

pub(crate) async fn probe_site_system_type_via_chrome(
    base_url: &str,
    profile_ids: &[String],
) -> Option<String> {
    if let Some(system_type) = system_type_hint_from_url(base_url) {
        return Some(system_type.into());
    }
    let marker = format!(
        "openhub-system-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let script = chrome_system_probe_script(&marker);
    let existing_attempt = tauri::async_runtime::spawn_blocking({
        let base_url = base_url.to_string();
        let script = script.clone();
        move || {
            chrome_sync::run_javascript_in_existing_chrome_tab(
                &base_url,
                &script,
                Duration::from_secs(15),
            )
        }
    })
    .await
    .ok()?;
    if let Ok(Some(value)) = existing_attempt {
        if let Some(system_type) = parse_chrome_system_probe(&value) {
            return Some(system_type);
        }
    }

    let profile_id = profile_ids.first()?.clone();
    let mut target_url = Url::parse(base_url).ok()?.join("/api/status").ok()?;
    target_url.set_fragment(Some(&marker));
    let background_attempt = tauri::async_runtime::spawn_blocking({
        let target_url = target_url.to_string();
        let marker = marker.clone();
        move || {
            chrome_sync::run_javascript_in_background_chrome_profile(
                &target_url,
                &profile_id,
                &marker,
                &script,
                Duration::from_secs(20),
                None,
            )
        }
    })
    .await
    .ok()?;
    background_attempt
        .ok()
        .and_then(|value| parse_chrome_system_probe(&value))
}

pub(crate) fn cached_profile_ids_for_sites(
    database: &Database,
    site_ids: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, String> {
    if site_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let connection = database.lock_conn()?;
    let mut statement = connection
        .prepare(
            "SELECT site_id, profile_id FROM site_accounts
             WHERE TRIM(profile_id) <> ''
             ORDER BY site_id, is_valid DESC, updated_at DESC, profile_id",
        )
        .map_err(|error| error.to_string())?;
    let mut profiles = HashMap::<String, Vec<String>>::new();
    for row in statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|error| error.to_string())?
    {
        let (site_id, profile_id) = row.map_err(|error| error.to_string())?;
        if !site_ids.contains(&site_id) {
            continue;
        }
        let entry = profiles.entry(site_id).or_default();
        if !entry.contains(&profile_id) {
            entry.push(profile_id);
        }
    }
    Ok(profiles)
}

pub(crate) async fn probe_site_system_types(
    client: &reqwest::Client,
    targets: Vec<(String, String)>,
    profile_ids: HashMap<String, Vec<String>>,
) -> HashMap<String, String> {
    let jobs = targets
        .into_iter()
        .filter(|(_, base_url)| !base_url.trim().is_empty())
        .map(|(site_id, base_url)| {
            let client = client.clone();
            tauri::async_runtime::spawn(async move {
                let (system_type, challenge) =
                    probe_site_system_type_details(&client, &base_url).await;
                (site_id, base_url, system_type, challenge)
            })
        })
        .collect::<Vec<_>>();

    let mut detected = HashMap::new();
    let mut challenge_targets = Vec::new();
    for job in jobs {
        if let Ok((site_id, base_url, system_type, challenge)) = job.await {
            if let Some(system_type) = system_type {
                detected.insert(site_id, system_type);
            } else if challenge {
                challenge_targets.push((site_id, base_url));
            }
        }
    }
    for (site_id, base_url) in challenge_targets {
        if let Some(system_type) = probe_site_system_type_via_chrome(
            &base_url,
            profile_ids
                .get(&site_id)
                .map(Vec::as_slice)
                .unwrap_or_default(),
        )
        .await
        {
            detected.insert(site_id, system_type);
        }
    }
    detected
}

pub(crate) fn normalize_remote_url(value: &str, base_url: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }

    let parsed = Url::parse(value).ok().or_else(|| {
        if value.starts_with('/') {
            Url::parse(base_url).ok()?.join(value).ok()
        } else {
            None
        }
    });

    parsed
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
        .unwrap_or_default()
}

pub(crate) fn normalize_remote_site(mut site: SiteRecord) -> Result<SiteRecord, String> {
    let base_url = site.api_base_url.trim().to_string();
    site.checkin_url = normalize_remote_url(&site.checkin_url, &base_url);
    site.benefit_url = normalize_remote_url(&site.benefit_url, &base_url);
    site.status_url = normalize_remote_url(&site.status_url, &base_url);

    for maintainer in &mut site.maintainers {
        maintainer.profile_url = normalize_remote_url(&maintainer.profile_url, &base_url);
    }
    site.extension_links = site
        .extension_links
        .into_iter()
        .filter_map(|mut link| {
            link.url = normalize_remote_url(&link.url, &base_url);
            (!link.url.is_empty()).then_some(link)
        })
        .collect();

    normalize_site(site)
}

pub(crate) fn generated_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("local-{nanos:x}")
}
