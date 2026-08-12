//! 单实例守卫：启动时自动关闭旧实例。
//!
//! 背景：旧实例常驻会占用内核端口（17896+），新实例只能顺延到别的端口，
//! 导致浏览器/书签还指向旧实例（旧前端、旧内核），表现为"改了不生效 / 黑屏"。
//!
//! 机制：在应用数据目录写一个 pid 锁文件。启动时若锁里的进程仍然存活且确实是
//! OpenHub，先杀掉旧实例再继续，保证新启动的一定是最新构建、独占端口。
//! 旧进程崩溃留下的死锁会在下次启动被 `ps` 校验识破并覆盖，无需额外清理。

use std::fs;
use std::path::Path;
use std::process::Command;

const LOCK_FILENAME: &str = "openhub.pid";

/// 在打开数据库 / 绑定内核端口之前调用。
pub fn claim(app_data_dir: &Path) {
    let lock_path = app_data_dir.join(LOCK_FILENAME);
    if let Ok(content) = fs::read_to_string(&lock_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            if is_openhub_process(pid) {
                eprintln!("OpenHub 检测到旧实例（pid {pid}），正在自动关闭…");
                kill_process(pid);
            }
        }
    }
    // 无论旧锁是否有效，都用自身 pid 覆盖；死锁里的进程已不存在或不是本应用。
    let _ = fs::write(&lock_path, std::process::id().to_string());
}

fn is_openhub_process(pid: u32) -> bool {
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
                text.contains("openhub") || text.contains("open-hub")
            }
            _ => false,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let out = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .output();
        match out {
            Ok(output) if output.status.success() => {
                let name = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
                name.contains("open-hub") || name.contains("openhub")
            }
            _ => false,
        }
    }
}

fn kill_process(pid: u32) {
    #[cfg(target_os = "windows")]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .status();
    #[cfg(not(target_os = "windows"))]
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
}
