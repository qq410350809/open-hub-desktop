use super::store;
use super::types::*;
use crate::context::AppContext;
use crate::model::gateway::types::{ChannelConfig, ModelProxyContext};
use serde_json::{json, Value as JsonValue};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::warn;

/// 单个被测任务的运行时描述。
#[derive(Debug, Clone)]
struct JobSpec {
    target: ProbeTarget,
    channel_name: String,
    /// 带渠道别名前缀的请求模型名（{alias}/{model}），由网关前缀路由定向到该渠道。
    request_model: String,
}

/// 把「渠道+模型」拼成带渠道别名前缀的请求模型名，并校验渠道存在且启用。
pub(crate) fn resolve_probe_model(
    channels: &[ChannelConfig],
    target: &ProbeTarget,
) -> Result<String, String> {
    let channel = channels
        .iter()
        .find(|channel| channel.id == target.channel_id)
        .ok_or_else(|| format!("渠道 {} 不存在或已被删除", target.channel_id))?;
    if !channel.enabled {
        return Err(format!("渠道「{}」未启用，请先在模型代理页开启", channel.name));
    }
    if target.model.trim().is_empty() {
        return Err(format!("渠道「{}」存在未指定模型的空目标", channel.name));
    }
    Ok(format!("{}/{}", channel.effective_alias(), target.model.trim()))
}

fn split_keywords(value: &str) -> Vec<String> {
    value
        .split([',', '，', ';', '；', '\n'])
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(str::to_string)
        .collect()
}

fn truncate_text(content: &str, limit: usize) -> String {
    if content.chars().count() <= limit {
        return content.to_string();
    }
    let mut truncated: String = content.chars().take(limit).collect();
    truncated.push('…');
    truncated
}

/// 从自由文本中提取所有数字（忽略千分位逗号），供 number 判分比对。
pub(crate) fn extract_numbers(content: &str) -> Vec<f64> {
    let cleaned: String = content.chars().filter(|ch| *ch != ',').collect();
    let mut numbers = Vec::new();
    let mut current = String::new();
    for ch in cleaned.chars() {
        let keep = ch.is_ascii_digit()
            || ch == '.'
            || (ch == '-' && current.is_empty());
        if keep {
            current.push(ch);
        } else if !current.is_empty() {
            if let Ok(value) = current.parse::<f64>() {
                numbers.push(value);
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(value) = current.parse::<f64>() {
            numbers.push(value);
        }
    }
    numbers
}

/// 客观题自动判分。
pub(crate) fn run_auto_check(check: &CheckSpec, content: &str) -> AutoCheckOutcome {
    let mut outcome = AutoCheckOutcome {
        kind: check.kind.clone(),
        passed: false,
        detail: String::new(),
    };
    match check.kind.as_str() {
        "contains" | "not_contains" => {
            let keywords = split_keywords(&check.value);
            if keywords.is_empty() {
                outcome.detail = "未配置关键词".to_string();
                return outcome;
            }
            let lowered = content.to_lowercase();
            let hits: Vec<String> = keywords
                .iter()
                .filter(|keyword| lowered.contains(&keyword.to_lowercase()))
                .cloned()
                .collect();
            if check.kind == "contains" {
                outcome.passed = !hits.is_empty();
                outcome.detail = if hits.is_empty() {
                    format!("未命中任何关键词（{}）", keywords.join("、"))
                } else {
                    format!("命中关键词：{}", hits.join("、"))
                };
            } else {
                outcome.passed = hits.is_empty();
                outcome.detail = if hits.is_empty() {
                    "未出现禁用内容".to_string()
                } else {
                    format!("出现了禁用内容：{}", hits.join("、"))
                };
            }
        }
        "number" => {
            let expected: f64 = match check.value.trim().parse::<f64>() {
                Ok(value) => value,
                Err(_) => {
                    outcome.detail = format!("期望值不是数字：{}", check.value);
                    return outcome;
                }
            };
            let found = extract_numbers(content);
            let tolerance = if check.tolerance.is_finite() {
                check.tolerance.abs()
            } else {
                0.0
            }
            .max(1e-9);
            let hit = found
                .iter()
                .copied()
                .find(|value| (value - expected).abs() <= tolerance);
            outcome.passed = hit.is_some();
            outcome.detail = match hit {
                Some(value) => format!("命中数值 {value}"),
                None => {
                    let listed = found
                        .iter()
                        .take(8)
                        .map(|value| value.to_string())
                        .collect::<Vec<_>>()
                        .join("、");
                    format!("期望 {expected}，回答中的数值：{listed}")
                }
            };
        }
        "json" => {
            match crate::token::mapping::ai::extract_json(content) {
                None => outcome.detail = "回答不是合法 JSON".to_string(),
                Some(value) => {
                    let expected = check.value.trim();
                    if expected.is_empty() {
                        outcome.passed = true;
                        outcome.detail = "JSON 合法".to_string();
                    } else {
                        let serialized = value.to_string();
                        outcome.passed = serialized.contains(expected);
                        outcome.detail = if outcome.passed {
                            "JSON 合法且包含期望内容".to_string()
                        } else {
                            format!(
                                "JSON 合法但未包含期望内容：{}",
                                truncate_text(&serialized, 200)
                            )
                        };
                    }
                }
            }
        }
        other => outcome.detail = format!("未知判分类型：{other}"),
    }
    outcome
}

fn content_from_payload(payload: &JsonValue) -> String {
    payload
        .pointer("/choices/0/message/content")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// 用评审模型给开放题回答打分（0-10）。失败不致命：score 为空并记录原因。
async fn judge_response(
    gateway_ctx: &ModelProxyContext,
    judge_model: &str,
    prompt: &ProbePrompt,
    response: &str,
) -> JudgeOutcome {
    let mut text = format!(
        "你是严格的模型能力评审员，请给「被测模型的回答」打 0-10 分。\n\
         评分标准：0-2 明显胡编、拒答或完全跑题；3-5 部分正确但有明显缺陷；\
         6-8 大体正确、有小瑕疵；9-10 完全正确且表达清晰。\n\n\
         【题目】\n{}\n\n【被测模型的回答】\n{}",
        prompt.text, response
    );
    if let Some(check) = prompt.check.as_ref().filter(|check| !check.value.trim().is_empty()) {
        text.push_str(&format!("\n\n【参考答案要点】\n{}", check.value.trim()));
    }
    text.push_str("\n\n只输出 JSON，形如 {\"score\": 8.5, \"reason\": \"一句中文理由\"}");

    let body = json!({
        "model": judge_model,
        "temperature": 0,
        "max_tokens": 512,
        "stream": false,
        "messages": [{ "role": "user", "content": text }],
    });
    match crate::model::gateway::handlers::chat::internal_chat_completion_body(
        gateway_ctx,
        body,
        "OpenHub-ModelTestJudge",
    )
    .await
    {
        Err(error) => JudgeOutcome {
            score: None,
            reason: format!("评审调用失败：{error}"),
        },
        Ok(payload) => {
            let content = content_from_payload(&payload);
            let Some(value) = crate::token::mapping::ai::extract_json(&content) else {
                return JudgeOutcome {
                    score: None,
                    reason: format!("评审输出不是 JSON：{}", truncate_text(&content, 200)),
                };
            };
            let score = value
                .get("score")
                .and_then(|score| {
                    score
                        .as_f64()
                        .or_else(|| score.as_str().and_then(|raw| raw.trim().parse::<f64>().ok()))
                })
                .map(|score| score.clamp(0.0, 10.0));
            let reason = value
                .get("reason")
                .and_then(JsonValue::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            match score {
                Some(score) => JudgeOutcome {
                    score: Some(score),
                    reason,
                },
                None => JudgeOutcome {
                    score: None,
                    reason: format!("评审输出缺少 score：{}", truncate_text(&content, 200)),
                },
            }
        }
    }
}

const RESPONSE_TEXT_LIMIT: usize = 60_000;

/// 执行单个「目标×提示词」测试。
async fn execute_one(
    gateway_ctx: &ModelProxyContext,
    job: &JobSpec,
    prompt: &ProbePrompt,
    timeout_seconds: u64,
    judge_model: Option<&str>,
) -> ProbeResult {
    let mut result = ProbeResult {
        channel_id: job.target.channel_id.clone(),
        channel_name: job.channel_name.clone(),
        model: job.target.model.clone(),
        prompt_id: prompt.id.clone(),
        prompt_name: prompt.name.clone(),
        category: prompt.category.clone(),
        ..Default::default()
    };
    let started = Instant::now();
    let body = json!({
        "model": job.request_model,
        "temperature": prompt.temperature,
        "max_tokens": prompt.max_tokens,
        "stream": false,
        "messages": [{ "role": "user", "content": prompt.text }],
    });
    let timeout = Duration::from_secs(timeout_seconds.max(1));
    let call = crate::model::gateway::handlers::chat::internal_chat_completion_body(
        gateway_ctx,
        body,
        "OpenHub-ModelTest",
    );
    let payload = match tokio::time::timeout(timeout, call).await {
        Err(_) => {
            result.duration_ms = Some(started.elapsed().as_millis() as u64);
            result.error = Some(format!("请求超时（>{timeout_seconds}s）"));
            return result;
        }
        Ok(Err(error)) => {
            result.duration_ms = Some(started.elapsed().as_millis() as u64);
            result.error = Some(error);
            return result;
        }
        Ok(Ok(payload)) => payload,
    };

    result.duration_ms = Some(started.elapsed().as_millis() as u64);
    if let Some(usage) = payload.get("usage") {
        result.prompt_tokens = usage
            .get("prompt_tokens")
            .or_else(|| usage.get("input_tokens"))
            .and_then(JsonValue::as_u64);
        result.completion_tokens = usage
            .get("completion_tokens")
            .or_else(|| usage.get("output_tokens"))
            .and_then(JsonValue::as_u64);
    }
    if let Some(tokens) = result.completion_tokens {
        let seconds = result.duration_ms.unwrap_or(0) as f64 / 1000.0;
        if seconds > 0.0 {
            result.tokens_per_sec = Some(tokens as f64 / seconds);
        }
    }

    let content = content_from_payload(&payload);
    if content.is_empty() {
        result.error =
            Some("响应内容为空（可能只输出了思考链，或被 max_tokens 截断）".to_string());
        return result;
    }
    result.response_text = Some(truncate_text(&content, RESPONSE_TEXT_LIMIT));
    result.ok = true;

    if let Some(check) = &prompt.check {
        let outcome = run_auto_check(check, &content);
        if !outcome.passed {
            result.ok = false;
            result.error = Some(format!("自动判分未通过：{}", outcome.detail));
        }
        if !prompt.judge {
            result.score = Some(if outcome.passed { 10.0 } else { 0.0 });
        }
        result.auto_check = Some(outcome);
    }

    if prompt.judge {
        if let Some(judge_model) = judge_model {
            let outcome = judge_response(gateway_ctx, judge_model, prompt, &content).await;
            if result.score.is_none() {
                result.score = outcome.score;
            }
            result.judge = Some(outcome);
        }
    }
    result
}

fn average(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        None
    } else {
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }
}

/// 汇总：按目标（模型维度）与按提示词（题目维度）各聚合一份。
pub(crate) fn build_summary(params: &RunParams, results: &[ProbeResult]) -> RunSummary {
    let mut models = Vec::new();
    for target in &params.targets {
        let mine: Vec<&ProbeResult> = results
            .iter()
            .filter(|result| {
                result.channel_id == target.channel_id && result.model == target.model
            })
            .collect();
        if mine.is_empty() {
            continue;
        }
        let channel_name = mine[0].channel_name.clone();
        let durations: Vec<f64> = mine
            .iter()
            .filter_map(|result| result.duration_ms.map(|value| value as f64))
            .collect();
        let speeds: Vec<f64> = mine.iter().filter_map(|result| result.tokens_per_sec).collect();
        models.push(ModelSummary {
            channel_id: target.channel_id.clone(),
            channel_name,
            model: target.model.clone(),
            total: mine.len() as u32,
            ok_count: mine.iter().filter(|result| result.ok).count() as u32,
            avg_score: average(
                &mine
                    .iter()
                    .filter_map(|result| result.score)
                    .collect::<Vec<_>>(),
            ),
            avg_duration_ms: average(&durations).map(|value| value.round() as u64),
            avg_tokens_per_sec: average(&speeds),
        });
    }

    let mut prompts = Vec::new();
    for prompt in &params.prompts {
        let mine: Vec<&ProbeResult> = results
            .iter()
            .filter(|result| result.prompt_id == prompt.id)
            .collect();
        if mine.is_empty() {
            continue;
        }
        let durations: Vec<f64> = mine
            .iter()
            .filter_map(|result| result.duration_ms.map(|value| value as f64))
            .collect();
        prompts.push(PromptSummary {
            prompt_id: prompt.id.clone(),
            prompt_name: prompt.name.clone(),
            category: prompt.category.clone(),
            total: mine.len() as u32,
            ok_count: mine.iter().filter(|result| result.ok).count() as u32,
            avg_score: average(
                &mine
                    .iter()
                    .filter_map(|result| result.score)
                    .collect::<Vec<_>>(),
            ),
            avg_duration_ms: average(&durations).map(|value| value.round() as u64),
        });
    }
    RunSummary { models, prompts }
}

fn validate_params(params: &RunParams) -> Result<(), String> {
    if params.targets.is_empty() {
        return Err("请先选择要测试的模型".to_string());
    }
    if params.prompts.is_empty() {
        return Err("请至少选择一条测试提示词".to_string());
    }
    if params.targets.iter().any(|target| target.channel_id.trim().is_empty()) {
        return Err("存在未指定渠道的测试目标".to_string());
    }
    if params
        .prompts
        .iter()
        .any(|prompt| prompt.text.trim().is_empty())
    {
        return Err("存在提示词内容为空的测试项".to_string());
    }
    Ok(())
}

/// 启动一次测试：校验 → 落库 running → 后台并发执行 → 立即返回运行句柄。
pub async fn start_model_test(
    ctx: Arc<AppContext>,
    gateway_ctx: Arc<ModelProxyContext>,
    params: RunParams,
) -> Result<RunStartInfo, String> {
    let runtime = ctx.model_probe.clone();
    if runtime
        .running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("已有模型测试正在运行，请等待完成或先取消".to_string());
    }
    if let Err(error) = validate_params(&params) {
        runtime.running.store(false, Ordering::SeqCst);
        return Err(error);
    }

    let channels = gateway_ctx.config.read().await.channels.clone();
    let mut jobs = Vec::new();
    for target in &params.targets {
        let request_model = match resolve_probe_model(&channels, target) {
            Ok(model) => model,
            Err(error) => {
                runtime.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let channel_name = channels
            .iter()
            .find(|channel| channel.id == target.channel_id)
            .map(|channel| channel.name.clone())
            .unwrap_or_else(|| target.channel_id.clone());
        jobs.push(JobSpec {
            target: target.clone(),
            channel_name,
            request_model,
        });
    }
    let judge_model = match params.judge.as_ref() {
        Some(spec) => {
            let resolved = resolve_probe_model(
                &channels,
                &ProbeTarget {
                    channel_id: spec.channel_id.clone(),
                    model: spec.model.clone(),
                },
            );
            match resolved {
                Ok(model) => Some(model),
                Err(error) => {
                    runtime.running.store(false, Ordering::SeqCst);
                    return Err(format!("评审模型不可用：{error}"));
                }
            }
        }
        None => None,
    };
    if params.prompts.iter().any(|prompt| prompt.judge) && judge_model.is_none() {
        runtime.running.store(false, Ordering::SeqCst);
        return Err("所选提示词包含需要 LLM 评审的开放题，请先选择评审模型".to_string());
    }

    let total = (jobs.len() * params.prompts.len()) as u32;
    let _ = store::reap_stale_runs(&ctx.database, None);
    let run_id = store::insert_run(&ctx.database, &crate::model::gateway::current_timestamp(), &params)?;

    let token = CancellationToken::new();
    if let Ok(mut guard) = runtime.active_cancellation.lock() {
        *guard = Some(token.clone());
    }
    if let Ok(mut guard) = runtime.active_run_id.lock() {
        *guard = Some(run_id);
    }

    let start_info = RunStartInfo { run_id, total };
    crate::context::spawn(run_all(
        ctx,
        gateway_ctx,
        params,
        jobs,
        judge_model,
        run_id,
        total,
        token,
    ));
    Ok(start_info)
}

#[allow(clippy::too_many_arguments)]
async fn run_all(
    ctx: Arc<AppContext>,
    gateway_ctx: Arc<ModelProxyContext>,
    params: RunParams,
    jobs: Vec<JobSpec>,
    judge_model: Option<String>,
    run_id: i64,
    total: u32,
    token: CancellationToken,
) {
    let runtime = ctx.model_probe.clone();
    let concurrency = params.concurrency.clamp(1, 16) as usize;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let completed = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::with_capacity(jobs.len() * params.prompts.len());
    for job in &jobs {
        for prompt in &params.prompts {
            let gateway = gateway_ctx.clone();
            let job_ctx = ctx.clone();
            let semaphore = semaphore.clone();
            let job_token = token.clone();
            let completed = completed.clone();
            let job = job.clone();
            let prompt = prompt.clone();
            let timeout_seconds = params.timeout_seconds;
            let judge_model = judge_model.clone();
            handles.push(crate::context::spawn(async move {
                let _permit = match semaphore.acquire_owned().await {
                    Ok(permit) => permit,
                    Err(error) => {
                        warn!("[model-test] 信号量获取失败：{error}");
                        return None;
                    }
                };
                if job_token.is_cancelled() {
                    return None;
                }
                let result = execute_one(
                    &gateway,
                    &job,
                    &prompt,
                    timeout_seconds,
                    judge_model.as_deref(),
                )
                .await;
                if let Err(error) = store::insert_result(&job_ctx.database, run_id, &result) {
                    warn!("[model-test] 结果写入失败：{error}");
                }
                let done = completed.fetch_add(1, Ordering::Relaxed) as u32 + 1;
                job_ctx.event_bus.emit(
                    "model-test-progress",
                    RunProgress {
                        run_id,
                        phase: "running".to_string(),
                        completed: done,
                        total,
                        result: Some(result),
                    },
                );
                Some(())
            }));
        }
    }
    let _ = futures_util::future::join_all(handles).await;

    // 收集结果重新从库里读，保证与落库内容一致
    let results = store::get_run_results(&ctx.database, run_id).unwrap_or_default();
    let cancelled = token.is_cancelled();
    let status = if cancelled { "cancelled" } else { "finished" };
    let summary = build_summary(&params, &results);
    if let Err(error) = store::finish_run(&ctx.database, run_id, status, &summary) {
        warn!("[model-test] 运行收尾失败：{error}");
    }

    if let Ok(mut guard) = runtime.active_cancellation.lock() {
        *guard = None;
    }
    if let Ok(mut guard) = runtime.active_run_id.lock() {
        *guard = None;
    }
    runtime.running.store(false, Ordering::SeqCst);

    ctx.event_bus.emit(
        "model-test-progress",
        RunProgress {
            run_id,
            phase: status.to_string(),
            completed: total,
            total,
            result: None,
        },
    );
}

/// 取消当前正在运行的测试。
pub fn cancel_model_test(ctx: &Arc<AppContext>) -> Result<(), String> {
    let runtime = &ctx.model_probe;
    if !runtime.running.load(Ordering::SeqCst) {
        return Err("当前没有正在运行的模型测试".to_string());
    }
    let token = runtime
        .active_cancellation
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    match token {
        Some(token) => {
            token.cancel();
            Ok(())
        }
        None => Err("当前没有正在运行的模型测试".to_string()),
    }
}
