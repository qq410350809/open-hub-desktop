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
    let outcome = runner::run_auto_check(&check("contains", "312211, 看行"), "下一行是 312211。");
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
fn exact_check_ignores_whitespace_and_case() {
    let outcome = runner::run_auto_check(&check("exact", "甲乙丙"), "甲\n乙\n丙\n");
    assert!(outcome.passed);

    let extra = runner::run_auto_check(&check("exact", "甲乙丙"), "第一行：甲\n第二行：乙\n第三行：丙");
    assert!(!extra.passed);
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
fn family_of_model_infers_claimed_family() {
    assert_eq!(
        fingerprints::family_of_model("gpt-4o-2024-11-20").as_deref(),
        Some("gpt")
    );
    assert_eq!(
        fingerprints::family_of_model("claude-sonnet-4").as_deref(),
        Some("claude")
    );
    assert_eq!(
        fingerprints::family_of_model("deepseek-r1").as_deref(),
        Some("deepseek")
    );
    assert_eq!(
        fingerprints::family_of_model("Qwen3-235B").as_deref(),
        Some("qwen")
    );
    assert_eq!(fingerprints::family_of_model("some-random-model"), None);
}

#[test]
fn identity_family_detection_prefers_majority_keywords() {
    // 声称是 GPT 的渠道实际部署了 Qwen：自述里 qwen 关键词占优
    let text = "我是通义千问（Qwen），由阿里巴巴集团通义实验室开发。";
    assert_eq!(
        fingerprints::detect_identity_family(text).as_deref(),
        Some("qwen")
    );
    // 明确自报 Claude
    assert_eq!(
        fingerprints::detect_identity_family("I am Claude, made by Anthropic.").as_deref(),
        Some("claude")
    );
    // 无家族线索
    assert_eq!(fingerprints::detect_identity_family("我是一个人工智能助手。"), None);
}

#[test]
fn fingerprint_match_uses_family_patterns() {
    let probe = fingerprints::builtin_probes()
        .into_iter()
        .find(|probe| probe.id == "fp-developer")
        .expect("fp-developer exists");
    assert_eq!(
        fingerprints::match_family("我的开发者是 Anthropic。", &probe.expected).as_deref(),
        Some("claude")
    );
    assert_eq!(
        fingerprints::match_family("由深度求索（DeepSeek）公司创造。", &probe.expected).as_deref(),
        Some("deepseek")
    );
    assert_eq!(fingerprints::match_family("我不知道。", &probe.expected), None);
}

#[test]
fn builtin_probes_are_wellformed() {
    let probes = fingerprints::builtin_probes();
    assert!(!probes.is_empty());
    let mut ids = std::collections::HashSet::new();
    for probe in &probes {
        assert!(ids.insert(probe.id.clone()), "重复探测题 id：{}", probe.id);
        assert!(!probe.text.trim().is_empty());
        // 去同质化：每题至少 3 个同义变体，且默认文本在变体中
        assert!(
            probe.variants.len() >= 3,
            "探测题 {} 变体不足：{}",
            probe.id,
            probe.variants.len()
        );
        assert!(
            probe.variants.iter().any(|v| v == &probe.text),
            "探测题 {} 的默认文本不在变体中",
            probe.id
        );
        match probe.category.as_str() {
            "identity" | "capability" => {}
            "fingerprint" => assert!(
                !probe.expected.is_empty(),
                "指纹题 {} 缺少期望答案",
                probe.id
            ),
            other => panic!("未知探测类别：{other}"),
        }
    }
    // 一致性采样题至少有一道
    assert!(probes.iter().any(|probe| probe.repeats));
}

#[test]
fn compose_messages_wraps_probe_in_chat_history() {
    let probe = fingerprints::builtin_probes()
        .into_iter()
        .find(|probe| probe.id == "cap-multiply")
        .expect("cap-multiply exists");
    let mut rng = fingerprints::Rng::new(42);
    let (messages, asked) = fingerprints::compose_messages(&probe, &mut rng);
    // 至少一轮闲聊 + 最终提问；角色以 user 开始、user 结束、交替出现
    assert!(messages.len() >= 3 && messages.len() <= 5);
    assert_eq!(messages.first().unwrap()["role"], "user");
    assert_eq!(messages.last().unwrap()["role"], "user");
    for window in messages.windows(2) {
        assert_ne!(window[0]["role"], window[1]["role"]);
    }
    // 最终提问必须是某个变体（可带过渡前缀），判分答案不变
    assert!(
        probe.variants.iter().any(|v| asked.ends_with(v.as_str())),
        "最终提问不在变体集合中：{asked}"
    );
}

#[test]
fn compose_messages_is_randomized_across_calls() {
    let probe = fingerprints::builtin_probes()
        .into_iter()
        .find(|probe| probe.id == "id-direct")
        .expect("id-direct exists");
    // 不同种子应产生不同的对话包装（闲聊前缀或变体不同）
    let mut seen = std::collections::HashSet::new();
    for seed in 1..=12 {
        let mut rng = fingerprints::Rng::new(seed);
        let (messages, _) = fingerprints::compose_messages(&probe, &mut rng);
        seen.insert(messages);
    }
    assert!(seen.len() >= 6, "对话包装随机性不足：{} 种", seen.len());
}

fn result(probe_id: &str, category: &str, model: &str, ok: bool) -> ProbeResult {
    ProbeResult {
        channel_id: "c1".into(),
        channel_name: "渠道一".into(),
        model: model.into(),
        probe_id: probe_id.into(),
        probe_name: probe_id.into(),
        category: category.into(),
        ok,
        ..Default::default()
    }
}

#[test]
fn verdict_flags_impersonation_on_identity_mismatch() {
    let mut claimed_qwen = result("id-direct", "identity", "qwen-plus", true);
    claimed_qwen.family_match = Some("glm".into());
    let verdicts = runner::build_verdicts(&[claimed_qwen]);
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].verdict, "impersonation");
    assert_eq!(verdicts[0].identity_consistent, Some(false));
    assert!(!verdicts[0].issues.is_empty());
}

#[test]
fn verdict_flags_low_capability_pass_rate() {
    let results = vec![
        result("cap-sequence", "capability", "gpt-4o", true),
        result("cap-multiply", "capability", "gpt-4o", false),
        result("cap-snail", "capability", "gpt-4o", false),
        result("id-direct", "identity", "gpt-4o", true),
    ];
    let verdicts = runner::build_verdicts(&results);
    assert_eq!(verdicts[0].verdict, "suspicious");
    assert_eq!(verdicts[0].capability_passed, 1);
    assert_eq!(verdicts[0].capability_total, 3);
}

#[test]
fn verdict_ok_when_consistent_and_capable() {
    let base = result("cap-sequence", "capability", "gpt-4o", true);
    let mut second = base.clone();
    second.sample_index = 1;
    let mut third = base.clone();
    third.sample_index = 2;
    let mut identity = result("id-direct", "identity", "gpt-4o", true);
    identity.family_match = Some("gpt".into());
    let verdicts = runner::build_verdicts(&[base, second, third, identity]);
    assert_eq!(verdicts[0].verdict, "ok");
    assert_eq!(verdicts[0].consistency_rate, Some(1.0));
    assert_eq!(verdicts[0].identity_consistent, Some(true));
}

#[test]
fn verdict_unreachable_when_all_fail() {
    let results = vec![
        result("cap-math", "capability", "gpt-4o", false),
        result("id-direct", "identity", "gpt-4o", false),
    ];
    let verdicts = runner::build_verdicts(&results);
    assert_eq!(verdicts[0].verdict, "unreachable");
}

#[test]
fn consistency_detects_verdict_variance() {
    // 同一采样题两次结果判分结论不一致（问法随机变体，比对结论而非文本）
    let a = result("cap-sequence", "capability", "gpt-4o", true);
    let mut b = a.clone();
    b.sample_index = 1;
    b.ok = false;
    let mut identity = result("id-direct", "identity", "gpt-4o", true);
    identity.family_match = Some("gpt".into());
    let verdicts = runner::build_verdicts(&[a, b, identity]);
    assert_eq!(verdicts[0].consistency_rate, Some(0.0));
    assert_eq!(verdicts[0].verdict, "suspicious");
}

#[test]
fn verdicts_group_multiple_targets() {
    let results = vec![
        result("cap-math", "capability", "gpt-4o", true),
        result("cap-math", "capability", "claude-sonnet-4", true),
        result("cap-json", "capability", "gpt-4o", true),
    ];
    let verdicts = runner::build_verdicts(&results);
    assert_eq!(verdicts.len(), 2);
    assert_eq!(verdicts[0].model, "gpt-4o");
    assert_eq!(verdicts[0].total_requests, 2);
    assert_eq!(verdicts[1].model, "claude-sonnet-4");
    assert_eq!(verdicts[1].total_requests, 1);
}
