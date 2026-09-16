use crate::models::TokenSessionTokens;
use crate::token::collector::normalizer::{
    project_key_from_encoded_dir_name, project_key_from_location,
};
use crate::token::collector::time_utils::{iso_from_millis, update_bounds};
use crate::token::collector::types::{
    fingerprint, normalize_usage, number, token_session, CachedFile, InputSemantics, RawUsage,
    UsageEvent, UNKNOWN_WORKBUDDY_MODEL,
};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// WorkBuddy AI 本地配置目录。默认 `~/.workbuddy-ai`，可用环境变量覆盖。
pub fn workbuddy_config_dir(home: &Path) -> PathBuf {
    crate::token::collector::aggregator::env_path_override("WORKBUDDY_CONFIG_DIR")
        .unwrap_or_else(|| home.join(".workbuddy-ai"))
}

/// 会话转录文件：`<config>/projects/<项目目录>/<会话 uuid>.jsonl`。
/// 同级的 `*.file-rollback.ndjson` 与 `<会话>/tool-results/` 下的文件不参与统计。
pub fn collect_workbuddy_source_files(home: &Path) -> Vec<(String, PathBuf)> {
    collect_workbuddy_source_files_in(&workbuddy_config_dir(home))
}

/// 从显式指定的配置目录收集会话转录（不读环境变量，便于测试隔离）。
pub fn collect_workbuddy_source_files_in(config_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut files = Vec::new();
    collect_workbuddy_jsonl(&config_dir.join("projects"), &mut files);
    files
}

fn collect_workbuddy_jsonl(dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // 跳过会话附带的大对象目录（工具输出正文），里面没有用量记录。
            if path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == "tool-results")
                .unwrap_or(false)
            {
                continue;
            }
            collect_workbuddy_jsonl(&path, out);
            continue;
        }
        if is_workbuddy_transcript_path(&path) {
            out.push(("workbuddy".to_string(), path));
        }
    }
}

/// 会话转录：`.jsonl` 且非 `.file-rollback.ndjson`，且不在 `tool-results/` 下。
pub fn is_workbuddy_transcript_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.ends_with(".file-rollback.ndjson") {
        return false;
    }
    if path
        .components()
        .any(|component| component.as_os_str() == "tool-results")
    {
        return false;
    }
    path.extension().and_then(|value| value.to_str()) == Some("jsonl")
}

/// 从 `providerData` 读取本轮使用的模型标识。
pub fn workbuddy_model(provider_data: &JsonValue) -> Option<String> {
    for key in ["model", "requestModelId", "requestModelName"] {
        if let Some(value) = provider_data
            .get(key)
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    None
}

/// 从 `rawUsage` 读取思考 token。OpenAI 语义下思考 token 是输出的子集，
/// 只作独立字段上报，不计入 total。
pub fn workbuddy_reasoning(raw_usage: &JsonValue, provider_usage: &JsonValue) -> i64 {
    let detail = number(
        raw_usage,
        &[
            "completion_thinking_tokens",
            "reasoning_tokens",
            "reasoningTokens",
        ],
    );
    if detail > 0 {
        return detail;
    }
    let nested = raw_usage
        .get("completion_tokens_details")
        .or_else(|| raw_usage.get("completionTokensDetails"))
        .map(|details| number(details, &["reasoning_tokens", "reasoningTokens"]))
        .unwrap_or(0);
    if nested > 0 {
        return nested;
    }
    provider_usage
        .get("outputTokensDetails")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| number(item, &["reasoning_tokens", "reasoningTokens"]))
                .sum()
        })
        .unwrap_or(0)
}

/// 从 `rawUsage` 读取缓存写入 token。
pub fn workbuddy_cache_write(raw_usage: &JsonValue, message_usage: &JsonValue) -> i64 {
    number(
        message_usage,
        &["cache_creation_input_tokens", "cacheCreationInputTokens"],
    )
    .max(number(
        raw_usage,
        &[
            "cache_creation_input_tokens",
            "prompt_cache_write_tokens",
            "cacheCreationInputTokens",
        ],
    ))
}

/// 从 `message.usage` / `providerData.usage` / `providerData.rawUsage` 读取缓存命中 token。
pub fn workbuddy_cache_read(
    message_usage: &JsonValue,
    provider_usage: &JsonValue,
    raw_usage: &JsonValue,
) -> i64 {
    let direct = number(
        message_usage,
        &["cache_read_input_tokens", "cacheReadInputTokens"],
    )
    .max(number(
        provider_usage,
        &["cache_read_input_tokens", "cacheReadInputTokens"],
    ));
    if direct > 0 {
        return direct;
    }
    // 上游把命中量写在 prompt_cache_hit_tokens / prompt_tokens_details.cached_tokens。
    let hit = number(raw_usage, &["prompt_cache_hit_tokens", "promptCacheHitTokens"]);
    if hit > 0 {
        return hit;
    }
    let nested = raw_usage
        .get("prompt_tokens_details")
        .or_else(|| raw_usage.get("promptTokensDetails"))
        .map(|details| number(details, &["cached_tokens", "cachedTokens"]))
        .unwrap_or(0);
    if nested > 0 {
        return nested;
    }
    provider_usage
        .get("inputTokensDetails")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| number(item, &["cached_tokens", "cachedTokens"]))
                .sum()
        })
        .unwrap_or(0)
}

/// WorkBuddy 项目目录名 → 项目键。
/// 目录名是绝对路径把 `/` 换成 `-` 的结果，但**省略了前导 `-`**
/// （如 `/Applications/custom/OpenHub` → `Applications-custom-OpenHub`），
/// 与 Claude 的 `-Users-name-dir` 不同，需补回前导 `-` 才能反解。
pub fn workbuddy_project_from_dir(dir_name: &str) -> String {
    let raw = dir_name.trim();
    if raw.is_empty() {
        return "WorkBuddy".to_string();
    }
    let encoded = if raw.starts_with('-') {
        raw.to_string()
    } else {
        format!("-{raw}")
    };
    project_key_from_encoded_dir_name(&encoded, "WorkBuddy")
}

pub fn parse_workbuddy_file(path: &Path) -> CachedFile {
    let Ok(text) = fs::read_to_string(path) else {
        return CachedFile {
            fingerprint: fingerprint(path),
            ..Default::default()
        };
    };
    let fallback_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string();
    let mut session_id = fallback_id.clone();
    let dir_project_key = workbuddy_project_from_dir(
        path.parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or(""),
    );
    let mut cwd_key: Option<String> = None;
    let mut model = String::new();
    let mut first_ts = String::new();
    let mut last_ts = String::new();
    let mut user_events: BTreeMap<String, String> = BTreeMap::new();
    let mut usage_events: BTreeMap<String, UsageEvent> = BTreeMap::new();
    let mut last_user_ts = String::new();
    let mut user_models: BTreeMap<String, String> = BTreeMap::new();
    let mut pending_user_ids: Vec<String> = Vec::new();

    for (index, line) in text.lines().enumerate() {
        let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
            continue;
        };
        if let Some(value) = value
            .get("sessionId")
            .or_else(|| value.get("session_id"))
            .and_then(JsonValue::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            session_id = value.to_string();
        }
        // 会话归属 = 首次进入的工作目录；工具调用结果等行不改变归属。
        if cwd_key.is_none() {
            if let Some(cwd) = value
                .get("cwd")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.trim().is_empty())
            {
                cwd_key = project_key_from_location(cwd);
            }
        }
        // 时间边界覆盖会话的全部活动（含工具调用、快照等），
        // 否则以工具调用收尾的会话 ended_at 会早于真实结束时间。
        let timestamp = workbuddy_timestamp(&value);
        update_bounds(&mut first_ts, &mut last_ts, &timestamp);
        let kind = value.get("type").and_then(JsonValue::as_str).unwrap_or("");
        let role = value.get("role").and_then(JsonValue::as_str).unwrap_or("");
        if kind != "message" && role.is_empty() {
            continue;
        }

        if role == "user" {
            last_user_ts = timestamp.clone();
            let id = value
                .get("id")
                .and_then(JsonValue::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("{session_id}:user:{index}"));
            user_events.entry(id.clone()).or_insert(timestamp);
            pending_user_ids.push(id);
            continue;
        }

        let provider_data = value.get("providerData").unwrap_or(&JsonValue::Null);
        let message_usage = value
            .get("message")
            .and_then(|message| message.get("usage"))
            .filter(|usage| usage.is_object())
            .cloned();
        let provider_usage = provider_data
            .get("usage")
            .filter(|usage| usage.is_object())
            .cloned()
            .unwrap_or(JsonValue::Null);
        let raw_usage = provider_data
            .get("rawUsage")
            .filter(|usage| usage.is_object())
            .cloned()
            .unwrap_or(JsonValue::Null);
        let usage = match message_usage.as_ref().filter(|usage| usage.is_object()) {
            Some(usage) => usage.clone(),
            None => provider_usage.clone(),
        };
        if !usage.is_object() {
            continue;
        }

        let input = number(&usage, &["input_tokens", "inputTokens", "prompt_tokens"]);
        let output = number(
            &usage,
            &["output_tokens", "outputTokens", "completion_tokens"],
        );
        let cached = workbuddy_cache_read(&usage, &provider_usage, &raw_usage);
        let cache_write = workbuddy_cache_write(&raw_usage, &usage);
        let reasoning = workbuddy_reasoning(&raw_usage, &provider_usage);
        // 实测口径：input_tokens/prompt_tokens 为**总输入**，缓存命中是其子集
        // （prompt_cache_hit_tokens + prompt_cache_miss_tokens = prompt_tokens）。
        // 因此按 InclusiveOfCacheRead 拆分，避免缓存命中被重复叠加导致 total 虚高。
        let (input, cached, _cache_write, output, reasoning, total) = normalize_usage(RawUsage {
            input,
            semantics: InputSemantics::InclusiveOfCacheRead,
            cache_read: cached,
            cache_write,
            output,
            reasoning,
        });
        if total <= 0 || timestamp.is_empty() {
            continue;
        }
        let turn_model = workbuddy_model(provider_data).unwrap_or_else(|| {
            if model.is_empty() {
                UNKNOWN_WORKBUDDY_MODEL.to_string()
            } else {
                model.clone()
            }
        });
        if model.is_empty() {
            model = turn_model.clone();
        }
        for pending_id in pending_user_ids.drain(..) {
            user_models.entry(pending_id).or_insert(turn_model.clone());
        }
        let message_id = value
            .get("id")
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("{session_id}:assistant:{index}"));
        let event = UsageEvent {
            id: message_id.clone(),
            source: "workbuddy".to_string(),
            model: turn_model,
            timestamp: if last_user_ts.is_empty() {
                timestamp
            } else {
                last_user_ts.clone()
            },
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_input_tokens: cache_write,
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            total_tokens: total,
            conversation_count: 0,
            cost_usd: 0.0,
            pricing_available: false,
            estimated_tokens: 0,
            ..Default::default()
        };
        let should_replace = usage_events
            .get(&message_id)
            .map(|existing| event.total_tokens > existing.total_tokens)
            .unwrap_or(true);
        if should_replace {
            usage_events.insert(message_id, event);
        }
    }

    if model.is_empty() {
        model = UNKNOWN_WORKBUDDY_MODEL.to_string();
    }
    let project_key = cwd_key.unwrap_or(dir_project_key);
    let mut events = usage_events.into_values().collect::<Vec<_>>();
    events.extend(user_events.into_iter().map(|(id, timestamp)| UsageEvent {
        id: format!("u:{id}"),
        source: "workbuddy".to_string(),
        model: user_models
            .get(&id)
            .cloned()
            .unwrap_or_else(|| model.clone()),
        timestamp,
        conversation_count: 1,
        ..Default::default()
    }));
    for event in &mut events {
        event.project_key = project_key.clone();
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
    let turns = events.iter().map(|event| event.conversation_count).sum();
    let session = token_session(
        session_id,
        "workbuddy",
        project_key,
        model,
        first_ts,
        last_ts,
        turns,
        tokens,
        0.0,
    );
    CachedFile {
        fingerprint: fingerprint(path),
        events,
        sessions: vec![session],
    }
}

/// WorkBuddy 转录里的时间戳是毫秒 epoch 数值；兼容 ISO 字符串与秒级数值。
pub fn workbuddy_timestamp(value: &JsonValue) -> String {
    if let Some(text) = value.get("timestamp").and_then(JsonValue::as_str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let raw = number(value, &["timestamp", "created_at", "createdAt", "time"]);
    if raw <= 0 {
        return String::new();
    }
    // 阈值取 10^11：毫秒时间戳（≥2001 年）保持原样，秒级时间戳换算为毫秒。
    let millis = if raw < 100_000_000_000 { raw * 1000 } else { raw };
    iso_from_millis(millis)
}
