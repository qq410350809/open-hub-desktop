use tracing::info;
use flate2::read::GzDecoder;
use futures_util::future::join_all;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{copy, Cursor, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::Mutex;

const MIHOMO_REPO_API: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";
const GITHUB_PROXY_PREFIXES: &[&str] = &[
    "https://gh-proxy.com/",
    "https://ghfast.top/",
    "https://gh.ddlc.top/",
    "https://ghps.cc/",
    "https://github.boki.moe/",
    "https://ghproxy.net/",
    "", // Direct
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
    if let Ok(value) = std::env::var("OPENHUB_MIHOMO_PATH") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }

    if let Some(app) = app {
        if let Ok(bin_dir) = get_app_bin_dir(app) {
            let binary_name = if cfg!(target_os = "windows") { "mihomo.exe" } else { "mihomo" };
            let candidate = bin_dir.join(binary_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }

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

/// 启动时若 AppData 目录尚无文件，先自动释放安装包自带的内置基础版内核与 GeoIP 数据库
pub fn ensure_bundled_assets_installed(app: &AppHandle) -> Result<(), String> {
    let app_data = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let bin_dir = app_data.join("bin");
    let _ = fs::create_dir_all(&bin_dir);

    let binary_name = if cfg!(target_os = "windows") { "mihomo.exe" } else { "mihomo" };
    let target_binary = bin_dir.join(binary_name);
    let target_geoip = app_data.join("Country.mmdb");

    // 1. 如果私有 bin/ 目录下没有 mihomo，尝试从安装包内置 resources 释放
    if !target_binary.is_file() {
        let mut candidates = Vec::new();
        if let Ok(res_dir) = app.path().resource_dir() {
            candidates.push(res_dir.join("bin").join(binary_name));
            candidates.push(res_dir.join("resources").join("bin").join(binary_name));
            candidates.push(res_dir.join(binary_name));
        }
        candidates.push(PathBuf::from("resources/bin").join(binary_name));
        candidates.push(PathBuf::from("src-tauri/resources/bin").join(binary_name));

        for src in candidates {
            if src.is_file() {
                if fs::copy(&src, &target_binary).is_ok() {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = fs::metadata(&target_binary) {
                            let mut perms = meta.permissions();
                            perms.set_mode(0o755);
                            let _ = fs::set_permissions(&target_binary, perms);
                        }
                    }
                    info!("[OpenHub] 已成功从安装包内置资源释放 Mihomo 内核");
                    break;
                }
            }
        }
    }

    // 2. 如果私有 AppData 目录下没有 Country.mmdb，尝试从安装包内置 resources 释放
    if !target_geoip.is_file() {
        let mut candidates = Vec::new();
        if let Ok(res_dir) = app.path().resource_dir() {
            candidates.push(res_dir.join("Country.mmdb"));
            candidates.push(res_dir.join("resources").join("Country.mmdb"));
        }
        candidates.push(PathBuf::from("resources/Country.mmdb"));
        candidates.push(PathBuf::from("src-tauri/resources/Country.mmdb"));

        for src in candidates {
            if src.is_file() {
                if fs::copy(&src, &target_geoip).is_ok() {
                    let _ = fs::copy(&target_geoip, bin_dir.join("Country.mmdb"));
                    info!("[OpenHub] 已成功从安装包内置资源释放 GeoIP 国家数据库");
                    break;
                }
            }
        }
    }

    Ok(())
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

pub async fn query_latest_release(mirror_prefix: Option<&str>) -> Result<(String, String), String> {
    let (arch_keyword, _) = target_asset_keywords()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .user_agent("OpenHub-Desktop/0.3.0")
        .build()
        .map_err(|e| e.to_string())?;

    let mut prefixes: Vec<String> = Vec::new();
    if let Some(m) = mirror_prefix {
        let trimmed = m.trim();
        if !trimmed.is_empty() && trimmed != "auto" {
            if trimmed == "direct" {
                prefixes.push("".to_string());
            } else {
                let p = if trimmed.ends_with('/') { trimmed.to_string() } else { format!("{trimmed}/") };
                prefixes.push(p);
            }
        }
    }
    for p in GITHUB_PROXY_PREFIXES {
        if !prefixes.iter().any(|existing| existing == p) {
            prefixes.push(p.to_string());
        }
    }

    let mut last_error = String::new();
    for proxy_prefix in prefixes {
        let url = format!("{proxy_prefix}{MIHOMO_REPO_API}");
        match client.get(&url).send().await {
            Ok(res) if res.status().is_success() => {
                if let Ok(release) = res.json::<GitHubRelease>().await {
                    let matched_asset = release.assets.iter().find(|a| {
                        let name = a.name.to_lowercase();
                        name.contains(arch_keyword)
                            && !name.contains("compatible")
                            && !name.contains("go120")
                            && (name.ends_with(".gz") || name.ends_with(".zip"))
                    }).or_else(|| {
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
pub async fn check_mihomo_kernel_update(
    app: AppHandle,
    mirror: Option<String>,
) -> Result<MihomoKernelStatus, String> {
    let mut status = get_mihomo_kernel_status(app).await?;
    match query_latest_release(mirror.as_deref()).await {
        Ok((tag, _)) => {
            status.latest_version = Some(tag);
            Ok(status)
        }
        Err(e) => Err(format!("检查更新失败：{e}")),
    }
}

/// 并发测速并获取最快镜像源列表
async fn rank_fastest_mirrors(raw_url: &str) -> Vec<String> {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(3000))
        .user_agent("OpenHub-Desktop/0.3.0")
        .build()
    {
        Ok(c) => c,
        Err(_) => return GITHUB_PROXY_PREFIXES.iter().map(|p| format!("{p}{raw_url}")).collect(),
    };

    let mut checks = Vec::new();
    for prefix in GITHUB_PROXY_PREFIXES {
        let full_url = format!("{prefix}{raw_url}");
        let c = client.clone();
        checks.push(async move {
            let t0 = Instant::now();
            match c.head(&full_url).send().await {
                Ok(res) if res.status().is_success() => {
                    Some((full_url, t0.elapsed()))
                }
                _ => None,
            }
        });
    }

    let results = join_all(checks).await;
    let mut successful: Vec<(String, Duration)> = results.into_iter().flatten().collect();
    successful.sort_by_key(|(_, d)| *d);

    let mut urls: Vec<String> = successful.into_iter().map(|(u, _)| u).collect();
    if urls.is_empty() {
        for prefix in GITHUB_PROXY_PREFIXES {
            urls.push(format!("{prefix}{raw_url}"));
        }
    }
    urls
}

/// 真正的高性能抢占式动态微块并发下载引擎（避免大分块尾部短板阻塞）
pub async fn download_bytes_with_dynamic_racing(
    raw_download_url: &str,
    chosen_mirror: Option<&str>,
    app: &AppHandle,
    progress_event: &str,
    item_display_name: &str,
) -> Result<Vec<u8>, String> {
    let emit_progress = |stage: &str, progress: f64, message: &str| {
        let _ = app.emit(
            progress_event,
            MihomoDownloadProgress {
                stage: stage.to_string(),
                progress,
                message: message.to_string(),
            },
        );
    };

    let chosen_mirror = chosen_mirror.unwrap_or_default().trim();
    let is_auto = chosen_mirror.is_empty() || chosen_mirror == "auto";
    let is_direct = chosen_mirror == "direct";

    let (ranked_urls, best_url) = if is_direct {
        (vec![raw_download_url.to_string()], raw_download_url.to_string())
    } else if !is_auto {
        let prefix = if chosen_mirror.ends_with('/') {
            chosen_mirror.to_string()
        } else {
            format!("{chosen_mirror}/")
        };
        let single_url = format!("{prefix}{raw_download_url}");
        (vec![single_url.clone()], single_url)
    } else {
        emit_progress("checking", 0.1, &format!("正在智能竞速测试 {item_display_name} 高速节点…"));
        let ranked = rank_fastest_mirrors(raw_download_url).await;
        let first = ranked.first().cloned().unwrap_or_else(|| raw_download_url.to_string());
        (ranked, first)
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("OpenHub-Desktop/0.3.0")
        .build()
        .map_err(|e| e.to_string())?;

    // 1. 获取文件总大小并检测是否支持 Range
    let head_res = client.head(&best_url).send().await.map_err(|e| e.to_string())?;
    let total_size = head_res.content_length().unwrap_or(0);
    let accept_ranges = head_res.headers()
        .get(reqwest::header::ACCEPT_RANGES)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.contains("bytes"))
        .unwrap_or(false);

    let downloaded_bytes = Arc::new(AtomicU64::new(0));
    let total_mb = total_size as f64 / 1_048_576.0;

    // 进度广播定时器
    let app_handle_clone = app.clone();
    let downloaded_counter = downloaded_bytes.clone();
    let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel_flag_task = cancel_flag.clone();
    let progress_event_name = progress_event.to_string();
    let display_name_clone = item_display_name.to_string();

    let progress_reporter = tokio::spawn(async move {
        let mut last_bytes = 0u64;
        let mut last_time = Instant::now();

        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if cancel_flag_task.load(Ordering::Relaxed) {
                break;
            }
            let cur_bytes = downloaded_counter.load(Ordering::Relaxed);
            let cur_time = Instant::now();
            let dt = cur_time.duration_since(last_time).as_secs_f64();
            let speed_bps = if dt > 0.0 { (cur_bytes.saturating_sub(last_bytes)) as f64 / dt } else { 0.0 };
            last_bytes = cur_bytes;
            last_time = cur_time;

            let cur_mb = cur_bytes as f64 / 1_048_576.0;
            let speed_mb = speed_bps / 1_048_576.0;
            let pct = if total_size > 0 { (cur_bytes as f64 / total_size as f64).min(0.99) } else { 0.5 };

            let eta_secs = if speed_bps > 1024.0 && total_size > cur_bytes {
                let s = (total_size - cur_bytes) as f64 / speed_bps;
                format!(" · 剩余 {:.0}s", s)
            } else {
                String::new()
            };

            let msg = if speed_mb > 0.05 {
                format!("正在极速拉取 {}: {:.1}MB / {:.1}MB ({:.1} MB/s{})", display_name_clone, cur_mb, total_mb, speed_mb, eta_secs)
            } else {
                format!("正在极速拉取 {}: {:.1}MB / {:.1}MB", display_name_clone, cur_mb, total_mb)
            };

            let _ = app_handle_clone.emit(
                &progress_event_name,
                MihomoDownloadProgress {
                    stage: "downloading".to_string(),
                    progress: 0.1 + pct * 0.75,
                    message: msg,
                },
            );
        }
    });

    let final_archive_bytes: Vec<u8> = if accept_ranges && total_size > 524_288 {
        // 动态微块抢占式多连接并发下载（512KB 一块，全速并发无长尾阻塞）
        let block_size: u64 = 512 * 1024;
        let num_blocks = ((total_size + block_size - 1) / block_size) as usize;

        let block_queue = Arc::new(Mutex::new((0..num_blocks).collect::<VecDeque<usize>>()));
        let blocks: Arc<Vec<Mutex<Option<Vec<u8>>>>> = Arc::new((0..num_blocks).map(|_| Mutex::new(None)).collect());
        
        // 优选前 2 个最快镜像节点进行并发连接
        let top_mirrors = if ranked_urls.len() >= 2 {
            vec![ranked_urls[0].clone(), ranked_urls[1].clone()]
        } else {
            ranked_urls.clone()
        };

        let num_workers = 6;
        let mut workers = Vec::new();

        for worker_id in 0..num_workers {
            let queue = block_queue.clone();
            let blocks_ref = blocks.clone();
            let client_ref = client.clone();
            let mirrors = top_mirrors.clone();
            let counter = downloaded_bytes.clone();

            workers.push(tokio::spawn(async move {
                let mut retry_count = 0;
                loop {
                    let maybe_idx = {
                        let mut q = queue.lock().await;
                        q.pop_front()
                    };

                    let Some(idx) = maybe_idx else {
                        // 队列已清空，当前 Worker 退出
                        break;
                    };

                    let start = (idx as u64) * block_size;
                    let end = (((idx as u64) + 1) * block_size - 1).min(total_size - 1);
                    let expected_len = (end - start + 1) as usize;
                    let range_header = format!("bytes={start}-{end}");

                    let mirror_url = &mirrors[(worker_id + retry_count) % mirrors.len()];
                    let req = client_ref
                        .get(mirror_url)
                        .header(reqwest::header::RANGE, &range_header)
                        .send();

                    // 单块设置 3.5 秒严格超时，彻底杜绝单块卡死/掉速
                    let fetch_result = tokio::time::timeout(Duration::from_millis(3500), req).await;

                    let mut success = false;
                    if let Ok(Ok(res)) = fetch_result {
                        if res.status().is_success() || res.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                            if let Ok(Ok(body)) = tokio::time::timeout(Duration::from_millis(3500), res.bytes()).await {
                                if body.len() == expected_len {
                                    counter.fetch_add(body.len() as u64, Ordering::Relaxed);
                                    let mut slot = blocks_ref[idx].lock().await;
                                    *slot = Some(body.to_vec());
                                    success = true;
                                    retry_count = 0;
                                }
                            }
                        }
                    }

                    if !success {
                        // 该块在当前节点遇到丢包或慢速，立刻放回队头由其它 Worker 或备用节点接力
                        retry_count += 1;
                        let mut q = queue.lock().await;
                        q.push_front(idx);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }));
        }

        join_all(workers).await;
        cancel_flag.store(true, Ordering::Relaxed);
        let _ = progress_reporter.await;

        let mut full = Vec::with_capacity(total_size as usize);
        for i in 0..num_blocks {
            let mut slot = blocks[i].lock().await;
            if let Some(data) = slot.take() {
                full.extend_from_slice(&data);
            } else {
                return Err(format!("动态分块下载未完全完成 (缺少分块 {i})"));
            }
        }
        full
    } else {
        // 单流流式下载兜底
        let res = client.get(&best_url).send().await.map_err(|e| format!("连接镜像失败: {e}"))?;
        let mut buffer = Vec::with_capacity(total_size as usize);
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| format!("下载中断: {e}"))?;
            downloaded_bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            buffer.extend_from_slice(&bytes);
        }
        cancel_flag.store(true, Ordering::Relaxed);
        let _ = progress_reporter.await;
        buffer
    };

    Ok(final_archive_bytes)
}

#[tauri::command]
pub async fn download_or_update_mihomo_kernel(
    app: AppHandle,
    mirror: Option<String>,
) -> Result<MihomoKernelStatus, String> {
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

    emit_progress("checking", 0.05, "正在检测最新内核版本…");
    let (tag, raw_download_url) = query_latest_release(mirror.as_deref()).await?;

    let bin_dir = get_app_bin_dir(&app)?;
    let (_, binary_filename) = target_asset_keywords()?;
    let target_binary_path = bin_dir.join(binary_filename);
    let temp_extracted_path = bin_dir.join(format!("{binary_filename}.extract.tmp"));

    let final_archive_bytes = download_bytes_with_dynamic_racing(
        &raw_download_url,
        mirror.as_deref(),
        &app,
        "mihomo-kernel-progress",
        &format!("Mihomo {tag}"),
    ).await?;

    emit_progress("extracting", 0.88, "下载完成，正在秒级解压内核…");

    // 解压 .gz 文件
    let is_gz = raw_download_url.ends_with(".gz");
    if is_gz {
        let gz_cursor = Cursor::new(final_archive_bytes);
        let mut decoder = GzDecoder::new(gz_cursor);
        let mut out = File::create(&temp_extracted_path).map_err(|e| format!("创建解压文件失败：{e}"))?;
        copy(&mut decoder, &mut out).map_err(|e| format!("解压 GZ 内核失败：{e}"))?;
    } else {
        let mut out = File::create(&temp_extracted_path).map_err(|e| format!("创建解压文件失败：{e}"))?;
        out.write_all(&final_archive_bytes).map_err(|e| format!("写入内核文件失败：{e}"))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&temp_extracted_path).map_err(|e| e.to_string())?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_extracted_path, perms).map_err(|e| format!("赋予执行权限失败：{e}"))?;
    }

    emit_progress("verifying", 0.96, "正在校验内核有效性…");

    let verified_ver = read_mihomo_version(&temp_extracted_path).ok_or("下载的文件无法作为 Mihomo 内核正常执行")?;

    // 原子替换目标内核文件
    let _ = fs::remove_file(&target_binary_path);
    fs::rename(&temp_extracted_path, &target_binary_path).map_err(|e| format!("替换目标内核失败：{e}"))?;

    emit_progress("completed", 1.0, &format!("Mihomo 内核已成功安装 ({verified_ver})"));

    Ok(MihomoKernelStatus {
        installed: true,
        path: target_binary_path.display().to_string(),
        version: verified_ver,
        is_custom: false,
        latest_version: Some(tag),
    })
}
