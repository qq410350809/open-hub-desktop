use crate::models::{
    LocalAgentEnvOverride, LocalAgentPathEntry, LocalAgentPaths, LocalAgentPathsReport,
    RawConversation, RawRequest, RawSession,
};
use crate::token::stats::catpawai::catpawai_data_roots;
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub fn parse_claude_file(
    path: &Path,
    project: &str,
    sessions: &mut Vec<RawSession>,
    conversations: &mut Vec<RawConversation>,
    requests: &mut Vec<RawRequest>,
) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let number = |field: &JsonValue, key: &str| -> i64 {
        field
            .get(key)
            .and_then(JsonValue::as_f64)
            .map(|value| value as i64)
            .unwrap_or(0)
    };
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;
    let mut conv_index = 0i64;
    let mut session_tokens = 0i64;
    let mut current: Option<(RawConversation, String)> = None;
    let mut counted_message_ids: HashSet<String> = HashSet::new();

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let is_sidechain = value
            .get("isSidechain")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        if kind != "user" && kind != "assistant" {
            continue;
        }
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let uuid = value
            .get("uuid")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let msg_model = value
            .get("message")
            .and_then(|message| message.get("model"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        if model.is_empty() && !msg_model.is_empty() {
            model = msg_model.clone();
        }
        if first_ts.is_empty() {
            first_ts = ts.clone();
        }
        last_ts = ts.clone();
        message_count += 1;

        if kind == "user" {
            if is_sidechain {
                continue;
            }
            let content = value
                .get("message")
                .and_then(|message| message.get("content"))
                .cloned()
                .unwrap_or(JsonValue::Null);
            if !crate::token::collector::claude_user_line_is_human(&value, &content) {
                continue;
            }
            if let Some((conv, _)) = current.take() {
                conversations.push(conv);
            }
            conv_index += 1;
            current = Some((
                RawConversation {
                    id: format!("{session_id}#{conv_index}"),
                    session_id: session_id.clone(),
                    source: "claude".into(),
                    project: project.to_string(),
                    index: conv_index,
                    started_at: ts.clone(),
                    ..Default::default()
                },
                ts.clone(),
            ));
            continue;
        }

        let message_id = value
            .get("message")
            .and_then(|message| message.get("id"))
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        let usage = value
            .get("message")
            .and_then(|message| message.get("usage"));
        let Some(usage) = usage.filter(|u| u.is_object()) else {
            continue;
        };
        let input = number(usage, "input_tokens");
        let cache_read = number(usage, "cache_read_input_tokens");
        let cache_creation = number(usage, "cache_creation_input_tokens");
        let output = number(usage, "output_tokens");
        let total = input + cache_read + cache_creation + output;
        if total <= 0 {
            continue;
        }
        if !message_id.is_empty() && !counted_message_ids.insert(message_id.clone()) {
            continue;
        }
        session_tokens += total;
        if let Some((conv, conv_last)) = current.as_mut() {
            conv.request_count += 1;
            if !msg_model.is_empty() {
                conv.model = msg_model.clone();
            }
            if ts > *conv_last {
                *conv_last = ts.clone();
            }
            conv.total_tokens += total;
            requests.push(RawRequest {
                id: if message_id.is_empty() {
                    if uuid.is_empty() {
                        format!("{session_id}#{message_count}")
                    } else {
                        uuid
                    }
                } else {
                    message_id
                },
                session_id: session_id.clone(),
                conversation_id: conv.id.clone(),
                source: "claude".into(),
                timestamp: ts,
                role: kind.to_string(),
                model: msg_model,
                input_tokens: input,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
                output_tokens: output,
                total_tokens: total,
            });
        }
    }
    if let Some((mut conv, last)) = current.take() {
        conv.ended_at = last;
        conversations.push(conv);
    }
    sessions.push(RawSession {
        id: session_id,
        source: "claude".into(),
        project: project.to_string(),
        started_at: first_ts,
        ended_at: last_ts,
        message_count,
        conversation_count: conv_index,
        model,
        total_tokens: session_tokens,
    });
}

pub fn parse_codex_file(path: &Path, sessions: &mut Vec<RawSession>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    let session_id = path
        .file_stem()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut message_count = 0i64;

    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if value.get("type").and_then(JsonValue::as_str) != Some("response_item") {
            continue;
        }
        let payload = value.get("payload");
        if payload
            .and_then(|p| p.get("type"))
            .and_then(JsonValue::as_str)
            != Some("message")
        {
            continue;
        }
        let role = payload
            .and_then(|p| p.get("role"))
            .and_then(JsonValue::as_str)
            .unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        if first_ts.is_empty() {
            first_ts = ts.clone();
        }
        last_ts = ts.clone();
        message_count += 1;
    }
    sessions.push(RawSession {
        id: session_id,
        source: "codex".into(),
        project: String::new(),
        started_at: first_ts,
        ended_at: last_ts,
        message_count,
        conversation_count: 0,
        model: String::new(),
        total_tokens: 0,
    });
}

pub fn collect_codex_files(dir: &Path, sessions: &mut Vec<RawSession>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_codex_files(&path, sessions);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with("rollout-") && name.ends_with(".jsonl"))
            .unwrap_or(false)
        {
            parse_codex_file(&path, sessions);
        }
    }
}

pub fn path_display(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

pub fn size_human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn path_detail(path: &Path) -> String {
    if path.is_file() {
        return fs::metadata(path)
            .map(|meta| size_human(meta.len()))
            .unwrap_or_default();
    }
    if path.is_dir() {
        let count = fs::read_dir(path)
            .map(|entries| entries.take(1001).count())
            .unwrap_or(0);
        return if count == 0 {
            String::new()
        } else if count > 1000 {
            "1000+ 项".to_string()
        } else {
            format!("{count} 项")
        };
    }
    String::new()
}

pub fn push_agent_path(
    entries: &mut Vec<LocalAgentPathEntry>,
    kind: &str,
    label: &str,
    path: Option<&Path>,
) {
    let Some(path) = path else {
        return;
    };
    entries.push(LocalAgentPathEntry {
        kind: kind.to_string(),
        label: label.to_string(),
        exists: path.exists(),
        detail: path_detail(path),
        path: path_display(path),
    });
}

pub fn finish_agent(
    source: &str,
    name: &str,
    root: Option<&Path>,
    mut entries: Vec<LocalAgentPathEntry>,
) -> LocalAgentPaths {
    let root_path = match root {
        Some(path) => path_display(path),
        None => entries
            .first()
            .map(|entry| {
                Path::new(&entry.path)
                    .parent()
                    .map(path_display)
                    .unwrap_or_else(|| entry.path.clone())
            })
            .unwrap_or_default(),
    };
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.path.cmp(&right.path))
    });
    let detected = entries.iter().any(|entry| entry.exists);
    LocalAgentPaths {
        source: source.to_string(),
        name: name.to_string(),
        root: root_path,
        detected,
        paths: entries,
        collected_sessions: 0,
        collected_events: 0,
    }
}

pub fn kiro_legacy_storage_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let global_storage = home
            .join("Library")
            .join("Application Support")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        roots.push(global_storage.join("kiro.kiroagent"));
        roots.push(global_storage.join("kiro.kiro-agent"));
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(app_data) = std::env::var_os("APPDATA") {
            let global_storage = PathBuf::from(app_data)
                .join("Kiro")
                .join("User")
                .join("globalStorage");
            roots.push(global_storage.join("kiro.kiroagent"));
            roots.push(global_storage.join("kiro.kiro-agent"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let global_storage = home
            .join(".config")
            .join("Kiro")
            .join("User")
            .join("globalStorage");
        roots.push(global_storage.join("kiro.kiroagent"));
        roots.push(global_storage.join("kiro.kiro-agent"));
    }
    roots
}

pub fn collect_local_agent_paths(home: &Path) -> LocalAgentPathsReport {
    let mut agents = Vec::<LocalAgentPaths>::new();

    // Codex
    {
        let root = crate::token::collector::codex_home(home);
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "配置 config.toml",
            Some(&root.join("config.toml")),
        );
        push_agent_path(
            &mut entries,
            "config",
            "认证 auth.json",
            Some(&root.join("auth.json")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions",
            Some(&root.join("sessions")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "归档会话 archived_sessions",
            Some(&root.join("archived_sessions")),
        );
        agents.push(finish_agent("codex", "Codex", Some(&root), entries));
    }

    // Claude Code
    {
        let root = crate::token::collector::claude_config_dir(home);
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "项目设置 settings.json",
            Some(&root.join("settings.json")),
        );
        push_agent_path(
            &mut entries,
            "config",
            "全局配置 ~/.claude.json",
            Some(&home.join(".claude.json")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "会话项目 projects",
            Some(&root.join("projects")),
        );
        agents.push(finish_agent("claude", "Claude Code", Some(&root), entries));
    }

    // Command Code
    {
        let root = home.join(".commandcode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话项目 projects",
            Some(&root.join("projects")),
        );
        agents.push(finish_agent(
            "command-code",
            "Command Code",
            Some(&root),
            entries,
        ));
    }

    // Antigravity (Gemini 客户端)
    {
        let root = home.join(".gemini");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "转录 antigravity-cli",
            Some(&root.join("antigravity-cli")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "转录 antigravity-ide",
            Some(&root.join("antigravity-ide")),
        );
        agents.push(finish_agent(
            "antigravity",
            "Antigravity (Gemini)",
            Some(&root),
            entries,
        ));
    }

    // Kiro
    {
        let root = home.join(".kiro");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions (v2)",
            Some(&root.join("sessions")),
        );
        for (index, legacy) in kiro_legacy_storage_roots(home).iter().enumerate() {
            let label = if index == 0 {
                "旧版全局存储 (globalStorage)"
            } else {
                "旧版全局存储 (备用)"
            };
            push_agent_path(&mut entries, "data", label, Some(legacy));
        }
        agents.push(finish_agent("kiro", "Kiro", Some(&root), entries));
    }

    // DSH (DeepSeek)
    {
        let root = home.join(".dsh");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions (.jsonl.zstd)",
            Some(&root.join("sessions")),
        );
        agents.push(finish_agent("dsh", "DSH (DeepSeek)", Some(&root), entries));
    }

    // GitHub Copilot / VS Code Copilot
    {
        let copilot_root = home.join(".copilot");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "Copilot CLI 运行时",
            Some(&copilot_root.join("session-state")),
        );
        #[cfg(target_os = "macos")]
        {
            let code_user = home.join("Library/Application Support/Code/User");
            push_agent_path(
                &mut entries,
                "data",
                "VS Code 全局会话",
                Some(&code_user.join("globalStorage/emptyWindowChatSessions")),
            );
            push_agent_path(
                &mut entries,
                "data",
                "VS Code 工作区会话",
                Some(&code_user.join("workspaceStorage")),
            );
        }
        #[cfg(target_os = "windows")]
        {
            if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
                let code_user = appdata.join("Code/User");
                push_agent_path(
                    &mut entries,
                    "data",
                    "VS Code 全局会话",
                    Some(&code_user.join("globalStorage/emptyWindowChatSessions")),
                );
                push_agent_path(
                    &mut entries,
                    "data",
                    "VS Code 工作区会话",
                    Some(&code_user.join("workspaceStorage")),
                );
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let code_user = home.join(".config/Code/User");
            push_agent_path(
                &mut entries,
                "data",
                "VS Code 全局会话",
                Some(&code_user.join("globalStorage/emptyWindowChatSessions")),
            );
            push_agent_path(
                &mut entries,
                "data",
                "VS Code 工作区会话",
                Some(&code_user.join("workspaceStorage")),
            );
        }
        agents.push(finish_agent(
            "copilot",
            "GitHub Copilot (VS Code)",
            Some(&copilot_root),
            entries,
        ));
    }

    // OpenCode
    {
        let data_root = crate::token::collector::xdg_data_home(home).join("opencode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "配置目录",
            Some(&home.join(".config").join("opencode")),
        );
        push_agent_path(
            &mut entries,
            "database",
            "数据库 opencode.db",
            Some(&data_root.join("opencode.db")),
        );
        agents.push(finish_agent(
            "opencode",
            "OpenCode",
            Some(&data_root),
            entries,
        ));
    }

    // MiMo Code
    {
        let root = crate::token::collector::xdg_data_home(home).join("mimocode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "database",
            "数据库 mimocode.db",
            Some(&root.join("mimocode.db")),
        );
        agents.push(finish_agent("mimo", "MiMo Code", Some(&root), entries));
    }

    // ZCode
    {
        let root = home.join(".zcode");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "database",
            "数据库 db.sqlite",
            Some(&crate::token::collector::zcode_db_path(home)),
        );
        agents.push(finish_agent("zcode", "ZCode", Some(&root), entries));
    }

    // CatPawAI
    {
        let roots = catpawai_data_roots(home);
        let primary = roots.first().cloned();
        let mut entries = Vec::new();
        for (index, data_root) in roots.iter().enumerate() {
            let label = if index == 0 {
                "数据库 globalCache.sqlite"
            } else {
                "数据库 globalCache.sqlite (备用)"
            };
            push_agent_path(
                &mut entries,
                "database",
                label,
                Some(&data_root.join("globalCache.sqlite")),
            );
        }
        agents.push(finish_agent(
            "catpawai",
            "CatPawAI",
            primary.as_deref(),
            entries,
        ));
    }

    // Cursor
    {
        let mut entries = Vec::new();
        for p in crate::token::collector::collect_cursor_db_paths(home) {
            push_agent_path(&mut entries, "database", "SQLite state.vscdb", Some(&p));
        }
        let root = entries
            .first()
            .and_then(|e| PathBuf::from(&e.path).parent().map(|p| p.to_path_buf()));
        agents.push(finish_agent("cursor", "Cursor", root.as_deref(), entries));
    }

    // Windsurf
    {
        let mut entries = Vec::new();
        for p in crate::token::collector::collect_windsurf_db_paths(home) {
            push_agent_path(&mut entries, "database", "SQLite state.vscdb", Some(&p));
        }
        let root = entries
            .first()
            .and_then(|e| PathBuf::from(&e.path).parent().map(|p| p.to_path_buf()));
        agents.push(finish_agent(
            "windsurf",
            "Windsurf",
            root.as_deref(),
            entries,
        ));
    }

    // Zed
    {
        let mut entries = Vec::new();
        for p in crate::token::collector::collect_zed_source_files(home) {
            push_agent_path(&mut entries, "data", "会话 .zed.json", Some(&p));
        }
        let root = entries
            .first()
            .and_then(|e| PathBuf::from(&e.path).parent().map(|p| p.to_path_buf()));
        agents.push(finish_agent("zed", "Zed Editor", root.as_deref(), entries));
    }

    // Cline
    let cline_files = crate::token::collector::collect_cline_source_files(home);
    {
        let mut entries = Vec::new();
        for (src, p) in &cline_files {
            if src == "cline" {
                push_agent_path(&mut entries, "data", "任务 ui_messages.json", Some(p));
            }
        }
        let root = entries
            .first()
            .and_then(|e| PathBuf::from(&e.path).parent().map(|p| p.to_path_buf()));
        agents.push(finish_agent("cline", "Cline", root.as_deref(), entries));
    }

    // Roo-Code
    {
        let mut entries = Vec::new();
        for (src, p) in &cline_files {
            if src == "roo-code" {
                push_agent_path(&mut entries, "data", "任务 ui_messages.json", Some(p));
            }
        }
        let root = entries
            .first()
            .and_then(|e| PathBuf::from(&e.path).parent().map(|p| p.to_path_buf()));
        agents.push(finish_agent(
            "roo-code",
            "Roo Code",
            root.as_deref(),
            entries,
        ));
    }

    // Continue.dev
    {
        let root = home.join(".continue");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "config",
            "配置 config.json",
            Some(&root.join("config.json")),
        );
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions",
            Some(&root.join("sessions")),
        );
        agents.push(finish_agent(
            "continue",
            "Continue.dev",
            Some(&root),
            entries,
        ));
    }

    // Aider
    {
        let mut entries = Vec::new();
        for p in crate::token::collector::collect_aider_source_files(home) {
            push_agent_path(&mut entries, "data", "聊天历史 / 分析", Some(&p));
        }
        agents.push(finish_agent(
            "aider",
            "Aider",
            Some(&home.join(".aider")),
            entries,
        ));
    }

    // Goose AI
    {
        let root = home.join(".local").join("share").join("goose");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions",
            Some(&root.join("sessions")),
        );
        agents.push(finish_agent("goose", "Goose AI", Some(&root), entries));
    }

    // OpenClaw
    {
        let root = home.join(".openclaw");
        let mut entries = Vec::new();
        push_agent_path(
            &mut entries,
            "data",
            "会话 sessions",
            Some(&root.join("sessions")),
        );
        agents.push(finish_agent("openclaw", "OpenClaw", Some(&root), entries));
    }

    let collected = crate::token::collector::collected_stats_by_source();
    let collected_at = collected
        .values()
        .next()
        .map(|stats| stats.updated_at.clone())
        .unwrap_or_default();
    for agent in &mut agents {
        if let Some(stats) = collected.get(&agent.source) {
            agent.collected_sessions = stats.sessions;
            agent.collected_events = stats.events;
        }
    }

    LocalAgentPathsReport {
        available: true,
        home: path_display(home),
        agents,
        env_overrides: collected_env_overrides(),
        collected_at,
    }
}

pub fn collected_env_overrides() -> Vec<LocalAgentEnvOverride> {
    [
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "XDG_DATA_HOME",
        "OPENHUB_CATPAWAI_DB_PATH",
    ]
    .iter()
    .filter_map(|key| {
        let value = std::env::var_os(key)?;
        (!value.is_empty()).then(|| LocalAgentEnvOverride {
            key: (*key).to_string(),
            value: path_display(&PathBuf::from(value)),
        })
    })
    .collect()
}
