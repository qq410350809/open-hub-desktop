//! 运行形态（dev / 正式版）隔离。
//!
//! `tauri dev` 是 debug 构建、`tauri build` 是 release 构建，以
//! `debug_assertions` 作为天然判据；`OPENHUB_PROFILE=dev|release`
//! 可显式覆盖，便于 debug 构建冒烟正式形态。
//!
//! 隔离三件套：
//! 1. 端口：dev 固定用 17996，正式版仍为 17896，互不抢占；
//! 2. 数据目录：dev 在原目录名后追加 `-dev`，数据库/配置/缓存全新独立；
//! 3. 单实例锁：锁文件随数据目录走，dev 不再误杀正在运行的正式版。

/// dev 形态的 Web UI / 模型网关端口（正式版 17896，见 web_server::DEFAULT_PORT）。
pub const DEV_SERVICE_PORT: u16 = 17996;

/// 正式版应用数据目录名（与 tauri.conf.json 的 identifier 一致）。
pub const RELEASE_DIR_NAME: &str = "com.dfeer.openhub.desktop";

/// 当前是否运行在 dev 隔离形态。
pub fn is_dev_profile() -> bool {
    resolve_profile(
        std::env::var("OPENHUB_PROFILE").ok().as_deref(),
        cfg!(debug_assertions),
    )
}

/// 纯函数形式的形态判定，便于在并行测试中无环境变量竞争地验证覆盖规则。
fn resolve_profile(env_value: Option<&str>, debug_assertions: bool) -> bool {
    match env_value {
        Some("dev") => true,
        Some("release") => false,
        _ => debug_assertions,
    }
}

/// 内嵌 Web 服务的期望端口：dev 走独立端口，被占用仍由 bind_listener 顺延。
pub fn preferred_service_port() -> u16 {
    if is_dev_profile() {
        DEV_SERVICE_PORT
    } else {
        crate::core::web_server::DEFAULT_PORT
    }
}

/// 应用数据目录名：dev 追加 `-dev` 后缀，实现与正式版的数据隔离。
pub fn app_support_dir_name() -> &'static str {
    if is_dev_profile() {
        "com.dfeer.openhub.desktop-dev"
    } else {
        RELEASE_DIR_NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_profile_uses_isolated_port_and_dir() {
        // 未设置环境变量时跟随构建 profile：release 构建恒为正式形态，
        // debug 构建为 dev 形态。分别断言两条分支的不变式。
        if cfg!(debug_assertions) {
            assert!(is_dev_profile());
            assert_eq!(preferred_service_port(), DEV_SERVICE_PORT);
            assert_eq!(app_support_dir_name(), "com.dfeer.openhub.desktop-dev");
        } else {
            assert!(!is_dev_profile());
            assert_eq!(
                preferred_service_port(),
                crate::core::web_server::DEFAULT_PORT
            );
            assert_eq!(app_support_dir_name(), RELEASE_DIR_NAME);
        }
        // 目录后缀必须以正式名开头，保证 Windows 注册表卸载清理等按前缀匹配的场景。
        assert!(app_support_dir_name().starts_with(RELEASE_DIR_NAME));
    }

    #[test]
    fn env_override_beats_build_profile() {
        // 环境变量覆盖优先于构建 profile；用纯函数验证，避免并行测试中的环境变量竞争。
        assert!(resolve_profile(Some("dev"), false));
        assert!(resolve_profile(Some("dev"), true));
        assert!(!resolve_profile(Some("release"), true));
        assert!(!resolve_profile(Some("release"), false));
        // 非法值忽略，回退构建 profile。
        assert_eq!(resolve_profile(Some("other"), true), true);
        assert_eq!(resolve_profile(Some("other"), false), false);
        assert_eq!(resolve_profile(None, true), true);
        assert_eq!(resolve_profile(None, false), false);
    }
}
