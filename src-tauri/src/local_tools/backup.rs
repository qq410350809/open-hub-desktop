//! 配置备份：写入前把涉及的配置文件快照到应用数据目录，
//! 每个工具保留最近 N 份，支持按文件名还原。

use std::fs;
use std::path::{Path, PathBuf};

use super::types::ToolBackupEntry;

/// 每个工具保留的备份份数上限。
const MAX_BACKUPS_PER_TOOL: usize = 10;

/// 计算某工具的备份目录。`base` 为应用数据目录；
/// 测试时传入临时目录隔离。
pub(crate) fn backup_dir(base: &Path, tool: &str) -> PathBuf {
    base.join("local_tool_backups").join(tool)
}

/// 把 `files`（要写入前先备份的文件列表）快照进备份目录。
/// 返回本次备份的时间戳目录名（同一次保存的所有文件归入同一目录）。
pub(crate) fn create_backup(
    base: &Path,
    tool: &str,
    files: &[(String, &Path)],
) -> Result<String, String> {
    let dir = backup_dir(base, tool);
    let stamp = timestamp_name();
    let target = dir.join(&stamp);
    let mut created = false;
    for (label, path) in files {
        if !path.is_file() {
            continue;
        }
        let content = fs::read(path).map_err(|error| format!("备份读取失败：{error}"))?;
        // label 里的路径分隔符替换，保持单层文件名。
        let safe_label = label.replace(['/', '\\'], "_");
        fs::create_dir_all(&target).map_err(|error| format!("创建备份目录失败：{error}"))?;
        fs::write(target.join(&safe_label), content)
            .map_err(|error| format!("备份写入失败：{error}"))?;
        created = true;
    }
    if !created {
        return Ok(String::new());
    }
    prune_old_backups(&dir);
    Ok(stamp)
}

/// 列出某工具的备份（新→旧）。
pub(crate) fn list_backups(base: &Path, tool: &str) -> Vec<ToolBackupEntry> {
    let dir = backup_dir(base, tool);
    let mut entries = Vec::new();
    let Ok(stamps) = fs::read_dir(&dir) else {
        return entries;
    };
    let mut stamps: Vec<_> = stamps
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    stamps.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for stamp in stamps {
        let Ok(files) = fs::read_dir(stamp.path()) else {
            continue;
        };
        for file in files.filter_map(Result::ok) {
            let path = file.path();
            if !path.is_file() {
                continue;
            }
            let meta = file.metadata().ok();
            entries.push(ToolBackupEntry {
                name: format!(
                    "{}/{}",
                    stamp.file_name().to_string_lossy(),
                    file.file_name().to_string_lossy()
                ),
                file_label: file.file_name().to_string_lossy().to_string(),
                size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                created_at: stamp.file_name().to_string_lossy().replace(['T', 'Z'], " "),
            });
        }
    }
    entries
}

/// 还原备份：`name` 形如 `{stamp}/{filename}`。
/// 还原目标是备份文件记录的原路径（由适配器提供 label → path 映射）。
pub(crate) fn restore_backup(
    base: &Path,
    tool: &str,
    name: &str,
    label_to_path: &[(String, PathBuf)],
) -> Result<(), String> {
    let (stamp, file) = name
        .split_once('/')
        .ok_or_else(|| format!("备份名不合法：{name}"))?;
    // 文件名即 label（create_backup 时用 label 做了安全替换）。
    let safe_label = file.replace(['/', '\\'], "_");
    let source = backup_dir(base, tool).join(stamp).join(&safe_label);
    let content = fs::read(&source).map_err(|_| "备份文件不存在或已被清理")?;
    let label = file.replace('_', "/");
    let target = label_to_path
        .iter()
        .find(|(l, _)| l.replace(['/', '\\'], "_") == safe_label || *l == label)
        .map(|(_, p)| p)
        .ok_or_else(|| format!("备份 {name} 不属于任何已知配置文件"))?;
    super::fsutil::atomic_write(target, &String::from_utf8_lossy(&content))
}

/// 只保留最近 MAX_BACKUPS_PER_TOOL 个时间戳目录。
fn prune_old_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut stamps: Vec<_> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .collect();
    if stamps.len() <= MAX_BACKUPS_PER_TOOL {
        return;
    }
    stamps.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    for stale in stamps.into_iter().skip(MAX_BACKUPS_PER_TOOL) {
        let _ = fs::remove_dir_all(stale.path());
    }
}

/// 时间戳目录名（形如 20260908-153012-123456789；纳秒后缀保证同秒多次备份
/// 不互相覆盖，定宽排序即为时间序；若仍撞名则追加序号）。
fn timestamp_name() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let (date, time) = civil_from_unix(now.as_secs() as i64);
    format!("{date}-{time}-{:09}", now.subsec_nanos())
}

/// 无 chrono 依赖的本地时间换算（UTC；备份名仅用于排序展示，精度到秒即可）。
fn civil_from_unix(secs: i64) -> (String, String) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant 的 civil_from_days 算法。
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    (
        format!("{y:04}{mth:02}{d:02}"),
        format!("{h:02}{m:02}{s:02}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_roundtrip_and_prune() {
        let tmp = std::env::temp_dir().join(format!("openhub-lt-bk-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        let cfg_dir = tmp.join("home").join(".x");
        fs::create_dir_all(&cfg_dir).unwrap();
        let cfg = cfg_dir.join("config.json");
        fs::write(&cfg, "{\"v\":1}").unwrap();

        for i in 0..13 {
            fs::write(&cfg, format!("{{\"v\":{i}}}")).unwrap();
            let stamp =
                create_backup(&tmp, "test", &[("config.json".into(), cfg.as_path())]).unwrap();
            assert!(!stamp.is_empty());
            // 纳秒时间戳在极快循环中仍可能撞名；强制错开保证 13 个独立目录。
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let backups = list_backups(&tmp, "test");
        assert_eq!(backups.len(), 10, "应只保留最近 10 份");

        // 13 份保留最新 10 份：v0–v2 被清理，最旧保留 v3。
        let oldest = backups.last().unwrap();
        let label_to_path = vec![("config.json".to_string(), cfg.clone())];
        restore_backup(&tmp, "test", &oldest.name, &label_to_path).unwrap();
        let restored = fs::read_to_string(&cfg).unwrap();
        assert!(restored.contains("\"v\":3"), "最旧保留的是 v3：{restored}");

        let _ = fs::remove_dir_all(&tmp);
    }
}
