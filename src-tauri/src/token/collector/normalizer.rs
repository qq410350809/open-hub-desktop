use serde_json::Value as JsonValue;
use std::fs;
use std::path::{Path, PathBuf};

pub fn is_common_subfolder(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "src"
            | "src-tauri"
            | "docs"
            | "target"
            | "bin"
            | "node_modules"
            | "pkg"
            | "app"
            | "core"
            | "client"
            | "server"
            | "ui"
            | "web"
            | "sys"
            | "staff"
            | "third"
            | "controller"
            | "controllers"
            | "model"
            | "models"
            | "view"
            | "views"
            | "service"
            | "services"
            | "scripts"
            | "frontend"
            | "backend"
            | "dist"
            | "build"
            | "test"
            | "tests"
            | "public"
            | "custom"
            | "applications"
    )
}

pub fn is_session_uuid(s: &str) -> bool {
    let s = s.trim();
    if s.len() == 36
        && s.as_bytes()[8] == b'-'
        && s.as_bytes()[13] == b'-'
        && s.as_bytes()[18] == b'-'
        && s.as_bytes()[23] == b'-'
    {
        return s.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    }
    false
}

pub fn find_project_root_from_path(path: &Path) -> Option<String> {
    let current = if path.is_file() { path.parent()? } else { path };

    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut top_matched: Option<String> = None;
    let mut curr_opt: Option<&Path> = Some(current);

    while let Some(dir) = curr_opt {
        if dir.as_os_str().is_empty()
            || dir == Path::new("/")
            || dir == Path::new("/Applications")
            || dir == Path::new("/Applications/custom")
        {
            break;
        }
        if let Some(ref h) = home {
            if dir == h {
                break;
            }
        }

        let has_marker = dir.join(".git").exists()
            || dir.join("Cargo.toml").exists()
            || dir.join("package.json").exists()
            || dir.join("pom.xml").exists()
            || dir.join("go.mod").exists()
            || dir.join("pyproject.toml").exists()
            || dir.join("build.gradle").exists()
            || dir.join(".code-workspace").exists();

        let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if has_marker && !dir_name.is_empty() && !is_common_subfolder(dir_name) {
            top_matched = Some(dir_name.to_string());
        }

        curr_opt = dir.parent();
    }

    if let Some(name) = top_matched {
        return Some(name);
    }

    let mut p = current;
    while let Some(name) = p.file_name().and_then(|n| n.to_str()) {
        if is_common_subfolder(name) {
            if let Some(parent) = p.parent() {
                p = parent;
                continue;
            }
        }
        if !name.trim().is_empty() && name != "/" && name != "Users" && name != "Applications" {
            if let Some(ref h) = home {
                if p == h {
                    return None;
                }
            }
            return Some(name.to_string());
        }
        break;
    }

    None
}

pub fn normalize_workspace_project_key(raw: &str, fallback: &str) -> String {
    let s = raw.trim();
    if s.is_empty() {
        return fallback.to_string();
    }

    if is_session_uuid(s) || s.starts_with("session-") || s.starts_with("rollout-") {
        return "临时任务 / 独立会话".to_string();
    }

    let cleaned = if let Some(stripped) = s.strip_prefix("file://") {
        percent_encoding::percent_decode_str(stripped)
            .decode_utf8_lossy()
            .to_string()
    } else {
        s.to_string()
    };
    let cleaned = cleaned.trim().trim_end_matches(['/', '\\']);

    if cleaned.ends_with(".code-workspace") {
        let name = Path::new(&cleaned)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if !name.is_empty() {
            return name.to_string();
        }
    }

    if cleaned.starts_with('/')
        || cleaned.starts_with('\\')
        || (cleaned.len() >= 2 && cleaned.as_bytes()[1] == b':')
    {
        let path = Path::new(&cleaned);
        if let Some(root_name) = find_project_root_from_path(path) {
            return root_name;
        }
    }

    let name = Path::new(&cleaned)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&cleaned);

    if is_session_uuid(name) {
        return "临时任务 / 独立会话".to_string();
    }

    if is_common_subfolder(name)
        && !fallback.is_empty()
        && fallback != name
        && fallback != "Claude"
        && fallback != "Codex"
        && fallback != "Command Code"
        && fallback != "Antigravity"
        && fallback != "OpenCode"
        && fallback != "CatPawAI"
        && fallback != "Kiro"
        && fallback != "DSH"
    {
        return fallback.to_string();
    }

    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

pub fn decode_encoded_dash_path(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }
    if raw.contains("-copilot-chats-") || is_session_uuid(raw.trim_matches('-')) {
        return "临时任务 / 独立会话".to_string();
    }

    let candidate = if raw.starts_with('-') {
        format!("/{}", raw.trim_start_matches('-'))
    } else {
        raw.to_string()
    };

    let parts: Vec<&str> = candidate.split('/').filter(|s| !s.is_empty()).collect();
    if !parts.is_empty() {
        let sub_parts: Vec<&str> = parts[0].split('-').filter(|s| !s.is_empty()).collect();
        let mut curr_path = PathBuf::from("/");
        let mut idx = 0;
        while idx < sub_parts.len() {
            let mut matched = false;
            for end in (idx + 1..=sub_parts.len()).rev() {
                let segment = sub_parts[idx..end].join("-");
                let test_dir = curr_path.join(&segment);
                if test_dir.exists() {
                    curr_path = test_dir;
                    idx = end;
                    matched = true;
                    break;
                }
            }
            if !matched {
                curr_path = curr_path.join(sub_parts[idx..].join("-"));
                break;
            }
        }
        if curr_path.exists() {
            if let Some(root_name) = find_project_root_from_path(&curr_path) {
                return root_name;
            }
            if let Some(name) = curr_path.file_name().and_then(|n| n.to_str()) {
                if !is_common_subfolder(name) && name != "/" {
                    return name.to_string();
                }
            }
        }
    }

    let parts: Vec<&str> = raw.split('-').filter(|s| !s.is_empty()).collect();
    if let Some(&last) = parts.last() {
        if !is_common_subfolder(last) && !is_session_uuid(last) {
            return last.to_string();
        }
    }
    if parts.len() >= 2 {
        let tail_2 = format!("{}-{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        if !is_common_subfolder(&tail_2) {
            return tail_2;
        }
    }
    String::new()
}

pub fn basename_or_fallback(path: &str, fallback: &str) -> String {
    normalize_workspace_project_key(path, fallback)
}

pub fn claude_project_from_path(path: &Path) -> String {
    let mut parent = path.parent();
    while parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(|name| name == "subagents")
        .unwrap_or(false)
    {
        parent = parent.and_then(Path::parent);
    }
    let raw = parent
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let decoded = decode_encoded_dash_path(raw);
    if !decoded.is_empty() {
        decoded
    } else {
        "Claude".to_string()
    }
}

pub fn command_code_project_from_path(path: &Path) -> String {
    let raw = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let decoded = decode_encoded_dash_path(raw);
    if !decoded.is_empty() {
        decoded
    } else {
        "Command Code".to_string()
    }
}

pub fn vscode_workspace_project_from_path(path: &Path) -> String {
    if path
        .components()
        .any(|c| c.as_os_str() == "emptyWindowChatSessions")
    {
        return "临时任务 / 独立会话".to_string();
    }
    let mut parent = path.parent();
    while let Some(p) = parent {
        if p.file_name().and_then(|n| n.to_str()) == Some("chatSessions") {
            if let Some(ws_dir) = p.parent() {
                let ws_json = ws_dir.join("workspace.json");
                if let Ok(text) = fs::read_to_string(&ws_json) {
                    if let Ok(val) = serde_json::from_str::<JsonValue>(&text) {
                        if let Some(folder_uri) = val
                            .get("folder")
                            .or_else(|| val.get("workspace"))
                            .and_then(JsonValue::as_str)
                        {
                            let resolved = normalize_workspace_project_key(folder_uri, "VS Code");
                            if !resolved.is_empty() && resolved != "VS Code" {
                                return resolved;
                            }
                        }
                    }
                }
            }
            break;
        }
        parent = p.parent();
    }
    "VS Code".to_string()
}

pub fn extract_antigravity_project_from_transcript(text: &str) -> Option<String> {
    for line in text.lines().take(25) {
        if let Ok(val) = serde_json::from_str::<JsonValue>(line) {
            if let Some(content) = val.get("content").and_then(JsonValue::as_str) {
                if let Some(pos) = content.find("Active Document: ") {
                    let sub = &content[pos + "Active Document: ".len()..];
                    let path_str = sub.lines().next().unwrap_or("").trim();
                    let clean = path_str.split('(').next().unwrap_or(path_str).trim();
                    if !clean.is_empty() {
                        let resolved = normalize_workspace_project_key(clean, "");
                        if !resolved.is_empty() && !is_common_subfolder(&resolved) {
                            return Some(resolved);
                        }
                    }
                }
                if let Some(pos) = content.find(" -> ") {
                    let before = &content[..pos];
                    if let Some(line_start) = before.rfind('\n') {
                        let cand = before[line_start + 1..].trim();
                        if cand.starts_with('/') {
                            let resolved = normalize_workspace_project_key(cand, "");
                            if !resolved.is_empty() && !is_common_subfolder(&resolved) {
                                return Some(resolved);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

pub fn command_code_sidecar_model(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path.with_extension("meta.json")) else {
        return String::new();
    };
    serde_json::from_str::<JsonValue>(&text)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(JsonValue::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_default()
}
