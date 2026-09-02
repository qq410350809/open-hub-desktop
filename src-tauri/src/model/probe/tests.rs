use super::*;
use crate::model::gateway::types::ChannelConfig;

fn check(kind: &str, value: &str) -> CheckSpec {
    CheckSpec {
        kind: kind.to_string(),
        value: value.to_string(),
        tolerance: 0.0,
    }
}

fn target(channel_id: &str, model: &str) -> ProbeTarget {
    ProbeTarget {
        channel_id: channel_id.to_string(),
        model: model.to_string(),
    }
}

fn channel_json(id: &str, alias: &str, enabled: bool) -> ChannelConfig {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": id,
        "enabled": enabled,
        "upstreamUrl": "https://example.com",
        "alias": alias,
    }))
    .expect("channel json should deserialize")
}

#[test]
fn contains_check_is_case_insensitive_and_multi_keyword() {
    let outcome = runner::run_auto_check(&check("contains", "111221, 看行"), "下一行是 111221。");
    assert!(outcome.passed);

    let missed = runner::run_auto_check(&check("contains", "abc"), "完全无关的回答");
    assert!(!missed.passed);
    assert!(missed.detail.contains("未命中"));
}

#[test]
fn not_contains_check_passes_when_clean() {
    let outcome = runner::run_auto_check(&check("not_contains", "抱歉,无法"), "正常的回答内容");
    assert!(outcome.passed);

    let leaked = runner::run_auto_check(&check("not_contains", "抱歉，无法"), "抱歉，无法回答");
    assert!(!leaked.passed);
}

#[test]
fn number_check_matches_with_tolerance() {
    let outcome = runner::run_auto_check(&check("number", "4898"), "计算结果为 4,898 元。");
    assert!(outcome.passed);

    let mut tolerant = check("number", "3.14");
    tolerant.tolerance = 0.01;
    assert!(runner::run_auto_check(&tolerant, "约等于 3.14159").passed);
    assert!(!runner::run_auto_check(&tolerant, "约等于 3.2").passed);

    let absent = runner::run_auto_check(&check("number", "42"), "我不知道。");
    assert!(!absent.passed);
}

#[test]
fn json_check_tolerates_code_fence() {
    let outcome = runner::run_auto_check(&check("json", ""), "```json\n{\"a\": 1}\n```");
    assert!(outcome.passed);

    let with_value = runner::run_auto_check(&check("json", "GLM"), "{\"model\": \"GLM-5\"}");
    assert!(with_value.passed);

    let invalid = runner::run_auto_check(&check("json", ""), "这不是 JSON");
    assert!(!invalid.passed);
}

#[test]
fn unknown_check_kind_fails() {
    let outcome = runner::run_auto_check(&check("regex", ".*"), "任意");
    assert!(!outcome.passed);
}

#[test]
fn probe_model_requires_enabled_channel() {
    let channels = vec![channel_json("c1", "X666", true), channel_json("c2", "", false)];
    assert_eq!(
        runner::resolve_probe_model(&channels, &target("c1", "glm-5")).unwrap(),
        "x666/glm-5"
    );
    assert!(runner::resolve_probe_model(&channels, &target("missing", "m")).is_err());
    assert!(runner::resolve_probe_model(&channels, &target("c2", "m")).is_err());
    assert!(runner::resolve_probe_model(&channels, &target("c1", " ")).is_err());
}

#[test]
fn extract_numbers_handles_thousands_and_decimals() {
    assert_eq!(
        runner::extract_numbers("总价 1,234.5 元，占比 12%"),
        vec![1234.5, 12.0]
    );
    assert!(runner::extract_numbers("没有数字").is_empty());
}

#[test]
fn build_summary_aggregates_by_model_and_prompt() {
    let params = RunParams {
        targets: vec![target("c1", "m1"), target("c1", "m2")],
        prompts: vec![super::types::ProbePrompt {
            id: "p1".into(),
            name: "题一".into(),
            category: "推理".into(),
            text: "t".into(),
            max_tokens: 64,
            temperature: 0.0,
            check: None,
            judge: false,
        }],
        concurrency: 2,
        timeout_seconds: 60,
        judge: None,
    };
    let mk = |model: &str, prompt: &str, ok: bool, score: Option<f64>, ms: u64| ProbeResult {
        channel_id: "c1".into(),
        channel_name: "渠道一".into(),
        model: model.into(),
        prompt_id: prompt.into(),
        prompt_name: "题一".into(),
        category: "推理".into(),
        ok,
        duration_ms: Some(ms),
        score,
        ..Default::default()
    };
    let results = vec![
        mk("m1", "p1", true, Some(10.0), 1000),
        mk("m1", "p1", true, Some(6.0), 3000),
        mk("m2", "p1", false, Some(0.0), 500),
    ];
    let summary = runner::build_summary(&params, &results);
    assert_eq!(summary.models.len(), 2);
    let m1 = summary
        .models
        .iter()
        .find(|item| item.model == "m1")
        .unwrap();
    assert_eq!(m1.ok_count, 2);
    assert_eq!(m1.avg_score, Some(8.0));
    assert_eq!(m1.avg_duration_ms, Some(2000));
    let m2 = summary
        .models
        .iter()
        .find(|item| item.model == "m2")
        .unwrap();
    assert_eq!(m2.ok_count, 0);
    assert_eq!(summary.prompts.len(), 1);
    assert_eq!(summary.prompts[0].total, 3);
    assert_eq!(summary.prompts[0].ok_count, 2);
}
