use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{copy, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

const MIHOMO_REPO_API: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";
const GITHUB_PROXY_PREFIXES: &[&str] = &[
    "", // Direct
    "https://ghfast.top/",
    "https://mirror.ghproxy.com/",
    "https://ghproxy.net/",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MihomoKernelStatus {
    pub installed: bool,
    pub path: String,
    pub version: String,
    pub is_custom: bool,
    pub latest_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MihomoDownloadProgress {
    pub stage: String,
    pub progress: f64,
    pub message: String,
}

pub fn get_app_bin_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let bin_dir = app_data.join("bin");
    let _ = fs::create_dir_all(&bin_dir);
    Ok(bin_dir)
}

pub fn resolve_mihomo_binary(app: Option<&AppHandle>) -> Option<PathBuf> {
    // 1. 用户自定义环境变量
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    // 2. OpenHub 专属 AppData bin 目录
    if let Some(app) = app {
        if let Ok(bin_dir) = get_app_bin_dir(app) {
            let binary_name = if cfg!(target_os = "windows") { "mihomo.exe" } else { "mihomo" };
            let candidate = bin_dir.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

        // 3. 安装包自带 Resource 目录
        if let Ok(resource_dir) = app.path().resource_dir() {
            let binary_name = if cfg!(target_os = "windows") { "mihomo.exe" } else { "mihomo" };
            let candidate = resource_dir.join("bin").join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

pub fn read_mihomo_version(binary_path: &Path) -> Option<String> {
    let output = Command::new(binary_path).arg("-v").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("Mihomo") {
            return Some(line.trim().to_string());
        }
    }
    if !text.trim().is_empty() {
        return Some(text.trim().to_string());
    }
    let err_text = String::from_utf8_lossy(&output.stderr);
    if !err_text.trim().is_empty() {
        return Some(err_text.trim().to_string());
    }
    None
}

fn target_asset_keywords() -> Result<(&'static str, &'static str), String> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok(("darwin-arm64", "mihomo"));
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Ok(("darwin-amd64", "mihomo"));
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok(("windows-amd64", "mihomo.exe"));
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok(("linux-amd64", "mihomo"));
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok(("linux-arm64", "mihomo"));

    #[allow(unreachable_code)]
    Err("当前操作系统或架构暂不支持自动下载 Mihomo 内核".to_string())
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

pub async fn query_latest_release() -> Result<(String, String), String> {
    let (arch_keyword, _) = target_asset_keywords()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("OpenHub-Desktop/0.3.0")
        .build()
        .map_err(|e| e.to_string())?;

    let mut last_error = String::new();
    for proxy_prefix in GITHUB_PROXY_PREFIXES {
        let url = format!("{proxy_prefix}{MIHOMO_REPO_API}");
        match client.get(&url).send().await {
            Ok(res) if res.status().is_success() => {
                if let Ok(release) = res.json::<GitHubRelease>().await {
                    // 匹配符合当前架构的压缩包，排除带有兼容包或其它附带文件的特殊包
                    // 例如: mihomo-darwin-arm64-v1.19.1.gz
                    let matched_asset = release.assets.iter().find(|a| {
                        let name = a.name.to_lowercase();
                        name.contains(arch_keyword)
                            && !name.contains("compatible")
                            && !name.contains("go120")
                            && (name.ends_with(".gz") || name.ends_with(".zip"))
                    }).or_else(|| {
                        // 降级匹配任意包含 arch_keyword 的压缩包
                        release.assets.iter().find(|a| {
                            let name = a.name.to_lowercase();
                            name.contains(arch_keyword) && (name.ends_with(".gz") || name.ends_with(".zip"))
                        })
                    });

                    if let Some(asset) = matched_asset {
                        return Ok((release.tag_name, asset.browser_download_url.clone()));
                    }
                }
            }
            Ok(res) => last_error = format!("HTTP {}", res.status()),
            Err(e) => last_error = e.to_string(),
        }
    }

    Err(format!("获取最新 Mihomo 内核发布版本失败：{last_error}"))
}

#[tauri::command]
pub async fn get_mihomo_kernel_status(app: AppHandle) -> Result<MihomoKernelStatus, String> {
    let resolved = resolve_mihomo_binary(Some(&app));
    let (installed, path, version, is_custom) = if let Some(p) = resolved {
        let ver = read_mihomo_version(&p).unwrap_or_else(|| "未知版本".to_string());
        let custom = std::env::var("OPENHUB_MIHOMO_PATH").is_ok();
        (true, p.display().to_string(), ver, custom)
    } else {
        (false, String::new(), String::new(), false)
    };

    Ok(MihomoKernelStatus {
        installed,
        path,
        version,
        is_custom,
        latest_version: None,
    })
}

#[tauri::command]
pub async fn check_mihomo_kernel_update(app: AppHandle) -> Result<MihomoKernelStatus, String> {
    let mut status = get_mihomo_kernel_status(app).await?;
    match query_latest_release().await {
        Ok((tag, _)) => {
            status.latest_version = Some(tag);
            Ok(status)
        }
        Err(e) => Err(format!("检查更新失败：{e}")),
    }
}

#[tauri::command]
pub async fn download_or_update_mihomo_kernel(app: AppHandle) -> Result<MihomoKernelStatus, String> {
    let emit_progress = |stage: &str, progress: f64, message: &str| {
        let _ = app.emit(
            "mihomo-kernel-progress",
            MihomoDownloadProgress {
                stage: stage.to_string(),
                progress,
                message: message.to_string(),
            },
        );
    };

    emit_progress("checking", 0.05, "正在获取最新内核版本信息…");
    let (tag, raw_download_url) = query_latest_release().await?;

    let bin_dir = get_app_bin_dir(&app)?;
    let (_, binary_filename) = target_asset_keywords()?;
    let target_binary_path = bin_dir.join(binary_filename);
    let temp_download_path = bin_dir.join(format!("{binary_filename}.download.tmp"));
    let temp_extracted_path = bin_dir.join(format!("{binary_filename}.extract.tmp"));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("OpenHub-Desktop/0.3.0")
        .build()
        .map_err(|e| e.to_string())?;

    emit_progress("downloading", 0.1, &format!("开始下载 Mihomo {tag}…"));

    let mut download_res = None;
    let mut last_err = String::new();

    for proxy_prefix in GITHUB_PROXY_PREFIXES {
        let full_url = if proxy_prefix.is_empty() {
            raw_download_url.clone()
        } else {
            format!("{proxy_prefix}{raw_download_url}")
        };

        match client.get(&full_url).send().await {
            Ok(res) if res.status().is_success() => {
                download_res = Some(res);
                break;
            }
            Ok(res) => last_err = format!("HTTP {}", res.status()),
            Err(e) => last_err = e.to_string(),
        }
    }

    let mut res = download_res.ok_or_else(|| format!("下载内核文件失败：{last_err}"))?;
    let total_size = res.content_length().unwrap_or(0);

    let mut file = File::create(&temp_download_path).map_err(|e| format!("创建临时下载文件失败：{e}"))?;
    let mut downloaded: u64 = 0;

    use futures_util::StreamExt;
    let mut stream = res.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("下载中断：{e}"))?;
        use std::io::Write;
        file.write_all(&chunk).map_err(|e| format!("写入文件失败：{e}"))?;
        downloaded += chunk.len() as u64;

        if total_size > 0 {
            let pct = 0.1 + (downloaded as f64 / total_size as f64) * 0.7;
            emit_progress("downloading", pct, &format!("正在下载：{:.1}MB / {:.1}MB", downloaded as f64 / 1_048_576.0, total_size as f64 / 1_048_576.0));
        }
    }
    drop(file);

    emit_progress("extracting", 0.85, "下载完成，正在解压内核…");

    // 解压 .gz 文件
    let is_gz = raw_download_url.ends_with(".gz");
    if is_gz {
        let gz_file = File::open(&temp_download_path).map_err(|e| format!("打开压缩文件失败：{e}"))?;
        let mut decoder = GzDecoder::new(gz_file);
        let mut out = File::create(&temp_extracted_path).map_err(|e| format!("创建解压文件失败：{e}"))?;
        copy(&mut decoder, &mut out).map_err(|e| format!("解压 GZ 内核失败：{e}"))?;
    } else {
        // 如果是原始可执行文件或已解压文件
        fs::copy(&temp_download_path, &temp_extracted_path).map_err(|e| format!("复制文件失败：{e}"))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_extracted_path).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_extracted_path, perms).map_err(|e| format!("赋予执行权限失败：{e}"))?;
    }

    emit_progress("verifying", 0.95, "正在校验内核有效性…");

    let verified_ver = read_mihomo_version(&temp_extracted_path).ok_or("下载的文件无法作为 Mihomo 内核正常执行")?;

    // 替换目标内核文件
    let _ = fs::remove_file(&target_binary_path);
    fs::rename(&temp_extracted_path, &target_binary_path).map_err(|e| format!("替换目标内核失败：{e}"))?;
    let _ = fs::remove_file(&temp_download_path);

    emit_progress("completed", 1.0, &format!("Mihomo 内核已成功安装 ({verified_ver})"));

    Ok(MihomoKernelStatus {
        installed: true,
        path: target_binary_path.display().to_string(),
        version: verified_ver,
        is_custom: false,
        latest_version: Some(tag),
    })
}
