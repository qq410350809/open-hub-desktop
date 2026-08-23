use crate::models::Database;
use crate::proxypool::types::ParsedNode;
use base64::{engine::general_purpose, Engine as _};
use rusqlite::params;
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use url::Url;

pub fn stable_id(parts: &[&str]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    hex::encode(digest.finalize())
}

pub fn canonical_json(value: &JsonValue, remove_name: bool) -> JsonValue {
    match value {
        JsonValue::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            let mut result = JsonMap::new();
            for key in keys {
                if remove_name && key == "name" {
                    continue;
                }
                result.insert(key.clone(), canonical_json(&map[key], false));
            }
            JsonValue::Object(result)
        }
        JsonValue::Array(items) => JsonValue::Array(
            items
                .iter()
                .map(|item| canonical_json(item, false))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn speed_value_end(lower: &str, from: usize) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut index = from;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == from {
        return None;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    let rest = &lower[index..];
    let after_space = rest.trim_start_matches(' ');
    let space_len = rest.len() - after_space.len();
    let units = [
        "mb/s", "kb/s", "gb/s", "tb/s", "mbps", "kbps", "gbps", "tbps", "mbs", "kbs", "gbs", "tbs",
        "mbit/s", "kbit/s", "gbit/s", "tbit/s", "mbits/s", "kbits/s", "gbits/s", "tbits/s",
        "mb/秒", "kb/秒", "gb/秒", "tb/秒", "m/s", "k/s", "g/s", "t/s", "m/秒", "k/秒", "g/秒",
        "t/秒",
    ];
    units
        .iter()
        .find(|unit| after_space.starts_with(**unit))
        .map(|unit| index + space_len + unit.len())
}

pub fn speed_result_start(lower: &str, original: &str) -> Option<usize> {
    let bytes = lower.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let separated = index == 0
            || original[..index]
                .chars()
                .next_back()
                .is_some_and(|character| {
                    matches!(
                        character,
                        '|' | '｜'
                            | '·'
                            | '•'
                            | '-'
                            | '—'
                            | ':'
                            | '：'
                            | '/'
                            | '('
                            | '（'
                            | '['
                            | '【'
                            | ' '
                            | '\t'
                    )
                });
        if separated && speed_value_end(lower, index).is_some() {
            return Some(index);
        }
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index < bytes.len() && bytes[index] == b'.' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
    }
    None
}

pub fn clean_node_name(name: &str) -> String {
    let lower = name.to_lowercase();
    let markers = [
        "测速结果",
        "测速",
        "速度测试",
        "延迟测试",
        "时延",
        "延迟",
        "速度",
        "speedtest",
        "speed test",
        "latency",
        "rtt",
        "ping",
    ];
    let mut cut = name.len();
    for marker in markers {
        let mut offset = 0;
        while let Some(found) = lower[offset..].find(marker) {
            let index = offset + found;
            let separated = name[..index]
                .chars()
                .next_back()
                .map(|character| {
                    matches!(
                        character,
                        '|' | '｜'
                            | '·'
                            | '•'
                            | '-'
                            | '—'
                            | ':'
                            | '：'
                            | '/'
                            | '('
                            | '（'
                            | '['
                            | '【'
                            | ' '
                            | '\t'
                    )
                })
                .unwrap_or(true);
            if separated {
                cut = cut.min(index);
                break;
            }
            offset = index + marker.len();
        }
    }
    if let Some(speed_cut) = speed_result_start(&lower, name) {
        cut = cut.min(speed_cut);
    }
    let cleaned = name[..cut].trim_end_matches(|character: char| {
        matches!(
            character,
            '|' | '｜'
                | '·'
                | '•'
                | '-'
                | '—'
                | ':'
                | '：'
                | '/'
                | '('
                | '（'
                | '['
                | '【'
                | ' '
                | '\t'
        )
    });
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        name.trim().to_string()
    } else {
        cleaned.to_string()
    }
}

pub fn sanitize_proxy_node_json(value: &mut JsonValue) {
    let Some(object) = value.as_object_mut() else {
        return;
    };

    if let Some(alpn_val) = object.get("alpn") {
        let items: Vec<JsonValue> = match alpn_val {
            JsonValue::String(s) => s
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| JsonValue::String(item.to_string()))
                .collect(),
            JsonValue::Array(arr) => arr
                .iter()
                .filter_map(JsonValue::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(|item| JsonValue::String(item.to_string()))
                .collect(),
            _ => Vec::new(),
        };
        if items.is_empty() {
            object.remove("alpn");
        } else {
            object.insert("alpn".to_string(), JsonValue::Array(items));
        }
    }

    if let Some(tls_val) = object.get("tls") {
        if let Some(tls_str) = tls_val.as_str() {
            let is_tls = !tls_str.is_empty()
                && !tls_str.eq_ignore_ascii_case("none")
                && !tls_str.eq_ignore_ascii_case("false")
                && !tls_str.eq_ignore_ascii_case("0");
            object.insert("tls".to_string(), JsonValue::Bool(is_tls));
        }
    }

    if let Some(skip_val) = object.get("skip-cert-verify") {
        if let Some(skip_str) = skip_val.as_str() {
            let is_skip = skip_str.eq_ignore_ascii_case("true") || skip_str == "1";
            object.insert("skip-cert-verify".to_string(), JsonValue::Bool(is_skip));
        }
    }

    if let Some(udp_val) = object.get("udp") {
        if let Some(udp_str) = udp_val.as_str() {
            let is_udp = udp_str.eq_ignore_ascii_case("true") || udp_str == "1";
            object.insert("udp".to_string(), JsonValue::Bool(is_udp));
        }
    }

    if let Some(port_val) = object.get("port") {
        if let Some(port_str) = port_val.as_str() {
            if let Ok(port_num) = port_str.parse::<i64>() {
                object.insert("port".to_string(), json!(port_num));
            }
        }
    }
}

pub fn node_from_json(mut value: JsonValue) -> Option<ParsedNode> {
    sanitize_proxy_node_json(&mut value);
    let object = value.as_object_mut()?;
    let name = clean_node_name(object.get("name")?.as_str()?.trim());
    let proxy_type = object.get("type")?.as_str()?.trim().to_ascii_lowercase();
    if name.is_empty() || proxy_type.is_empty() {
        return None;
    }
    let server = object
        .get("server")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let port = object
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    let cipher = object
        .get("cipher")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    let udp = object
        .get("udp")
        .and_then(JsonValue::as_bool)
        .unwrap_or(false);
    let canonical = canonical_json(&value, true).to_string();
    Some(ParsedNode {
        id: stable_id(&["proxy-node", &canonical]),
        name,
        proxy_type,
        server,
        port,
        cipher,
        udp,
        raw_json: value,
    })
}

pub fn unique_name(base: &str, used: &mut HashSet<String>) -> String {
    if used.insert(base.to_string()) {
        return base.to_string();
    }
    for index in 2..10000 {
        let candidate = format!("{base} [{index}]");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    format!("{base} [{}]", stable_id(&[base])[..6].to_string())
}

pub fn repair_stored_node_names(database: &Database) -> Result<usize, String> {
    let connection = database.lock_conn()?;
    let rows = connection
        .prepare("SELECT id, name, raw_json FROM proxy_pool_nodes ORDER BY name COLLATE NOCASE")
        .map_err(|error| error.to_string())?
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Ok(0);
    }
    let mut used_names = rows
        .iter()
        .map(|(_, name, _)| name.clone())
        .collect::<HashSet<_>>();
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| error.to_string())?;
    let mut repaired = 0usize;
    for (id, name, raw_json) in rows {
        let cleaned = clean_node_name(&name);
        let mut config: JsonValue = serde_json::from_str(&raw_json).unwrap_or(JsonValue::Null);
        let orig_config_str = config.to_string();
        sanitize_proxy_node_json(&mut config);

        let need_name_update = cleaned != name;
        let need_json_update = config.to_string() != orig_config_str;

        if !need_name_update && !need_json_update {
            continue;
        }

        let final_name = if need_name_update {
            unique_name(&cleaned, &mut used_names)
        } else {
            name.clone()
        };

        if let Some(object) = config.as_object_mut() {
            object.insert("name".into(), JsonValue::String(final_name.clone()));
        }
        transaction
            .execute(
                "UPDATE proxy_pool_nodes SET name = ?2, raw_json = ?3 WHERE id = ?1",
                params![id, final_name, config.to_string()],
            )
            .map_err(|error| error.to_string())?;
        repaired += 1;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(repaired)
}

pub fn parse_clash_document(body: &str) -> Option<Vec<ParsedNode>> {
    let yaml: serde_yaml::Value = serde_yaml::from_str(body).ok()?;
    let value = serde_json::to_value(yaml).ok()?;
    let proxies = value.get("proxies")?.as_array()?;
    let nodes = proxies
        .iter()
        .filter_map(|item| node_from_json(item.clone()))
        .collect::<Vec<_>>();
    if nodes.is_empty() {
        None
    } else {
        Some(nodes)
    }
}

pub fn decode_base64(value: &str) -> Option<Vec<u8>> {
    let compact = value
        .chars()
        .filter(|char| !char.is_whitespace())
        .collect::<String>();
    for engine in [
        &general_purpose::STANDARD,
        &general_purpose::STANDARD_NO_PAD,
        &general_purpose::URL_SAFE,
        &general_purpose::URL_SAFE_NO_PAD,
    ] {
        if let Ok(decoded) = engine.decode(&compact) {
            return Some(decoded);
        }
    }
    None
}

pub fn decoded_fragment(url: &Url, fallback: &str) -> String {
    url.fragment()
        .and_then(|value| {
            percent_encoding::percent_decode_str(value)
                .decode_utf8()
                .ok()
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

pub fn parse_vmess(line: &str) -> Option<ParsedNode> {
    let decoded = decode_base64(line.strip_prefix("vmess://")?)?;
    let source: JsonValue = serde_json::from_slice(&decoded).ok()?;
    let server = source.get("add").and_then(JsonValue::as_str)?.to_string();
    let port = source
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    let name = source
        .get("ps")
        .and_then(JsonValue::as_str)
        .filter(|item| !item.trim().is_empty())
        .unwrap_or("VMess")
        .to_string();
    let network = source
        .get("net")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("tcp")
        .to_ascii_lowercase();
    let host = source
        .get("host")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or_default()
        .to_string();
    let path = source
        .get("path")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .unwrap_or("/")
        .to_string();
    let mut object = json!({
        "name": name,
        "type": "vmess",
        "server": server,
        "port": port,
        "uuid": source.get("id").and_then(JsonValue::as_str).unwrap_or_default(),
        "alterId": source.get("aid").and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok())).unwrap_or(0),
        "cipher": source.get("scy").and_then(JsonValue::as_str).filter(|item| !item.trim().is_empty()).unwrap_or("auto"),
        "network": network,
        "udp": true
    });
    match network.as_str() {
        "ws" => {
            let mut ws_opts = json!({ "path": path });
            if !host.is_empty() {
                ws_opts["headers"] = json!({ "Host": host });
            }
            object["ws-opts"] = ws_opts;
        }
        "h2" | "http" => {
            let mut opts = json!({ "path": [path] });
            if !host.is_empty() {
                opts["host"] = json!([host]);
            }
            object["h2-opts"] = opts;
        }
        "grpc" => {
            object["grpc-opts"] = json!({
                "grpc-service-name": path.trim_start_matches('/'),
            });
        }
        _ => {}
    }
    let tls = source
        .get("tls")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .unwrap_or_default();
    if !tls.is_empty() && !tls.eq_ignore_ascii_case("none") {
        object["tls"] = JsonValue::Bool(true);
        let sni = source
            .get("sni")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .unwrap_or(host.as_str());
        if !sni.is_empty() {
            object["servername"] = json!(sni);
        }
        if let Some(alpn) = source.get("alpn").and_then(JsonValue::as_str) {
            let values = alpn
                .split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if !values.is_empty() {
                object["alpn"] = json!(values);
            }
        }
    }
    node_from_json(object)
}

pub fn split_userinfo_host_port(value: &str) -> Option<(String, String, String, i64)> {
    let (userinfo, hostport) = value.rsplit_once('@')?;
    let (server, port_text) = hostport.rsplit_once(':')?;
    let port = port_text.parse::<i64>().ok()?;
    if !(1..=65535).contains(&port) || server.trim().is_empty() {
        return None;
    }
    let (username, password) = match userinfo.split_once(':') {
        Some((user, pass)) => (user.to_string(), pass.to_string()),
        None => (userinfo.to_string(), String::new()),
    };
    Some((username, password, server.trim().to_string(), port))
}

pub fn parse_encoded_userinfo_host_port(encoded: &str) -> Option<(String, String, String, i64)> {
    let decoded = decode_base64(encoded).and_then(|bytes| String::from_utf8(bytes).ok())?;
    split_userinfo_host_port(decoded.trim())
}

pub fn parse_ssocks(line: &str) -> Option<ParsedNode> {
    let rest = line.strip_prefix("ssocks://")?;
    let (payload, query) = rest
        .split_once('?')
        .map(|(left, right)| (left, right))
        .unwrap_or((rest, ""));
    let (username, password, server, port) = parse_encoded_userinfo_host_port(payload)?;
    let params = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<HashMap<_, _>>();
    let name = params
        .get("remarks")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Socks5")
        .to_string();
    node_from_json(json!({
        "name": name,
        "type": "socks5",
        "server": server,
        "port": port,
        "username": username,
        "password": password,
        "udp": true
    }))
}

pub fn parse_https_or_http_proxy_uri(line: &str) -> Option<ParsedNode> {
    let lower = line.to_ascii_lowercase();
    let is_https = lower.starts_with("https://");
    let is_http = lower.starts_with("http://");
    if !is_https && !is_http {
        return None;
    }

    if let Some(rest) = line
        .strip_prefix("https://")
        .or_else(|| line.strip_prefix("http://"))
        .or_else(|| line.strip_prefix("HTTPS://"))
        .or_else(|| line.strip_prefix("HTTP://"))
    {
        let (payload, fragment) = rest
            .split_once('#')
            .map(|(left, right)| (left, Some(right)))
            .unwrap_or((rest, None));
        if !payload.contains('@') {
            if let Some((username, password, server, port)) =
                parse_encoded_userinfo_host_port(payload)
            {
                let name = fragment
                    .and_then(|value| {
                        percent_encoding::percent_decode_str(value)
                            .decode_utf8()
                            .ok()
                    })
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| {
                        format!(
                            "{} {server}:{port}",
                            if is_https { "HTTPS" } else { "HTTP" }
                        )
                    });
                return node_from_json(json!({
                    "name": name,
                    "type": "http",
                    "server": server,
                    "port": port,
                    "username": username,
                    "password": password,
                    "tls": is_https
                }));
            }
        }
    }

    let url = Url::parse(line).ok()?;
    let server = url.host_str()?.to_string();
    let port = url
        .port()
        .or_else(|| if is_https { Some(443) } else { Some(80) })? as i64;
    let fallback = format!(
        "{} {server}:{port}",
        if is_https { "HTTPS" } else { "HTTP" }
    );
    let name = decoded_fragment(&url, &fallback);
    node_from_json(json!({
        "name": name,
        "type": "http",
        "server": server,
        "port": port,
        "username": url.username(),
        "password": url.password().unwrap_or_default(),
        "tls": is_https
    }))
}

pub fn parse_uri_node(line: &str) -> Option<ParsedNode> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if line.starts_with("vmess://") {
        return parse_vmess(line);
    }
    if line.starts_with("ssocks://") {
        return parse_ssocks(line);
    }
    if line.starts_with("http://")
        || line.starts_with("https://")
        || line.starts_with("HTTP://")
        || line.starts_with("HTTPS://")
    {
        return parse_https_or_http_proxy_uri(line);
    }

    let url = Url::parse(line).ok()?;
    let scheme = url.scheme().to_ascii_lowercase();
    let server = url.host_str()?.to_string();
    let port = url.port()? as i64;
    let fallback = format!("{} {}:{}", scheme.to_ascii_uppercase(), server, port);
    let name = decoded_fragment(&url, &fallback);
    let query = url.query_pairs().collect::<HashMap<_, _>>();
    let object = match scheme.as_str() {
        "trojan" => {
            let mut obj = json!({
                "name": name, "type": "trojan", "server": server, "port": port,
                "password": url.username(), "sni": query.get("sni").or_else(|| query.get("peer")).map(|v| v.as_ref()).unwrap_or(&server),
                "skip-cert-verify": query.get("allowInsecure").is_some_and(|v| v == "1" || v == "true"), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "anytls" => json!({
            "name": name, "type": "anytls", "server": server, "port": port,
            "password": url.username(),
            "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server),
            "udp": true
        }),
        "vless" => {
            let mut obj = json!({
                "name": name, "type": "vless", "server": server, "port": port,
                "uuid": url.username(), "tls": query.get("security").is_some_and(|v| v == "tls" || v == "reality"),
                "servername": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "hysteria2" | "hy2" => {
            let mut obj = json!({
                "name": name, "type": "hysteria2", "server": server, "port": port,
                "password": url.username(), "sni": query.get("sni").map(|v| v.as_ref()).unwrap_or(&server), "udp": true
            });
            if let Some(alpn) = query.get("alpn") {
                let items: Vec<String> = alpn
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
                    .collect();
                if !items.is_empty() {
                    obj["alpn"] = json!(items);
                }
            }
            obj
        }
        "tuic" => {
            let password = url.password().unwrap_or_default().to_string();
            let uuid = url.username().to_string();
            let alpn = query
                .get("alpn")
                .map(|v| {
                    v.split(',')
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(|item| item.to_string())
                        .collect::<Vec<_>>()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| vec!["h3".to_string()]);
            json!({
                "name": name,
                "type": "tuic",
                "server": server,
                "port": port,
                "uuid": uuid,
                "password": password,
                "alpn": alpn,
                "congestion-controller": query
                    .get("congestion_control")
                    .or_else(|| query.get("congestion-controller"))
                    .map(|v| v.as_ref())
                    .unwrap_or("bbr"),
                "udp": true
            })
        }
        "socks" | "socks5" => json!({
            "name": name, "type": "socks5", "server": server, "port": port,
            "username": url.username(), "password": url.password().unwrap_or_default(), "udp": true
        }),
        "ss" => {
            let mut cipher = query
                .get("method")
                .map(|v| v.to_string())
                .unwrap_or_default();
            let mut password = url.password().unwrap_or_default().to_string();
            let username = url.username();
            if let Some(decoded) =
                decode_base64(username).and_then(|bytes| String::from_utf8(bytes).ok())
            {
                if let Some((method, secret)) = decoded.split_once(':') {
                    cipher = method.to_string();
                    password = secret.to_string();
                }
            }
            json!({ "name": name, "type": "ss", "server": server, "port": port, "cipher": cipher, "password": password, "udp": true })
        }
        _ => return None,
    };
    node_from_json(object)
}

pub fn parse_subscription(body: &str) -> Result<Vec<ParsedNode>, String> {
    if let Some(nodes) = parse_clash_document(body) {
        return Ok(nodes);
    }
    if let Some(decoded) = decode_base64(body).and_then(|bytes| String::from_utf8(bytes).ok()) {
        if let Some(nodes) = parse_clash_document(&decoded) {
            return Ok(nodes);
        }
        let nodes = decoded
            .lines()
            .filter_map(|line| parse_uri_node(line.trim()))
            .collect::<Vec<_>>();
        if !nodes.is_empty() {
            return Ok(nodes);
        }
    }
    let nodes = body
        .lines()
        .filter_map(|line| parse_uri_node(line.trim()))
        .collect::<Vec<_>>();
    if !nodes.is_empty() {
        return Ok(nodes);
    }
    Err("没有找到可识别的代理节点；支持 Clash YAML、Base64 订阅和常见代理节点链接".into())
}

pub fn validate_source(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("请输入订阅地址或代理节点链接".into());
    }
    if value.lines().count() > 1 {
        if value
            .lines()
            .filter(|line| !line.trim().is_empty())
            .all(|line| parse_uri_node(line.trim()).is_some())
        {
            return Ok(value.to_string());
        }
        return Err("多行导入时，每一行都必须是代理节点链接".into());
    }
    let url = Url::parse(value).map_err(|_| "链接格式无效".to_string())?;
    if matches!(url.scheme(), "http" | "https") || parse_uri_node(value).is_some() {
        Ok(value.to_string())
    } else {
        Err("仅支持 HTTP(S) 订阅地址或代理节点链接".into())
    }
}

pub fn required_text<'a>(value: &'a JsonValue, key: &str) -> &'a str {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

pub fn basic_node_config_error(value: &JsonValue) -> Option<String> {
    let proxy_type = required_text(value, "type").to_ascii_lowercase();
    let port = value
        .get("port")
        .and_then(|item| item.as_i64().or_else(|| item.as_str()?.parse().ok()))
        .unwrap_or_default();
    if required_text(value, "name").is_empty()
        || required_text(value, "server").is_empty()
        || !(1..=65535).contains(&port)
    {
        return Some("节点缺少名称、服务器或端口".into());
    }
    let missing = match proxy_type.as_str() {
        "ss" => {
            required_text(value, "cipher").is_empty() || required_text(value, "password").is_empty()
        }
        "vmess" | "vless" => required_text(value, "uuid").is_empty(),
        "trojan" | "anytls" => required_text(value, "password").is_empty(),
        "tuic" => {
            required_text(value, "uuid").is_empty() || required_text(value, "password").is_empty()
        }
        "hysteria2" => {
            required_text(value, "password").is_empty() && required_text(value, "auth").is_empty()
        }
        "http" | "socks5" => false,
        _ => false,
    };
    missing.then(|| format!("{proxy_type} 节点缺少必要的认证或加密参数"))
}
