use super::fingerprints;
use super::store;
use super::types::*;
use crate::context::AppContext;
use crate::model::gateway::types::{ChannelConfig, ModelProxyContext};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
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
        "exact" => {
            let normalized: String = content
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
                .to_lowercase();
            let expected: String = check
                .value
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
                .to_lowercase();
            outcome.passed = normalized == expected;
            outcome.detail = if outcome.passed {
                "与期望输出完全一致".to_string()
            } else {
                format!(
                    "期望「{}」，实际「{}」",
                    check.value,
                    truncate_text(&normalized, 80)
                )
            };
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

const RESPONSE_TEXT_LIMIT: usize = 60_000;

/// 执行单个「目标×探测题×采样」请求。
async fn execute_one(
    gateway_ctx: &ModelProxyContext,
    job: &JobSpec,
    probe: &DetectionProbe,
    sample_index: u32,
    timeout_seconds: u64,
) -> ProbeResult {
    let mut result = ProbeResult {
        channel_id: job.target.channel_id.clone(),
        channel_name: job.channel_name.clone(),
        model: job.target.model.clone(),
        probe_id: probe.id.clone(),
        probe_name: probe.name.clone(),
        category: probe.category.clone(),
        sample_index,
        ..Default::default()
    };
    let started = Instant::now();
    // 对话伪装：随机变体 + 随机闲聊前缀，去除同质化、避免被渠道识别为测试流量
    let mut rng = fingerprints::Rng::from_entropy();
    let (messages, asked) = fingerprints::compose_messages(probe, &mut rng);
    result.request_text = Some(truncate_text(&asked, 2000));
    let body = json!({
        "model": job.request_model,
        "temperature": probe.temperature,
        "max_tokens": probe.max_tokens,
        "stream": false,
        "messages": messages,
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

    // 家族命中：指纹题按期望答案匹配；身份题按自述关键词检测
    result.family_match = match probe.category.as_str() {
        "fingerprint" => fingerprints::match_family(&content, &probe.expected),
        "identity" => fingerprints::detect_identity_family(&content),
        _ => None,
    };

    if let Some(check) = &probe.check {
        let outcome = run_auto_check(check, &content);
        if !outcome.passed {
            result.ok = false;
            result.error = Some(format!("判分未通过：{}", outcome.detail));
        }
        result.auto_check = Some(outcome);
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

/// 对某家族投票取多数派；无票或并列返回 None。
fn plurality_family(votes: &[String]) -> Option<String> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for vote in votes {
        *counts.entry(vote.as_str()).or_default() += 1;
    }
    let mut best: Option<(&str, usize)> = None;
    let mut tie = false;
    for (family, count) in counts {
        match best {
            Some((_, best_count)) if count > best_count => {
                best = Some((family, count));
                tie = false;
            }
            Some((_, best_count)) if count == best_count => tie = true,
            Some(_) => {}
            None => best = Some((family, count)),
        }
    }
    if tie {
        None
    } else {
        best.map(|(family, _)| family.to_string())
    }
}

/// 按目标聚合验真结论。results 须为同一 run 的全部探测结果。
pub(crate) fn build_verdicts(results: &[ProbeResult]) -> Vec<TargetVerdict> {
    // 按「渠道×模型」分组，保持首次出现顺序
    let mut order: Vec<(String, String)> = Vec::new();
    let mut groups: HashMap<(String, String), Vec<&ProbeResult>> = HashMap::new();
    for result in results {
        let key = (result.channel_id.clone(), result.model.clone());
        groups.entry(key.clone()).or_default().push(result);
        if !order.contains(&key) {
            order.push(key);
        }
    }

    order
        .into_iter()
        .filter_map(|key| {
            let mine = groups.get(&key)?;
            let first = mine[0];
            let mut verdict = TargetVerdict {
                channel_id: first.channel_id.clone(),
                channel_name: first.channel_name.clone(),
                model: first.model.clone(),
                total_requests: mine.len() as u32,
                ok_count: mine.iter().filter(|r| r.ok).count() as u32,
                ..Default::default()
            };
            let mut issues = Vec::new();

            verdict.claimed_family = fingerprints::family_of_model(&verdict.model);

            // 身份自述：命中家族的票取多数派
            let identity_votes: Vec<String> = mine
                .iter()
                .filter(|r| r.category == "identity" && r.ok)
                .filter_map(|r| r.family_match.clone())
                .collect();
            verdict.identity_family = plurality_family(&identity_votes);
            verdict.identity_consistent = match (&verdict.claimed_family, &verdict.identity_family)
            {
                (Some(claimed), Some(reported)) => Some(claimed == reported),
                _ => None,
            };

            // 指纹投票
            let fingerprint_votes: Vec<String> = mine
                .iter()
                .filter(|r| r.category == "fingerprint" && r.ok)
                .filter_map(|r| r.family_match.clone())
                .collect();
            verdict.detected_family = plurality_family(&fingerprint_votes);

            // 能力通过率
            let capability: Vec<&&ProbeResult> = mine
                .iter()
                .filter(|r| r.category == "capability")
                .collect();
            verdict.capability_total = capability.len() as u32;
            verdict.capability_passed = capability.iter().filter(|r| r.ok).count() as u32;

            // 一致性：采样数 > 1 的探测题，各采样的判分结论（ok + 家族命中）须一致。
            // 问法经随机变体包装，原文比对无意义，故比对结论而非文本。
            let mut sample_groups: HashMap<&str, Vec<(bool, Option<&str>)>> = HashMap::new();
            for r in mine.iter() {
                sample_groups
                    .entry(r.probe_id.as_str())
                    .or_default()
                    .push((r.ok, r.family_match.as_deref()));
            }
            let repeat_groups: Vec<bool> = sample_groups
                .values()
                .filter(|samples| samples.len() > 1)
                .map(|samples| {
                    samples.windows(2).all(|pair| pair[0] == pair[1])
                })
                .collect();
            verdict.consistency_rate = if repeat_groups.is_empty() {
                None
            } else {
                let consistent = repeat_groups.iter().filter(|ok| **ok).count();
                Some(consistent as f64 / repeat_groups.len() as f64)
            };

            let durations: Vec<f64> = mine
                .iter()
                .filter_map(|r| r.duration_ms.map(|v| v as f64))
                .collect();
            let speeds: Vec<f64> = mine.iter().filter_map(|r| r.tokens_per_sec).collect();
            verdict.avg_duration_ms = average(&durations).map(|v| v.round() as u64);
            verdict.avg_tokens_per_sec = average(&speeds);

            // —— 结论判定 ——
            verdict.verdict = if verdict.ok_count == 0 {
                issues.push(format!(
                    "{} 次探测全部失败，渠道或模型不可用",
                    verdict.total_requests
                ));
                "unreachable".to_string()
            } else {
                let mut impersonated = false;
                if verdict.identity_consistent == Some(false) {
                    impersonated = true;
                    issues.push(format!(
                        "模型自述为「{}」系，与标称「{}」不符",
                        verdict.identity_family.clone().unwrap_or_default(),
                        verdict.claimed_family.clone().unwrap_or_default()
                    ));
                }
                if let (Some(claimed), Some(detected)) =
                    (&verdict.claimed_family, &verdict.detected_family)
                {
                    if claimed != detected {
                        impersonated = true;
                        issues.push(format!(
                            "指纹题投票指向「{detected}」系，与标称「{claimed}」不符"
                        ));
                    }
                }
                if impersonated {
                    "impersonation".to_string()
                } else {
                    let mut suspicious = false;
                    let capability_rate = if verdict.capability_total > 0 {
                        verdict.capability_passed as f64 / (verdict.capability_total as f64)
                    } else {
                        1.0
                    };
                    if verdict.capability_total > 0 && capability_rate < 0.5 {
                        suspicious = true;
                        issues.push(format!(
                            "能力题通过率仅 {}/{}，疑似降智（量化/蒸馏/小模型冒充）",
                            verdict.capability_passed, verdict.capability_total
                        ));
                    }
                    if verdict.consistency_rate == Some(0.0) {
                        suspicious = true;
                        issues.push("重复采样答案完全不一致，渠道可能随机偷换模型".to_string());
                    }
                    if verdict.ok_count < verdict.total_requests {
                        suspicious = true;
                        issues.push(format!(
                            "{}/{} 次探测失败",
                            verdict.total_requests - verdict.ok_count,
                            verdict.total_requests
                        ));
                    }
                    if suspicious { "suspicious" } else { "ok" }.to_string()
                }
            };
            verdict.issues = issues;
            verdict.results = mine.iter().map(|r| (*r).clone()).collect();
            Some(verdict)
        })
        .collect()
}

/// 汇总为整次运行的摘要（存 summary_json，不含明细）。
pub(crate) fn build_summary(results: &[ProbeResult]) -> RunSummary {
    RunSummary {
        targets: build_verdicts(results)
            .into_iter()
            .map(|mut verdict| {
                verdict.results.clear();
                verdict
            })
            .collect(),
    }
}

fn validate_params(params: &RunParams) -> Result<Vec<DetectionProbe>, String> {
    if params.targets.is_empty() {
        return Err("请先选择要检测的渠道模型".to_string());
    }
    if params.probe_ids.is_empty() {
        return Err("请至少选择一道探测题".to_string());
    }
    if params.targets.iter().any(|target| target.channel_id.trim().is_empty()) {
        return Err("存在未指定渠道的检测目标".to_string());
    }
    let catalog = fingerprints::builtin_probes();
    let mut probes = Vec::new();
    for id in &params.probe_ids {
        let probe = catalog
            .iter()
            .find(|probe| &probe.id == id)
            .ok_or_else(|| format!("探测题 {id} 不在内置目录中"))?;
        probes.push(probe.clone());
    }
    Ok(probes)
}

/// 启动一次验真检测：校验 → 落库 running → 后台并发执行 → 立即返回运行句柄。
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
        return Err("已有模型验真正在运行，请等待完成或先取消".to_string());
    }
    let cleanup = |runtime: &Arc<crate::model::probe::types::ProbeRuntime>| {
        runtime.running.store(false, Ordering::SeqCst);
    };
    let probes = match validate_params(&params) {
        Ok(probes) => probes,
        Err(error) => {
            cleanup(&runtime);
            return Err(error);
        }
    };

    let channels = gateway_ctx.config.read().await.channels.clone();
    let mut jobs = Vec::new();
    for target in &params.targets {
        let request_model = match resolve_probe_model(&channels, target) {
            Ok(model) => model,
            Err(error) => {
                cleanup(&runtime);
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

    let repeats = params.repeats.clamp(1, 5);
    let samples_per_probe: Vec<u32> = probes
        .iter()
        .map(|probe| if probe.repeats { repeats } else { 1 })
        .collect();
    let total: u32 = (jobs.len() as u32) * samples_per_probe.iter().sum::<u32>();

    let _ = store::reap_stale_runs(&ctx.database, None);
    let run_id =
        store::insert_run(&ctx.database, &crate::model::gateway::current_timestamp(), &params)?;

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
        probes,
        jobs,
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
    probes: Vec<DetectionProbe>,
    jobs: Vec<JobSpec>,
    run_id: i64,
    total: u32,
    token: CancellationToken,
) {
    let runtime = ctx.model_probe.clone();
    let repeats = params.repeats.clamp(1, 5);
    let concurrency = params.concurrency.clamp(1, 16) as usize;
    let semaphore = Arc::new(Semaphore::new(concurrency));
    let completed = Arc::new(AtomicUsize::new(0));

    let mut task_count = 0usize;
    let mut handles = Vec::new();
    for job in &jobs {
        for probe in &probes {
            let samples = if probe.repeats { repeats } else { 1 };
            for sample_index in 0..samples {
                task_count += 1;
                let gateway = gateway_ctx.clone();
                let job_ctx = ctx.clone();
                let semaphore = semaphore.clone();
                let job_token = token.clone();
                let completed = completed.clone();
                let job = job.clone();
                let probe = probe.clone();
                let timeout_seconds = params.timeout_seconds;
                handles.push(crate::context::spawn(async move {
                    let _permit = match semaphore.acquire_owned().await {
                        Ok(permit) => permit,
                        Err(error) => {
                            warn!("[model-test] 信号量获取失败：{error}");
                            return None;
                        }
                    };
                    let result = if job_token.is_cancelled() {
                        // 取消后跳过的任务也计入进度，避免进度条永远停在中间
                        let done = completed.fetch_add(1, Ordering::Relaxed) as u32 + 1;
                        job_ctx.event_bus.emit(
                            "model-test-progress",
                            RunProgress {
                                run_id,
                                phase: "running".to_string(),
                                completed: done,
                                total,
                                result: None,
                            },
                        );
                        return None;
                    } else {
                        execute_one(&gateway, &job, &probe, sample_index, timeout_seconds).await
                    };
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
    }
    debug_assert_eq!(task_count, total as usize);
    let _ = futures_util::future::join_all(handles).await;

    // 收集结果重新从库里读，保证与落库内容一致
    let results = store::get_run_results(&ctx.database, run_id).unwrap_or_default();
    let cancelled = token.is_cancelled();
    let status = if cancelled { "cancelled" } else { "finished" };
    let summary = build_summary(&results);
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

/// 取消当前正在运行的检测。
pub fn cancel_model_test(ctx: &Arc<AppContext>) -> Result<(), String> {
    let runtime = &ctx.model_probe;
    if !runtime.running.load(Ordering::SeqCst) {
        return Err("当前没有正在运行的模型验真".to_string());
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
        None => Err("当前没有正在运行的模型验真".to_string()),
    }
}
