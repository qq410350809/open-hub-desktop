use crate::models::Database;
use crate::proxy_pool::types::ProxyRuntime;
use maxminddb::{geoip2, Reader};
use rusqlite::params;
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;

pub fn classify_ip(ip: IpAddr) -> &'static str {
    match ip {
        IpAddr::V4(value) => {
            if value.is_loopback() {
                "loopback"
            } else if value.is_private() {
                "private"
            } else if value.is_link_local() {
                "linkLocal"
            } else if value.is_unspecified() {
                "unspecified"
            } else {
                "public"
            }
        }
        IpAddr::V6(value) => {
            let first = value.segments()[0];
            if value.is_loopback() {
                "loopback"
            } else if value.is_unspecified() {
                "unspecified"
            } else if value.is_unicast_link_local() {
                "linkLocal"
            } else if first & 0xfe00 == 0xfc00 {
                "private"
            } else {
                "public"
            }
        }
    }
}

pub fn classify_node_location(
    name: &str,
    server: &str,
    port: i64,
    geoip_reader: Option<&Reader<Vec<u8>>>,
) -> (String, String, String, String) {
    let _ = port;
    if let Ok(ip) = server.parse::<IpAddr>() {
        let classification = classify_ip(ip).to_string();
        if classification == "local" {
            return (
                "LOCAL".to_string(),
                "本地网络".to_string(),
                classification,
                ip.to_string(),
            );
        }
        if let Some(reader) = geoip_reader {
            if let Some((code, country_name)) = geoip_country(reader, ip) {
                return (code, country_name, classification, ip.to_string());
            }
        }
        if let Some((code, country_name)) = inferred_country(name) {
            return (code, country_name, classification, ip.to_string());
        }
        return (
            "ZZ".to_string(),
            "未知地区".to_string(),
            classification,
            ip.to_string(),
        );
    }

    if let Some((code, country_name)) = inferred_country(name) {
        return (code, country_name, "public".to_string(), String::new());
    }
    if let Some((code, country_name)) = inferred_country(server) {
        return (code, country_name, "public".to_string(), String::new());
    }

    (
        "ZZ".to_string(),
        "未知地区".to_string(),
        "unresolved".to_string(),
        String::new(),
    )
}

pub fn find_geoip_database(runtime: &ProxyRuntime) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("OPENHUB_GEOIP_DB") {
        candidates.push(PathBuf::from(path));
    }
    candidates.extend([
        runtime.directory.join("Country.mmdb"),
        runtime.directory.join("country.mmdb"),
        runtime.directory.join("GeoLite2-Country.mmdb"),
    ]);
    if let Some(parent) = runtime.directory.parent() {
        candidates.extend([
            parent.join("Country.mmdb"),
            parent.join("country.mmdb"),
            parent.join("GeoLite2-Country.mmdb"),
            parent.join("bin").join("Country.mmdb"),
            parent.join("bin").join("country.mmdb"),
        ]);
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.extend([
            home.join("Library/Application Support/com.dfeer.openhub.desktop/Country.mmdb"),
            home.join(".config/com.dfeer.openhub.desktop/Country.mmdb"),
            home.join("Library/Application Support/io.github.clash-verge-rev.clash-verge-rev/Country.mmdb"),
            home.join("Library/Application Support/mihomo-party/work/country.mmdb"),
            home.join("Library/Application Support/mihomo-party/test/country.mmdb"),
            home.join(".config/mihomo/Country.mmdb"),
            home.join(".config/clash/Country.mmdb"),
            home.join(".local/share/GeoIP/GeoLite2-Country.mmdb"),
        ]);
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/share/GeoIP/GeoLite2-Country.mmdb"),
        PathBuf::from("/usr/local/share/GeoIP/GeoLite2-Country.mmdb"),
        PathBuf::from("/usr/share/GeoIP/GeoLite2-Country.mmdb"),
    ]);
    candidates.into_iter().find(|path| path.is_file())
}

pub fn open_geoip_reader(runtime: &ProxyRuntime) -> Option<Reader<Vec<u8>>> {
    let path = find_geoip_database(runtime)?;
    Reader::open_readfile(path).ok()
}

pub fn repair_node_locations_with_geoip(
    database: &Database,
    runtime: &ProxyRuntime,
) -> Result<usize, String> {
    let geoip_reader = match open_geoip_reader(runtime) {
        Some(r) => r,
        None => return Ok(0),
    };

    let connection = database.lock_conn()?;
    let mut stmt = connection
        .prepare("SELECT id, name, server, port, country_code, country_name, classification, primary_ip FROM proxy_pool_nodes")
        .map_err(|e| e.to_string())?;

    #[allow(dead_code)]
    struct Candidate {
        id: String,
        name: String,
        server: String,
        port: i64,
        country_code: String,
        country_name: String,
        classification: String,
        primary_ip: String,
    }

    let rows = stmt
        .query_map([], |row| {
            Ok(Candidate {
                id: row.get(0)?,
                name: row.get(1)?,
                server: row.get(2)?,
                port: row.get(3)?,
                country_code: row.get(4)?,
                country_name: row.get(5)?,
                classification: row.get(6)?,
                primary_ip: row.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let mut updated = 0;
    for c in rows {
        let is_unknown = c.country_code.trim().is_empty() || c.country_code == "ZZ";
        let (code, name, class, ip) = classify_node_location(&c.name, &c.server, c.port, Some(&geoip_reader));
        if code != "ZZ" && (is_unknown || c.country_code != code) {
            let _ = connection.execute(
                "UPDATE proxy_pool_nodes
                 SET country_code=?2, country_name=?3, classification=?4,
                     primary_ip=CASE WHEN ?5 != '' THEN ?5 ELSE primary_ip END
                 WHERE id=?1",
                params![c.id, code, name, class, ip],
            );
            updated += 1;
        }
    }

    Ok(updated)
}

pub fn geoip_country(reader: &Reader<Vec<u8>>, ip: IpAddr) -> Option<(String, String)> {
    let result = reader.lookup(ip).ok()?;
    let record = result.decode::<geoip2::Country>().ok()??;
    for country in [&record.country, &record.registered_country] {
        let Some(raw_code) = country.iso_code else {
            continue;
        };
        let code = raw_code.trim().to_ascii_uppercase();
        if code.len() != 2
            || !code
                .chars()
                .all(|character| character.is_ascii_alphabetic())
        {
            continue;
        }
        let name = country
            .names
            .simplified_chinese
            .or(country.names.english)
            .unwrap_or(code.as_str())
            .to_string();
        return Some((code, name));
    }
    None
}

pub fn inferred_country(value: &str) -> Option<(String, String)> {
    let lower = value.to_ascii_lowercase();
    let text_matches = [
        ("🇭🇰", "HK", "香港"),
        ("香港", "HK", "香港"),
        ("hong kong", "HK", "香港"),
        ("🇹🇼", "TW", "台湾"),
        ("台湾", "TW", "台湾"),
        ("taiwan", "TW", "台湾"),
        ("🇯🇵", "JP", "日本"),
        ("日本", "JP", "日本"),
        ("japan", "JP", "日本"),
        ("东京", "JP", "日本"),
        ("大阪", "JP", "日本"),
        ("🇸🇬", "SG", "新加坡"),
        ("新加坡", "SG", "新加坡"),
        ("singapore", "SG", "新加坡"),
        ("狮城", "SG", "新加坡"),
        ("🇺🇸", "US", "美国"),
        ("美国", "US", "美国"),
        ("united states", "US", "美国"),
        ("洛杉矶", "US", "美国"),
        ("硅谷", "US", "美国"),
        ("🇰🇷", "KR", "韩国"),
        ("韩国", "KR", "韩国"),
        ("korea", "KR", "韩国"),
        ("首尔", "KR", "韩国"),
        ("🇬🇧", "GB", "英国"),
        ("英国", "GB", "英国"),
        ("united kingdom", "GB", "英国"),
        ("london", "GB", "英国"),
        ("🇩🇪", "DE", "德国"),
        ("德国", "DE", "德国"),
        ("germany", "DE", "德国"),
        ("🇫🇷", "FR", "法国"),
        ("法国", "FR", "法国"),
        ("france", "FR", "法国"),
        ("🇳🇱", "NL", "荷兰"),
        ("荷兰", "NL", "荷兰"),
        ("netherlands", "NL", "荷兰"),
        ("🇨🇦", "CA", "加拿大"),
        ("加拿大", "CA", "加拿大"),
        ("canada", "CA", "加拿大"),
        ("🇦🇺", "AU", "澳大利亚"),
        ("澳大利亚", "AU", "澳大利亚"),
        ("australia", "AU", "澳大利亚"),
        ("🇷🇺", "RU", "俄罗斯"),
        ("俄罗斯", "RU", "俄罗斯"),
        ("russia", "RU", "俄罗斯"),
        ("🇮🇳", "IN", "印度"),
        ("印度", "IN", "印度"),
        ("india", "IN", "印度"),
        ("🇹🇷", "TR", "土耳其"),
        ("土耳其", "TR", "土耳其"),
        ("turkey", "TR", "土耳其"),
        ("🇧🇷", "BR", "巴西"),
        ("巴西", "BR", "巴西"),
        ("brazil", "BR", "巴西"),
        ("🇲🇾", "MY", "马来西亚"),
        ("马来西亚", "MY", "马来西亚"),
        ("malaysia", "MY", "马来西亚"),
        ("🇹🇭", "TH", "泰国"),
        ("泰国", "TH", "泰国"),
        ("thailand", "TH", "泰国"),
        ("🇻🇳", "VN", "越南"),
        ("越南", "VN", "越南"),
        ("vietnam", "VN", "越南"),
        ("🇵🇭", "PH", "菲律宾"),
        ("菲律宾", "PH", "菲律宾"),
        ("philippines", "PH", "菲律宾"),
        ("🇮🇩", "ID", "印度尼西亚"),
        ("印度尼西亚", "ID", "印度尼西亚"),
        ("indonesia", "ID", "印度尼西亚"),
        ("🇦🇪", "AE", "阿联酋"),
        ("阿联酋", "AE", "阿联酋"),
        ("dubai", "AE", "阿联酋"),
    ];
    for (pattern, code, name) in text_matches {
        if lower.contains(&pattern.to_ascii_lowercase()) {
            return Some((code.to_string(), name.to_string()));
        }
    }
    let tokens = value
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|item| !item.is_empty())
        .map(|item| item.to_ascii_uppercase())
        .collect::<HashSet<_>>();
    let codes = [
        ("HK", "HK", "香港"),
        ("TW", "TW", "台湾"),
        ("JP", "JP", "日本"),
        ("SG", "SG", "新加坡"),
        ("US", "US", "美国"),
        ("USA", "US", "美国"),
        ("KR", "KR", "韩国"),
        ("UK", "GB", "英国"),
        ("GB", "GB", "英国"),
        ("DE", "DE", "德国"),
        ("FR", "FR", "法国"),
        ("NL", "NL", "荷兰"),
        ("CA", "CA", "加拿大"),
        ("AU", "AU", "澳大利亚"),
        ("RU", "RU", "俄罗斯"),
        ("IN", "IN", "印度"),
        ("TR", "TR", "土耳其"),
        ("BR", "BR", "巴西"),
        ("MY", "MY", "马来西亚"),
        ("TH", "TH", "泰国"),
        ("VN", "VN", "越南"),
        ("PH", "PH", "菲律宾"),
        ("ID", "ID", "印度尼西亚"),
        ("AE", "AE", "阿联酋"),
    ];
    codes
        .into_iter()
        .find(|(token, _, _)| tokens.contains(*token))
        .map(|(_, code, name)| (code.to_string(), name.to_string()))
}
