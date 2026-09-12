//! 文件原子写工具：temp + rename，保留 0600 权限，避免写一半被工具读到。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

/// 原子覆盖写入：先写同目录临时文件，再 rename 到目标。
pub(crate) fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("无法定位父目录：{}", path.display()))?;
    fs::create_dir_all(parent).map_err(|error| format!("创建目录失败：{error}"))?;

    // 保留既有权限（工具配置多为 0600）；新文件默认 0600。
    let mode = existing_mode(path).unwrap_or(0o600);

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".openhub-tmp");
    let tmp = PathBuf::from(tmp);

    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|error| format!("写入临时文件失败：{error}"))?;
        file.write_all(content.as_bytes())
            .map_err(|error| format!("写入临时文件失败：{error}"))?;
        // 确保落盘后再 rename。
        file.sync_all()
            .map_err(|error| format!("刷盘失败：{error}"))?;
    }
    apply_mode(&tmp, mode);
    fs::rename(&tmp, path).map_err(|error| format!("替换配置文件失败：{error}"))?;
    Ok(())
}

/// 受管配置写入：先把文本里的指纹占位盖成真实指纹，再原子落盘。
/// 适配器写自己管辖的配置文件走这里；备份还原等原样回写走 `atomic_write`。
pub(crate) fn atomic_write_stamped(path: &Path, content: &str) -> Result<(), String> {
    atomic_write(path, &super::mark::stamp(content))
}

/// 读取文本文件（UTF-8），不存在时返回 None。
pub(crate) fn read_text(path: &Path) -> Result<Option<String>, String> {
    match File::open(path) {
        Ok(mut file) => {
            let mut buf = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut buf)
                .map_err(|error| format!("读取失败（{}）：{error}", path.display()))?;
            String::from_utf8(buf).map(Some).map_err(|_| {
                format!(
                    "{} 不是有效的 UTF-8 文本，已跳过编辑以避免损坏",
                    path.display()
                )
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("读取失败（{}）：{error}", path.display())),
    }
}

#[cfg(unix)]
fn existing_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: u32) {}

use std::path::PathBuf;
