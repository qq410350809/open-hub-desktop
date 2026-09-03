use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::vscode_workspace_project_from_path;
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    collect_jsonl_files, fingerprint, normalize_usage, token_session, CachedFile, FileFingerprint,
    InputSemantics, RawUsage, UsageEvent, LOCAL_ESTIMATED_CONTEXT_LIMIT, UNKNOWN_COPILOT_MODEL,
};
use serde_json::{json, Value as JsonValue};
use std::fs;
use std::path::{Path, PathBuf};

pub fn normalize_copilot_model_name(raw: &str) -> String {
    let mut s = raw.trim();
    if s.is_empty() {
        return UNKNOWN_COPILOT_MODEL.to_string();
    }
    if s.contains("@provider=") {
        let parts: Vec<&str> = s.split(':').collect();
        if let Some(last) = parts.last() {
            let last_clean = last.trim();
            if !last_clean.is_empty() {
                if last_clean == "sonnet" {
                    return "claude-3-7-sonnet".to_string();
                } else if last_clean == "fable" || last_clean == "haiku" {
                    return "claude-3-5-haiku".to_string();
                } else if last_clean == "opus" {
                    return "claude-3-opus".to_string();
                }
                return last_clean.to_string();
            }
        }
    }
    // 第三方扩展（如 opencode-copilot-chat）会给模型 ID 追加动态会话戳，
    // 例如 "opencodezen:x-preview-f-free::session-2026-05-21-b"；
    // 不剥离会导致同一模型被拆散成大量伪模型条目，统计严重失真。
    if let Some((base, _)) = s.split_once("::") {
        let base = base.trim();
        if !base.is_empty() {
            s = base;
        }
    }
    // VSCode 模型标识符为多段式 "{vendor}/{显示名}/{id}"，取最后一段；
    // Copilot CLI 的 "{org}/{model}" 同样归一到纯模型名。
    if s.contains('/') {
        let last = s.rsplit('/').next().unwrap_or(s).trim();
        if !last.is_empty() {
            s = last;
        }
    }
    // 剥离 "{vendor}:" 前缀（如 "opencodezen:"、"github-copilot:"）
    if let Some((vendor, rest)) = s.split_once(':') {
        let looks_like_vendor = !vendor.is_empty()
            && !rest.is_empty()
            && !vendor.contains('.')
            && vendor
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
        if looks_like_vendor {
            let rest = rest.trim();
            if !rest.is_empty() {
                s = rest;
            }
        }
    }
    if s.is_empty() || s == "auto" || s == "copilotcli:auto" || s == "agent-host-copilotcli:auto" {
        return UNKNOWN_COPILOT_MODEL.to_string();
    }
    s.to_string()
}

pub fn collect_copilot_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let mut base_dirs = Vec::new();

    #[cfg(target_os = "macos")]
    {
        let app_support = home.join("Library").join("Application Support");
        base_dirs.push(app_support.join("Code").join("User"));
        base_dirs.push(app_support.join("Code - Insiders").join("User"));
        base_dirs.push(app_support.join("VSCodium").join("User"));
    }

    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            base_dirs.push(appdata.join("Code").join("User"));
            base_dirs.push(appdata.join("Code - Insiders").join("User"));
            base_dirs.push(appdata.join("VSCodium").join("User"));
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let config = home.join(".config");
        base_dirs.push(config.join("Code").join("User"));
        base_dirs.push(config.join("Code - Insiders").join("User"));
        base_dirs.push(config.join("VSCodium").join("User"));
    }

    for user_dir in base_dirs {
        if !user_dir.is_dir() {
            continue;
        }

        let empty_window_sessions = user_dir
            .join("globalStorage")
            .join("emptyWindowChatSessions");
        if empty_window_sessions.is_dir() {
            collect_jsonl_files(
                &empty_window_sessions,
                &|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
                &mut files,
            );
        }

        let workspace_storage = user_dir.join("workspaceStorage");
        if workspace_storage.is_dir() {
            if let Ok(entries) = fs::read_dir(&workspace_storage) {
                for entry in entries.flatten() {
                    // VSCode 内核的 UI 状态存储：requests 只记「用户轮次」，
                    // agent 模式一轮会触发多次 LLM 请求，此处严重低估
                    let chat_sessions = entry.path().join("chatSessions");
                    if chat_sessions.is_dir() {
                        collect_jsonl_files(
                            &chat_sessions,
                            &|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
                            &mut files,
                        );
                    }
                    // Copilot Chat 扩展的真实请求日志：assistant.turn_end 每次
                    // 都是一次完整 LLM 请求（agent 工具循环），是请求数的权威来源
                    let transcripts = entry.path().join("GitHub.copilot-chat").join("transcripts");
                    if transcripts.is_dir() {
                        collect_jsonl_files(
                            &transcripts,
                            &|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"),
                            &mut files,
                        );
                    }
                }
            }
        }
    }

    let copilot_cli_root = home.join(".copilot").join("session-state");
    if copilot_cli_root.is_dir() {
        collect_jsonl_files(
            &copilot_cli_root,
            &|path| path.file_name().and_then(|n| n.to_str()) == Some("events.jsonl"),
            &mut files,
        );
    }

    files
        .into_iter()
        .map(|path| ("copilot".to_string(), path))
        .collect()
}

/// 判断路径段是否为数组下标（数字）。
fn is_index_segment(segment: &JsonValue) -> bool {
    segment.is_i64() || segment.is_u64()
}

/// 沿路径逐段定位（必要时创建容器），返回终点处的可变引用。
///
/// 数字段按数组下标处理（越界用 null 补齐），字符串段按对象键处理；
/// 下一段的类型决定当前缺失时应创建数组还是对象。
fn ensure_container_at<'a>(
    document: &'a mut JsonValue,
    path: &[JsonValue],
) -> Option<&'a mut JsonValue> {
    let mut current = document;
    for (index, segment) in path.iter().enumerate() {
        let next_is_index = path
            .get(index + 1)
            .map(|seg| is_index_segment(seg))
            .unwrap_or(false);
        if is_index_segment(segment) {
            let idx = segment.as_i64()?.max(0) as usize;
            if !current.is_array() {
                *current = json!([]);
            }
            let array = current.as_array_mut()?;
            while array.len() <= idx {
                array.push(JsonValue::Null);
            }
            if array[idx].is_null() {
                array[idx] = if next_is_index { json!([]) } else { json!({}) };
            }
            current = array.get_mut(idx)?;
        } else {
            let key = segment.as_str()?.to_string();
            if key.is_empty() {
                return None;
            }
            if !current.is_object() {
                *current = json!({});
            }
            let object = current.as_object_mut()?;
            object
                .entry(key)
                .or_insert_with(|| if next_is_index { json!([]) } else { json!({}) });
            current = object.get_mut(segment.as_str()?)?;
        }
    }
    Some(current)
}

/// 应用一条会话操作日志：kind 0 全量替换、kind 1 按路径设值、kind 2 数组追加。
fn apply_chat_log_patch(
    document: &mut JsonValue,
    kind: i64,
    key_path: &[JsonValue],
    payload: JsonValue,
) {
    match kind {
        0 => *document = payload,
        1 => {
            let Some((last, parents)) = key_path.split_last() else {
                *document = payload;
                return;
            };
            let Some(parent) = ensure_container_at(document, parents) else {
                return;
            };
            if is_index_segment(last) {
                let idx = last.as_i64().unwrap_or(0).max(0) as usize;
                if !parent.is_array() {
                    *parent = json!([]);
                }
                if let Some(array) = parent.as_array_mut() {
                    while array.len() <= idx {
                        array.push(JsonValue::Null);
                    }
                    array[idx] = payload;
                }
            } else if let Some(key) = last.as_str() {
                if !parent.is_object() {
                    *parent = json!({});
                }
                if let Some(object) = parent.as_object_mut() {
                    object.insert(key.to_string(), payload);
                }
            }
        }
        2 => {
            let Some(target) = ensure_container_at(document, key_path) else {
                return;
            };
            if !target.is_array() {
                *target = json!([]);
            }
            if let Some(array) = target.as_array_mut() {
                match payload {
                    JsonValue::Array(items) => array.extend(items),
                    other => array.push(other),
                }
            }
        }
        _ => {}
    }
}

pub fn parse_vscode_chat_session(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("copilot-session")
        .to_string();
    let mut session_creation_ts = String::new();
    let project_key = vscode_workspace_project_from_path(path);
    let mut model_fallback = UNKNOWN_COPILOT_MODEL.to_string();

    let mut events = Vec::<UsageEvent>::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut turns = 0i64;

    // VS Code 新版把会话文件从「整行快照」改成了增量操作日志：
    // kind 0 = 全量快照；kind 1 = 按路径 k 设值（数字段为数组下标）；
    // kind 2 = 向路径 k 的数组追加 v 中的元素。
    // 旧解析器只读每行的 requests 字段，而新格式只有首行带空 requests，
    // 后续请求全部以补丁形式追加——导致 Copilot 统计几乎全部丢失。
    // 这里先回放整份日志得到合并后的最终状态，再统一提取。
    let mut document = JsonValue::Null;
    for line in text.lines() {
        let Ok(op) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if op.get("kind").is_some() {
            let kind = op.get("kind").and_then(JsonValue::as_i64).unwrap_or(-1);
            let empty_path = Vec::new();
            let key_path = op
                .get("k")
                .and_then(JsonValue::as_array)
                .unwrap_or(&empty_path);
            let payload = op.get("v").cloned().unwrap_or(JsonValue::Null);
            apply_chat_log_patch(&mut document, kind, key_path, payload);
        } else {
            // 旧格式兼容：无 kind 字段的行就是一份完整快照。
            apply_chat_log_patch(&mut document, 0, &[], op);
        }
    }

    let v = &document;

    if let Some(sid) = v.get("sessionId").and_then(JsonValue::as_str) {
        if !sid.is_empty() {
            session_id = sid.to_string();
        }
    }
    if let Some(ms) = v.get("creationDate").and_then(JsonValue::as_i64) {
        session_creation_ts = iso_from_millis(ms);
    }

    if let Some(selected) = v.get("inputState").and_then(|is| is.get("selectedModel")) {
        if let Some(raw_id) = selected.get("identifier").and_then(JsonValue::as_str) {
            let m = normalize_copilot_model_name(raw_id);
            if m != UNKNOWN_COPILOT_MODEL {
                model_fallback = m;
            }
        }
    }

    if let Some(requests) = v.get("requests").and_then(JsonValue::as_array) {
        for (req_idx, req) in requests.iter().enumerate() {
            let req_id = req
                .get("requestId")
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:req:{req_idx}"));

            // 注意：timeSpentWaiting 是等待「时长」（毫秒数），绝不能当时间戳解析，
            // 否则 iso_from_millis(5000) 会生成 1970-01-01，事件全部落入错误时间桶。
            let mut timestamp = req
                .get("modelState")
                .and_then(|ms| ms.get("completedAt"))
                .and_then(JsonValue::as_i64)
                .map(iso_from_millis)
                .unwrap_or_default();
            if timestamp.is_empty() {
                timestamp = session_creation_ts.clone();
            }
            update_bounds(&mut first_ts, &mut last_ts, &timestamp);

            let mut req_model = req
                .get("result")
                .and_then(|res| res.get("metadata"))
                .and_then(|meta| meta.get("resolvedModel"))
                .and_then(JsonValue::as_str)
                .map(normalize_copilot_model_name)
                .unwrap_or_else(|| UNKNOWN_COPILOT_MODEL.to_string());

            if req_model == UNKNOWN_COPILOT_MODEL {
                if let Some(m) = req.get("modelId").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        req_model = clean;
                    }
                }
            }
            if req_model == UNKNOWN_COPILOT_MODEL {
                if let Some(m) = req.get("usedModel").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        req_model = clean;
                    }
                }
            }
            if req_model == UNKNOWN_COPILOT_MODEL {
                req_model = model_fallback.clone();
            }

            let user_text = req
                .get("message")
                .and_then(|m| m.get("text"))
                .and_then(JsonValue::as_str)
                .unwrap_or("");
            if !user_text.trim().is_empty() {
                turns += 1;
                events.push(UsageEvent {
                    id: format!("u:{req_id}"),
                    source: "copilot".to_string(),
                    model: req_model.clone(),
                    project_key: project_key.clone(),
                    timestamp: timestamp.clone(),
                    conversation_count: 1,
                    ..Default::default()
                });
            }

            let mut prompt_tokens = req
                .get("promptTokens")
                .and_then(JsonValue::as_i64)
                .or_else(|| {
                    req.get("result")
                        .and_then(|res| res.get("metadata"))
                        .and_then(|meta| meta.get("promptTokens"))
                        .and_then(JsonValue::as_i64)
                })
                .unwrap_or(0);

            let mut output_tokens = req
                .get("completionTokens")
                .or_else(|| req.get("outputTokens"))
                .and_then(JsonValue::as_i64)
                .or_else(|| {
                    req.get("result")
                        .and_then(|res| res.get("metadata"))
                        .and_then(|meta| meta.get("outputTokens"))
                        .and_then(JsonValue::as_i64)
                })
                .unwrap_or(0);

            let cached_tokens = req
                .get("cachedTokens")
                .and_then(JsonValue::as_i64)
                .unwrap_or(0);

            let mut reasoning_tokens = 0i64;
            if let Some(resp_arr) = req.get("response").and_then(JsonValue::as_array) {
                for item in resp_arr {
                    if item.get("kind").and_then(JsonValue::as_str) == Some("thinking") {
                        let text_len = item
                            .get("value")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("")
                            .len();
                        reasoning_tokens += (text_len as i64 / 4).max(1);
                    }
                }
            }

            let mut estimated_tokens = 0i64;
            if prompt_tokens == 0 && output_tokens == 0 {
                let user_tokens = (user_text.len() as i64 / 4).max(1);
                prompt_tokens = user_tokens + 128;
                let mut resp_text_len = 0usize;
                if let Some(resp_arr) = req.get("response").and_then(JsonValue::as_array) {
                    for item in resp_arr {
                        // 思考文本单独计入 reasoning_output_tokens，不得混入输出（避免双算）
                        if item.get("kind").and_then(JsonValue::as_str) == Some("thinking") {
                            continue;
                        }
                        if let Some(v) = item.get("value").and_then(JsonValue::as_str) {
                            resp_text_len += v.len();
                        }
                    }
                }
                output_tokens = (resp_text_len as i64 / 4).max(1);
                estimated_tokens = prompt_tokens + output_tokens;
            }

            // 口径：total = 全新输入 + 缓存命中 + 输出；思考 token 独立上报，不计入 total。
            // Copilot 的 promptTokens 为上游总量（已包含 cachedTokens），必须拆出全新输入
            // ——input 字段一旦存入含缓存总量，下游 fresh+cached+output 即双计。
            let (fresh_input, cached_read, _write, output_final, _reasoning, total_tokens) =
                normalize_usage(RawUsage {
                    input: prompt_tokens,
                    semantics: InputSemantics::InclusiveOfCacheRead,
                    cache_read: cached_tokens,
                    cache_write: 0,
                    output: output_tokens,
                    reasoning: reasoning_tokens,
                });

            if total_tokens > 0 || !user_text.is_empty() {
                events.push(UsageEvent {
                    id: req_id,
                    source: "copilot".to_string(),
                    model: req_model,
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens: fresh_input,
                    cached_input_tokens: cached_read,
                    cache_creation_input_tokens: 0,
                    output_tokens: output_final,
                    reasoning_output_tokens: reasoning_tokens,
                    total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens,
                });
            }
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });

    let session = token_session(
        session_id,
        "copilot",
        project_key,
        model_fallback,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

pub fn parse_copilot_cli_events(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let session_id = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("copilot-cli-session")
        .to_string();

    let project_key = "Copilot CLI".to_string();
    let mut model = UNKNOWN_COPILOT_MODEL.to_string();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut turns = 0i64;
    let mut events = Vec::<UsageEvent>::new();
    let mut visible_context_tokens = 0i64;

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let event_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &ts);

        let data = value.get("data").unwrap_or(&JsonValue::Null);

        match event_type {
            "session.start" => {
                if let Some(m) = data.get("selectedModel").and_then(JsonValue::as_str) {
                    let clean = normalize_copilot_model_name(m);
                    if clean != UNKNOWN_COPILOT_MODEL {
                        model = clean;
                    }
                }
            }
            "user.message" => {
                let content = data
                    .get("content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                if !content.trim().is_empty() {
                    turns += 1;
                    let event_id = value.get("id").and_then(JsonValue::as_str).unwrap_or("");
                    events.push(UsageEvent {
                        id: format!(
                            "u:{session_id}:{}",
                            if event_id.is_empty() {
                                index.to_string()
                            } else {
                                event_id.to_string()
                            }
                        ),
                        source: "copilot".to_string(),
                        model: model.clone(),
                        project_key: project_key.clone(),
                        timestamp: ts.clone(),
                        conversation_count: 1,
                        ..Default::default()
                    });
                    visible_context_tokens += (content.len() as i64 / 4).max(1);
                }
            }
            "assistant.message" => {
                let req_id = data
                    .get("requestId")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                let event_id = if !req_id.is_empty() {
                    format!("{session_id}:{req_id}")
                } else {
                    format!("{session_id}:{index}")
                };

                let output_tokens = data
                    .get("outputTokens")
                    .and_then(JsonValue::as_i64)
                    .unwrap_or_else(|| {
                        let content = data
                            .get("content")
                            .and_then(JsonValue::as_str)
                            .unwrap_or("");
                        (content.len() as i64 / 4).max(1)
                    });

                let reasoning_tokens = data
                    .get("reasoningText")
                    .and_then(JsonValue::as_str)
                    .map(|r| (r.len() as i64 / 4).max(1))
                    .unwrap_or(0);
                let input_tokens = visible_context_tokens
                    .saturating_add(64)
                    .min(LOCAL_ESTIMATED_CONTEXT_LIMIT);
                // 口径：total = 全新输入 + 缓存命中 + 输出；思考 token 独立上报，不计入 total。
                let (_fresh, _read, _write, _out, _reasoning, total_tokens) = normalize_usage(
                    RawUsage {
                        input: input_tokens,
                        semantics: InputSemantics::Fresh,
                        cache_write: 0,
                        output: output_tokens,
                        ..Default::default()
                    },
                );

                events.push(UsageEvent {
                    id: event_id,
                    source: "copilot".to_string(),
                    model: model.clone(),
                    project_key: project_key.clone(),
                    timestamp: ts,
                    input_tokens,
                    cached_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                    output_tokens,
                    reasoning_output_tokens: reasoning_tokens,
                    total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: total_tokens,
                });
            }
            _ => {}
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });

    let session = token_session(
        session_id.clone(),
        "copilot",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

/// 解析 Copilot Chat 扩展的 transcript 日志
/// （workspaceStorage/*/GitHub.copilot-chat/transcripts/*.jsonl）。
///
/// 仅用于统计「用户对话轮次」（conversation_count）。token/请求数不在此产生：
/// 同一批请求在 VSCode 输出通道日志中有精确 usage（见 parse_vscode_opencode_log），
/// 在此重复估算会导致请求数与 token 双重计数。
pub fn parse_copilot_transcript(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let mut session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("copilot-transcript")
        .to_string();
    let project_key = vscode_workspace_project_from_path(path);
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut turns = 0i64;
    let mut events = Vec::<UsageEvent>::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        let event_type = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let ts = value
            .get("timestamp")
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_string();
        update_bounds(&mut first_ts, &mut last_ts, &ts);
        let data = value.get("data").unwrap_or(&JsonValue::Null);

        match event_type {
            "session.start" => {
                if let Some(sid) = data.get("sessionId").and_then(JsonValue::as_str) {
                    if !sid.is_empty() {
                        session_id = sid.to_string();
                    }
                }
            }
            "user.message" => {
                let content = data
                    .get("content")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("");
                if content.trim().is_empty()
                    && data
                        .get("attachments")
                        .and_then(JsonValue::as_array)
                        .map(Vec::is_empty)
                        != Some(false)
                {
                    continue;
                }
                turns += 1;
                let event_id = value.get("id").and_then(JsonValue::as_str).unwrap_or("");
                events.push(UsageEvent {
                    id: format!(
                        "u:{session_id}:{}",
                        if event_id.is_empty() {
                            index.to_string()
                        } else {
                            event_id.to_string()
                        }
                    ),
                    source: "copilot".to_string(),
                    model: UNKNOWN_COPILOT_MODEL.to_string(),
                    project_key: project_key.clone(),
                    timestamp: ts,
                    conversation_count: 1,
                    ..Default::default()
                });
            }
            // 修复：前向兼容性 - 尝试提取未知事件类型中的 token 数据
            // 避免未来版本新增字段时数据丢失
            unknown_type => {
                // 检查是否包含 usage/tokens 字段
                if let Some(usage) = data.get("usage").or_else(|| data.get("tokens")) {
                    // 尝试提取 token 计量数据
                    let input = usage
                        .get("input_tokens")
                        .or_else(|| usage.get("prompt_tokens"))
                        .and_then(JsonValue::as_i64)
                        .unwrap_or(0);
                    let output = usage
                        .get("output_tokens")
                        .or_else(|| usage.get("completion_tokens"))
                        .and_then(JsonValue::as_i64)
                        .unwrap_or(0);

                    if input > 0 || output > 0 {
                        eprintln!(
                            "[Copilot] 发现未知事件类型 '{}' 包含 token 数据 (in:{}, out:{})",
                            unknown_type, input, output
                        );
                        eprintln!("[Copilot] 请更新采集器以支持此事件类型");
                    }
                }
            }
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });

    let session = token_session(
        session_id,
        "copilot",
        project_key,
        UNKNOWN_COPILOT_MODEL.to_string(),
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}

pub fn parse_copilot_file(path: &Path) -> CachedFile {
    let file_fingerprint = fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };

    if path.file_name().and_then(|n| n.to_str()) == Some("events.jsonl") {
        parse_copilot_cli_events(path, &text, file_fingerprint)
    } else if path
        .components()
        .any(|component| component.as_os_str().to_str() == Some("transcripts"))
    {
        parse_copilot_transcript(path, &text, file_fingerprint)
    } else {
        parse_vscode_chat_session(path, &text, file_fingerprint)
    }
}

/// 收集 VSCode 输出通道中的 OpenCode 扩展请求日志
/// （logs/<会话>/window*/exthost/output_logging_<ts>/<N>-OpenCode.log）。
/// 该日志由 opencode-copilot-chat 扩展输出，每个 LLM 请求一行精确 usage，
/// 是 VSCode 场景下唯一包含真实缓存命中（cachedTokens）的数据源。
pub fn collect_vscode_opencode_log_files(home: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    let mut base_dirs = Vec::new();
    #[cfg(target_os = "macos")]
    {
        base_dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("Code")
                .join("logs"),
        );
        base_dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("Code - Insiders")
                .join("logs"),
        );
        base_dirs.push(
            home.join("Library")
                .join("Application Support")
                .join("VSCodium")
                .join("logs"),
        );
    }
    #[cfg(target_os = "windows")]
    {
        if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
            base_dirs.push(appdata.join("Code").join("logs"));
            base_dirs.push(appdata.join("Code - Insiders").join("logs"));
            base_dirs.push(appdata.join("VSCodium").join("logs"));
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        base_dirs.push(home.join(".config").join("Code").join("logs"));
        base_dirs.push(home.join(".config").join("Code - Insiders").join("logs"));
        base_dirs.push(home.join(".config").join("VSCodium").join("logs"));
    }

    for logs_root in base_dirs {
        let Ok(session_dirs) = fs::read_dir(&logs_root) else {
            continue;
        };
        for session_dir in session_dirs.flatten() {
            let windows = session_dir.path().join("window");
            let Ok(window_entries) = fs::read_dir(&windows) else {
                continue;
            };
            for window_entry in window_entries.flatten() {
                let exthost = window_entry.path().join("exthost");
                let Ok(exthost_entries) = fs::read_dir(&exthost) else {
                    continue;
                };
                for output_dir in exthost_entries.flatten() {
                    if !output_dir
                        .file_name()
                        .to_str()
                        .map(|name| name.starts_with("output_logging_"))
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let Ok(channel_files) = fs::read_dir(output_dir.path()) else {
                        continue;
                    };
                    for channel_file in channel_files.flatten() {
                        let is_opencode_chat_log = channel_file
                            .file_name()
                            .to_str()
                            .map(|name| {
                                name.ends_with("-OpenCode.log")
                                    || name.ends_with("-OpenCode Completions.log")
                            })
                            .unwrap_or(false);
                        if is_opencode_chat_log {
                            files.push(("vscode-opencode".to_string(), channel_file.path()));
                        }
                    }
                }
            }
        }
    }
    files
}

/// 便捷入口：自读文件并解析 VSCode OpenCode 输出通道日志
pub fn parse_vscode_opencode_log_file(path: &Path) -> CachedFile {
    let file_fingerprint = fingerprint(path);
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: file_fingerprint,
            ..Default::default()
        };
    };
    parse_vscode_opencode_log(path, &text, file_fingerprint)
}

/// 从 output_logging_<YYYYMMDDTHHMMSS> 目录名解析本地时间（毫秒时间戳）
fn output_logging_start_millis(path: &Path) -> Option<i64> {
    for component in path.components().rev() {
        let text = component.as_os_str().to_str()?;
        if let Some(stamp) = text.strip_prefix("output_logging_") {
            if stamp.len() < 15 {
                return None;
            }
            let year: i32 = stamp.get(0..4)?.parse().ok()?;
            let month: u32 = stamp.get(4..6)?.parse().ok()?;
            let day: u32 = stamp.get(6..8)?.parse().ok()?;
            let hour: u32 = stamp.get(9..11)?.parse().ok()?;
            let minute: u32 = stamp.get(11..13)?.parse().ok()?;
            let second: u32 = stamp.get(13..15)?.parse().ok()?;
            use chrono::TimeZone;
            return chrono::Local
                .with_ymd_and_hms(year, month, day, hour, minute, second)
                .single()
                .map(|dt| dt.timestamp_millis());
        }
    }
    None
}

fn parse_key_values(segment: &str) -> Vec<(String, i64)> {
    segment
        .split_whitespace()
        .filter_map(|pair| pair.split_once('='))
        .filter_map(|(key, value)| value.parse::<i64>().ok().map(|n| (key.to_string(), n)))
        .collect()
}

/// 解析 VSCode 输出通道的 OpenCode 请求日志。
///
/// 行格式（无行级时间戳，事件时间取自 output_logging_ 目录名）：
///   [stream-summary model=x-preview-f-free] textChars=... toolCalls=...
///   [response-summary] status=200 durationMs=... promptTokens=... completionTokens=... cachedTokens=...
pub fn parse_vscode_opencode_log(
    path: &Path,
    text: &str,
    file_fingerprint: FileFingerprint,
) -> CachedFile {
    let default_ts = output_logging_start_millis(path)
        .map(iso_from_millis)
        .unwrap_or_default();
    let path_key = path.to_string_lossy().to_string();
    let project_key = "VSCode".to_string();

    let mut events = Vec::<UsageEvent>::new();
    let mut current_model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();

    for (index, line) in text.lines().enumerate() {
        let Some((bracket, rest)) = line.split_once(']') else {
            continue;
        };
        // 标签可能携带属性（如 "[stream-summary model=xxx]"），取首个空格前的部分
        let tag = bracket
            .trim_start_matches('[')
            .split_whitespace()
            .next()
            .unwrap_or("");
        match tag {
            "stream-summary" => {
                // 用最近一次 stream-summary 的模型名标注随后的 response-summary；
                // model 属性位于方括号内，故在整行中查找
                if let Some(model_pos) = line.find("model=") {
                    let model_part = &line[model_pos + "model=".len()..];
                    let model = model_part
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(']')
                        .trim()
                        .to_string();
                    if !model.is_empty() {
                        current_model = model;
                    }
                }
            }
            "response-summary" => {
                let fields = parse_key_values(rest);
                let get = |key: &str| {
                    fields
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, v)| *v)
                        .unwrap_or(0)
                };
                let status = get("status");
                if !(200..300).contains(&status) {
                    continue;
                }
                let prompt_tokens = get("promptTokens");
                let completion_tokens = get("completionTokens");
                let cached_tokens = get("cachedTokens");
                if prompt_tokens + completion_tokens <= 0 {
                    continue;
                }
                let timestamp = default_ts.clone();
                update_bounds(&mut first_ts, &mut last_ts, &timestamp);
                // OpenAI 语义：promptTokens 已含 cachedTokens，需拆出全新输入。
                let (fresh_input, cached_read, _write, output_final, _reasoning, total_tokens) =
                    normalize_usage(RawUsage {
                        input: prompt_tokens,
                        semantics: InputSemantics::InclusiveOfCacheRead,
                        cache_read: cached_tokens,
                        output: completion_tokens,
                        ..Default::default()
                    });
                events.push(UsageEvent {
                    id: format!("{path_key}:{index}"),
                    source: "vscode-opencode".to_string(),
                    model: if current_model.is_empty() {
                        UNKNOWN_COPILOT_MODEL.to_string()
                    } else {
                        current_model.clone()
                    },
                    project_key: project_key.clone(),
                    timestamp,
                    input_tokens: fresh_input,
                    cached_input_tokens: cached_read,
                    cache_creation_input_tokens: 0,
                    output_tokens: output_final,
                    reasoning_output_tokens: 0,
                    total_tokens,
                    conversation_count: 0,
                    cost_usd: 0.0,
                    pricing_available: false,
                    estimated_tokens: 0,
                });
            }
            _ => {}
        }
    }

    let tokens = events
        .iter()
        .fold(TokenSessionTokens::default(), |mut total, event| {
            total.input_tokens += event.input_tokens;
            total.cached_input_tokens += event.cached_input_tokens;
            total.cache_creation_input_tokens += event.cache_creation_input_tokens;
            total.output_tokens += event.output_tokens;
            total.reasoning_output_tokens += event.reasoning_output_tokens;
            total.total_tokens += event.total_tokens;
            total
        });

    let session = token_session(
        format!("vscode-opencode:{path_key}"),
        "vscode-opencode",
        project_key,
        if current_model.is_empty() {
            UNKNOWN_COPILOT_MODEL.to_string()
        } else {
            current_model
        },
        first_ts,
        last_ts,
        events.iter().filter(|e| e.conversation_count > 0).count() as i64,
        tokens,
        0.0,
    );

    CachedFile {
        fingerprint: file_fingerprint,
        events,
        sessions: vec![session],
    }
}
